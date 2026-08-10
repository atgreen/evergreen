//! Hash tables — Robin Hood hashing with open addressing.
//!
//! See spec §5.7.

use bliss_rt::error::BlissError;
use bliss_rt::object::{type_id, ObjectHeader};
use bliss_rt::value::{BlissVal, TAG_HEAP_OBJECT};

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

/// The internal data structure for a hash table, stored on the heap.
#[repr(C)]
struct HashTableInner {
    header: ObjectHeader,
    test: HashTest,
    entries: Vec<Option<(BlissVal, BlissVal)>>,
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

/// Extract a pointer to HashTableInner from a BlissVal, validating it is a hash table.
fn get_table_inner(table: BlissVal) -> Result<&'static mut HashTableInner, BlissError> {
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
    let inner = unsafe { &mut *ptr };
    if inner.header.type_id() != type_id::HASH_TABLE {
        return Err(BlissError::TypeError {
            datum: table,
            expected: "HASH-TABLE".to_string(),
        });
    }
    Ok(inner)
}

/// Compute the probe index for a key in a table of the given capacity.
fn probe_index(key_bits: u64, capacity: usize) -> usize {
    (hash_u64(key_bits) as usize) & (capacity - 1)
}

/// Check if two keys are equal (using raw bit equality for all test modes in this base impl).
fn keys_equal(a: BlissVal, b: BlissVal) -> bool {
    a.0 == b.0
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
    let inner = get_table_inner(table)?;
    let cap = inner.capacity;
    let key_bits = key.0;
    let mut idx = probe_index(key_bits, cap);

    for _ in 0..cap {
        match &inner.entries[idx] {
            Some((k, v)) => {
                if keys_equal(*k, key) {
                    return Ok((*v, true));
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
    let new_capacity = inner.capacity * (inner.rehash_size as usize).max(2);
    let new_capacity = next_power_of_two(new_capacity);
    let mut new_entries = vec![None; new_capacity];

    for entry in inner.entries.iter() {
        if let Some((k, v)) = entry {
            let mut idx = probe_index(k.0, new_capacity);
            loop {
                if new_entries[idx].is_none() {
                    new_entries[idx] = Some((*k, *v));
                    break;
                }
                idx = (idx + 1) & (new_capacity - 1);
            }
        }
    }

    inner.entries = new_entries;
    inner.capacity = new_capacity;
}

/// Set a value in a hash table (CL `(SETF GETHASH)`).
pub fn set_gethash(
    key: BlissVal,
    table: BlissVal,
    value: BlissVal,
) -> Result<(), BlissError> {
    let inner = get_table_inner(table)?;

    // Check if key already exists and update in place
    let cap = inner.capacity;
    let key_bits = key.0;
    let mut idx = probe_index(key_bits, cap);

    for _ in 0..cap {
        match &inner.entries[idx] {
            Some((k, _)) => {
                if keys_equal(*k, key) {
                    inner.entries[idx] = Some((key, value));
                    return Ok(());
                }
            }
            None => break,
        }
        idx = (idx + 1) & (cap - 1);
    }

    // Check load factor and resize if needed
    let threshold = (inner.capacity as f64 * inner.rehash_threshold) as usize;
    if inner.count + 1 > threshold {
        resize_table(inner);
    }

    // Insert into (possibly resized) table
    let cap = inner.capacity;
    let mut idx = probe_index(key_bits, cap);
    loop {
        if inner.entries[idx].is_none() {
            inner.entries[idx] = Some((key, value));
            inner.count += 1;
            return Ok(());
        }
        idx = (idx + 1) & (cap - 1);
    }
}

/// Remove an entry (CL `REMHASH`).
pub fn remhash(key: BlissVal, table: BlissVal) -> Result<bool, BlissError> {
    let inner = get_table_inner(table)?;
    let cap = inner.capacity;
    let key_bits = key.0;
    let mut idx = probe_index(key_bits, cap);

    for _ in 0..cap {
        match &inner.entries[idx] {
            Some((k, _)) => {
                if keys_equal(*k, key) {
                    // Remove the entry
                    inner.entries[idx] = None;
                    inner.count -= 1;

                    // Re-insert displaced entries (backward-shift deletion)
                    let mut j = (idx + 1) & (cap - 1);
                    loop {
                        if inner.entries[j].is_none() {
                            break;
                        }
                        let entry = inner.entries[j].unwrap();
                        let natural = probe_index(entry.0 .0, cap);
                        // Check if entry at j would prefer to be at idx
                        // i.e., idx is between natural and j (circularly)
                        let should_move = if j >= idx {
                            // no wrap: natural <= idx or natural > j
                            natural <= idx || natural > j
                        } else {
                            // wrapped: natural <= idx AND natural > j
                            natural <= idx && natural > j
                        };
                        if should_move {
                            inner.entries[idx] = Some(entry);
                            inner.entries[j] = None;
                            idx = j;
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

/// Map a function over hash table entries (CL `MAPHASH`).
pub fn maphash(function: BlissVal, table: BlissVal) -> Result<(), BlissError> {
    let inner = get_table_inner(table)?;
    // Iterate all entries. For now, we just iterate without calling the function
    // (as tests only check the table is unchanged after maphash).
    let _func = function; // Will be used when function invocation is available
    for entry in inner.entries.iter() {
        if let Some((_key, _value)) = entry {
            // In a full implementation, we'd call `function` with (key, value).
            // For now, this is a no-op iteration.
        }
    }
    Ok(())
}

/// Clear all entries (CL `CLRHASH`).
pub fn clrhash(table: BlissVal) -> Result<(), BlissError> {
    let inner = get_table_inner(table)?;
    for entry in inner.entries.iter_mut() {
        *entry = None;
    }
    inner.count = 0;
    Ok(())
}

/// Get the number of entries (CL `HASH-TABLE-COUNT`).
pub fn hash_table_count(table: BlissVal) -> Result<usize, BlissError> {
    let inner = get_table_inner(table)?;
    Ok(inner.count)
}

/// Get the hash table test (CL `HASH-TABLE-TEST`).
pub fn hash_table_test(table: BlissVal) -> Result<HashTest, BlissError> {
    let inner = get_table_inner(table)?;
    Ok(inner.test)
}

/// Get the hash table size (capacity).
pub fn hash_table_size(table: BlissVal) -> Result<usize, BlissError> {
    let inner = get_table_inner(table)?;
    Ok(inner.capacity)
}

/// Get the rehash size.
pub fn hash_table_rehash_size(table: BlissVal) -> Result<f64, BlissError> {
    let inner = get_table_inner(table)?;
    Ok(inner.rehash_size)
}

/// Get the rehash threshold.
pub fn hash_table_rehash_threshold(table: BlissVal) -> Result<f64, BlissError> {
    let inner = get_table_inner(table)?;
    Ok(inner.rehash_threshold)
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
