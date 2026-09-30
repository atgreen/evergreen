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
/// the heap lock, so it only reads the table registry and the (stable, leaked)
/// entry Vecs — it never allocates on the GC heap.
fn scan_hash_table_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let guard = LIVE_TABLES.lock().unwrap();
    let Some(tables) = guard.as_ref() else {
        return;
    };
    for &addr in tables {
        // SAFETY: entries in LIVE_TABLES are leaked HashTableInner allocations
        // that live for the process; their entry Vec is stable during a GC.
        unsafe {
            let inner = addr as *mut HashTableInner;
            let weakness = (*inner).weakness;
            // A WEAK half is deliberately not visited: yielding it here would
            // make the table keep its own referent alive. The collector calls
            // `process_weak_table_entries` instead, which relocates survivors and
            // drops dead entries (R3.13).
            let key_is_weak = matches!(weakness, Some(Weakness::Key | Weakness::KeyAndValue));
            let value_is_weak = matches!(weakness, Some(Weakness::Value | Weakness::KeyAndValue));
            for slot in (*inner).entries.iter_mut() {
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

/// Delivery follows entries only when their table is semantically reached,
/// using `hash_table_entries`. The GC scanner includes package membership and
/// orphaned tables, which must not root every function during tree shaking.
pub fn delivery_root_scanner() -> egcl_rt::gc::RootScanner {
    scan_hash_table_roots
}

/// Collector callback for weak tables (R3.13): relocate each weak referent that
/// survived and drop every entry whose weak referent died.
///
/// `resolve` relocates the slot it is given and answers whether that object is
/// still live. Runs under the collector's heap lock, so it touches only the table
/// registry and the (leaked, stable) entry vectors and performs no EGCL
/// allocation. Removing entries changes bucket occupancy, so a table that lost
/// any entry is rehashed in place — the same pure reordering
/// `maybe_rehash_for_gc` performs.
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
            for slot in (*inner).entries.iter_mut() {
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
                rehash_in_place(&mut *inner);
                (*inner).gc_gen = egcl_rt::gc::gc_move_epoch();
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

fn is_simple_vector(v: EgclVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    // Safety: heap objects start with an ObjectHeader.
    let header = unsafe { *(v.as_ptr() as *const ObjectHeader) };
    header.type_id() == type_id::SIMPLE_VECTOR
}

fn vector_length(v: EgclVal) -> usize {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(8) as *const u64) as usize }
}

fn vector_elt(v: EgclVal, idx: usize) -> EgclVal {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(16 + idx * 8) as *const EgclVal) }
}

/// CL `EQUAL` — structural equality.
///
/// Recurses into cons cells and compares strings by content
/// (case-sensitive).  All other types fall back to bit (EQL) equality.
/// Value equality for two heap numerics of the SAME type (bignum/ratio/complex/
/// double-float) — so two distinct objects with equal value compare equal under
/// EQL/EQUAL/EQUALP. Returns false for non-heap-numeric or mismatched types.
fn heap_numeric_equal(a: EgclVal, b: EgclVal) -> bool {
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
                let a0 = *(pa.add(8) as *const EgclVal);
                let a1 = *(pa.add(16) as *const EgclVal);
                let b0 = *(pb.add(8) as *const EgclVal);
                let b1 = *(pb.add(16) as *const EgclVal);
                (a0.0 == b0.0 || heap_numeric_equal(a0, b0))
                    && (a1.0 == b1.0 || heap_numeric_equal(a1, b1))
            }
            type_id::DOUBLE_FLOAT => *(pa.add(8) as *const f64) == *(pb.add(8) as *const f64),
            _ => false,
        }
    }
}

fn cl_equal(a: EgclVal, b: EgclVal) -> bool {
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

    if a.is_heap_object() && b.is_heap_object() {
        // Pathnames compare by components. Without this the EQUAL hash table's
        // key comparison fell through to `false`, so even a correct hash would
        // not have made a pathname-keyed lookup hit (bliss-kssh). The CLI's own
        // EQUAL already routed here via `pathnames_equal`; the stdlib table did
        // not, which is how the two came apart.
        if crate::pathnames::is_pathname(a) || crate::pathnames::is_pathname(b) {
            return crate::pathnames::pathnames_equal(a, b);
        }
        // Strings: byte-level content comparison (case-sensitive).
        {
            if let (Some(sa), Some(sb)) = (extract_string_bytes(a), extract_string_bytes(b)) {
                return sa == sb;
            }
        }
        // Bit-vectors compare by bit content (ANSI EQUAL treats bit-vectors and
        // strings specially — but NOT general arrays, which are EQ).
        if let (Some(la), Some(lb)) = (
            egcl_rt::types::bit_vector_len(a),
            egcl_rt::types::bit_vector_len(b),
        ) {
            return la == lb
                && (0..la).all(|i| {
                    egcl_rt::types::bit_vector_ref(a, i) == egcl_rt::types::bit_vector_ref(b, i)
                });
        }
    }

    // Everything else — including a SIMPLE general vector, which ANSI EQUAL
    // treats as EQ (compared by identity, already handled by the bit fast path
    // at the top) — is bit equality (matches EQL for fixnums, characters,
    // symbols, single-floats, etc.).
    false
}

/// CL `EQUALP` — case-insensitive structural equality.
///
/// Like `EQUAL`, but characters and strings are compared case-insensitively
/// and numbers of different types are compared by numeric value.
fn cl_equalp(a: EgclVal, b: EgclVal) -> bool {
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
        // Two pathnames are EQUALP exactly when they are EQUAL — their
        // components match (ansi merge-pathnames.1 compares pathnames with
        // EQUALP). Must mirror the pathname case in `equalp_hash` or an EQUALP
        // table keyed by a pathname hashes to the right bucket and then fails
        // the key comparison (bliss-kssh).
        if crate::pathnames::is_pathname(a) || crate::pathnames::is_pathname(b) {
            return crate::pathnames::pathnames_equal(a, b);
        }
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
fn keys_equal(a: EgclVal, b: EgclVal, test: HashTest) -> bool {
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

/// Hash the active-element sequence of a bit-vector (simple or fill-pointered)
/// so an equal simple bit-vector and complex bit-vector hash alike (ANSI SXHASH
/// similarity, sxhash.4/.6/.22). Each bit is hashed as its fixnum value, matching
/// the per-element hash a complex (fixnum-backed) bit-vector produces. Returns
/// `None` if `object` is not a bit-vector.
fn bit_vector_content_hash(object: EgclVal) -> Option<u64> {
    let n = egcl_rt::types::bit_vector_len(object)?;
    let mut h = hash_u64(n as u64);
    for i in 0..n {
        let bit = egcl_rt::types::bit_vector_ref(object, i).unwrap_or(0);
        h = combine_hashes(h, hash_u64(bit as u64));
    }
    Some(h)
}

/// Hash the active elements (0..fill-pointer) of a NON-string complex vector by
/// value. A fill-pointer bit-vector is backed by a general complex vector of
/// fixnum bits, so hashing its active fixnum elements the same way
/// `bit_vector_content_hash` hashes each bit keeps the two consistent.
fn complex_vector_content_hash(object: EgclVal, depth: usize, equalp: bool) -> Option<u64> {
    if !crate::sequences::is_complex_vector(object) || crate::sequences::cvec_is_string(object) {
        return None;
    }
    let n = crate::sequences::cvec_fill_pointer(object);
    let mut h = hash_u64(n as u64);
    for i in 0..n {
        let e = crate::sequences::elt(object, i).ok()?;
        // A fixnum bit hashes as its integer value, matching bit_vector_content_hash.
        let eh = if let Some(bits) = e.is_fixnum().then(|| hash_u64(e.as_fixnum() as u64)) {
            bits
        } else if equalp {
            equalp_hash(e, depth.saturating_sub(1))
        } else {
            equal_hash(e, depth.saturating_sub(1))
        };
        h = combine_hashes(h, eh);
    }
    Some(h)
}

fn equal_hash(object: EgclVal, depth: usize) -> u64 {
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
        unsafe {
            let cell = &*(object.as_ptr() as *const ConsCell);
            return combine_hashes(
                equal_hash(cell.car, depth - 1),
                equal_hash(cell.cdr, depth - 1),
            );
        }
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
        if let Some(bytes) =
            extract_string_bytes(object).or_else(|| crate::sequences::string_content_bytes(object))
        {
            return hash_bytes(&bytes, false);
        }
        if let Some(h) = bit_vector_content_hash(object) {
            return h;
        }
        if let Some(h) = complex_vector_content_hash(object, depth, false) {
            return h;
        }
        // A SIMPLE general vector (and every other array) is EQUAL only to itself
        // (ANSI treats non-string/non-bit arrays as EQ), so it hashes by identity
        // — and stays stable when its contents change (sxhash.7).
    }
    hash_u64(object.0)
}

fn equalp_hash(object: EgclVal, depth: usize) -> u64 {
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
        // Pathnames are EQUALP exactly when they are EQUAL (components match),
        // so they must hash alike here too. Case-folded like every other EQUALP
        // string hash.
        if let Some(h) = crate::pathnames::with_pathname_equal_key(object, |key| {
            hash_bytes(key.as_bytes(), true)
        }) {
            return h;
        }
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

fn hash_for_test(object: EgclVal, test: HashTest) -> u64 {
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
// `hash_for_test` falls through to `hash_u64(object.0)` — the raw EgclVal bits,
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

/// Whether hashing `object` under `test` would depend on a movable object's
/// address (mirrors the address fall-throughs in `equal_hash`/`equalp_hash`/
/// `hash_for_test`). If so, a table holding this key must be rehashed after the
/// GC relocates it.
fn key_address_sensitive(object: EgclVal, test: HashTest) -> bool {
    fn structural(object: EgclVal, depth: usize, equalp: bool) -> bool {
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
            return structural(cell.car, depth - 1, equalp)
                || structural(cell.cdr, depth - 1, equalp);
        }
        if object.is_heap_object() {
            if extract_string_bytes(object).is_some() {
                return false; // content-hashed
            }
            if is_simple_vector(object) {
                // EQUALP hashes a simple vector element-wise, so its
                // address-sensitivity follows its elements'. EQUAL hashes it by
                // identity (ANSI treats non-string/non-bit arrays as EQ), so it is
                // address-sensitive iff it is a movable pointer.
                if equalp {
                    for i in 0..vector_length(object) {
                        if structural(vector_elt(object, i), depth - 1, equalp) {
                            return true;
                        }
                    }
                    return false;
                }
                return is_movable_pointer(object);
            }
        }
        // Fall-through: hashed by address iff it is a movable pointer.
        is_movable_pointer(object)
    }
    match test {
        HashTest::Eq => is_movable_pointer(object),
        HashTest::Eql => {
            numeric_value_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT).is_none()
                && is_movable_pointer(object)
        }
        HashTest::Equal => structural(object, STRUCTURAL_HASH_DEPTH_LIMIT, false),
        HashTest::Equalp => structural(object, STRUCTURAL_HASH_DEPTH_LIMIT, true),
    }
}

/// Rebuild the table's bucket placement at the current capacity from the live
/// entries' *current* key hashes. Pure reordering of existing `EgclVal`s — no
/// EGCL allocation — so it is GC-safe (no collection can fire during it).
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
    let epoch = egcl_rt::gc::gc_move_epoch();
    if inner.gc_gen != epoch {
        rehash_in_place(inner);
        inner.gc_gen = epoch;
    }
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
        gc_gen: egcl_rt::gc::gc_move_epoch(),
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
pub fn set_gethash(key: EgclVal, table: EgclVal, value: EgclVal) -> Result<(), EgclError> {
    let ptr = get_table_inner(table)?;
    // Safety: ptr is valid, non-null, and points to a leaked Box<HashTableInner>.
    // We create exactly one &mut reference from the raw pointer per call.
    let inner = unsafe { &mut *ptr };
    maybe_rehash_for_gc(inner);
    // A key hashed by a movable object's address makes this table need
    // rehashing after every GC that relocates it (bliss-jtc.22 / bliss-cpje).
    if !inner.address_sensitive && key_address_sensitive(key, inner.test) {
        inner.address_sensitive = true;
        inner.gc_gen = egcl_rt::gc::gc_move_epoch();
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
pub fn remhash(key: EgclVal, table: EgclVal) -> Result<bool, EgclError> {
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
    // Safety: single &mut from raw pointer, valid for the function's duration.
    let inner = unsafe { &mut *ptr };

    Ok(inner
        .entries
        .iter()
        .filter_map(|slot| slot.map(|e| (e.key, e.value)))
        .collect())
}

/// Clear all entries (CL `CLRHASH`).
pub fn clrhash(table: EgclVal) -> Result<(), EgclError> {
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
pub fn hash_table_count(table: EgclVal) -> Result<usize, EgclError> {
    let ptr = get_table_inner(table)?;
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

/// Get the hash table size (capacity).
pub fn hash_table_size(table: EgclVal) -> Result<usize, EgclError> {
    let ptr = get_table_inner(table)?;
    Ok(unsafe { (*ptr).capacity })
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
        (equal_hash(object, STRUCTURAL_HASH_DEPTH_LIMIT) & MOST_POSITIVE_FIXNUM_MASK) as i64,
    )
}
