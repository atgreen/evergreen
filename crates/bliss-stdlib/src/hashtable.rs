//! Hash tables — Robin Hood hashing with open addressing.
//!
//! See spec §5.7.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

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
        unimplemented!("MakeHashTableOptions::default")
    }
}

// ── Hash table operations ──────────────────────────────────────────

/// Create a new hash table (CL `MAKE-HASH-TABLE`). R5.31, R5.33.
pub fn make_hash_table(options: &MakeHashTableOptions) -> Result<BlissVal, BlissError> {
    unimplemented!("make_hash_table")
}

/// Get a value from a hash table (CL `GETHASH`).
/// Returns `(value, present-p)`.
pub fn gethash(key: BlissVal, table: BlissVal, default: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    unimplemented!("gethash")
}

/// Set a value in a hash table (CL `(SETF GETHASH)`).
pub fn set_gethash(key: BlissVal, table: BlissVal, value: BlissVal) -> Result<(), BlissError> {
    unimplemented!("set_gethash")
}

/// Remove an entry (CL `REMHASH`).
pub fn remhash(key: BlissVal, table: BlissVal) -> Result<bool, BlissError> {
    unimplemented!("remhash")
}

/// Map a function over hash table entries (CL `MAPHASH`).
pub fn maphash(function: BlissVal, table: BlissVal) -> Result<(), BlissError> {
    unimplemented!("maphash")
}

/// Clear all entries (CL `CLRHASH`).
pub fn clrhash(table: BlissVal) -> Result<(), BlissError> {
    unimplemented!("clrhash")
}

/// Get the number of entries (CL `HASH-TABLE-COUNT`).
pub fn hash_table_count(table: BlissVal) -> Result<usize, BlissError> {
    unimplemented!("hash_table_count")
}

/// Get the hash table test (CL `HASH-TABLE-TEST`).
pub fn hash_table_test(table: BlissVal) -> Result<HashTest, BlissError> {
    unimplemented!("hash_table_test")
}

/// Get the hash table size (capacity).
pub fn hash_table_size(table: BlissVal) -> Result<usize, BlissError> {
    unimplemented!("hash_table_size")
}

/// Get the rehash size.
pub fn hash_table_rehash_size(table: BlissVal) -> Result<f64, BlissError> {
    unimplemented!("hash_table_rehash_size")
}

/// Get the rehash threshold.
pub fn hash_table_rehash_threshold(table: BlissVal) -> Result<f64, BlissError> {
    unimplemented!("hash_table_rehash_threshold")
}

// ── SXHASH ─────────────────────────────────────────────────────────

/// Compute the hash code for an object (CL `SXHASH`). R5.32.
/// Returns a non-negative fixnum.
pub fn sxhash(object: BlissVal) -> BlissVal {
    unimplemented!("sxhash")
}
