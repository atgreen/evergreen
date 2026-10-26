// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Hash tables — Robin Hood hashing with open addressing.
//!
//! Robin Hood hashing: on insert, each entry tracks its probe distance
//! (displacement from its natural slot). When an incoming entry has a
//! longer probe distance than the occupant, they swap — this bounds
//! the variance of probe lengths, giving O(log n) worst-case lookup.
//!
//! See spec §5.7.

use std::collections::HashSet;
use std::sync::Once;
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{LockLevel, OrderedMutex};
use egcl_rt::object::{CompiledFunctionData, ConsCell, ObjectHeader, type_id};
use egcl_rt::value::{TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_SYMBOL, EgclVal};

// ── GC root tracking for hash-table storage (bliss-jtc.8) ────────────────────
//
// A hash table's entries live in a Rust `Vec<Option<RHEntry>>` outside the GC
// heap, so its keys/values are invisible to the collector. We track every live
// table and register a root scanner (egcl_rt::gc) that yields each entry's key
// and value slot, so the collector marks and relocates them like any other root.
static LIVE_TABLES: OrderedMutex<Option<HashSet<usize>>> =
    OrderedMutex::new(LockLevel::GcWorld, 8, "hash-table GC roots", None);
static REGISTER_SCANNER: Once = Once::new();

/// Track a newly-created table and ensure the GC root scanner is registered.
fn register_live_table(ptr: usize) {
    REGISTER_SCANNER.call_once(|| {
        egcl_rt::gc::register_root_scanner(scan_hash_table_roots);
        egcl_rt::gc::register_weak_container_hook(process_weak_table_entries);
    });
    LIVE_TABLES
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .insert(ptr);
}

/// GC external root scanner: yield every live hash table's entry key and value
/// slots so the collector marks + relocates them. Runs while the collector holds
/// the heap lock, so it only reads the table registry and each table's current
/// [`Buckets`] — it never allocates on the GC heap.
fn scan_hash_table_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let guard = LIVE_TABLES.lock().unwrap();
    let Some(tables) = guard.as_ref() else {
        return;
    };
    for &addr in tables {
        // SAFETY: entries in LIVE_TABLES are leaked HashTableInner allocations
        // that live for the process; the world is stopped, so nothing else
        // touches their buckets.
        unsafe {
            let inner = addr as *mut HashTableInner;
            let weakness = (*inner).weakness;
            // A WEAK half is deliberately not visited: yielding it here would
            // make the table keep its own referent alive. The collector calls
            // `process_weak_table_entries` instead, which relocates survivors and
            // drops dead entries (R3.13).
            let key_is_weak = matches!(weakness, Some(Weakness::Key | Weakness::KeyAndValue));
            let value_is_weak = matches!(weakness, Some(Weakness::Value | Weakness::KeyAndValue));
            for slot in buckets_mut(&mut (*inner).buckets).entries.iter_mut() {
                if let Some(entry) = slot.as_mut() {
                    if !key_is_weak {
                        visit(&mut entry.key as *mut EgclVal);
                    }
                    if !value_is_weak {
                        visit(&mut entry.value as *mut EgclVal);
                    }
                }
            }
        }
    }
}

/// Shaking follows entries only when their table is semantically reached,
/// using `hash_table_entries`. The GC scanner includes package membership and
/// orphaned tables, which must not root every function during tree shaking.
pub fn shake_root_scanner() -> egcl_rt::gc::RootScanner {
    scan_hash_table_roots
}

/// Collector callback for weak tables (R3.13): relocate each weak referent that
/// survived and drop every entry whose weak referent died.
///
/// `resolve` relocates the slot it is given and answers whether that object is
/// still live. Runs under the collector's heap lock, so it touches only the table
/// registry and the tables' buckets and performs no EGCL allocation. Removing
/// entries changes bucket occupancy, so a table that lost any entry gets freshly
/// placed buckets, as `placed_buckets` would build.
///
/// Every collection runs this with the world stopped, so it is also where the
/// buckets retired since the previous collection are freed.
fn process_weak_table_entries(resolve: &dyn Fn(*mut EgclVal) -> bool) {
    let guard = LIVE_TABLES.lock().unwrap();
    let Some(tables) = guard.as_ref() else {
        return;
    };
    for &addr in tables {
        // SAFETY: as for `scan_hash_table_roots`.
        unsafe {
            let inner = addr as *mut HashTableInner;
            let Some(weakness) = (*inner).weakness else {
                continue;
            };
            let mut removed = 0usize;
            for slot in buckets_mut(&mut (*inner).buckets).entries.iter_mut() {
                let Some(entry) = slot.as_mut() else {
                    continue;
                };
                // Ask only about the weak half/halves; the strong half was
                // already visited (and so relocated) by the root scanner.
                let key_live = match weakness {
                    Weakness::Key | Weakness::KeyAndValue => {
                        resolve(&mut entry.key as *mut EgclVal)
                    }
                    Weakness::Value => true,
                };
                let value_live = match weakness {
                    Weakness::Value | Weakness::KeyAndValue => {
                        resolve(&mut entry.value as *mut EgclVal)
                    }
                    Weakness::Key => true,
                };
                let keep = match weakness {
                    Weakness::Key => key_live,
                    Weakness::Value => value_live,
                    // CLHS has no weakness; this follows Trivial-Garbage:
                    // :key-and-value keeps the entry only while BOTH live.
                    Weakness::KeyAndValue => key_live && value_live,
                };
                if !keep {
                    *slot = None;
                    removed += 1;
                }
            }
            if removed > 0 {
                (*inner).count -= removed;
                let rebuilt = build_buckets((*inner).buckets(), (*inner).capacity(), (*inner).test);
                (*inner).publish(rebuilt);
                (*inner)
                    .placement
                    .store(placed_at(egcl_rt::gc::gc_move_epoch()), Ordering::Release);
            }
        }
    }
    drop(guard);
    free_retired_buckets();
}

/// Hash table test function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashTest {
    Eq,
    Eql,
    Equal,
    Equalp,
}

/// Options for MAKE-HASH-TABLE.
pub struct MakeHashTableOptions {
    pub test: HashTest,
    pub size: usize,
    pub rehash_size: f64,
    pub rehash_threshold: f64,
    pub synchronized: bool,
    pub weakness: Option<Weakness>,
}

/// Weakness mode for weak hash tables.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weakness {
    Key,
    Value,
    KeyAndValue,
}

impl Default for MakeHashTableOptions {
    fn default() -> Self {
        MakeHashTableOptions {
            test: HashTest::Eql,
            size: 16,
            rehash_size: 2.0,
            rehash_threshold: 0.75,
            synchronized: false,
            weakness: None,
        }
    }
}

// ── Internal representation ───────────────────────────────────────

/// An entry in the Robin Hood table, tracking its probe distance.
#[derive(Clone, Copy)]
struct RHEntry {
    key: EgclVal,
    value: EgclVal,
    /// Distance from the entry's natural (home) slot.
    probe_dist: usize,
}

/// The internal data structure for a hash table, stored on the heap.
#[repr(C)]
struct HashTableInner {
    header: ObjectHeader,
    test: HashTest,
    /// The current [`Buckets`]: replaced, never edited, while readers may share it.
    buckets: AtomicPtr<Buckets>,
    count: usize,
    rehash_size: f64,
    rehash_threshold: f64,
    synchronized: bool,
    weakness: Option<Weakness>,
    /// True once a key has been inserted whose hash falls through to an
    /// object's ADDRESS (a movable, non-pinned heap object under the table's
    /// test — e.g. a CLOS instance under EQ/EQL/EQUAL). The moving GC relocates
    /// such a key and rewrites the stored pointer, but the key then sits in the
    /// bucket for its OLD address; a fresh `gethash` probes the NEW-address
    /// bucket and misses a live key. Tables that never hold such a key
    /// (fixnum/char/string/symbol/cons-of-stable keys) stay `false` and are
    /// never rehashed for GC (bliss-jtc.22 / bliss-cpje).
    address_sensitive: bool,
    /// `placed_at(epoch)` for the `gc_move_epoch()` at which the buckets'
    /// positions last matched their keys' hashes, with [`REHASHING`] set while a
    /// reader rebuilds them. When the epoch is stale and the table is
    /// `address_sensitive`, the next access rebuilds first (`placed_buckets`).
    placement: AtomicU64,
    /// Taken by every operation on a `:synchronized t` table (R5.33).
    lock: TableLock,
}

/// The per-table lock of a synchronized hash table (R5.33).
///
/// Concurrent readers are safe on any table (see [`Buckets`]), but a writer
/// updates the buckets in place, so a table that threads both read and write --
/// such as the tree-walker's shared macro-expansion cache -- needs this lock.
///
/// Reentrant, because EQUALP hashes and compares hash-table keys by reading
/// them while the outer table is held. A holder never allocates or reaches a
/// safepoint (hashing and `crate::equality` allocate no Lisp objects), so a
/// waiter spins for one short operation and a stopped world never leaves the
/// lock held.
struct TableLock {
    /// [`lock_owner_token`] of the holding thread, or 0 when free.
    owner: AtomicUsize,
    /// Nesting depth; only the owner touches it.
    depth: AtomicU32,
}

impl TableLock {
    const fn new() -> Self {
        Self {
            owner: AtomicUsize::new(0),
            depth: AtomicU32::new(0),
        }
    }
}

/// A nonzero token unique to each live thread: the address of a thread-local.
fn lock_owner_token() -> usize {
    thread_local! {
        static TOKEN: u8 = const { 0 };
    }
    TOKEN.with(|token| token as *const u8 as usize)
}

/// Holds a synchronized table's lock until dropped; a no-op otherwise.
struct TableGuard(*const TableLock);

impl Drop for TableGuard {
    fn drop(&mut self) {
        if self.0.is_null() {
            return;
        }
        // Safety: the lock lives in a table that is never freed while in use.
        let lock = unsafe { &*self.0 };
        if lock.depth.fetch_sub(1, Ordering::Relaxed) == 1 {
            lock.owner.store(0, Ordering::Release);
        }
    }
}

/// Take `ptr`'s lock if the table is synchronized. Every access to its entries,
/// even a read, must happen while the returned guard is alive.
fn lock_table(ptr: *mut HashTableInner) -> TableGuard {
    // Safety: `ptr` came from `get_table_inner`; `synchronized` never changes
    // and the lock is only reached through atomics.
    if !unsafe { (*ptr).synchronized } {
        return TableGuard(std::ptr::null());
    }
    let lock = unsafe { &*std::ptr::addr_of!((*ptr).lock) };
    let me = lock_owner_token();
    if lock.owner.load(Ordering::Relaxed) != me {
        let mut spins = 0u32;
        while lock
            .owner
            .compare_exchange_weak(0, me, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spins += 1;
            if spins < 64 {
                std::hint::spin_loop();
            } else {
                std::thread::yield_now();
            }
        }
    }
    lock.depth.fetch_add(1, Ordering::Relaxed);
    TableGuard(lock)
}

/// One generation of a table's slots, with the miss filter that summarizes them.
///
/// Like SBCL's, an unsynchronized table may be read by several threads at once
/// (only a write needs exclusive access), even though a read can have to rebuild
/// the table: after a collection relocates an address-hashed key, the key sits in
/// the bucket for its old address. The first reader to notice claims the rebuild
/// (`placed_buckets`), places the entries into fresh `Buckets` and publishes
/// them. It never edits the buckets it replaces, which other readers may still
/// be probing; it retires them, and the next collection frees them. A probe
/// allocates nothing and so reaches no safepoint, which means none can still be
/// running when that collection stops the world. Only a writer, which has the
/// table to itself, updates buckets in place.
struct Buckets {
    entries: Vec<Option<RHEntry>>,
    /// A clear bit proves absence without reading an entry. Eight bit positions
    /// per bucket keep false positives uncommon at normal load. Deletion leaves
    /// bits set; clearing or rebuilding removes those harmless false positives.
    miss_filter: Vec<u64>,
    /// The next entry of [`RETIRED_BUCKETS`], once these are retired.
    next_retired: *mut Buckets,
}

impl Buckets {
    fn empty(capacity: usize) -> Box<Self> {
        Box::new(Buckets {
            entries: vec![None; capacity],
            miss_filter: empty_miss_filter(capacity),
            next_retired: std::ptr::null_mut(),
        })
    }

    fn capacity(&self) -> usize {
        self.entries.len()
    }
}

/// A table's buckets, for a writer, which has the table to itself. Borrowing
/// only the field leaves the table's other fields free to update.
fn buckets_mut(buckets: &mut AtomicPtr<Buckets>) -> &mut Buckets {
    // Safety: a table always holds published buckets, which stay allocated
    // while they are current.
    unsafe { &mut **buckets.get_mut() }
}

impl HashTableInner {
    /// The current buckets, which other readers may be probing too.
    fn buckets(&self) -> &Buckets {
        // Safety: buckets stay allocated until the collection after they are
        // replaced, and no table operation can span a collection.
        unsafe { &*self.buckets.load(Ordering::Acquire) }
    }

    fn capacity(&self) -> usize {
        self.buckets().capacity()
    }

    /// Make `new` current and retire the buckets it replaces.
    fn publish(&self, new: Box<Buckets>) {
        retire_buckets(self.buckets.swap(Box::into_raw(new), Ordering::AcqRel));
    }
}

/// Buckets replaced since the last collection. Any thread may push; only a
/// collection, with the world stopped, drains them.
static RETIRED_BUCKETS: AtomicPtr<Buckets> = AtomicPtr::new(std::ptr::null_mut());

fn retire_buckets(old: *mut Buckets) {
    let mut head = RETIRED_BUCKETS.load(Ordering::Relaxed);
    loop {
        // Safety: `old` is no longer published, so only this thread links it.
        unsafe { (*old).next_retired = head };
        match RETIRED_BUCKETS.compare_exchange_weak(head, old, Ordering::Release, Ordering::Relaxed)
        {
            Ok(_) => return,
            Err(current) => head = current,
        }
    }
}

/// Free every retired [`Buckets`]. Only while the world is stopped.
fn free_retired_buckets() {
    let mut next = RETIRED_BUCKETS.swap(std::ptr::null_mut(), Ordering::Acquire);
    while !next.is_null() {
        // Safety: retired buckets are unreachable from every table, and no
        // reader can still hold them once the world has stopped.
        let retired = unsafe { Box::from_raw(next) };
        next = retired.next_retired;
    }
}

/// Set in [`HashTableInner::placement`] while a reader rebuilds the buckets.
const REHASHING: u64 = 1;

/// The placement of buckets positioned by their keys' hashes as of `epoch`.
fn placed_at(epoch: u64) -> u64 {
    epoch << 1
}

// Small tables already fit in cache; avoid the extra filter lookup there.
const MISS_FILTER_MIN_CAPACITY: usize = 4096;

fn empty_miss_filter(capacity: usize) -> Vec<u64> {
    if capacity < MISS_FILTER_MIN_CAPACITY {
        Vec::new()
    } else {
        vec![0; capacity / 8]
    }
}

fn record_key_hash(filter: &mut [u64], hash: u64) {
    if !filter.is_empty() {
        let bit = hash as usize & (filter.len() * 64 - 1);
        filter[bit / 64] |= 1 << (bit % 64);
    }
}

fn hash_may_be_present(filter: &[u64], hash: u64) -> bool {
    let bit = hash as usize & (filter.len() * 64 - 1);
    filter[bit / 64] & (1 << (bit % 64)) != 0
}

/// Round up to the next power of two. If already a power of two, returns it.
fn next_power_of_two(n: usize) -> usize {
    if n == 0 {
        return 1;
    }
    n.next_power_of_two()
}

/// FNV-1a-inspired hash of a u64 value.
fn hash_u64(val: u64) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    // Mix all 8 bytes
    for i in 0..8 {
        let byte = ((val >> (i * 8)) & 0xFF) as u8;
        h ^= byte as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// Extract a raw pointer to HashTableInner from a EgclVal, validating it is a hash table.
///
/// Returns a raw pointer to avoid creating multiple `&mut` references (UB).
/// Callers use unsafe raw-pointer operations to access the inner data.
fn get_table_inner(table: EgclVal) -> Result<*mut HashTableInner, EgclError> {
    if table.tag() != TAG_HEAP_OBJECT {
        return Err(EgclError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    let ptr = unsafe { table.as_ptr() } as *mut HashTableInner;
    if ptr.is_null() {
        return Err(EgclError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    // Safety: ptr is non-null and was created from a valid heap allocation
    if unsafe { (*ptr).header.type_id() } != type_id::HASH_TABLE {
        return Err(EgclError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    Ok(ptr)
}

/// Return whether `value` is a live hash table (CL `HASH-TABLE-P`).
///
/// Consult the allocation registry rather than dereferencing every heap-tagged
/// value: some bootstrap objects use heap-tagged sentinels that are not valid
/// pointers, while every table created by `make_hash_table` is registered here.
pub fn hash_table_p(value: EgclVal) -> bool {
    if value.tag() != TAG_HEAP_OBJECT {
        return false;
    }
    let addr = unsafe { value.as_ptr() } as usize;
    LIVE_TABLES
        .lock()
        .unwrap()
        .as_ref()
        .is_some_and(|tables| tables.contains(&addr))
}

/// Compute the probe index for a key in a table of the given capacity.
fn probe_index(key_bits: u64, capacity: usize) -> usize {
    (key_bits as usize) & (capacity - 1)
}

/// Extract the bytes of a string value for hashing/equality, or `None` for a
/// non-string. Handles BOTH real heap strings (SIMPLE-BASE-STRING /
/// SIMPLE-CHARACTER-STRING) and registry-backed string sentinels. A sentinel is
/// heap-tagged but its bits are a string hash, NOT a real pointer, so it must be
/// resolved through the string registry and never dereferenced — otherwise
/// EQUAL/EQUALP hashing segfaults on e.g. a pathname namestring key
/// (bliss-lb6.14).
///
/// Real string layout: [ObjectHeader (8)] [length: u64 (8)] [bytes...].
fn extract_string_bytes(v: EgclVal) -> Option<Vec<u8>> {
    if !v.is_heap_object() {
        return None;
    }
    // Sentinel: resolve content via the registry, without dereferencing.
    if let Some(s) = crate::pathnames::registered_string(v) {
        return Some(s.into_bytes());
    }
    // A registry lookup miss means this is a genuine heap object with a real
    // pointer, so reading its header (and, if a string, its payload) is safe.
    unsafe {
        let ptr = v.as_ptr();
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        if tid != type_id::SIMPLE_BASE_STRING && tid != type_id::SIMPLE_CHARACTER_STRING {
            return None;
        }
        // The UTF-8 CONTENT bytes, not the raw fixed-width storage: string
        // EQUAL/hashing is by character, so a base string and a character string
        // with the same characters compare and hash alike.
        Some(egcl_rt::object::read_simple_string(ptr).into_bytes())
    }
}

/// Check if two keys are equal according to the given hash test.
///
/// EQL, EQUAL and EQUALP are the shared definitions in [`crate::equality`],
/// the same ones the evaluator's predicates use, so a table finds exactly the
/// keys its test function calls equal. The table used to carry its own copy,
/// which had drifted: an EQUALP table missed `1d0` for a key of `1`.
fn keys_equal(a: EgclVal, b: EgclVal, test: HashTest) -> bool {
    #[cfg(test)]
    tests::KEY_COMPARISONS.with(|count| count.set(count.get() + 1));
    match test {
        HashTest::Eq => a.0 == b.0,
        HashTest::Eql => crate::equality::eql(a, b),
        HashTest::Equal => crate::equality::equal(a, b),
        HashTest::Equalp => crate::equality::equalp(a, b),
    }
}

const STRUCTURAL_HASH_DEPTH_LIMIT: usize = 4;
const MOST_POSITIVE_FIXNUM_MASK: u64 = 0x0FFF_FFFF_FFFF_FFFF;

fn hash_bytes(bytes: &[u8], case_fold: bool) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for &byte in bytes {
        let mixed = if case_fold {
            byte.to_ascii_lowercase()
        } else {
            byte
        };
        h ^= mixed as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

/// [`hash_bytes`] of a simple string's UTF-8 encoding, computed in place.
///
/// # Safety
/// `ptr` must be a live simple-string object.
unsafe fn simple_string_hash(ptr: *const u8) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    let mut buf = [0u8; 4];
    for c in unsafe { egcl_rt::object::simple_string_chars(ptr) } {
        for &byte in c.encode_utf8(&mut buf).as_bytes() {
            h ^= byte as u64;
            h = h.wrapping_mul(FNV_PRIME);
        }
    }
    h
}

fn combine_hashes(a: u64, b: u64) -> u64 {
    a.rotate_left(13) ^ b.rotate_right(7) ^ 0x9e37_79b9_7f4a_7c15
}

/// Value-based hash for a heap numeric (bignum / ratio / complex / double-float)
/// so two distinct-but-numerically-equal objects hash equal — otherwise a hash
/// table keyed on such a value never finds its entry, because the fall-through
/// hashed the object's ADDRESS (`object.0`) rather than its value. Returns `None`
/// for anything that is not a heap numeric.
fn numeric_value_hash(object: EgclVal, depth: usize) -> Option<u64> {
    if depth == 0 || !object.is_heap_object() {
        return None;
    }
    let tid = unsafe { (*(object.as_ptr() as *const ObjectHeader)).type_id() };
    unsafe {
        let p = object.as_ptr();
        match tid {
            type_id::BIGNUM => {
                let sign = *(p.add(8) as *const i32);
                let n = *(p.add(12) as *const u32) as usize;
                let mut h = hash_u64(sign as u64);
                for i in 0..n {
                    h = combine_hashes(h, hash_u64(*(p.add(16 + i * 8) as *const u64)));
                }
                Some(h)
            }
            type_id::RATIO | type_id::COMPLEX => {
                // RatioData / ComplexData both hold two EgclVal parts at +8/+16.
                let a = *(p.add(8) as *const EgclVal);
                let b = *(p.add(16) as *const EgclVal);
                Some(combine_hashes(
                    numeric_or_scalar_hash(a, depth - 1),
                    numeric_or_scalar_hash(b, depth - 1),
                ))
            }
            type_id::DOUBLE_FLOAT => {
                // Canonicalize signed zeros: +0.0 and -0.0 are similar under
                // ANSI SXHASH and must hash alike (sxhash.17-19).
                let f = *(p.add(8) as *const f64);
                Some(hash_u64(if f == 0.0 {
                    FLOAT_ZERO_HASH
                } else {
                    f.to_bits()
                }))
            }
            _ => None,
        }
    }
}

/// Canonical hash input for a floating zero of any format, so `+0.0` and `-0.0`
/// (similar per ANSI) hash identically.
const FLOAT_ZERO_HASH: u64 = 0x7a7a_5a5a_0f0f_a5a5;

/// Hash a scalar leaf by value, canonicalizing signed float zeros. A non-zero
/// single-float keeps its tagged-bit hash; everything else hashes by raw bits.
fn scalar_leaf_hash(v: EgclVal) -> u64 {
    if v.is_single_float() {
        let f = v.as_single_float();
        if f == 0.0 {
            return hash_u64(FLOAT_ZERO_HASH);
        }
    }
    hash_u64(v.0)
}

/// Hash a numeric part by value: a heap numeric via [`numeric_value_hash`], an
/// immediate (fixnum / single-float) by its bits.
fn numeric_or_scalar_hash(object: EgclVal, depth: usize) -> u64 {
    numeric_value_hash(object, depth).unwrap_or_else(|| scalar_leaf_hash(object))
}

/// Hash the active bits of a bit vector, simple or fill-pointered, so the two
/// hash alike when EQUAL says they are (ANSI SXHASH similarity,
/// sxhash.4/.6/.22). `None` if `object` is not a bit vector.
fn bit_vector_content_hash(object: EgclVal) -> Option<u64> {
    let n = crate::equality::bit_vector_length(object)?;
    let mut h = hash_u64(n as u64);
    for i in 0..n {
        h = combine_hashes(
            h,
            hash_u64(u64::from(crate::equality::bit_vector_bit(object, i))),
        );
    }
    Some(h)
}

/// EQUALP hash of a float's value; both signed zeros hash like integer 0.
fn equalp_float_hash(f: f64) -> u64 {
    hash_u64(if f == 0.0 { 0 } else { f.to_bits() })
}

/// Inspect an integer's sign and little-endian magnitude without copying it.
/// The callback must not allocate Lisp objects or reach a GC safepoint: the
/// borrowed bignum limbs point into the moving heap. Fixnums use stack storage.
fn with_integer_magnitude<R>(v: EgclVal, inspect: impl FnOnce(i8, &[u64]) -> R) -> R {
    if v.is_fixnum() {
        let n = v.as_fixnum();
        let magnitude = [n.unsigned_abs()];
        return inspect(n.signum() as i8, if n == 0 { &[] } else { &magnitude });
    }
    // SAFETY: callers supply integer values from a number or a normalized
    // ratio. The callback neither allocates Lisp objects nor retains the slice.
    unsafe {
        let p = v.as_ptr();
        debug_assert_eq!((*(p as *const ObjectHeader)).type_id(), type_id::BIGNUM);
        let sign = *(p.add(8) as *const i32);
        let n = *(p.add(12) as *const u32) as usize;
        inspect(
            sign.signum() as i8,
            std::slice::from_raw_parts(p.add(16) as *const u64, n),
        )
    }
}

fn integer_magnitude_hash(sign: i8, limbs: &[u64]) -> u64 {
    limbs.iter().fold(hash_u64(sign as u64), |h, &limb| {
        combine_hashes(h, hash_u64(limb))
    })
}

/// Exact binary64 encoding of sign * magnitude / 2^denominator_shift.
/// Reject excess precision, overflow and subnormal rounding before extracting
/// the (at most 53-bit) odd significand. No floating arithmetic is needed.
fn exact_dyadic_float_bits(sign: i8, limbs: &[u64], denominator_shift: usize) -> Option<u64> {
    let Some(&top) = limbs.last() else {
        return Some(0);
    };
    let bit_length = 64 * (limbs.len() - 1) + (64 - top.leading_zeros() as usize);
    let first = limbs.iter().position(|&limb| limb != 0)?;
    let trailing = 64 * first + limbs[first].trailing_zeros() as usize;
    let precision = bit_length - trailing;
    let exponent = bit_length as i128 - 1 - denominator_shift as i128;
    let scale = trailing as i128 - denominator_shift as i128;
    if precision > 53 || exponent > 1023 || scale < -1074 {
        return None;
    }
    let offset = trailing % 64;
    let mut significand = limbs[first] >> offset;
    if offset != 0 {
        significand |= limbs.get(first + 1).copied().unwrap_or(0) << (64 - offset);
    }
    let sign_bit = u64::from(sign < 0) << 63;
    Some(
        sign_bit
            | if exponent >= -1022 {
                (((exponent + 1023) as u64) << 52)
                    | ((significand << (53 - precision)) & ((1_u64 << 52) - 1))
            } else {
                significand << ((scale + 1074) as usize)
            },
    )
}

/// EQUALP hashes floats by their binary64 value, and a rational by that same
/// value only if exactly representable. Other normalized rationals hash their
/// existing numerator/denominator limbs, without conversion, GCD or products.
fn equalp_real_hash(v: EgclVal) -> u64 {
    if v.is_fixnum() {
        let n = v.as_fixnum();
        let f = n as f64;
        if f as i128 == i128::from(n) {
            return equalp_float_hash(f);
        }
    }
    if let Some(f) = crate::equality::float_value(v) {
        return equalp_float_hash(f);
    }
    let (num, den) = egcl_rt::bignum::ratio_parts_val(v).unwrap_or((v, EgclVal::from_fixnum(1)));
    with_integer_magnitude(den, |den_sign, den_limbs| {
        let denominator_shift = if den_sign > 0 {
            den_limbs.iter().position(|&limb| limb != 0).and_then(|i| {
                let limb = den_limbs[i];
                (limb.is_power_of_two() && i + 1 == den_limbs.len())
                    .then(|| i * 64 + limb.trailing_zeros() as usize)
            })
        } else {
            None
        };
        with_integer_magnitude(num, |sign, limbs| {
            if let Some(bits) =
                denominator_shift.and_then(|shift| exact_dyadic_float_bits(sign, limbs, shift))
            {
                return equalp_float_hash(f64::from_bits(bits));
            }
            combine_hashes(
                integer_magnitude_hash(sign, limbs),
                integer_magnitude_hash(den_sign, den_limbs),
            )
        })
    })
}

/// EQUALP hash of any number. A complex with a zero imaginary part is `=` to
/// its real part, so it hashes as that real.
fn equalp_number_hash(v: EgclVal) -> Option<u64> {
    if !crate::equality::numberp(v) {
        return None;
    }
    let (re, im) = crate::equality::number_parts(v);
    let h = equalp_real_hash(re);
    Some(
        if crate::numbers::zerop(im).expect("numeric imaginary part") {
            h
        } else {
            combine_hashes(h, equalp_real_hash(im))
        },
    )
}

/// EQUALP hash of a character, case-folded the way EQUALP compares.
fn equalp_char_hash(c: char) -> u64 {
    hash_u64(crate::characters::char_upcase(c) as u64)
}

/// Seed for [`KeyHasher::identity`]'s per-type SXHASH value.
const STABLE_IDENTITY_HASH: u64 = 0x51a6_1e1d_e47e_4a5b;

/// Computes a key's hash under a table test, and records whether that hash
/// depended on the ADDRESS of an object the GC can move (bliss-jtc.22 /
/// bliss-cpje). Deriving that flag from the very hash that consulted the
/// address keeps the two in step; it used to be a separate predicate that had
/// to mirror every fall-through by hand.
struct KeyHasher {
    /// SXHASH: hash an identity-compared object by its type, never its
    /// address. SXHASH must not change while the object is unmodified, and a
    /// moving GC changes addresses.
    stable: bool,
    address_sensitive: bool,
}

impl KeyHasher {
    fn new(stable: bool) -> Self {
        KeyHasher {
            stable,
            address_sensitive: false,
        }
    }

    /// Hash an object that is equal only to itself.
    fn identity(&mut self, object: EgclVal) -> u64 {
        if is_movable_pointer(object) {
            if self.stable {
                let kind = egcl_rt::gc::heap_object_type_id(object).map_or(object.tag(), u64::from);
                return hash_u64(STABLE_IDENTITY_HASH ^ kind);
            }
            self.address_sensitive = true;
        }
        hash_u64(object.0)
    }

    fn eql(&mut self, object: EgclVal) -> u64 {
        // EQL of two distinct heap numerics is T by value, so they hash by value.
        numeric_value_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT)
            .unwrap_or_else(|| self.identity(object))
    }

    fn equal(&mut self, object: EgclVal, depth: usize) -> u64 {
        if depth == 0 {
            return 0;
        }
        // Signed float zeros are similar per ANSI: hash +0.0 and -0.0 alike.
        if object.is_single_float() && object.as_single_float() == 0.0 {
            return hash_u64(FLOAT_ZERO_HASH);
        }
        if let Some(h) = numeric_value_hash(object, depth) {
            return h;
        }
        if object.tag() == TAG_CONS {
            // Safety: a TAG_CONS value points at a ConsCell.
            let cell = unsafe { &*(object.as_ptr() as *const ConsCell) };
            return combine_hashes(
                self.equal(cell.car, depth - 1),
                self.equal(cell.cdr, depth - 1),
            );
        }
        // A symbol hashes by its NAME so two symbols with the same name (in any
        // package, interned or uninterned) hash alike (ANSI; sxhash.13/.15/.23).
        // Only real TAG_SYMBOL values carry a symbol index — NIL/T are represented
        // specially and hash by their (stable) bits.
        if object.tag() == TAG_SYMBOL {
            if let Some(name) = egcl_rt::symbols::symbol_name(object.as_symbol_index()) {
                return hash_bytes(name.as_bytes(), false);
            }
        }
        // A simple string, the usual EQUAL key: hash its characters in place,
        // exactly as the string branch below would after a pathname-registry
        // lookup and decoding it into a fresh `String` (bliss-a2bso).
        if let Some(ptr) = crate::equality::simple_string_ptr(object) {
            // Safety: a live simple string, and hashing allocates nothing.
            return unsafe { simple_string_hash(ptr) };
        }
        if object.is_heap_object() {
            // A pathname hashes by the SAME namestring `pathnames_equal` compares,
            // or EQUAL and SXHASH disagree and every EQUAL hash table keyed by a
            // pathname misses (bliss-kssh). Checked before the string branch so a
            // pathname hashes identically whether it reaches here as a PATHNAME heap
            // object or as a registry-backed namestring sentinel.
            if let Some(h) = crate::pathnames::with_pathname_equal_key(object, |key| {
                hash_bytes(key.as_bytes(), false)
            }) {
                return h;
            }
            // Any string (simple or complex/fill-pointer/displaced) — and a
            // registry-backed string sentinel such as a pathname namestring, which
            // `extract_string_bytes` resolves without dereferencing (sxhash.20) —
            // hashes by its characters.
            if let Some(bytes) = extract_string_bytes(object)
                .or_else(|| crate::sequences::string_content_bytes(object))
            {
                return hash_bytes(&bytes, false);
            }
            if let Some(h) = bit_vector_content_hash(object) {
                return h;
            }
        }
        // Every other array -- simple or not -- is EQUAL only to itself, so it
        // hashes by identity. Hashing a fill-pointer vector's ELEMENTS made the
        // key unfindable once one was changed (and changed its SXHASH, which
        // must not change while EQUAL could not see a difference).
        self.identity(object)
    }

    fn equalp(&mut self, object: EgclVal, depth: usize) -> u64 {
        if depth == 0 {
            return 0;
        }
        if let Some(h) = equalp_number_hash(object) {
            return h;
        }
        if object.is_character() {
            return equalp_char_hash(object.as_char());
        }
        if object.tag() == TAG_CONS {
            // Safety: a TAG_CONS value points at a ConsCell.
            let cell = unsafe { &*(object.as_ptr() as *const ConsCell) };
            return combine_hashes(
                self.equalp(cell.car, depth - 1),
                self.equalp(cell.cdr, depth - 1),
            );
        }
        if object.is_heap_object() {
            // Pathnames are EQUALP exactly when they are EQUAL (components match),
            // so they must hash alike here too. Case-folded like every other EQUALP
            // string hash.
            if let Some(h) = crate::pathnames::with_pathname_equal_key(object, |key| {
                hash_bytes(key.as_bytes(), true)
            }) {
                return h;
            }
            // A string is EQUALP to a general vector holding the same characters,
            // so both hash as a length followed by per-element hashes.
            if let Some(text) = crate::equality::string_text(object) {
                let mut h = hash_u64(text.chars().count() as u64);
                for c in text.chars() {
                    h = combine_hashes(h, if depth == 1 { 0 } else { equalp_char_hash(c) });
                }
                return h;
            }
            if crate::equality::vectorp(object) {
                let n = crate::sequences::length(object).unwrap_or(0);
                let mut h = hash_u64(n as u64);
                for i in 0..n {
                    let element = if depth == 1 {
                        0
                    } else if let Some(raw) =
                        crate::sequences::double_float_array_element(object, i)
                    {
                        // Read unboxed: boxing a double would allocate.
                        equalp_float_hash(raw)
                    } else {
                        crate::sequences::elt(object, i).map_or(0, |e| self.equalp(e, depth - 1))
                    };
                    h = combine_hashes(h, element);
                }
                return h;
            }
            if let (Some(dims), Some(storage)) = (
                egcl_rt::types::md_array_dims(object),
                egcl_rt::types::md_array_storage(object),
            ) {
                return combine_hashes(
                    self.equalp(dims, depth - 1),
                    self.equalp(storage, depth - 1),
                );
            }
            // A structure is EQUALP to another of its type with EQUALP slots.
            if let Some(class) = crate::equality::structure_class(object) {
                let mut h = hash_u64(crate::clos::class_name(class).0);
                for slot in crate::clos::effective_slots(class) {
                    let slot_hash = crate::clos::slot_value(object, slot)
                        .map_or(0, |value| self.equalp(value, depth - 1));
                    h = combine_hashes(h, slot_hash);
                }
                return h;
            }
            // Hash tables are EQUALP by test, count and values; the first two
            // are enough to agree with that.
            if hash_table_p(object) {
                let test = hash_table_test(object).map_or(0, test_to_u8);
                let count = hash_table_count(object).unwrap_or(0);
                return combine_hashes(hash_u64(u64::from(test)), hash_u64(count as u64));
            }
        }
        self.identity(object)
    }
}

/// A key's hash under `test`, and whether it depends on a movable address.
fn hash_key(object: EgclVal, test: HashTest) -> (u64, bool) {
    let mut hasher = KeyHasher::new(false);
    let hash = match test {
        HashTest::Eq => hasher.identity(object),
        HashTest::Eql => hasher.eql(object),
        HashTest::Equal => hasher.equal(object, STRUCTURAL_HASH_DEPTH_LIMIT),
        HashTest::Equalp => hasher.equalp(object, STRUCTURAL_HASH_DEPTH_LIMIT),
    };
    (hash, hasher.address_sensitive)
}

fn hash_for_test(object: EgclVal, test: HashTest) -> u64 {
    hash_key(object, test).0
}

// ── Moving-GC hash stability (bliss-jtc.22 / bliss-cpje) ──────────────
//
// A key hashed by identity hashes by its ADDRESS when it is a movable heap
// object (a CLOS instance, a function, a cons under EQ, …). The moving GC
// relocates such objects and rewrites the pointer stored in the table's entry,
// but never recomputes the entry's bucket, so a later probe (which hashes the
// NEW address) looks in the wrong bucket and misses a live key. `hash_key`
// reports which keys those are, and a table holding one gets freshly placed
// buckets the first time it is touched in a new GC generation (see [`Buckets`]
// for how concurrent readers share that rebuild). Symbols and their name
// strings are pinned and immortal, so symbol/string keys are stable and never
// trigger a rehash.

/// True if `object` is a heap pointer the GC may relocate: a cons (never
/// pinned), or a `TAG_HEAP_OBJECT` whose header is not pinned. Immediates
/// (fixnum/char/nil/t/single-float) are stable.
fn is_movable_pointer(object: EgclVal) -> bool {
    if object.tag() == TAG_CONS {
        return true;
    }
    if object.is_heap_object() {
        // Safety: a TAG_HEAP_OBJECT value points at an ObjectHeader.
        return unsafe { !(*(object.as_ptr() as *const ObjectHeader)).is_pinned() };
    }
    false
}

/// Place `old`'s entries into fresh buckets of `capacity`, by their keys'
/// *current* hashes. Pure reordering of existing `EgclVal`s — no EGCL
/// allocation — so it is GC-safe (no collection can fire during it).
fn build_buckets(old: &Buckets, capacity: usize, test: HashTest) -> Box<Buckets> {
    let mut new = Buckets::empty(capacity);
    for e in old.entries.iter().flatten() {
        let hash = hash_for_test(e.key, test);
        record_key_hash(&mut new.miss_filter, hash);
        let mut idx = probe_index(hash, capacity);
        let mut incoming = RHEntry {
            key: e.key,
            value: e.value,
            probe_dist: 0,
        };
        loop {
            match &new.entries[idx] {
                None => {
                    new.entries[idx] = Some(incoming);
                    break;
                }
                Some(occupant) => {
                    // Robin Hood: swap if incoming has traveled farther
                    if incoming.probe_dist > occupant.probe_dist {
                        let displaced = *occupant;
                        new.entries[idx] = Some(incoming);
                        incoming = displaced;
                    }
                }
            }
            incoming.probe_dist += 1;
            idx = (idx + 1) & (capacity - 1);
        }
    }
    new
}

/// What a probe should search.
enum Lookup<'a> {
    /// Buckets positioned by their keys' current hashes: probe by hash.
    Placed(&'a Buckets),
    /// Another reader is rebuilding the placement. These buckets hold every
    /// entry but in stale positions, so search them linearly.
    Unplaced(&'a Buckets),
}

/// The buckets to probe. If this table hashes any key by a movable address and
/// the GC has run since its placement was valid, the first caller rebuilds the
/// buckets by the current hashes (bliss-jtc.22 / bliss-cpje); concurrent
/// readers meanwhile search the old ones linearly, much as SBCL's do. Must be
/// called by every access that probes by hash.
fn placed_buckets(inner: &HashTableInner) -> Lookup<'_> {
    if !inner.address_sensitive {
        return Lookup::Placed(inner.buckets());
    }
    let current = placed_at(egcl_rt::gc::gc_move_epoch());
    loop {
        let state = inner.placement.load(Ordering::Acquire);
        if state == current {
            return Lookup::Placed(inner.buckets());
        }
        if state & REHASHING != 0 {
            return Lookup::Unplaced(inner.buckets());
        }
        if inner
            .placement
            .compare_exchange(
                state,
                state | REHASHING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_ok()
        {
            let old = inner.buckets();
            inner.publish(build_buckets(old, old.capacity(), inner.test));
            inner.placement.store(current, Ordering::Release);
            return Lookup::Placed(inner.buckets());
        }
    }
}

/// Find `key` without relying on slot positions.
fn search_unplaced(buckets: &Buckets, key: EgclVal, test: HashTest) -> Option<EgclVal> {
    buckets
        .entries
        .iter()
        .flatten()
        .find(|entry| keys_equal(entry.key, key, test))
        .map(|entry| entry.value)
}

// ── Hash table operations ──────────────────────────────────────────

/// Create a new hash table (CL `MAKE-HASH-TABLE`). R5.31, R5.33.
pub fn make_hash_table(options: &MakeHashTableOptions) -> Result<EgclVal, EgclError> {
    // Validate rehash_size > 1.0
    if options.rehash_size <= 1.0 {
        return Err(EgclError::TypeError {
            datum: EgclVal::from_fixnum(0),
            expected: "rehash-size > 1.0".to_string(),
        });
    }
    // Validate 0 < rehash_threshold <= 1.0
    if options.rehash_threshold <= 0.0 || options.rehash_threshold > 1.0 {
        return Err(EgclError::TypeError {
            datum: EgclVal::from_fixnum(0),
            expected: "rehash-threshold in (0, 1]".to_string(),
        });
    }

    let capacity = next_power_of_two(options.size.max(16));

    let inner = Box::new(HashTableInner {
        header: ObjectHeader::new(type_id::HASH_TABLE, 0),
        test: options.test,
        buckets: AtomicPtr::new(Box::into_raw(Buckets::empty(capacity))),
        count: 0,
        rehash_size: options.rehash_size,
        rehash_threshold: options.rehash_threshold,
        synchronized: options.synchronized,
        weakness: options.weakness,
        address_sensitive: false,
        placement: AtomicU64::new(placed_at(egcl_rt::gc::gc_move_epoch())),
        lock: TableLock::new(),
    });

    let ptr = Box::into_raw(inner) as *mut u8;
    register_live_table(ptr as usize);
    let val = unsafe { EgclVal::from_heap_ptr(ptr) };
    Ok(val)
}

// ── Core-image (off-heap) serialization (bliss-x0f2 M3) ────────────────
//
// A hash-table body is a Box'd `HashTableInner` OFF the GC heap, so a heap
// snapshot cannot capture it (walk_heap never visits it) and the restored
// heap-tagged reference dangles in a fresh process. These two functions let the
// image system carry hash-tables in a dedicated section: `serialize_live_tables`
// writes every live table's test/entries keyed by its old address;
// `restore_live_tables` re-creates each table, remapping its keys/values through
// the heap old→new map, and returns (old_addr, new_addr) so the loader relocates
// every reference to it. Registered with `egcl_rt::gc::set_offheap_hooks`.

fn test_to_u8(t: HashTest) -> u8 {
    match t {
        HashTest::Eq => 0,
        HashTest::Eql => 1,
        HashTest::Equal => 2,
        HashTest::Equalp => 3,
    }
}

fn u8_to_test(b: u8) -> HashTest {
    match b {
        0 => HashTest::Eq,
        2 => HashTest::Equal,
        3 => HashTest::Equalp,
        _ => HashTest::Eql,
    }
}

fn weakness_to_u8(w: Option<Weakness>) -> u8 {
    match w {
        None => 0,
        Some(Weakness::Key) => 1,
        Some(Weakness::Value) => 2,
        Some(Weakness::KeyAndValue) => 3,
    }
}

fn u8_to_weakness(b: u8) -> Option<Weakness> {
    match b {
        1 => Some(Weakness::Key),
        2 => Some(Weakness::Value),
        3 => Some(Weakness::KeyAndValue),
        _ => None,
    }
}

/// One serialized hash-table slot value: either a raw tagged word (remapped
/// through the heap map on restore) or, for an OFF-HEAP interned string (a
/// `make_lisp_string` object `std::alloc`'d outside the GC heap, invisible to
/// the snapshot — e.g. every package symbol-table key), the string CONTENT,
/// re-created via `make_lisp_string` on restore. EQUAL/EQUALP hash strings by
/// content and `make_lisp_string` interns, so the recreated key behaves
/// identically.
enum SlotRec {
    Raw(u64),
    Str(String),
}

fn serialize_slot(out: &mut Vec<u8>, v: EgclVal) {
    let off_heap_string =
        v.is_string() && !egcl_rt::gc::is_in_heap(unsafe { v.as_ptr() } as usize);
    if off_heap_string {
        let s = v.as_string();
        out.push(1u8);
        out.extend_from_slice(&(s.len() as u32).to_le_bytes());
        out.extend_from_slice(s.as_bytes());
    } else {
        out.push(0u8);
        out.extend_from_slice(&v.to_raw().to_le_bytes());
    }
}

fn read_slot(data: &[u8], off: &mut usize) -> Option<SlotRec> {
    if data.len() < *off + 1 {
        return None;
    }
    let tag = data[*off];
    *off += 1;
    match tag {
        1 => {
            let len = read_u32_at(data, off)? as usize;
            if data.len() < *off + len {
                return None;
            }
            let s = String::from_utf8_lossy(&data[*off..*off + len]).into_owned();
            *off += len;
            Some(SlotRec::Str(s))
        }
        _ => read_u64_at(data, off).map(SlotRec::Raw),
    }
}

/// Serialize every live hash table for a core image. Record layout:
/// `[n u32]` then per table `[old_addr u64][test u8][weakness u8][sync u8]
/// [n_entries u32](key-slot value-slot)*`, where a slot is
/// `[0 u8][raw u64]` or `[1 u8][len u32][utf8 bytes]` (see [`SlotRec`]).
pub fn serialize_live_tables() -> Vec<u8> {
    let mut out = Vec::new();
    let addrs: Vec<usize> = {
        let guard = LIVE_TABLES.lock().unwrap();
        match guard.as_ref() {
            Some(t) => t.iter().copied().collect(),
            None => Vec::new(),
        }
    };
    out.extend_from_slice(&(addrs.len() as u32).to_le_bytes());
    for addr in addrs {
        // SAFETY: LIVE_TABLES holds leaked HashTableInner allocations valid for
        // the process; we only read fields here.
        let inner = addr as *const HashTableInner;
        unsafe {
            out.extend_from_slice(&(addr as u64).to_le_bytes());
            out.push(test_to_u8((*inner).test));
            out.push(weakness_to_u8((*inner).weakness));
            out.push((*inner).synchronized as u8);
            let live: Vec<(EgclVal, EgclVal)> = (*inner)
                .buckets()
                .entries
                .iter()
                .filter_map(|e| e.as_ref().map(|e| (e.key, e.value)))
                .collect();
            out.extend_from_slice(&(live.len() as u32).to_le_bytes());
            for (k, v) in live {
                serialize_slot(&mut out, k);
                serialize_slot(&mut out, v);
            }
        }
    }
    out
}

fn read_u32_at(data: &[u8], off: &mut usize) -> Option<u32> {
    if data.len() < *off + 4 {
        return None;
    }
    let v = u32::from_le_bytes(data[*off..*off + 4].try_into().unwrap());
    *off += 4;
    Some(v)
}

fn read_u64_at(data: &[u8], off: &mut usize) -> Option<u64> {
    if data.len() < *off + 8 {
        return None;
    }
    let v = u64::from_le_bytes(data[*off..*off + 8].try_into().unwrap());
    *off += 8;
    Some(v)
}

// Restore is two-phase (see egcl_rt::gc::set_offheap_hooks): `allocate` creates
// the empty tables (so references to them can be relocated in the heap's Pass 2)
// and stashes their entry lists; `populate` fills them once Pass 2 has made every
// key object structurally hashable.
type PendingTable = (EgclVal, Vec<(SlotRec, SlotRec)>);
thread_local! {
    static PENDING_TABLES: std::cell::RefCell<Vec<PendingTable>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Phase 1: create an empty table per serialized record, stashing its (raw) key/
/// value pairs for `populate_live_tables`. Returns (old_addr, new_addr) so the
/// loader folds each into the heap old→new map. GC-safe: only off-heap (Box)
/// allocation, no key hashing yet.
pub fn allocate_live_tables(data: &[u8]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut off = 0usize;
    let Some(n) = read_u32_at(data, &mut off) else {
        return pairs;
    };
    for _ in 0..n {
        let Some(old_addr) = read_u64_at(data, &mut off) else {
            break;
        };
        if data.len() < off + 3 {
            break;
        }
        let test = u8_to_test(data[off]);
        let weakness = u8_to_weakness(data[off + 1]);
        let synchronized = data[off + 2] != 0;
        off += 3;
        let Some(m) = read_u32_at(data, &mut off) else {
            break;
        };
        let opts = MakeHashTableOptions {
            test,
            size: (m as usize).saturating_mul(2).max(16),
            rehash_size: 2.0,
            rehash_threshold: 0.75,
            synchronized,
            weakness,
        };
        let table = match make_hash_table(&opts) {
            Ok(t) => t,
            Err(_) => return pairs,
        };
        let mut entries = Vec::with_capacity(m as usize);
        for _ in 0..m {
            let (Some(k), Some(v)) = (read_slot(data, &mut off), read_slot(data, &mut off)) else {
                break;
            };
            entries.push((k, v));
        }
        let new_addr = unsafe { table.as_ptr() } as usize;
        PENDING_TABLES.with(|p| p.borrow_mut().push((table, entries)));
        pairs.push((old_addr as usize, new_addr));
    }
    pairs
}

/// Phase 2: fill every table allocated by `allocate_live_tables`, remapping each
/// stored key/value reference through `remap` (the now-final heap old→new map)
/// and re-creating off-heap-string slots from their carried content. GC-safe:
/// `set_gethash` stores already-remapped values and `make_lisp_string`
/// allocates only off the GC heap; key objects' internals are remapped by now,
/// so hashing is valid.
pub fn populate_live_tables(remap: &dyn Fn(u64) -> u64) {
    let mut skipped = 0usize;
    let pending = PENDING_TABLES.with(|p| std::mem::take(&mut *p.borrow_mut()));
    for (table, entries) in pending {
        for (k, v) in entries {
            // A by-content slot re-creates a live off-heap string in THIS
            // process — always valid. A raw slot that did not relocate into the
            // restored heap points at an object we could not carry (an off-heap
            // body of a type without a by-content record); skip it rather than
            // dereference a stale pointer while hashing the key.
            let resolve = |slot: SlotRec| match slot {
                SlotRec::Raw(raw) => {
                    let mapped = remap(raw);
                    let val = EgclVal::from_raw(mapped);
                    // Carried off-heap bodies (interned strings, pathnames,
                    // tables) remap to NEW off-heap addresses — a value the
                    // fold rewrote is valid wherever it lives. Only an
                    // UNMAPPED reference to an object outside the restored
                    // heap is a stale pointer we must not hash.
                    let bad = mapped == raw
                        && val.is_heap_object()
                        && !egcl_rt::gc::is_in_heap(unsafe { val.as_ptr() } as usize);
                    if bad { None } else { Some(val) }
                }
                SlotRec::Str(s) => Some(crate::streams::make_lisp_string(&s)),
            };
            let (Some(rk), Some(rv)) = (resolve(k), resolve(v)) else {
                skipped += 1;
                continue;
            };
            let _ = set_gethash(rk, table, rv);
        }
    }
    if skipped > 0 && std::env::var_os("EGCL_OFFHEAP_DBG").is_some() {
        eprintln!(
            ";; core load: {skipped} hash-table entries with off-heap-body keys/values skipped"
        );
    }
}

/// Get a value from a hash table (CL `GETHASH`).
/// Returns `(value, present-p)`.
pub fn gethash(
    key: EgclVal,
    table: EgclVal,
    default: EgclVal,
) -> Result<(EgclVal, bool), EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // Other threads may be reading it too, so take only a shared reference.
    let inner = unsafe { &*ptr };
    let test = inner.test;
    let buckets = match placed_buckets(inner) {
        Lookup::Placed(buckets) => buckets,
        Lookup::Unplaced(buckets) => {
            return Ok(
                search_unplaced(buckets, key, test).map_or((default, false), |value| (value, true))
            );
        }
    };
    let cap = buckets.capacity();
    let key_hash = hash_for_test(key, test);
    if cap >= MISS_FILTER_MIN_CAPACITY && !hash_may_be_present(&buckets.miss_filter, key_hash) {
        return Ok((default, false));
    }
    let mut idx = probe_index(key_hash, cap);

    for dist in 0..cap {
        match &buckets.entries[idx] {
            Some(entry) => {
                // Robin Hood: if the occupant's probe distance is less than
                // our current search distance, the key can't be present.
                if entry.probe_dist < dist {
                    return Ok((default, false));
                }
                if keys_equal(entry.key, key, test) {
                    return Ok((entry.value, true));
                }
            }
            None => {
                return Ok((default, false));
            }
        }
        idx = (idx + 1) & (cap - 1);
    }
    Ok((default, false))
}

/// Does the string KEY stored in a table hold exactly `text`? Allocation-free
/// for a real simple string; a registry-backed sentinel (whose bits are not a
/// pointer) is resolved through the registry, as [`extract_string_bytes`] does.
fn string_key_eq_str(key: EgclVal, text: &str) -> bool {
    if !key.is_heap_object() {
        return false;
    }
    if crate::pathnames::is_registered_string(key) {
        return crate::pathnames::registered_string(key).is_some_and(|s| s == text);
    }
    // Safety: not a sentinel, so a heap-tagged value points at a live header.
    unsafe {
        let ptr = key.as_ptr();
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        if tid != type_id::SIMPLE_BASE_STRING && tid != type_id::SIMPLE_CHARACTER_STRING {
            return false;
        }
        egcl_rt::object::simple_string_eq_str(ptr, text)
    }
}

/// `GETHASH` of the string whose characters are `text` in an EQUAL table,
/// without making a Lisp string to probe with.
///
/// Package symbol tables are probed by name on every FIND-SYMBOL, INTERN,
/// EXPORT and IMPORT. Probing through [`gethash`] needed a Lisp string key
/// (the interned-string table: a global mutex and a SipHash of the name), then
/// hashed and compared it by decoding both strings into fresh Rust `String`s,
/// after first asking the pathname registries whether either was a pathname.
/// This hashes the text exactly as an EQUAL string key is hashed and compares
/// candidates in place, so it allocates nothing and cannot reach a safepoint.
///
/// Tables of any other test fall back to [`gethash`] with a fresh key.
pub fn gethash_string(text: &str, table: EgclVal) -> Result<Option<EgclVal>, EgclError> {
    let ptr = get_table_inner(table)?;
    // Safety: `test` never changes after creation.
    if unsafe { (*ptr).test } != HashTest::Equal {
        let key = crate::streams::make_lisp_string_fresh(text);
        return gethash(key, table, egcl_rt::value::NIL)
            .map(|(value, present)| present.then_some(value));
    }
    let _lock = lock_table(ptr);
    // Safety: as in `gethash`, a shared reference to a live table.
    let inner = unsafe { &*ptr };
    let buckets = match placed_buckets(inner) {
        Lookup::Placed(buckets) => buckets,
        Lookup::Unplaced(buckets) => {
            return Ok(buckets
                .entries
                .iter()
                .flatten()
                .find(|entry| string_key_eq_str(entry.key, text))
                .map(|entry| entry.value));
        }
    };
    let cap = buckets.capacity();
    let key_hash = hash_bytes(text.as_bytes(), false);
    if cap >= MISS_FILTER_MIN_CAPACITY && !hash_may_be_present(&buckets.miss_filter, key_hash) {
        return Ok(None);
    }
    let mut idx = probe_index(key_hash, cap);
    for dist in 0..cap {
        match &buckets.entries[idx] {
            Some(entry) => {
                if entry.probe_dist < dist {
                    return Ok(None);
                }
                if string_key_eq_str(entry.key, text) {
                    return Ok(Some(entry.value));
                }
            }
            None => return Ok(None),
        }
        idx = (idx + 1) & (cap - 1);
    }
    Ok(None)
}

/// The values of a table's live entries, without its keys.
pub fn hash_table_values(table: EgclVal) -> Result<Vec<EgclVal>, EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: a shared reference; other threads may be reading the table too.
    let inner = unsafe { &*ptr };
    Ok(inner
        .buckets()
        .entries
        .iter()
        .filter_map(|slot| slot.map(|e| e.value))
        .collect())
}

/// Resize the table when load factor exceeds threshold.
fn resize_table(inner: &mut HashTableInner) {
    // Issue #1 fix: use float multiplication to preserve fractional rehash_size
    let capacity = inner.capacity();
    let new_capacity = (capacity as f64 * inner.rehash_size) as usize;
    let new_capacity = next_power_of_two(new_capacity.max(capacity + 1));
    inner.publish(build_buckets(inner.buckets(), new_capacity, inner.test));
}

/// Set a value in a hash table (CL `(SETF GETHASH)`).
pub fn set_gethash(key: EgclVal, table: EgclVal, value: EgclVal) -> Result<(), EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    // A writer has the table to itself, so this places the buckets.
    placed_buckets(inner);
    // A key hashed by a movable object's address makes this table need
    // rehashing after every GC that relocates it (bliss-jtc.22 / bliss-cpje).
    let test = inner.test;
    let (key_hash, address_sensitive) = hash_key(key, test);
    if address_sensitive && !inner.address_sensitive {
        inner.address_sensitive = true;
        *inner.placement.get_mut() = placed_at(egcl_rt::gc::gc_move_epoch());
    }

    // Check if key already exists and update in place
    {
        let buckets = buckets_mut(&mut inner.buckets);
        let cap = buckets.capacity();
        let mut idx = probe_index(key_hash, cap);
        for dist in 0..cap {
            match &buckets.entries[idx] {
                Some(entry) => {
                    if entry.probe_dist < dist {
                        break; // Robin Hood: key can't be present beyond this point
                    }
                    if keys_equal(entry.key, key, test) {
                        buckets.entries[idx] = Some(RHEntry {
                            key,
                            value,
                            probe_dist: entry.probe_dist,
                        });
                        return Ok(());
                    }
                }
                None => break,
            }
            idx = (idx + 1) & (cap - 1);
        }
    }

    // Check load factor and resize if needed
    let threshold = (inner.capacity() as f64 * inner.rehash_threshold) as usize;
    if inner.count + 1 > threshold {
        resize_table(inner);
    }

    let buckets = buckets_mut(&mut inner.buckets);
    record_key_hash(&mut buckets.miss_filter, key_hash);
    // Insert into (possibly resized) table using Robin Hood insertion
    let cap = buckets.capacity();
    let mut idx = probe_index(key_hash, cap);
    let mut incoming = RHEntry {
        key,
        value,
        probe_dist: 0,
    };
    loop {
        match &buckets.entries[idx] {
            None => {
                buckets.entries[idx] = Some(incoming);
                inner.count += 1;
                return Ok(());
            }
            Some(occupant) => {
                // Robin Hood: if incoming has traveled farther, swap
                if incoming.probe_dist > occupant.probe_dist {
                    let displaced = *occupant;
                    buckets.entries[idx] = Some(incoming);
                    incoming = displaced;
                }
            }
        }
        incoming.probe_dist += 1;
        idx = (idx + 1) & (cap - 1);
    }
}

/// Remove an entry (CL `REMHASH`).
pub fn remhash(key: EgclVal, table: EgclVal) -> Result<bool, EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    placed_buckets(inner);
    let test = inner.test;
    let buckets = buckets_mut(&mut inner.buckets);
    let cap = buckets.capacity();
    let key_hash = hash_for_test(key, test);
    let mut idx = probe_index(key_hash, cap);

    for dist in 0..cap {
        match &buckets.entries[idx] {
            Some(entry) => {
                if entry.probe_dist < dist {
                    return Ok(false); // Robin Hood: key not present
                }
                if keys_equal(entry.key, key, test) {
                    // Remove the entry and backward-shift to maintain
                    // Robin Hood invariant
                    buckets.entries[idx] = None;
                    inner.count -= 1;

                    // Backward-shift: move subsequent entries back to fill
                    // the gap, decrementing their probe distances.
                    let mut j = (idx + 1) & (cap - 1);
                    loop {
                        match buckets.entries[j] {
                            None => break,
                            Some(next) => {
                                if next.probe_dist == 0 {
                                    break; // entry is at its natural slot
                                }
                                buckets.entries[idx] = Some(RHEntry {
                                    probe_dist: next.probe_dist - 1,
                                    ..next
                                });
                                buckets.entries[j] = None;
                                idx = j;
                            }
                        }
                        j = (j + 1) & (cap - 1);
                    }

                    return Ok(true);
                }
            }
            None => {
                return Ok(false);
            }
        }
        idx = (idx + 1) & (cap - 1);
    }
    Ok(false)
}

/// Attempt to invoke a EgclVal function with two arguments (key, value).
///
/// If `function` is a compiled-function pointer (TAG_FUNCTION), we call
/// its native entry point with the two-argument ABI:
///     `extern "C" fn(EgclVal, EgclVal) -> EgclVal`.
///
/// For non-function values (e.g. T used as a placeholder in tests, or
/// interpreted/closure objects that require the evaluator), this is a
/// no-op — the function is recorded but cannot be invoked at the
/// stdlib layer without the full runtime evaluator.  This matches the
/// CL spec: MAPHASH's return value is unspecified, and the only
/// observable effect is the side-effects of the function.
fn try_invoke_function(function: EgclVal, key: EgclVal, value: EgclVal) {
    if function.tag() != TAG_FUNCTION {
        // Not a native function pointer — cannot invoke at this layer.
        return;
    }
    unsafe {
        let ptr = function.as_ptr() as *const CompiledFunctionData;
        if ptr.is_null() {
            return;
        }
        let entry = (*ptr).entry_point;
        if entry.is_null() {
            return;
        }
        // Cast the entry point to the two-argument calling convention.
        let func: extern "C" fn(EgclVal, EgclVal) -> EgclVal = std::mem::transmute(entry);
        // Call the function; discard the return value per CL spec.
        let _ = func(key, value);
    }
}

/// Map a function over hash table entries (CL `MAPHASH`).
///
/// Iterates every entry in the table and calls `function` with each
/// `(key, value)` pair.  The return value of each call is discarded.
/// The table must not be structurally modified during iteration (per
/// the CL spec).
pub fn maphash(function: EgclVal, table: EgclVal) -> Result<(), EgclError> {
    let snapshot = hash_table_entries(table)?;
    for (key, value) in snapshot {
        try_invoke_function(function, key, value);
    }
    Ok(())
}

/// Snapshot all live entries in a hash table.
pub fn hash_table_entries(table: EgclVal) -> Result<Vec<(EgclVal, EgclVal)>, EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: a shared reference; other threads may be reading the table too.
    let inner = unsafe { &*ptr };

    Ok(inner
        .buckets()
        .entries
        .iter()
        .filter_map(|slot| slot.map(|e| (e.key, e.value)))
        .collect())
}

/// Clear all entries (CL `CLRHASH`).
pub fn clrhash(table: EgclVal) -> Result<(), EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: single &mut from raw pointer, valid for the function's duration.
    let inner = unsafe { &mut *ptr };
    let buckets = buckets_mut(&mut inner.buckets);
    for entry in buckets.entries.iter_mut() {
        *entry = None;
    }
    buckets.miss_filter.fill(0);
    inner.count = 0;
    Ok(())
}

/// Get the number of entries (CL `HASH-TABLE-COUNT`).
pub fn hash_table_count(table: EgclVal) -> Result<usize, EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    // Safety: single shared read from raw pointer.
    Ok(unsafe { (*ptr).count })
}

/// Get the hash table test (CL `HASH-TABLE-TEST`).
pub fn hash_table_test(table: EgclVal) -> Result<HashTest, EgclError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).test })
}

/// The table's weakness, or `None` for an ordinary (strong) table. R3.13.
pub fn hash_table_weakness(table: EgclVal) -> Result<Option<Weakness>, EgclError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).weakness })
}

/// Whether `table` was made with `:synchronized t` (EGCL-EXT:HASH-TABLE-SYNCHRONIZED-P).
pub fn hash_table_synchronized_p(table: EgclVal) -> Result<bool, EgclError> {
    let ptr = get_table_inner(table)?;
    // Safety: `synchronized` never changes after construction.
    Ok(unsafe { (*ptr).synchronized })
}

/// Get the hash table size (capacity).
pub fn hash_table_size(table: EgclVal) -> Result<usize, EgclError> {
    let ptr = get_table_inner(table)?;
    let _lock = lock_table(ptr);
    Ok(unsafe { (*ptr).capacity() })
}

/// Get the rehash size.
pub fn hash_table_rehash_size(table: EgclVal) -> Result<f64, EgclError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).rehash_size })
}

/// Get the rehash threshold.
pub fn hash_table_rehash_threshold(table: EgclVal) -> Result<f64, EgclError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).rehash_threshold })
}

// ── SXHASH ─────────────────────────────────────────────────────────

/// Compute the hash code for an object (CL `SXHASH`). R5.32.
/// Returns a non-negative fixnum.
pub fn sxhash(object: EgclVal) -> EgclVal {
    EgclVal::from_fixnum(
        (KeyHasher::new(true).equal(object, STRUCTURAL_HASH_DEPTH_LIMIT)
            & MOST_POSITIVE_FIXNUM_MASK) as i64,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::value::NIL;
    use std::cell::Cell;

    thread_local! {
        pub(super) static KEY_COMPARISONS: Cell<usize> = const { Cell::new(0) };
    }

    fn cons(car: EgclVal, cdr: EgclVal) -> EgclVal {
        let body = egcl_rt::alloc_typed(16, type_id::CONS).expect("alloc cons");
        // Safety: a fresh 16-byte cons body [car | cdr].
        unsafe {
            *(body as *mut EgclVal) = car;
            *(body as *mut EgclVal).add(1) = cdr;
            EgclVal::from_cons_ptr(body)
        }
    }

    /// Two numbers EQUALP calls equal must land in the same bucket of an
    /// EQUALP table, whatever their types.
    #[test]
    fn equalp_numbers_hash_alike_when_equalp() {
        use egcl_rt::bignum::{BigInt, BigRat};
        let numbers = [
            EgclVal::from_fixnum(1),
            EgclVal::from_single_float(1.0),
            egcl_rt::gc::alloc_double_float(1.0),
            EgclVal::from_fixnum(0),
            EgclVal::from_single_float(-0.0),
            egcl_rt::gc::alloc_double_float(-0.0),
            BigRat::new(BigInt::from_i64(1), BigInt::from_i64(2)).to_val(),
            EgclVal::from_single_float(0.5),
            egcl_rt::gc::alloc_double_float(0.5),
            BigRat::new(BigInt::from_i64(1), BigInt::from_i64(3)).to_val(),
            BigInt::from_mag(1, vec![0, 1]).to_val(),
            egcl_rt::gc::alloc_double_float(18446744073709551616.0),
            EgclVal::from_fixnum((1 << 53) + 1),
            egcl_rt::gc::alloc_double_float(9007199254740992.0),
        ];
        let mut equal_pairs = 0;
        for &a in &numbers {
            for &b in &numbers {
                if crate::equality::equalp(a, b) {
                    equal_pairs += 1;
                    assert_eq!(
                        hash_for_test(a, HashTest::Equalp),
                        hash_for_test(b, HashTest::Equalp),
                        "{a:?} and {b:?} are EQUALP but hash differently"
                    );
                }
            }
        }
        // 3 ones, 3 zeros, 3 halves (each pair both ways), 2^64 twice; the
        // rest only equal themselves.
        assert_eq!(equal_pairs, 9 + 9 + 9 + 4 + 1 + 1 + 1);
    }

    #[test]
    fn equalp_hash_handles_ratio_constructor_normalization() {
        use egcl_rt::bignum::alloc_ratio_cli;
        for (num, den, expected_num, expected_den) in
            [(1, -2, -1, 2), (2, 6, 1, 3), (0, 3, 0, 1), (6, 3, 2, 1)]
        {
            egcl_rt::rooted!(
                a = alloc_ratio_cli(EgclVal::from_fixnum(num), EgclVal::from_fixnum(den))
            );
            egcl_rt::rooted!(
                b = alloc_ratio_cli(
                    EgclVal::from_fixnum(expected_num),
                    EgclVal::from_fixnum(expected_den)
                )
            );
            assert!(crate::equality::equalp(*a, *b));
            assert_eq!(equalp_real_hash(*a), equalp_real_hash(*b));
            if expected_den == 1 {
                assert_eq!(*a, EgclVal::from_fixnum(expected_num));
            }
        }
    }

    #[test]
    fn equalp_dyadic_encoding_handles_binary64_boundaries() {
        // Each sample is independently a binary64 value. Decode its exact
        // rational with the general arithmetic implementation, then require the
        // allocation-free encoder to recover the original bits.
        for bits in [
            0,
            1,
            2,
            (1_u64 << 52) - 1,
            1_u64 << 52,
            (1_u64 << 52) + 1,
            0x3fe0_0000_0000_0000,
            0x3ff0_0000_0000_0000,
            0x4340_0000_0000_0000,
            0x7fef_ffff_ffff_ffff,
        ] {
            for negative in [false, true] {
                let bits = bits | (u64::from(negative) << 63);
                let f = f64::from_bits(bits);
                let r = crate::numbers::float_exact_rational(f);
                let shift = egcl_rt::bignum::mag_bitlen(&r.den.mag) - 1;
                let expected = if f == 0.0 { 0 } else { bits };
                assert_eq!(
                    exact_dyadic_float_bits(r.num.sign, &r.num.mag, shift),
                    Some(expected)
                );
            }
        }
        // Rounding must never be mistaken for exact representability.
        assert_eq!(exact_dyadic_float_bits(1, &[1], 1075), None);
        assert_eq!(exact_dyadic_float_bits(1, &[3], 1075), None);
        assert_eq!(exact_dyadic_float_bits(1, &[(1 << 53) + 1], 0), None);
        let mut overflow = [0; 17];
        overflow[16] = 1;
        assert_eq!(exact_dyadic_float_bits(1, &overflow, 0), None);
        // Extract a 53-bit odd significand crossing a limb boundary.
        let significand = (1_u64 << 52) + 1;
        assert_eq!(
            exact_dyadic_float_bits(1, &[significand << 40, significand >> 24], 40),
            Some((significand as f64).to_bits()),
        );
    }

    #[test]
    fn equalp_rational_hash_matches_exact_arithmetic_reference() {
        use egcl_rt::bignum::{BigInt, BigRat};
        // Keep the old general-arithmetic algorithm as an independent oracle:
        // all existing hash values must survive this performance change.
        fn reference(r: &BigRat) -> u64 {
            let f = r.to_f64();
            if f.is_finite()
                && egcl_rt::bignum::bigrat_cmp(&crate::numbers::float_exact_rational(f), r)
                    == std::cmp::Ordering::Equal
            {
                equalp_float_hash(f)
            } else {
                combine_hashes(
                    integer_magnitude_hash(r.num.sign, &r.num.mag),
                    integer_magnitude_hash(r.den.sign, &r.den.mag),
                )
            }
        }
        for numerator in [0, 1, -1, 3, -3, (1_i64 << 53) + 1, i64::MAX, i64::MIN] {
            for denominator in [1, 2, 3, 64, (1_i64 << 53) + 1] {
                let r = BigRat::new(BigInt::from_i64(numerator), BigInt::from_i64(denominator));
                egcl_rt::rooted!(value = r.to_val());
                assert_eq!(equalp_real_hash(*value), reference(&r));
            }
        }
        for shift in [52, 53, 60, 63, 64, 65, 1023, 1024, 1074, 1075, 2048] {
            let mut power = vec![0; shift / 64 + 1];
            power[shift / 64] = 1 << (shift % 64);
            for sign in [-1, 1] {
                let integer = BigInt::from_mag(sign, power.clone());
                for r in [
                    BigRat::from_bigint(integer.clone()),
                    BigRat::new(
                        BigInt::from_i64(sign as i64),
                        BigInt::from_mag(1, power.clone()),
                    ),
                    BigRat::new(integer, BigInt::from_i64(3)),
                ] {
                    egcl_rt::rooted!(value = r.to_val());
                    assert_eq!(
                        equalp_real_hash(*value),
                        reference(&r),
                        "shift={shift}, sign={sign}"
                    );
                }
            }
        }
    }

    /// The address flag comes from the hash that consulted the address: a
    /// cons is hashed by address under EQ, but by contents under EQUAL and
    /// EQUALP, and an immediate never by a movable address.
    #[test]
    fn address_sensitivity_follows_the_hash() {
        let pair = cons(EgclVal::from_fixnum(1), EgclVal::from_fixnum(2));
        assert!(hash_key(pair, HashTest::Eq).1);
        assert!(hash_key(pair, HashTest::Eql).1);
        assert!(!hash_key(pair, HashTest::Equal).1);
        assert!(!hash_key(pair, HashTest::Equalp).1);
        assert!(!hash_key(EgclVal::from_fixnum(7), HashTest::Eq).1);
        // A cons nested in a cons is still content-hashed.
        let nested = cons(pair, NIL);
        assert!(!hash_key(nested, HashTest::Equalp).1);
    }

    /// SXHASH may not depend on an address the GC can change: an object
    /// compared by identity hashes by its type, so two distinct conses with no
    /// structure left to hash (past the depth limit) agree.
    #[test]
    fn stable_identity_hash_ignores_the_address() {
        let a = cons(EgclVal::from_fixnum(1), NIL);
        let b = cons(EgclVal::from_fixnum(2), NIL);
        assert_eq!(
            KeyHasher::new(true).identity(a),
            KeyHasher::new(true).identity(b)
        );
        let mut table_hasher = KeyHasher::new(false);
        table_hasher.identity(a);
        assert!(table_hasher.address_sensitive);
    }

    fn synchronized_table(test: HashTest) -> EgclVal {
        make_hash_table(&MakeHashTableOptions {
            test,
            synchronized: true,
            ..Default::default()
        })
        .unwrap()
    }

    /// Every operation on a synchronized table holds its lock (R5.33). Writers
    /// growing one small table from several threads used to replace and free
    /// its bucket vector under each other.
    #[test]
    fn synchronized_table_survives_concurrent_writers() {
        const THREADS: i64 = 4;
        const PER_THREAD: i64 = 5000;
        let table = synchronized_table(HashTest::Eql).0;
        let workers: Vec<_> = (0..THREADS)
            .map(|thread| {
                std::thread::spawn(move || {
                    let table = EgclVal::from_raw(table);
                    for i in 0..PER_THREAD {
                        let key = EgclVal::from_fixnum(thread * PER_THREAD + i);
                        set_gethash(key, table, key).unwrap();
                        assert_eq!(gethash(key, table, NIL).unwrap(), (key, true));
                        hash_table_count(table).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let table = EgclVal::from_raw(table);
        assert_eq!(
            hash_table_count(table).unwrap(),
            (THREADS * PER_THREAD) as usize
        );
        for key in 0..THREADS * PER_THREAD {
            let key = EgclVal::from_fixnum(key);
            assert_eq!(gethash(key, table, NIL).unwrap(), (key, true));
        }
    }

    /// EQUALP hashes and compares a hash-table key by reading it, so an
    /// operation can re-enter the very table it holds; that must not deadlock.
    #[test]
    fn synchronized_equalp_table_can_hold_itself_as_a_key() {
        let zero = EgclVal::from_fixnum(0);
        let table = synchronized_table(HashTest::Equalp);
        set_gethash(zero, table, zero).unwrap();
        // Hashing the key counts `table` while `table` is held.
        set_gethash(table, table, EgclVal::from_fixnum(1)).unwrap();
        assert_eq!(hash_table_count(table).unwrap(), 2);
        // `other` hashes like the stored key did (EQUALP, one entry), so the
        // probe compares them, counting `table` again while it is held.
        let other = synchronized_table(HashTest::Equalp);
        set_gethash(zero, other, zero).unwrap();
        KEY_COMPARISONS.with(|count| count.set(0));
        assert_eq!(gethash(other, table, NIL).unwrap(), (NIL, false));
        assert!(KEY_COMPARISONS.with(Cell::get) >= 1);
    }

    /// Readers of an unsynchronized table may all meet a stale placement at
    /// once, as after a collection that moved a key. One rebuilds it and the
    /// rest search the buckets it replaces, which stay allocated, so every key
    /// is still found. Each reader used to rebuild in place, freeing the
    /// buckets the others were probing.
    #[test]
    fn concurrent_readers_share_a_placement_rebuild() {
        const READERS: usize = 4;
        const KEYS: i64 = 512;
        let table = make_hash_table(&MakeHashTableOptions::default()).unwrap();
        for key in 0..KEYS {
            set_gethash(EgclVal::from_fixnum(key), table, EgclVal::from_fixnum(-key)).unwrap();
        }
        let inner = get_table_inner(table).unwrap();
        // Fixnum keys never move, so no other test's collection can disturb
        // them; claiming address sensitivity makes every stale placement
        // rebuild exactly as moved keys would.
        unsafe { (*inner).address_sensitive = true };
        let table = table.0;
        for _ in 0..200 {
            let stale = placed_at(egcl_rt::gc::gc_move_epoch().wrapping_sub(1));
            unsafe { (*inner).placement.store(stale, Ordering::SeqCst) };
            let start = std::sync::Arc::new(std::sync::Barrier::new(READERS));
            let readers: Vec<_> = (0..READERS)
                .map(|_| {
                    let start = std::sync::Arc::clone(&start);
                    std::thread::spawn(move || {
                        let table = EgclVal::from_raw(table);
                        start.wait();
                        for key in 0..KEYS {
                            assert_eq!(
                                gethash(EgclVal::from_fixnum(key), table, NIL).unwrap(),
                                (EgclVal::from_fixnum(-key), true)
                            );
                        }
                        hash_table_entries(table).unwrap().len()
                    })
                })
                .collect();
            for reader in readers {
                assert_eq!(reader.join().unwrap(), KEYS as usize);
            }
            // Placed again (by whichever reader won), with no rebuild left
            // claimed. A collection elsewhere may have moved the epoch on.
            let placement = unsafe { (*inner).placement.load(Ordering::SeqCst) };
            assert_eq!(placement & REHASHING, 0);
            assert_ne!(placement, stale);
        }
    }

    #[test]
    fn sparse_large_table_misses_avoid_most_key_comparisons() {
        let table = make_hash_table(&MakeHashTableOptions {
            size: 4096,
            ..Default::default()
        })
        .unwrap();
        for key in 0..2000 {
            set_gethash(EgclVal::from_fixnum(key), table, NIL).unwrap();
        }
        KEY_COMPARISONS.with(|count| count.set(0));
        for key in 100_000..104_096 {
            assert_eq!(
                gethash(EgclVal::from_fixnum(key), table, NIL).unwrap(),
                (NIL, false)
            );
        }
        let comparisons = KEY_COMPARISONS.with(Cell::get);
        assert!(
            comparisons < 410,
            "4096 absent keys required {comparisons} equality checks"
        );
    }
}
