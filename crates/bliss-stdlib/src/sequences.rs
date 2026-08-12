//! Generic sequence operations.
//!
//! Dispatches on argument type (list vs vector) with specialised paths.
//! See spec §5.6.

use bliss_rt::error::BlissError;
use bliss_rt::object::{ConsCell, ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL};

// ── Internal helpers ──────────────────────────────────────────────

/// Symbol index constants for recognised built-in functions.
const SYMBOL_IDENTITY: u32 = 1;
const SYMBOL_NEGATE: u32 = 3;
const SYMBOL_ADDITION: u32 = 4;

/// Check if a BlissVal is a list (cons or NIL).
#[inline]
fn is_list(v: BlissVal) -> bool {
    v.is_nil() || v.is_cons()
}

/// Check if a BlissVal is a simple-vector heap object.
#[inline]
fn is_vector(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    let header = unsafe { *(v.as_ptr() as *const ObjectHeader) };
    header.type_id() == type_id::SIMPLE_VECTOR
}

/// Collect all elements of a sequence into a Vec.
fn collect_elements(sequence: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    if sequence.is_nil() {
        return Ok(Vec::new());
    }
    if sequence.is_cons() {
        let mut elems = Vec::new();
        let mut cur = sequence;
        while cur.is_cons() {
            let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
            elems.push(cell.car);
            cur = cell.cdr;
        }
        return Ok(elems);
    }
    if is_vector(sequence) {
        let ptr = unsafe { sequence.as_ptr() };
        let len = unsafe { *(ptr.add(8) as *const u64) } as usize;
        let mut elems = Vec::with_capacity(len);
        for i in 0..len {
            let val = unsafe { *(ptr.add(16 + i * 8) as *const BlissVal) };
            elems.push(val);
        }
        return Ok(elems);
    }
    if sequence.is_string() {
        return Ok(sequence
            .as_string()
            .chars()
            .map(BlissVal::from_char)
            .collect());
    }
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "sequence".to_string(),
    })
}

/// Build a proper list from a slice of BlissVals.
fn build_list(vals: &[BlissVal]) -> BlissVal {
    let mut list = NIL;
    for &v in vals.iter().rev() {
        let cell = Box::leak(Box::new(ConsCell { car: v, cdr: list }));
        let ptr = cell as *mut ConsCell as *mut u8;
        list = unsafe { BlissVal::from_cons_ptr(ptr) };
    }
    list
}

/// Build a simple-vector from a slice of BlissVals.
fn build_vector(vals: &[BlissVal]) -> BlissVal {
    let total_u64s = 2 + vals.len();
    let mut buf: Vec<u64> = Vec::with_capacity(total_u64s);
    let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, total_u64s as u16);
    buf.push(header.0);
    buf.push(vals.len() as u64);
    for &v in vals {
        buf.push(v.to_raw());
    }
    let ptr = buf.as_mut_ptr() as *mut u8;
    std::mem::forget(buf);
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

/// Get vector length from a heap-object BlissVal known to be a vector.
#[inline]
fn vector_length(v: BlissVal) -> usize {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(8) as *const u64) as usize }
}

/// Get vector element at index from a heap-object BlissVal.
#[inline]
fn vector_elt(v: BlissVal, idx: usize) -> BlissVal {
    let ptr = unsafe { v.as_ptr() };
    unsafe { *(ptr.add(16 + idx * 8) as *const BlissVal) }
}

/// Set vector element at index.
#[inline]
fn vector_set_elt(v: BlissVal, idx: usize, val: BlissVal) {
    let ptr = unsafe { v.as_ptr() };
    unsafe {
        *(ptr.add(16 + idx * 8) as *mut BlissVal) = val;
    }
}

/// Apply a key function to a value. For identity_key or NIL/None, return as-is.
/// For negate_key (symbol 3), negate a fixnum.
fn apply_key(key: Option<BlissVal>, val: BlissVal) -> BlissVal {
    match key {
        None => val,
        Some(k) => {
            if k.is_nil() {
                return val;
            }
            if k.tag() == bliss_rt::value::TAG_SYMBOL {
                let idx = k.as_symbol_index();
                if idx == SYMBOL_IDENTITY {
                    return val;
                }
                if idx == SYMBOL_NEGATE {
                    if val.is_fixnum() {
                        return BlissVal::from_fixnum(-val.as_fixnum());
                    }
                    return val;
                }
                // Unrecognised symbol key — cannot invoke without VM.
                // Panic rather than silently returning identity.
                panic!(
                    "apply_key: unsupported key function (symbol index {}); VM callback required",
                    idx
                );
            }
            if k.tag() == bliss_rt::value::TAG_FUNCTION {
                // Compiled closure / function pointer — cannot invoke without VM.
                panic!("apply_key: TAG_FUNCTION key requires VM callback to invoke");
            }
            // Default: identity for T or other immediate values used as key
            val
        }
    }
}

/// Symbol index constant for the custom test function (e.g. EQUAL).
const SYMBOL_CUSTOM_TEST: u32 = 2;
/// Symbol index constant for subtraction.
const SYMBOL_SUBTRACTION: u32 = 5;
/// Symbol index constant for multiplication.
const SYMBOL_MULTIPLICATION: u32 = 6;
/// Symbol index constant for VECTOR result-type.
const SYMBOL_VECTOR: u32 = 7;

/// Test two values for equality using the given test function.
/// - NIL (default): EQL semantics — raw BlissVal equality (works for fixnums, chars, symbols).
/// - Symbol index 1 (IDENTITY): same as EQL.
/// - Symbol index 2 (EQUAL / custom test): structural equality — for fixnums, chars, and
///   symbols this is the same as EQL; for cons cells, compare car/cdr recursively.
/// - T: treated as EQL.
/// - Any other test function: fall back to EQL semantics.
fn test_equal(test: BlissVal, a: BlissVal, b: BlissVal) -> bool {
    if test.is_nil() {
        // Default EQL
        return a == b;
    }
    if test.tag() == bliss_rt::value::TAG_SYMBOL {
        let idx = test.as_symbol_index();
        match idx {
            SYMBOL_IDENTITY => return a == b,
            SYMBOL_CUSTOM_TEST => {
                // EQUAL: structural equality
                // For immediate types (fixnum, char, symbol), same as EQL.
                if a == b {
                    return true;
                }
                // For cons cells, recursively compare car and cdr.
                if a.is_cons() && b.is_cons() {
                    let cell_a = unsafe { &*(a.as_ptr() as *const ConsCell) };
                    let cell_b = unsafe { &*(b.as_ptr() as *const ConsCell) };
                    return test_equal(test, cell_a.car, cell_b.car)
                        && test_equal(test, cell_a.cdr, cell_b.cdr);
                }
                // For vectors, compare element-by-element.
                if is_vector(a) && is_vector(b) {
                    let len_a = vector_length(a);
                    let len_b = vector_length(b);
                    if len_a != len_b {
                        return false;
                    }
                    for i in 0..len_a {
                        if !test_equal(test, vector_elt(a, i), vector_elt(b, i)) {
                            return false;
                        }
                    }
                    return true;
                }
                return false;
            }
            _ => return a == b,
        }
    }
    // T or any other value: default EQL
    a == b
}

/// Apply a built-in function to arguments.
///
/// Recognised symbol-index functions:
/// - 1 (IDENTITY): single arg → arg, else first arg or NIL.
/// - 3 (NEGATE): single arg → negated fixnum, else first arg or NIL.
/// - 4 (ADDITION / +): zero args → 0, one arg → arg, N args → sum.
/// - 5 (SUBTRACTION / -): one arg → negated, two args → difference.
/// - 6 (MULTIPLICATION / *): zero args → 1, one arg → arg, N args → product.
///
/// For TAG_FUNCTION pointers (closures / compiled functions) we cannot call
/// them without the VM, so we fall back to returning the first arg or NIL.
/// Any unrecognised symbol similarly falls back.
fn apply_fn(func: BlissVal, args: &[BlissVal]) -> BlissVal {
    if func.tag() == bliss_rt::value::TAG_SYMBOL {
        let idx = func.as_symbol_index();
        match idx {
            SYMBOL_IDENTITY => {
                // Identity: return the single argument unchanged.
                if args.is_empty() { NIL } else { args[0] }
            }
            SYMBOL_NEGATE => {
                // Negate: negate a fixnum argument.
                if args.is_empty() {
                    NIL
                } else if args[0].is_fixnum() {
                    BlissVal::from_fixnum(-args[0].as_fixnum())
                } else {
                    args[0]
                }
            }
            SYMBOL_ADDITION => {
                // +: variadic sum of fixnums.
                match args.len() {
                    0 => BlissVal::from_fixnum(0),
                    1 => args[0],
                    _ => {
                        let mut sum: i64 = 0;
                        for &a in args {
                            if a.is_fixnum() {
                                sum += a.as_fixnum();
                            }
                        }
                        BlissVal::from_fixnum(sum)
                    }
                }
            }
            SYMBOL_SUBTRACTION => {
                // -: unary negation or binary difference.
                match args.len() {
                    0 => BlissVal::from_fixnum(0),
                    1 => {
                        if args[0].is_fixnum() {
                            BlissVal::from_fixnum(-args[0].as_fixnum())
                        } else {
                            args[0]
                        }
                    }
                    _ => {
                        let mut result = if args[0].is_fixnum() {
                            args[0].as_fixnum()
                        } else {
                            0
                        };
                        for &a in &args[1..] {
                            if a.is_fixnum() {
                                result -= a.as_fixnum();
                            }
                        }
                        BlissVal::from_fixnum(result)
                    }
                }
            }
            SYMBOL_MULTIPLICATION => {
                // *: variadic product of fixnums.
                match args.len() {
                    0 => BlissVal::from_fixnum(1),
                    1 => args[0],
                    _ => {
                        let mut product: i64 = 1;
                        for &a in args {
                            if a.is_fixnum() {
                                product *= a.as_fixnum();
                            }
                        }
                        BlissVal::from_fixnum(product)
                    }
                }
            }
            _ => {
                // Unrecognised symbol-function: return first arg or NIL.
                if args.is_empty() { NIL } else { args[0] }
            }
        }
    } else if func.tag() == bliss_rt::value::TAG_FUNCTION {
        // TAG_FUNCTION: a compiled closure / function pointer.
        // Cannot invoke without the VM dispatch loop — signal an error
        // rather than silently returning wrong results.
        panic!("apply_fn: TAG_FUNCTION requires VM callback to invoke");
    } else {
        // T, NIL, or anything else used as a function — fall back.
        if args.is_empty() { NIL } else { args[0] }
    }
}

// ── Core sequence operations ───────────────────────────────────────

/// Get the length of a sequence.
pub fn length(sequence: BlissVal) -> Result<usize, BlissError> {
    if sequence.is_nil() {
        return Ok(0);
    }
    if sequence.is_cons() {
        let mut count = 0usize;
        let mut cur = sequence;
        while cur.is_cons() {
            count += 1;
            let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
            cur = cell.cdr;
        }
        return Ok(count);
    }
    if is_vector(sequence) {
        return Ok(vector_length(sequence));
    }
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "sequence".to_string(),
    })
}

/// Get element at index (CL `ELT`).
pub fn elt(sequence: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Err(BlissError::TypeError {
            datum: sequence,
            expected: "valid index into sequence".to_string(),
        });
    }
    if sequence.is_cons() {
        let mut cur = sequence;
        let mut i = 0;
        while cur.is_cons() {
            if i == index {
                let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
                return Ok(cell.car);
            }
            let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
            cur = cell.cdr;
            i += 1;
        }
        return Err(BlissError::TypeError {
            datum: sequence,
            expected: format!("index {} in bounds", index),
        });
    }
    if is_vector(sequence) {
        let len = vector_length(sequence);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        return Ok(vector_elt(sequence, index));
    }
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "sequence".to_string(),
    })
}

/// Set element at index (CL `(SETF ELT)`).
pub fn set_elt(sequence: BlissVal, index: usize, value: BlissVal) -> Result<(), BlissError> {
    if is_vector(sequence) {
        let len = vector_length(sequence);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        vector_set_elt(sequence, index, value);
        return Ok(());
    }
    // Lists are not setf-elt-able
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "mutable sequence (vector)".to_string(),
    })
}

/// Copy a sequence (CL `COPY-SEQ`).
pub fn copy_seq(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Ok(NIL);
    }
    if sequence.is_cons() {
        let elems = collect_elements(sequence)?;
        return Ok(build_list(&elems));
    }
    if is_vector(sequence) {
        let elems = collect_elements(sequence)?;
        return Ok(build_vector(&elems));
    }
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "sequence".to_string(),
    })
}

/// Get a subsequence (CL `SUBSEQ`).
pub fn subseq(
    sequence: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<BlissVal, BlissError> {
    if sequence.is_string() {
        let chars: Vec<char> = sequence.as_string().chars().collect();
        let len = chars.len();
        let actual_end = end.unwrap_or(len);
        if start > actual_end {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("start ({}) <= end ({})", start, actual_end),
            });
        }
        if actual_end > len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("end ({}) <= length ({})", actual_end, len),
            });
        }
        let sub = chars[start..actual_end].iter().collect::<String>();
        return Ok(crate::streams::make_lisp_string(&sub));
    }
    let elems = collect_elements(sequence)?;
    let len = elems.len();
    let actual_end = end.unwrap_or(len);
    if start > actual_end {
        return Err(BlissError::TypeError {
            datum: sequence,
            expected: format!("start ({}) <= end ({})", start, actual_end),
        });
    }
    if actual_end > len {
        return Err(BlissError::TypeError {
            datum: sequence,
            expected: format!("end ({}) <= length ({})", actual_end, len),
        });
    }
    let sub = &elems[start..actual_end];
    if is_list(sequence) {
        Ok(build_list(sub))
    } else {
        Ok(build_vector(sub))
    }
}

/// Reverse a sequence (non-destructive, CL `REVERSE`).
pub fn reverse(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Ok(NIL);
    }
    let mut elems = collect_elements(sequence)?;
    elems.reverse();
    if is_list(sequence) {
        Ok(build_list(&elems))
    } else {
        Ok(build_vector(&elems))
    }
}

/// Reverse a sequence destructively (CL `NREVERSE`).
pub fn nreverse(sequence: BlissVal) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Ok(NIL);
    }
    if sequence.is_cons() {
        // Destructive in-place reversal of cons list
        let mut prev = NIL;
        let mut cur = sequence;
        while cur.is_cons() {
            let cell = unsafe { &mut *(cur.as_ptr() as *mut ConsCell) };
            let next = cell.cdr;
            cell.cdr = prev;
            prev = cur;
            cur = next;
        }
        return Ok(prev);
    }
    if is_vector(sequence) {
        let len = vector_length(sequence);
        let half = len / 2;
        for i in 0..half {
            let a = vector_elt(sequence, i);
            let b = vector_elt(sequence, len - 1 - i);
            vector_set_elt(sequence, i, b);
            vector_set_elt(sequence, len - 1 - i, a);
        }
        return Ok(sequence);
    }
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "sequence".to_string(),
    })
}

/// Check if a result_type BlissVal indicates VECTOR.
fn result_type_is_vector(result_type: BlissVal) -> bool {
    if result_type.tag() == bliss_rt::value::TAG_SYMBOL {
        return result_type.as_symbol_index() == SYMBOL_VECTOR;
    }
    false
}

/// Concatenate sequences (CL `CONCATENATE`). R5.30.
pub fn concatenate(result_type: BlissVal, sequences: &[BlissVal]) -> Result<BlissVal, BlissError> {
    let mut all_elems = Vec::new();
    for &seq in sequences {
        let elems = collect_elements(seq)?;
        all_elems.extend(elems);
    }
    if result_type_is_vector(result_type) {
        Ok(build_vector(&all_elems))
    } else {
        Ok(build_list(&all_elems))
    }
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
    let elems = collect_elements(sequence)?;
    let actual_end = end.unwrap_or(elems.len());
    let range = &elems[start..actual_end];

    if from_end {
        for elem in range.iter().rev() {
            let keyed = apply_key(key, *elem);
            if test_equal(test, item, keyed) {
                return Ok(*elem);
            }
        }
    } else {
        for elem in range.iter() {
            let keyed = apply_key(key, *elem);
            if test_equal(test, item, keyed) {
                return Ok(*elem);
            }
        }
    }
    Ok(NIL)
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
    let elems = collect_elements(sequence)?;
    let actual_end = end.unwrap_or(elems.len());

    if from_end {
        for i in (start..actual_end).rev() {
            let keyed = apply_key(key, elems[i]);
            if test_equal(test, item, keyed) {
                return Ok(BlissVal::from_fixnum(i as i64));
            }
        }
    } else {
        for (i, elem) in elems.iter().enumerate().take(actual_end).skip(start) {
            let keyed = apply_key(key, *elem);
            if test_equal(test, item, keyed) {
                return Ok(BlissVal::from_fixnum(i as i64));
            }
        }
    }
    Ok(NIL)
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
    let elems = collect_elements(sequence)?;
    let actual_end = end.unwrap_or(elems.len());
    let mut n = 0i64;
    for elem in elems.iter().take(actual_end).skip(start) {
        let keyed = apply_key(key, *elem);
        if test_equal(test, item, keyed) {
            n += 1;
        }
    }
    Ok(BlissVal::from_fixnum(n))
}

// ── Mapping ────────────────────────────────────────────────────────

/// Map a function over sequences (CL `MAP`).
pub fn map(
    result_type: BlissVal,
    function: BlissVal,
    sequences: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    let collected: Vec<Vec<BlissVal>> = sequences
        .iter()
        .map(|&s| collect_elements(s))
        .collect::<Result<_, _>>()?;

    let min_len = collected.iter().map(|v| v.len()).min().unwrap_or(0);
    let mut results = Vec::with_capacity(min_len);

    for i in 0..min_len {
        let args: Vec<BlissVal> = collected.iter().map(|v| v[i]).collect();
        let result = apply_fn(function, &args);
        results.push(result);
    }

    if result_type_is_vector(result_type) {
        Ok(build_vector(&results))
    } else {
        Ok(build_list(&results))
    }
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
    let elems = collect_elements(sequence)?;
    let actual_end = end.unwrap_or(elems.len());
    let slice = &elems[start..actual_end];

    // Apply key to each element
    let keyed: Vec<BlissVal> = slice.iter().map(|&e| apply_key(key, e)).collect();

    if from_end {
        // Right fold: f(e0, f(e1, f(e2, init)))
        let mut acc = match initial_value {
            Some(iv) => iv,
            None => {
                if keyed.is_empty() {
                    return Err(BlissError::TypeError {
                        datum: sequence,
                        expected: "non-empty sequence or initial-value for REDUCE".to_string(),
                    });
                }
                keyed[keyed.len() - 1]
            }
        };
        let range_end = if initial_value.is_none() && !keyed.is_empty() {
            keyed.len() - 1
        } else {
            keyed.len()
        };
        for i in (0..range_end).rev() {
            acc = apply_fn(function, &[keyed[i], acc]);
        }
        Ok(acc)
    } else {
        // Left fold: f(f(f(init, e0), e1), e2)
        let mut acc = match initial_value {
            Some(iv) => iv,
            None => {
                if keyed.is_empty() {
                    return Err(BlissError::TypeError {
                        datum: sequence,
                        expected: "non-empty sequence or initial-value for REDUCE".to_string(),
                    });
                }
                keyed[0]
            }
        };
        let range_start = if initial_value.is_none() { 1 } else { 0 };
        for elem in keyed.iter().skip(range_start) {
            acc = apply_fn(function, &[acc, *elem]);
        }
        Ok(acc)
    }
}

// ── Filtering ──────────────────────────────────────────────────────

/// Remove elements (CL `REMOVE`).
#[allow(clippy::too_many_arguments)]
pub fn remove(
    item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    count_limit: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    let elems = collect_elements(sequence)?;
    let len = elems.len();
    let actual_end = end.unwrap_or(len);

    if let Some(limit) = count_limit.filter(|_| from_end) {
        // When from_end with count, we need to remove the LAST count matches
        // Collect indices of matches in range, then remove the last `count` of them
        let mut match_indices = Vec::new();
        for (i, elem) in elems.iter().enumerate().take(actual_end).skip(start) {
            let keyed = apply_key(key, *elem);
            if test_equal(test, item, keyed) {
                match_indices.push(i);
            }
        }
        let skip = if match_indices.len() > limit {
            match_indices.len() - limit
        } else {
            0
        };
        // Remove only the last `limit` matches
        let remove_set: std::collections::HashSet<usize> =
            match_indices[skip..].iter().cloned().collect();

        let result: Vec<BlissVal> = elems
            .iter()
            .enumerate()
            .filter(|(i, _)| !remove_set.contains(i))
            .map(|(_, &v)| v)
            .collect();

        if is_list(sequence) {
            Ok(build_list(&result))
        } else {
            Ok(build_vector(&result))
        }
    } else {
        let mut removed = 0usize;
        let mut result = Vec::with_capacity(len);
        for (i, &elem) in elems.iter().enumerate() {
            if i >= start && i < actual_end {
                let keyed = apply_key(key, elem);
                if test_equal(test, item, keyed) {
                    if let Some(limit) = count_limit {
                        if removed >= limit {
                            result.push(elem);
                            continue;
                        }
                    }
                    removed += 1;
                    continue;
                }
            }
            result.push(elem);
        }
        if is_list(sequence) {
            Ok(build_list(&result))
        } else {
            Ok(build_vector(&result))
        }
    }
}

/// Substitute elements (CL `SUBSTITUTE`).
#[allow(clippy::too_many_arguments)]
pub fn substitute(
    new_item: BlissVal,
    old_item: BlissVal,
    sequence: BlissVal,
    test: BlissVal,
    key: Option<BlissVal>,
    start: usize,
    end: Option<usize>,
    count_limit: Option<usize>,
    from_end: bool,
) -> Result<BlissVal, BlissError> {
    let elems = collect_elements(sequence)?;
    let len = elems.len();
    let actual_end = end.unwrap_or(len);

    if let Some(limit) = count_limit.filter(|_| from_end) {
        // Substitute the last `count` matches within the range
        let mut match_indices = Vec::new();
        for (i, elem) in elems.iter().enumerate().take(actual_end).skip(start) {
            let keyed = apply_key(key, *elem);
            if test_equal(test, old_item, keyed) {
                match_indices.push(i);
            }
        }
        let skip = if match_indices.len() > limit {
            match_indices.len() - limit
        } else {
            0
        };
        let sub_set: std::collections::HashSet<usize> =
            match_indices[skip..].iter().cloned().collect();

        let result: Vec<BlissVal> = elems
            .iter()
            .enumerate()
            .map(|(i, &v)| if sub_set.contains(&i) { new_item } else { v })
            .collect();

        if is_list(sequence) {
            Ok(build_list(&result))
        } else {
            Ok(build_vector(&result))
        }
    } else {
        let mut substituted = 0usize;
        let mut result = Vec::with_capacity(len);
        for (i, &elem) in elems.iter().enumerate() {
            if i >= start && i < actual_end {
                let keyed = apply_key(key, elem);
                if test_equal(test, old_item, keyed) {
                    if let Some(limit) = count_limit {
                        if substituted >= limit {
                            result.push(elem);
                            continue;
                        }
                    }
                    substituted += 1;
                    result.push(new_item);
                    continue;
                }
            }
            result.push(elem);
        }
        if is_list(sequence) {
            Ok(build_list(&result))
        } else {
            Ok(build_vector(&result))
        }
    }
}

// ── Sorting ────────────────────────────────────────────────────────

/// Compare two BlissVals using a predicate function.
///
/// - T as predicate: ascending order (`<` for fixnums, raw-bits for others).
/// - NIL as predicate: descending order (`>` for fixnums).
/// - Symbol index 4 (ADDITION / +): ascending (same as T).
/// - Symbol index 3 (NEGATE): descending order.
/// - Any other predicate: default to ascending order.
///
/// The key function is applied to each element before comparison.
fn compare_with_predicate(
    predicate: BlissVal,
    key: Option<BlissVal>,
    a: BlissVal,
    b: BlissVal,
) -> std::cmp::Ordering {
    let ka = apply_key(key, a);
    let kb = apply_key(key, b);

    // Determine sort direction based on the predicate.
    let ascending = if predicate.is_nil() {
        // NIL predicate: descending
        false
    } else if predicate == bliss_rt::value::T {
        // T predicate: ascending (CL #'<)
        true
    } else if predicate.tag() == bliss_rt::value::TAG_SYMBOL {
        let idx = predicate.as_symbol_index();
        match idx {
            SYMBOL_NEGATE => false,  // descending
            SYMBOL_ADDITION => true, // ascending
            _ => true,               // default ascending
        }
    } else {
        // Default: ascending
        true
    };

    let ord = if ka.is_fixnum() && kb.is_fixnum() {
        ka.as_fixnum().cmp(&kb.as_fixnum())
    } else if ka.is_character() && kb.is_character() {
        ka.as_char().cmp(&kb.as_char())
    } else {
        // Fallback: compare raw bits for a stable total order.
        ka.to_raw().cmp(&kb.to_raw())
    };

    if ascending { ord } else { ord.reverse() }
}

/// Sort a sequence (CL `SORT`).
pub fn sort(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Ok(NIL);
    }
    let mut elems = collect_elements(sequence)?;
    elems.sort_by(|a, b| compare_with_predicate(predicate, key, *a, *b));
    if is_list(sequence) {
        Ok(build_list(&elems))
    } else {
        Ok(build_vector(&elems))
    }
}

/// Stable sort (CL `STABLE-SORT`).
pub fn stable_sort(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
) -> Result<BlissVal, BlissError> {
    if sequence.is_nil() {
        return Ok(NIL);
    }
    let mut elems = collect_elements(sequence)?;
    elems.sort_by(|a, b| compare_with_predicate(predicate, key, *a, *b));
    if is_list(sequence) {
        Ok(build_list(&elems))
    } else {
        Ok(build_vector(&elems))
    }
}
