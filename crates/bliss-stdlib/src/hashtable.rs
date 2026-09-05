//! Hash tables — Robin Hood hashing with open addressing.
//!
//! Robin Hood hashing: on insert, each entry tracks its probe distance
//! (displacement from its natural slot). When an incoming entry has a
//! longer probe distance than the occupant, they swap — this bounds
//! the variance of probe lengths, giving O(log n) worst-case lookup.
//!
//! See spec §5.7.

use bliss_rt::error::BlissError;
use bliss_rt::lock_order::{LockLevel, OrderedMutex};
use bliss_rt::object::{CompiledFunctionData, ConsCell, ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT};
use std::collections::HashSet;
use std::sync::Once;

// ── GC root tracking for hash-table storage (bliss-jtc.8) ────────────────────
//
// A hash table's entries live in a Rust `Vec<Option<RHEntry>>` outside the GC
// heap, so its keys/values are invisible to the collector. We track every live
// table and register a root scanner (bliss_rt::gc) that yields each entry's key
// and value slot, so the collector marks and relocates them like any other root.
static LIVE_TABLES: OrderedMutex<Option<HashSet<usize>>> =
    OrderedMutex::new(LockLevel::GcWorld, 8, "hash-table GC roots", None);
static REGISTER_SCANNER: Once = Once::new();

/// Track a newly-created table and ensure the GC root scanner is registered.
fn register_live_table(ptr: usize) {
    REGISTER_SCANNER.call_once(|| {
        bliss_rt::gc::register_root_scanner(scan_hash_table_roots);
    });
    LIVE_TABLES
        .lock()
        .unwrap()
        .get_or_insert_with(HashSet::new)
        .insert(ptr);
}

/// GC external root scanner: yield every live hash table's entry key and value
/// slots so the collector marks + relocates them. Runs while the collector holds
/// the heap lock, so it only reads the table registry and the (stable, leaked)
/// entry Vecs — it never allocates on the GC heap.
fn scan_hash_table_roots(visit: &mut dyn FnMut(*mut BlissVal)) {
    let guard = LIVE_TABLES.lock().unwrap();
    let Some(tables) = guard.as_ref() else {
        return;
    };
    for &addr in tables {
        // SAFETY: entries in LIVE_TABLES are leaked HashTableInner allocations
        // that live for the process; their entry Vec is stable during a GC.
        unsafe {
            let inner = addr as *mut HashTableInner;
            for slot in (*inner).entries.iter_mut() {
                if let Some(entry) = slot.as_mut() {
                    visit(&mut entry.key as *mut BlissVal);
                    visit(&mut entry.value as *mut BlissVal);
                }
            }
        }
    }
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
    key: BlissVal,
    value: BlissVal,
    /// Distance from the entry's natural (home) slot.
    probe_dist: usize,
}

/// The internal data structure for a hash table, stored on the heap.
#[repr(C)]
struct HashTableInner {
    header: ObjectHeader,
    test: HashTest,
    entries: Vec<Option<RHEntry>>,
    count: usize,
    capacity: usize,
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
    /// The `gc_move_epoch()` at which this table's bucket placement was last
    /// valid. When it differs from the current generation and the table is
    /// `address_sensitive`, the next access rehashes in place first.
    gc_gen: u64,
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

/// Extract a raw pointer to HashTableInner from a BlissVal, validating it is a hash table.
///
/// Returns a raw pointer to avoid creating multiple `&mut` references (UB).
/// Callers use unsafe raw-pointer operations to access the inner data.
fn get_table_inner(table: BlissVal) -> Result<*mut HashTableInner, BlissError> {
    if table.tag() != TAG_HEAP_OBJECT {
        return Err(BlissError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    let ptr = unsafe { table.as_ptr() } as *mut HashTableInner;
    if ptr.is_null() {
        return Err(BlissError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    // Safety: ptr is non-null and was created from a valid heap allocation
    if unsafe { (*ptr).header.type_id() } != type_id::HASH_TABLE {
        return Err(BlissError::TypeError {
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
pub fn hash_table_p(value: BlissVal) -> bool {
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
fn extract_string_bytes(v: BlissVal) -> Option<Vec<u8>> {
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
        Some(bliss_rt::object::read_simple_string(ptr).into_bytes())
    }
}

fn is_simple_vector(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    // Safety: heap objects start with an ObjectHeader.
    let header = unsafe { *(v.as_ptr() as *const ObjectHeader) };
    header.type_id() == type_id::SIMPLE_VECTOR
}

fn vector_length(v: BlissVal) -> usize {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(8) as *const u64) as usize }
}

fn vector_elt(v: BlissVal, idx: usize) -> BlissVal {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(16 + idx * 8) as *const BlissVal) }
}

/// CL `EQUAL` — structural equality.
///
/// Recurses into cons cells and compares strings by content
/// (case-sensitive).  All other types fall back to bit (EQL) equality.
/// Value equality for two heap numerics of the SAME type (bignum/ratio/complex/
/// double-float) — so two distinct objects with equal value compare equal under
/// EQL/EQUAL/EQUALP. Returns false for non-heap-numeric or mismatched types.
fn heap_numeric_equal(a: BlissVal, b: BlissVal) -> bool {
    if !a.is_heap_object() || !b.is_heap_object() {
        return false;
    }
    let (ta, tb) = unsafe {
        (
            (*(a.as_ptr() as *const ObjectHeader)).type_id(),
            (*(b.as_ptr() as *const ObjectHeader)).type_id(),
        )
    };
    if ta != tb {
        return false;
    }
    unsafe {
        let (pa, pb) = (a.as_ptr(), b.as_ptr());
        match ta {
            type_id::BIGNUM => {
                let (sa, na) = (
                    *(pa.add(8) as *const i32),
                    *(pa.add(12) as *const u32) as usize,
                );
                let (sb, nb) = (
                    *(pb.add(8) as *const i32),
                    *(pb.add(12) as *const u32) as usize,
                );
                sa == sb
                    && na == nb
                    && (0..na).all(|i| {
                        *(pa.add(16 + i * 8) as *const u64) == *(pb.add(16 + i * 8) as *const u64)
                    })
            }
            type_id::RATIO | type_id::COMPLEX => {
                let a0 = *(pa.add(8) as *const BlissVal);
                let a1 = *(pa.add(16) as *const BlissVal);
                let b0 = *(pb.add(8) as *const BlissVal);
                let b1 = *(pb.add(16) as *const BlissVal);
                (a0.0 == b0.0 || heap_numeric_equal(a0, b0))
                    && (a1.0 == b1.0 || heap_numeric_equal(a1, b1))
            }
            type_id::DOUBLE_FLOAT => *(pa.add(8) as *const f64) == *(pb.add(8) as *const f64),
            _ => false,
        }
    }
}

fn cl_equal(a: BlissVal, b: BlissVal) -> bool {
    // Fast path: identical bits ⇒ always equal.
    if a.0 == b.0 {
        return true;
    }
    // Distinct heap numerics with equal value (EQUAL is EQL on numbers).
    if heap_numeric_equal(a, b) {
        return true;
    }

    // Cons cells: compare car and cdr recursively.
    if a.tag() == TAG_CONS && b.tag() == TAG_CONS {
        unsafe {
            let ca = &*(a.as_ptr() as *const ConsCell);
            let cb = &*(b.as_ptr() as *const ConsCell);
            return cl_equal(ca.car, cb.car) && cl_equal(ca.cdr, cb.cdr);
        }
    }

    // Strings: byte-level content comparison (case-sensitive).
    if a.is_heap_object() && b.is_heap_object() {
        {
            if let (Some(sa), Some(sb)) = (extract_string_bytes(a), extract_string_bytes(b)) {
                return sa == sb;
            }
        }
        if is_simple_vector(a) && is_simple_vector(b) {
            let len = vector_length(a);
            if len != vector_length(b) {
                return false;
            }
            for i in 0..len {
                if !cl_equal(vector_elt(a, i), vector_elt(b, i)) {
                    return false;
                }
            }
            return true;
        }
    }

    // Everything else: bit equality (matches EQL semantics for
    // fixnums, characters, symbols, single-floats, etc.).
    false
}

/// CL `EQUALP` — case-insensitive structural equality.
///
/// Like `EQUAL`, but characters and strings are compared case-insensitively
/// and numbers of different types are compared by numeric value.
fn cl_equalp(a: BlissVal, b: BlissVal) -> bool {
    // Fast path: identical bits ⇒ always equal.
    if a.0 == b.0 {
        return true;
    }

    // Characters: case-insensitive comparison.
    if a.is_character() && b.is_character() {
        let ca = a.as_char().to_ascii_lowercase();
        let cb = b.as_char().to_ascii_lowercase();
        return ca == cb;
    }

    // Numeric cross-type equality.
    // Fixnum vs fixnum with same value would have same bits (caught above).
    // Fixnum vs single-float: compare numerically.
    if a.is_fixnum() && b.is_single_float() {
        return (a.as_fixnum() as f64) == (b.as_single_float() as f64);
    }
    if a.is_single_float() && b.is_fixnum() {
        return (a.as_single_float() as f64) == (b.as_fixnum() as f64);
    }
    // Two single-floats with different bit patterns but same numeric value
    // (e.g. +0.0 and -0.0 are == in Rust but have different bits).
    if a.is_single_float() && b.is_single_float() {
        return a.as_single_float() == b.as_single_float();
    }
    // Distinct heap numerics (bignum/ratio/complex/double) with equal value.
    if heap_numeric_equal(a, b) {
        return true;
    }

    // Cons cells: recurse with equalp semantics.
    if a.tag() == TAG_CONS && b.tag() == TAG_CONS {
        unsafe {
            let ca = &*(a.as_ptr() as *const ConsCell);
            let cb = &*(b.as_ptr() as *const ConsCell);
            return cl_equalp(ca.car, cb.car) && cl_equalp(ca.cdr, cb.cdr);
        }
    }

    // Strings: case-insensitive byte comparison.
    if a.is_heap_object() && b.is_heap_object() {
        {
            if let (Some(sa), Some(sb)) = (extract_string_bytes(a), extract_string_bytes(b)) {
                if sa.len() != sb.len() {
                    return false;
                }
                return sa
                    .iter()
                    .zip(sb.iter())
                    .all(|(&x, &y)| x.eq_ignore_ascii_case(&y));
            }
        }
        if is_simple_vector(a) && is_simple_vector(b) {
            let len = vector_length(a);
            if len != vector_length(b) {
                return false;
            }
            for i in 0..len {
                if !cl_equalp(vector_elt(a, i), vector_elt(b, i)) {
                    return false;
                }
            }
            return true;
        }
    }

    // All other types: bit equality.
    false
}

/// Check if two keys are equal according to the given hash test.
///
/// - Eq / Eql: identity (raw bit equality) — correct for fixnums, symbols, etc.
/// - Equal: structural equality — recurses into conses and compares strings
///   by content (case-sensitive).
/// - Equalp: case-insensitive structural equality — case-insensitive strings
///   and characters, numeric cross-type comparison.
fn keys_equal(a: BlissVal, b: BlissVal, test: HashTest) -> bool {
    match test {
        HashTest::Eq => a.0 == b.0,
        // EQL of two distinct heap numerics is T by value (e.g. two bignums).
        HashTest::Eql => a.0 == b.0 || heap_numeric_equal(a, b),
        HashTest::Equal => cl_equal(a, b),
        HashTest::Equalp => cl_equalp(a, b),
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

fn combine_hashes(a: u64, b: u64) -> u64 {
    a.rotate_left(13) ^ b.rotate_right(7) ^ 0x9e37_79b9_7f4a_7c15
}

/// Value-based hash for a heap numeric (bignum / ratio / complex / double-float)
/// so two distinct-but-numerically-equal objects hash equal — otherwise a hash
/// table keyed on such a value never finds its entry, because the fall-through
/// hashed the object's ADDRESS (`object.0`) rather than its value. Returns `None`
/// for anything that is not a heap numeric.
fn numeric_value_hash(object: BlissVal, depth: usize) -> Option<u64> {
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
                // RatioData / ComplexData both hold two BlissVal parts at +8/+16.
                let a = *(p.add(8) as *const BlissVal);
                let b = *(p.add(16) as *const BlissVal);
                Some(combine_hashes(
                    numeric_or_scalar_hash(a, depth - 1),
                    numeric_or_scalar_hash(b, depth - 1),
                ))
            }
            type_id::DOUBLE_FLOAT => Some(hash_u64((*(p.add(8) as *const f64)).to_bits())),
            _ => None,
        }
    }
}

/// Hash a numeric part by value: a heap numeric via [`numeric_value_hash`], an
/// immediate (fixnum / single-float) by its bits.
fn numeric_or_scalar_hash(object: BlissVal, depth: usize) -> u64 {
    numeric_value_hash(object, depth).unwrap_or_else(|| hash_u64(object.0))
}

fn equal_hash(object: BlissVal, depth: usize) -> u64 {
    if depth == 0 {
        return 0;
    }
    if let Some(h) = numeric_value_hash(object, depth) {
        return h;
    }
    if object.tag() == TAG_CONS {
        unsafe {
            let cell = &*(object.as_ptr() as *const ConsCell);
            return combine_hashes(
                equal_hash(cell.car, depth - 1),
                equal_hash(cell.cdr, depth - 1),
            );
        }
    }
    if object.is_heap_object() {
        {
            if let Some(bytes) = extract_string_bytes(object) {
                return hash_bytes(&bytes, false);
            }
        }
        if is_simple_vector(object) {
            let mut h = hash_u64(vector_length(object) as u64);
            for i in 0..vector_length(object) {
                h = combine_hashes(h, equal_hash(vector_elt(object, i), depth - 1));
            }
            return h;
        }
    }
    hash_u64(object.0)
}

fn equalp_hash(object: BlissVal, depth: usize) -> u64 {
    if depth == 0 {
        return 0;
    }
    if object.is_character() {
        return hash_u64(object.as_char().to_ascii_lowercase() as u64);
    }
    if object.is_fixnum() {
        return hash_u64((object.as_fixnum() as f64).to_bits());
    }
    if object.is_single_float() {
        return hash_u64((object.as_single_float() as f64).to_bits());
    }
    if let Some(h) = numeric_value_hash(object, depth) {
        return h;
    }
    if object.tag() == TAG_CONS {
        unsafe {
            let cell = &*(object.as_ptr() as *const ConsCell);
            return combine_hashes(
                equalp_hash(cell.car, depth - 1),
                equalp_hash(cell.cdr, depth - 1),
            );
        }
    }
    if object.is_heap_object() {
        {
            if let Some(bytes) = extract_string_bytes(object) {
                return hash_bytes(&bytes, true);
            }
        }
        if is_simple_vector(object) {
            let mut h = hash_u64(vector_length(object) as u64);
            for i in 0..vector_length(object) {
                h = combine_hashes(h, equalp_hash(vector_elt(object, i), depth - 1));
            }
            return h;
        }
    }
    hash_u64(object.0)
}

fn hash_for_test(object: BlissVal, test: HashTest) -> u64 {
    match test {
        // EQL of two distinct heap numerics (bignum/ratio/complex) is T by value,
        // so they must hash by value, not address (EQ stays identity-based).
        HashTest::Eq => hash_u64(object.0),
        HashTest::Eql => numeric_value_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT)
            .unwrap_or_else(|| hash_u64(object.0)),
        HashTest::Equal => equal_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT),
        HashTest::Equalp => equalp_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT),
    }
}

// ── Moving-GC hash stability (bliss-jtc.22 / bliss-cpje) ──────────────
//
// `hash_for_test` falls through to `hash_u64(object.0)` — the raw BlissVal bits,
// i.e. the object's ADDRESS — for any heap object it can't hash structurally
// (a CLOS instance, a function, a cons under EQ, …). The moving GC relocates
// such objects and rewrites the pointer stored in the table's entry, but never
// recomputes the entry's bucket, so a later probe (which hashes the NEW address)
// looks in the wrong bucket and misses a live key. We detect keys that hash by a
// *movable* address and rehash the whole table in place the first time it is
// touched in a new GC generation. Symbols and their name strings are pinned and
// immortal, so symbol/string keys are stable and never trigger a rehash.

/// True if `object` is a heap pointer the GC may relocate: a cons (never
/// pinned), or a `TAG_HEAP_OBJECT` whose header is not pinned. Immediates
/// (fixnum/char/nil/t/single-float) are stable.
fn is_movable_pointer(object: BlissVal) -> bool {
    if object.tag() == TAG_CONS {
        return true;
    }
    if object.is_heap_object() {
        // Safety: a TAG_HEAP_OBJECT value points at an ObjectHeader.
        return unsafe { !(*(object.as_ptr() as *const ObjectHeader)).is_pinned() };
    }
    false
}

/// Whether hashing `object` under `test` would depend on a movable object's
/// address (mirrors the address fall-throughs in `equal_hash`/`equalp_hash`/
/// `hash_for_test`). If so, a table holding this key must be rehashed after the
/// GC relocates it.
fn key_address_sensitive(object: BlissVal, test: HashTest) -> bool {
    fn structural(object: BlissVal, depth: usize, equalp: bool) -> bool {
        if depth == 0 {
            return false; // hash returns a constant here too — stable
        }
        // Value-hashed leaves never depend on an address.
        if numeric_value_hash(object, depth).is_some() {
            return false;
        }
        if equalp && (object.is_character() || object.is_fixnum() || object.is_single_float()) {
            return false;
        }
        if object.tag() == TAG_CONS {
            // Safety: TAG_CONS points at a ConsCell.
            let cell = unsafe { &*(object.as_ptr() as *const ConsCell) };
            return structural(cell.car, depth - 1, equalp) || structural(cell.cdr, depth - 1, equalp);
        }
        if object.is_heap_object() {
            if extract_string_bytes(object).is_some() {
                return false; // content-hashed
            }
            if is_simple_vector(object) {
                for i in 0..vector_length(object) {
                    if structural(vector_elt(object, i), depth - 1, equalp) {
                        return true;
                    }
                }
                return false;
            }
        }
        // Fall-through: hashed by address iff it is a movable pointer.
        is_movable_pointer(object)
    }
    match test {
        HashTest::Eq => is_movable_pointer(object),
        HashTest::Eql => numeric_value_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT).is_none()
            && is_movable_pointer(object),
        HashTest::Equal => structural(object, STRUCTURAL_HASH_DEPTH_LIMIT, false),
        HashTest::Equalp => structural(object, STRUCTURAL_HASH_DEPTH_LIMIT, true),
    }
}

/// Rebuild the table's bucket placement at the current capacity from the live
/// entries' *current* key hashes. Pure reordering of existing `BlissVal`s — no
/// Bliss allocation — so it is GC-safe (no collection can fire during it).
fn rehash_in_place(inner: &mut HashTableInner) {
    let cap = inner.capacity;
    let mut new_entries: Vec<Option<RHEntry>> = vec![None; cap];
    for e in inner.entries.iter().flatten() {
        let mut idx = probe_index(hash_for_test(e.key, inner.test), cap);
        let mut incoming = RHEntry {
            key: e.key,
            value: e.value,
            probe_dist: 0,
        };
        loop {
            match &new_entries[idx] {
                None => {
                    new_entries[idx] = Some(incoming);
                    break;
                }
                Some(occupant) => {
                    if incoming.probe_dist > occupant.probe_dist {
                        let displaced = *occupant;
                        new_entries[idx] = Some(incoming);
                        incoming = displaced;
                    }
                }
            }
            incoming.probe_dist += 1;
            idx = (idx + 1) & (cap - 1);
        }
    }
    inner.entries = new_entries;
}

/// If this table hashes any key by a movable address and the GC has run since it
/// was last valid, rehash it in place so bucket positions match current key
/// hashes. Must be called at the start of every access that probes by hash
/// (bliss-jtc.22 / bliss-cpje).
fn maybe_rehash_for_gc(inner: &mut HashTableInner) {
    if !inner.address_sensitive {
        return;
    }
    let epoch = bliss_rt::gc::gc_move_epoch();
    if inner.gc_gen != epoch {
        rehash_in_place(inner);
        inner.gc_gen = epoch;
    }
}

// ── Hash table operations ──────────────────────────────────────────

/// Create a new hash table (CL `MAKE-HASH-TABLE`). R5.31, R5.33.
pub fn make_hash_table(options: &MakeHashTableOptions) -> Result<BlissVal, BlissError> {
    // Validate rehash_size > 1.0
    if options.rehash_size <= 1.0 {
        return Err(BlissError::TypeError {
            datum: BlissVal::from_fixnum(0),
            expected: "rehash-size > 1.0".to_string(),
        });
    }
    // Validate 0 < rehash_threshold <= 1.0
    if options.rehash_threshold <= 0.0 || options.rehash_threshold > 1.0 {
        return Err(BlissError::TypeError {
            datum: BlissVal::from_fixnum(0),
            expected: "rehash-threshold in (0, 1]".to_string(),
        });
    }

    let capacity = next_power_of_two(options.size.max(16));
    let entries = vec![None; capacity];

    let inner = Box::new(HashTableInner {
        header: ObjectHeader::new(type_id::HASH_TABLE, 0),
        test: options.test,
        entries,
        count: 0,
        capacity,
        rehash_size: options.rehash_size,
        rehash_threshold: options.rehash_threshold,
        synchronized: options.synchronized,
        weakness: options.weakness,
        address_sensitive: false,
        gc_gen: bliss_rt::gc::gc_move_epoch(),
    });

    let ptr = Box::into_raw(inner) as *mut u8;
    register_live_table(ptr as usize);
    let val = unsafe { BlissVal::from_heap_ptr(ptr) };
    Ok(val)
}

/// Get a value from a hash table (CL `GETHASH`).
/// Returns `(value, present-p)`.
pub fn gethash(
    key: BlissVal,
    table: BlissVal,
    default: BlissVal,
) -> Result<(BlissVal, bool), BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    maybe_rehash_for_gc(inner);
    let cap = inner.capacity;
    let test = inner.test;
    let key_hash = hash_for_test(key, test);
    let mut idx = probe_index(key_hash, cap);

    for dist in 0..cap {
        match &inner.entries[idx] {
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

/// Resize the table when load factor exceeds threshold.
fn resize_table(inner: &mut HashTableInner) {
    // Issue #1 fix: use float multiplication to preserve fractional rehash_size
    let new_capacity = (inner.capacity as f64 * inner.rehash_size) as usize;
    let new_capacity = next_power_of_two(new_capacity.max(inner.capacity + 1));
    let mut new_entries: Vec<Option<RHEntry>> = vec![None; new_capacity];

    for e in inner.entries.iter().flatten() {
        let mut idx = probe_index(hash_for_test(e.key, inner.test), new_capacity);
        let mut incoming = RHEntry {
            key: e.key,
            value: e.value,
            probe_dist: 0,
        };
        loop {
            match &new_entries[idx] {
                None => {
                    new_entries[idx] = Some(incoming);
                    break;
                }
                Some(occupant) => {
                    // Robin Hood: swap if incoming has traveled farther
                    if incoming.probe_dist > occupant.probe_dist {
                        let displaced = *occupant;
                        new_entries[idx] = Some(incoming);
                        incoming = displaced;
                    }
                }
            }
            incoming.probe_dist += 1;
            idx = (idx + 1) & (new_capacity - 1);
        }
    }

    inner.entries = new_entries;
    inner.capacity = new_capacity;
}

/// Set a value in a hash table (CL `(SETF GETHASH)`).
pub fn set_gethash(key: BlissVal, table: BlissVal, value: BlissVal) -> Result<(), BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    maybe_rehash_for_gc(inner);
    // A key hashed by a movable object's address makes this table need
    // rehashing after every GC that relocates it (bliss-jtc.22 / bliss-cpje).
    if !inner.address_sensitive && key_address_sensitive(key, inner.test) {
        inner.address_sensitive = true;
        inner.gc_gen = bliss_rt::gc::gc_move_epoch();
    }
    let test = inner.test;
    let key_hash = hash_for_test(key, test);

    // Check if key already exists and update in place
    {
        let cap = inner.capacity;
        let mut idx = probe_index(key_hash, cap);
        for dist in 0..cap {
            match &inner.entries[idx] {
                Some(entry) => {
                    if entry.probe_dist < dist {
                        break; // Robin Hood: key can't be present beyond this point
                    }
                    if keys_equal(entry.key, key, test) {
                        inner.entries[idx] = Some(RHEntry {
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
    let threshold = (inner.capacity as f64 * inner.rehash_threshold) as usize;
    if inner.count + 1 > threshold {
        resize_table(inner);
    }

    // Insert into (possibly resized) table using Robin Hood insertion
    let cap = inner.capacity;
    let mut idx = probe_index(key_hash, cap);
    let mut incoming = RHEntry {
        key,
        value,
        probe_dist: 0,
    };
    loop {
        match &inner.entries[idx] {
            None => {
                inner.entries[idx] = Some(incoming);
                inner.count += 1;
                return Ok(());
            }
            Some(occupant) => {
                // Robin Hood: if incoming has traveled farther, swap
                if incoming.probe_dist > occupant.probe_dist {
                    let displaced = *occupant;
                    inner.entries[idx] = Some(incoming);
                    incoming = displaced;
                }
            }
        }
        incoming.probe_dist += 1;
        idx = (idx + 1) & (cap - 1);
    }
}

/// Remove an entry (CL `REMHASH`).
pub fn remhash(key: BlissVal, table: BlissVal) -> Result<bool, BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    maybe_rehash_for_gc(inner);
    let cap = inner.capacity;
    let test = inner.test;
    let key_hash = hash_for_test(key, test);
    let mut idx = probe_index(key_hash, cap);

    for dist in 0..cap {
        match &inner.entries[idx] {
            Some(entry) => {
                if entry.probe_dist < dist {
                    return Ok(false); // Robin Hood: key not present
                }
                if keys_equal(entry.key, key, test) {
                    // Remove the entry and backward-shift to maintain
                    // Robin Hood invariant
                    inner.entries[idx] = None;
                    inner.count -= 1;

                    // Backward-shift: move subsequent entries back to fill
                    // the gap, decrementing their probe distances.
                    let mut j = (idx + 1) & (cap - 1);
                    loop {
                        match inner.entries[j] {
                            None => break,
                            Some(next) => {
                                if next.probe_dist == 0 {
                                    break; // entry is at its natural slot
                                }
                                inner.entries[idx] = Some(RHEntry {
                                    probe_dist: next.probe_dist - 1,
                                    ..next
                                });
                                inner.entries[j] = None;
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

/// Attempt to invoke a BlissVal function with two arguments (key, value).
///
/// If `function` is a compiled-function pointer (TAG_FUNCTION), we call
/// its native entry point with the two-argument ABI:
///     `extern "C" fn(BlissVal, BlissVal) -> BlissVal`.
///
/// For non-function values (e.g. T used as a placeholder in tests, or
/// interpreted/closure objects that require the evaluator), this is a
/// no-op — the function is recorded but cannot be invoked at the
/// stdlib layer without the full runtime evaluator.  This matches the
/// CL spec: MAPHASH's return value is unspecified, and the only
/// observable effect is the side-effects of the function.
fn try_invoke_function(function: BlissVal, key: BlissVal, value: BlissVal) {
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
        let func: extern "C" fn(BlissVal, BlissVal) -> BlissVal = std::mem::transmute(entry);
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
pub fn maphash(function: BlissVal, table: BlissVal) -> Result<(), BlissError> {
    let snapshot = hash_table_entries(table)?;
    for (key, value) in snapshot {
        try_invoke_function(function, key, value);
    }
    Ok(())
}

/// Snapshot all live entries in a hash table.
pub fn hash_table_entries(table: BlissVal) -> Result<Vec<(BlissVal, BlissVal)>, BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: single &mut from raw pointer, valid for the function's duration.
    let inner = unsafe { &mut *ptr };

    Ok(inner
        .entries
        .iter()
        .filter_map(|slot| slot.map(|e| (e.key, e.value)))
        .collect())
}

/// Clear all entries (CL `CLRHASH`).
pub fn clrhash(table: BlissVal) -> Result<(), BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: single &mut from raw pointer, valid for the function's duration.
    let inner = unsafe { &mut *ptr };
    for entry in inner.entries.iter_mut() {
        *entry = None;
    }
    inner.count = 0;
    Ok(())
}

/// Get the number of entries (CL `HASH-TABLE-COUNT`).
pub fn hash_table_count(table: BlissVal) -> Result<usize, BlissError> {
    let ptr = get_table_inner(table)?;
    // Safety: single shared read from raw pointer.
    Ok(unsafe { (*ptr).count })
}

/// Get the hash table test (CL `HASH-TABLE-TEST`).
pub fn hash_table_test(table: BlissVal) -> Result<HashTest, BlissError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).test })
}

/// Get the hash table size (capacity).
pub fn hash_table_size(table: BlissVal) -> Result<usize, BlissError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).capacity })
}

/// Get the rehash size.
pub fn hash_table_rehash_size(table: BlissVal) -> Result<f64, BlissError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).rehash_size })
}

/// Get the rehash threshold.
pub fn hash_table_rehash_threshold(table: BlissVal) -> Result<f64, BlissError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).rehash_threshold })
}

// ── SXHASH ─────────────────────────────────────────────────────────

/// Compute the hash code for an object (CL `SXHASH`). R5.32.
/// Returns a non-negative fixnum.
pub fn sxhash(object: BlissVal) -> BlissVal {
    BlissVal::from_fixnum(
        (equal_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT) & MOST_POSITIVE_FIXNUM_MASK) as i64,
    )
}
