//! Generic sequence operations.
//!
//! Dispatches on argument type (list vs vector) with specialised paths.
//! See spec §5.6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Core sequence operations ───────────────────────────────────────

/// Get the length of a sequence.
pub fn length(sequence: BlissVal) -> Result<usize, BlissError> {
    unimplemented!("length")
}

/// Get element at index (CL `ELT`).
pub fn elt(sequence: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    unimplemented!("elt")
}

/// Set element at index (CL `(SETF ELT)`).
pub fn set_elt(sequence: BlissVal, index: usize, value: BlissVal) -> Result<(), BlissError> {
    unimplemented!("set_elt")
}

/// Copy a sequence (CL `COPY-SEQ`).
pub fn copy_seq(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("copy_seq")
}

/// Get a subsequence (CL `SUBSEQ`).
pub fn subseq(
    sequence: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<BlissVal, BlissError> {
    unimplemented!("subseq")
}

/// Reverse a sequence (non-destructive, CL `REVERSE`).
pub fn reverse(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("reverse")
}

/// Reverse a sequence destructively (CL `NREVERSE`).
pub fn nreverse(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("nreverse")
}

/// Concatenate sequences (CL `CONCATENATE`). R5.30.
pub fn concatenate(
    result_type: BlissVal,
    sequences: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    unimplemented!("concatenate")
}

// ── Search and comparison ──────────────────────────────────────────

/// Find an element (CL `FIND`).
pub fn find(
    item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    unimplemented!("find")
}

/// Find the position of an element (CL `POSITION`).
pub fn position(
    item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    unimplemented!("position")
}

/// Count occurrences (CL `COUNT`).
pub fn count(
    item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
) -> Result<BlissVal, BlissError> {
    unimplemented!("count")
}

// ── Mapping ────────────────────────────────────────────────────────

/// Map a function over sequences (CL `MAP`).
pub fn map(
    result_type: BlissVal,
    function: BlissVal,
    sequences: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    unimplemented!("map")
}

/// Reduce a sequence (CL `REDUCE`).
pub fn reduce(
    function: BlissVal,
    sequence: BlissVal,
    initial_value: Option<BlissVal>,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    unimplemented!("reduce")
}

// ── Filtering ──────────────────────────────────────────────────────

/// Remove elements (CL `REMOVE`).
pub fn remove(
    item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    count: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    unimplemented!("remove")
}

/// Substitute elements (CL `SUBSTITUTE`).
pub fn substitute(
    new_item: BlissVal,
    old_item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    count: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    unimplemented!("substitute")
}

// ── Sorting ────────────────────────────────────────────────────────

/// Sort a sequence (CL `SORT`). Uses introsort for vectors, merge sort for lists.
/// R5.28.
pub fn sort(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
) -> Result<BlissVal, BlissError> {
    unimplemented!("sort")
}

/// Stable sort (CL `STABLE-SORT`). Uses timsort for vectors, merge sort for lists.
/// R5.28.
pub fn stable_sort(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
) -> Result<BlissVal, BlissError> {
    unimplemented!("stable_sort")
}
