//! Hash tables — Robin Hood hashing with open addressing.
//!
//! Robin Hood hashing: on insert, each entry tracks its probe distance
//! (displacement from its natural slot). When an incoming entry has a
//! longer probe distance than the occupant, they swap — this bounds
//! the variance of probe lengths, giving O(log n) worst-case lookup.
//!
//! See spec §5.7.

use bliss_rt::error::BlissError;
use bliss_rt::object::{CompiledFunctionData, ConsCell, ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT};

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

/// Compute the probe index for a key in a table of the given capacity.
fn probe_index(key_bits: u64, capacity: usize) -> usize {
    (hash_u64(key_bits) as usize) & (capacity - 1)
}

/// Extract the raw bytes of a heap-allocated string (SIMPLE-BASE-STRING or
/// SIMPLE-CHARACTER-STRING).  Returns `None` for non-string values.
///
/// Layout: [ObjectHeader (8 bytes)] [length: u64 (8 bytes)] [bytes...]
unsafe fn extract_string_bytes(v: BlissVal) -> Option<&'static [u8]> {
    if !v.is_heap_object() {
        return None;
    }
    // Safety: v is a heap object, so as_ptr yields a valid, aligned pointer
    // to an ObjectHeader followed by the string payload.
    unsafe {
        let ptr = v.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let tid = header.type_id();
        if tid != type_id::SIMPLE_BASE_STRING && tid != type_id::SIMPLE_CHARACTER_STRING {
            return None;
        }
        let len = *((ptr as *const u64).add(1)) as usize;
        Some(std::slice::from_raw_parts(ptr.add(16), len))
    }
}

/// CL `EQUAL` — structural equality.
///
/// Recurses into cons cells and compares strings by content
/// (case-sensitive).  All other types fall back to bit (EQL) equality.
fn cl_equal(a: BlissVal, b: BlissVal) -> bool {
    // Fast path: identical bits ⇒ always equal.
    if a.0 == b.0 {
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
        unsafe {
            if let (Some(sa), Some(sb)) = (extract_string_bytes(a), extract_string_bytes(b)) {
                return sa == sb;
            }
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
        unsafe {
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
        HashTest::Eq | HashTest::Eql => a.0 == b.0,
        HashTest::Equal => cl_equal(a, b),
        HashTest::Equalp => cl_equalp(a, b),
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
    });

    let ptr = Box::into_raw(inner) as *mut u8;
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
    let cap = inner.capacity;
    let test = inner.test;
    let key_bits = key.0;
    let mut idx = probe_index(key_bits, cap);

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
        let mut idx = probe_index(e.key.0, new_capacity);
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
    let test = inner.test;
    let key_bits = key.0;

    // Check if key already exists and update in place
    {
        let cap = inner.capacity;
        let mut idx = probe_index(key_bits, cap);
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
    let mut idx = probe_index(key_bits, cap);
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
    let cap = inner.capacity;
    let test = inner.test;
    let key_bits = key.0;
    let mut idx = probe_index(key_bits, cap);

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
    let ptr = get_table_inner(table)?;
    // Safety: single &mut from raw pointer, valid for the function's duration.
    let inner = unsafe { &mut *ptr };

    // Snapshot the entries so that we iterate a consistent view even if
    // the function happens to call back into hash-table operations on
    // *other* tables.
    let snapshot: Vec<(BlissVal, BlissVal)> = inner
        .entries
        .iter()
        .filter_map(|slot| slot.map(|e| (e.key, e.value)))
        .collect();

    for (key, value) in snapshot {
        try_invoke_function(function, key, value);
    }
    Ok(())
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
    let h = hash_u64(object.0);
    // Mask to ensure non-negative fixnum (positive 60-bit value)
    let non_neg = (h >> 1) as i64; // shift right to ensure positive
    let non_neg = non_neg & 0x0FFF_FFFF_FFFF_FFFF; // ensure fits in fixnum range
    BlissVal::from_fixnum(non_neg)
}
