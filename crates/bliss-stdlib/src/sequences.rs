//! Generic sequence operations.
//!
//! Dispatches on argument type (list vs vector) with specialised paths.
//! See spec §5.6.

use bliss_rt::error::BlissError;
use bliss_rt::object::{ConsCell, ObjectHeader, type_id};
use bliss_rt::value::{BlissVal, NIL, T};

// ── Internal helpers ──────────────────────────────────────────────

/// Symbol index constants for recognised built-in functions.
const SYMBOL_IDENTITY: u32 = 1;
const SYMBOL_NEGATE: u32 = 3;
const SYMBOL_ADDITION: u32 = 4;

/// Mutate character `index` of the heap string `s` in place, returning the
/// stored character. Only real heap strings are mutable: registry-backed string
/// sentinels (a namestring hash wearing TAG_HEAP_OBJECT) are not real pointers,
/// and interning-shared literals must not be aliased-mutated, so both are
/// rejected. The write re-encodes the string with the replacement character and
/// stores it back if it still fits the allocated buffer (true for the common
/// same-byte-width case, e.g. ASCII), updating the length header.
pub fn string_set_char(s: BlissVal, index: usize, ch: BlissVal) -> Result<BlissVal, BlissError> {
    if crate::pathnames::registered_string(s).is_some() {
        return Err(BlissError::Internal(
            "cannot modify an interned string literal".into(),
        ));
    }
    if !s.is_heap_object() || !s.is_string() {
        return Err(BlissError::TypeError {
            datum: s,
            expected: "mutable string".into(),
        });
    }
    if !ch.is_character() {
        return Err(BlissError::TypeError {
            datum: ch,
            expected: "character".into(),
        });
    }
    let new_char = ch.as_char();
    unsafe {
        let ptr = s.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        let capacity = (header.size_units() as usize) * 8 - 16;
        let len = *((ptr as *const u64).add(1)) as usize;
        let bytes = std::slice::from_raw_parts(ptr.add(16), len);
        let text = std::str::from_utf8(bytes)
            .map_err(|_| BlissError::StreamError("invalid UTF-8 in string".into()))?;
        let mut chars: Vec<char> = text.chars().collect();
        if index >= chars.len() {
            return Err(BlissError::Internal(format!(
                "index {index} out of bounds for string of length {}",
                chars.len()
            )));
        }
        chars[index] = new_char;
        let updated: String = chars.into_iter().collect();
        let new_bytes = updated.as_bytes();
        if new_bytes.len() > capacity {
            return Err(BlissError::Internal(
                "string mutation would exceed the allocated buffer".into(),
            ));
        }
        std::ptr::copy_nonoverlapping(new_bytes.as_ptr(), ptr.add(16), new_bytes.len());
        *((ptr as *mut u64).add(1)) = new_bytes.len() as u64;
    }
    Ok(ch)
}

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
    // A registry-backed string sentinel is heap-tagged but its bits are a hash,
    // not a real pointer — recognise it via the registry rather than
    // dereferencing (which would segfault). A string is never a vector.
    if crate::pathnames::registered_string(v).is_some() {
        return false;
    }
    let header = unsafe { *(v.as_ptr() as *const ObjectHeader) };
    header.type_id() == type_id::SIMPLE_VECTOR
}

/// The character content of a string value — a real heap string OR a
/// registry-backed sentinel (whose bits are a hash, never dereferenced) — or
/// None for a non-string. Pathnames are excluded (they are not sequences).
fn string_content(v: BlissVal) -> Option<String> {
    if crate::pathnames::is_pathname(v) {
        return None;
    }
    if let Some(s) = crate::pathnames::registered_string(v) {
        return Some(s);
    }
    // A character-typed COMPLEX_ARRAY is a (fill-pointer / adjustable) STRING and
    // is a string sequence: materialise its active characters so SUBSEQ/REVERSE/
    // COPY-SEQ return a STRING (not a vector) for it, matching STRINGP (bliss-w5t).
    // Callers that must distinguish a fill-pointer vector from a string by shape
    // (LENGTH, ELT) test is_complex_vector BEFORE reaching here, so this does not
    // change their behaviour.
    if let Some(s) = cvec_char_contents(v) {
        return Some(s);
    }
    v.is_string().then(|| v.as_string())
}

/// True if `v` is a character string usable as a sequence — a real string, and
/// NOT a pathname. Pathnames are registry-backed values whose BlissVal can pass
/// `is_string()` (they carry a namestring), but they are not sequences; treating
/// one as a string in LENGTH/ELT reads a non-string layout and crashes or walks
/// off the end (bliss-lb6). STRINGP already excludes pathnames the same way.
#[inline]
fn is_char_seq(v: BlissVal) -> bool {
    string_content(v).is_some()
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
    if is_complex_vector(sequence) {
        // A fill-pointer / adjustable vector's active elements are the prefix
        // (0..fill-pointer) of its backing storage — the same view LENGTH and ELT
        // present. This is what lets SUBSEQ/REVERSE/CONCATENATE/COPY-SEQ accept
        // them (bliss-w5t); a char-typed complex vector (a fill-pointer STRING)
        // stores CHARACTER values here, so collecting the storage prefix is
        // correct for both general and character complex vectors.
        let storage = cvec_storage(sequence);
        let len = cvec_fill_pointer(sequence);
        let mut elems = Vec::with_capacity(len);
        for i in 0..len {
            elems.push(vector_elt(storage, i));
        }
        return Ok(elems);
    }
    if is_char_seq(sequence) {
        return Ok(string_content(sequence)
            .unwrap_or_default()
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
    bliss_rt::rooted!(vals = vals.to_vec());
    bliss_rt::rooted!(list = NIL);
    for &v in vals.iter().rev() {
        *list = alloc_cons(v, *list);
    }
    *list
}

fn alloc_cons(car: BlissVal, cdr: BlissVal) -> BlissVal {
    bliss_rt::rooted!(car = car);
    bliss_rt::rooted!(cdr = cdr);
    let body = match bliss_rt::gc::alloc_typed(16, type_id::CONS) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(std::alloc::Layout::new::<ConsCell>()),
    };
    unsafe {
        let cell = body as *mut ConsCell;
        (*cell).car = *car;
        (*cell).cdr = *cdr;
        BlissVal::from_cons_ptr(body)
    }
}

/// Build a simple-vector from a slice of BlissVals (public entry point).
pub fn build_simple_vector(vals: &[BlissVal]) -> BlissVal {
    build_vector(vals)
}

/// Build a simple-vector from a slice of BlissVals.
///
/// Allocated in the shared GC arena (via `alloc_typed`) so the collector traces
/// and reclaims it — vectors used to be `Vec::forget`'d off-heap and leaked
/// forever (bliss-18s). Body layout after the header: `[length:u64 |
/// elements…]`, matching the historical `[header | len | bytes]` scheme the
/// readers and the GC's SIMPLE_VECTOR tracer expect (the length is stored raw).
fn build_vector(vals: &[BlissVal]) -> BlissVal {
    bliss_rt::rooted!(vals = vals.to_vec());
    // Body = one length word + the elements.
    let body_size = 8 + vals.len() * 8;
    if let Some(body) = bliss_rt::gc::alloc_typed(body_size, type_id::SIMPLE_VECTOR) {
        unsafe {
            *(body as *mut u64) = vals.len() as u64;
            for (i, value) in vals.iter().enumerate() {
                *(body.add(8 + i * 8) as *mut u64) = value.to_raw();
            }
            // Value points at the object header (body − 8), like alloc_str.
            return BlissVal::from_heap_ptr(body.sub(8));
        }
    }
    // OOM fallback: a leaked block, so vector allocation never fails.
    let total_u64s = 2 + vals.len();
    let mut buf: Vec<u64> = Vec::with_capacity(total_u64s);
    let header = ObjectHeader::new(type_id::SIMPLE_VECTOR, total_u64s as u16);
    buf.push(header.0);
    buf.push(vals.len() as u64);
    for value in vals.iter() {
        buf.push(value.to_raw());
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

// ── COMPLEX_ARRAY: rank-1 vectors with a fill pointer and/or adjustability ──
//
// Like simple-vectors, these are leaked (off-GC) heap objects (bliss allocates
// vectors with `Vec::forget`, so nothing here interacts with the collector).
// Body layout after the 8-byte header:
//   word 0 (+8):  storage — a SIMPLE_VECTOR of capacity `array-total-size`
//   word 1 (+16): fill-pointer, a fixnum (the active LENGTH)
//   word 2 (+24): adjustable flag (T / NIL)
// The active length is the fill pointer; `array-total-size` is the storage's
// length; growth (`vector-push-extend`) replaces the storage with a larger one.

/// True if `v` is a COMPLEX_ARRAY (fill-pointer / adjustable vector).
pub fn is_complex_vector(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    if crate::pathnames::registered_string(v).is_some() {
        return false;
    }
    let header = unsafe { *(v.as_ptr() as *const ObjectHeader) };
    header.type_id() == type_id::COMPLEX_ARRAY
}

#[inline]
fn cvec_storage(v: BlissVal) -> BlissVal {
    unsafe { *(v.as_ptr().add(8) as *const BlissVal) }
}
#[inline]
fn cvec_set_storage(v: BlissVal, storage: BlissVal) {
    unsafe {
        *(v.as_ptr().add(8) as *mut BlissVal) = storage;
    }
}
/// The fill pointer (= active LENGTH) of a complex vector.
#[inline]
pub fn cvec_fill_pointer(v: BlissVal) -> usize {
    unsafe {
        (*(v.as_ptr().add(16) as *const BlissVal))
            .as_fixnum()
            .max(0) as usize
    }
}
#[inline]
fn cvec_set_fill_pointer_raw(v: BlissVal, n: usize) {
    unsafe {
        *(v.as_ptr().add(16) as *mut BlissVal) = BlissVal::from_fixnum(n as i64);
    }
}
/// Whether a complex vector is adjustable (can grow its storage).
#[inline]
pub fn cvec_adjustable(v: BlissVal) -> bool {
    unsafe { !(*(v.as_ptr().add(24) as *const BlissVal)).is_nil() }
}
/// Whether a complex vector has element-type CHARACTER — i.e. it is a
/// (fill-pointer / adjustable) STRING and must answer STRINGP / TYPEP STRING /
/// print as `"…"`. Encoded as an immediate fixnum tag in body word 3 (1 =
/// character, 0/absent = general T). Word 3 is only present on 4-word
/// COMPLEX_ARRAYs built by `build_complex_vector`; the reader is safe because
/// every COMPLEX_ARRAY this crate allocates now carries it.
#[inline]
pub fn cvec_is_string(v: BlissVal) -> bool {
    unsafe {
        let tag = *(v.as_ptr().add(32) as *const BlissVal);
        tag.is_fixnum() && tag.as_fixnum() == 1
    }
}
/// Materialise the active characters (0..fill-pointer) of a character-typed
/// complex vector into a Rust `String`. Returns `None` if `v` is not a
/// character-typed complex vector, or if any active slot is not a character
/// (NIL padding beyond an uninitialised element is treated as `\0`).
pub fn cvec_char_contents(v: BlissVal) -> Option<String> {
    if !is_complex_vector(v) || !cvec_is_string(v) {
        return None;
    }
    let storage = cvec_storage(v);
    let n = cvec_fill_pointer(v);
    let mut s = String::with_capacity(n);
    for i in 0..n {
        let e = elt(storage, i).ok()?;
        if e.is_character() {
            s.push(e.as_char());
        } else {
            s.push('\0');
        }
    }
    Some(s)
}
/// `array-total-size` — the capacity of the backing storage.
#[inline]
pub fn cvec_capacity(v: BlissVal) -> usize {
    vector_length(cvec_storage(v))
}

/// Build a COMPLEX_ARRAY. `capacity` is the backing array-total-size;
/// `elements` seed positions `0..elements.len()` (rest NIL); `fill_pointer` is
/// the active length; `adjustable` allows later growth. `element_is_char` marks
/// element-type CHARACTER, i.e. a (fill-pointer / adjustable) STRING.
pub fn build_complex_vector(
    elements: &[BlissVal],
    capacity: usize,
    fill_pointer: usize,
    adjustable: bool,
    element_is_char: bool,
) -> BlissVal {
    let cap = capacity.max(elements.len());
    let mut store: Vec<BlissVal> = Vec::with_capacity(cap);
    store.extend_from_slice(elements);
    store.resize(cap, NIL);
    let storage = build_vector(&store);
    bliss_rt::rooted!(storage = storage);
    let fp = BlissVal::from_fixnum(fill_pointer.min(cap) as i64);
    let adj = if adjustable { T } else { NIL };
    // Element-type tag: immediate fixnum, 1 = CHARACTER (a string), 0 = general.
    let elt = BlissVal::from_fixnum(if element_is_char { 1 } else { 0 });
    // Body = [storage-ref | fill-pointer(fixnum) | adjustable(T/NIL) |
    //         element-type(fixnum)]; only the storage word is a heap reference
    //         (the GC's COMPLEX_ARRAY tracer visits word 0 only).
    let body_size = 4 * 8;
    if let Some(body) = bliss_rt::gc::alloc_typed(body_size, type_id::COMPLEX_ARRAY) {
        unsafe {
            *(body as *mut u64) = (*storage).to_raw();
            *(body.add(8) as *mut u64) = fp.to_raw();
            *(body.add(16) as *mut u64) = adj.to_raw();
            *(body.add(24) as *mut u64) = elt.to_raw();
            return BlissVal::from_heap_ptr(body.sub(8));
        }
    }
    // OOM fallback: a leaked block (header + 4 body words).
    let mut buf: Vec<u64> = Vec::with_capacity(5);
    let header = ObjectHeader::new(type_id::COMPLEX_ARRAY, 5);
    buf.push(header.0);
    buf.push((*storage).to_raw());
    buf.push(fp.to_raw());
    buf.push(adj.to_raw());
    buf.push(elt.to_raw());
    let ptr = buf.as_mut_ptr() as *mut u8;
    std::mem::forget(buf);
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

/// Set the fill pointer of a complex vector (CL `(setf fill-pointer)`), clamped
/// to the backing capacity. Returns the clamped value.
pub fn set_fill_pointer(v: BlissVal, n: usize) -> Result<usize, BlissError> {
    if !is_complex_vector(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "vector with a fill pointer".to_string(),
        });
    }
    let n = n.min(cvec_capacity(v));
    cvec_set_fill_pointer_raw(v, n);
    Ok(n)
}

/// CL `VECTOR-PUSH`: store `value` at the fill pointer and increment it. Returns
/// the index used, or NIL if the vector is full (no extension).
pub fn vector_push(v: BlissVal, value: BlissVal) -> Result<BlissVal, BlissError> {
    if !is_complex_vector(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "vector with a fill pointer".to_string(),
        });
    }
    let fp = cvec_fill_pointer(v);
    if fp >= cvec_capacity(v) {
        return Ok(NIL);
    }
    vector_set_elt(cvec_storage(v), fp, value);
    cvec_set_fill_pointer_raw(v, fp + 1);
    Ok(BlissVal::from_fixnum(fp as i64))
}

/// CL `VECTOR-PUSH-EXTEND`: like VECTOR-PUSH, but grows an adjustable vector's
/// storage when full. Returns the index used.
pub fn vector_push_extend(
    v: BlissVal,
    value: BlissVal,
    extension: Option<usize>,
) -> Result<BlissVal, BlissError> {
    if !is_complex_vector(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "adjustable vector with a fill pointer".to_string(),
        });
    }
    let fp = cvec_fill_pointer(v);
    let cap = cvec_capacity(v);
    if fp >= cap {
        if !cvec_adjustable(v) {
            return Err(BlissError::TypeError {
                datum: v,
                expected: "adjustable vector".to_string(),
            });
        }
        // Grow: default to doubling (min 8), or the requested extension.
        let grow = extension.unwrap_or(cap.max(8));
        let new_cap = cap + grow.max(1);
        let mut store: Vec<BlissVal> = Vec::with_capacity(new_cap);
        for i in 0..cap {
            store.push(vector_elt(cvec_storage(v), i));
        }
        store.resize(new_cap, NIL);
        cvec_set_storage(v, build_vector(&store));
    }
    vector_set_elt(cvec_storage(v), fp, value);
    cvec_set_fill_pointer_raw(v, fp + 1);
    Ok(BlissVal::from_fixnum(fp as i64))
}

/// CL `ADJUST-ARRAY` for a rank-1 complex (fill-pointer / adjustable) vector:
/// ensure the backing storage holds at least `new_size` elements (existing ones
/// preserved, new slots set to `initial_element`), and set the fill pointer to
/// `fill_pointer` (defaulting to `new_size`). Adjusts IN PLACE and returns `v`
/// (cl-ppcre's optimize.lisp/convert.lisp grow adjustable char arrays this way,
/// discarding the result — so in-place is required) (bliss-omw).
pub fn adjust_complex_vector(
    v: BlissVal,
    new_size: usize,
    fill_pointer: Option<usize>,
    initial_element: BlissVal,
) -> Result<BlissVal, BlissError> {
    if !is_complex_vector(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "adjustable vector".to_string(),
        });
    }
    let cap = cvec_capacity(v);
    if new_size > cap {
        let mut store: Vec<BlissVal> = Vec::with_capacity(new_size);
        for i in 0..cap {
            store.push(vector_elt(cvec_storage(v), i));
        }
        store.resize(new_size, initial_element);
        cvec_set_storage(v, build_vector(&store));
    }
    let fp = fill_pointer.unwrap_or(new_size).min(new_size);
    cvec_set_fill_pointer_raw(v, fp);
    Ok(v)
}

/// CL `VECTOR-POP`: decrement the fill pointer and return the element there.
pub fn vector_pop(v: BlissVal) -> Result<BlissVal, BlissError> {
    if !is_complex_vector(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "vector with a fill pointer".to_string(),
        });
    }
    let fp = cvec_fill_pointer(v);
    if fp == 0 {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "non-empty vector".to_string(),
        });
    }
    let val = vector_elt(cvec_storage(v), fp - 1);
    cvec_set_fill_pointer_raw(v, fp - 1);
    Ok(val)
}

/// Collect cons-cell pointers from a proper list.
fn collect_cons_cells(sequence: BlissVal) -> Vec<*mut ConsCell> {
    let mut cells = Vec::new();
    let mut cur = sequence;
    while cur.is_cons() {
        let ptr = unsafe { cur.as_ptr() } as *mut ConsCell;
        cells.push(ptr);
        // Safety: `cur` is a cons cell for the duration of the walk.
        cur = unsafe { (*ptr).cdr };
    }
    cells
}

/// Destructively sort a vector in place.
fn sort_vector_in_place(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
    stable: bool,
) -> BlissVal {
    let ptr = unsafe { sequence.as_ptr() };
    let len = unsafe { *(ptr.add(8) as *const u64) as usize };
    let elems = unsafe { std::slice::from_raw_parts_mut(ptr.add(16) as *mut BlissVal, len) };
    if stable {
        elems.sort_by(|a, b| compare_with_predicate(predicate, key, *a, *b));
    } else {
        elems.sort_unstable_by(|a, b| compare_with_predicate(predicate, key, *a, *b));
    }
    sequence
}

/// Destructively sort a list by relinking existing cons cells.
fn sort_list_in_place(
    sequence: BlissVal,
    predicate: BlissVal,
    key: Option<BlissVal>,
    stable: bool,
) -> BlissVal {
    let mut cells = collect_cons_cells(sequence);
    if stable {
        cells.sort_by(|a, b| {
            let car_a = unsafe { (**a).car };
            let car_b = unsafe { (**b).car };
            compare_with_predicate(predicate, key, car_a, car_b)
        });
    } else {
        cells.sort_unstable_by(|a, b| {
            let car_a = unsafe { (**a).car };
            let car_b = unsafe { (**b).car };
            compare_with_predicate(predicate, key, car_a, car_b)
        });
    }

    for window in cells.windows(2) {
        // Safety: all pointers were collected from the original proper list.
        unsafe {
            (*window[0]).cdr = BlissVal::from_cons_ptr(window[1] as *mut u8);
        }
    }
    if let Some(&last) = cells.last() {
        unsafe {
            (*last).cdr = NIL;
            BlissVal::from_cons_ptr(cells[0] as *mut u8)
        }
    } else {
        NIL
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
    if is_complex_vector(sequence) {
        // A fill-pointer vector's LENGTH is its fill pointer.
        return Ok(cvec_fill_pointer(sequence));
    }
    if let Some(s) = string_content(sequence) {
        // Strings are sequences of characters (ANSI). Count characters, not bytes.
        return Ok(s.chars().count());
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
    if is_complex_vector(sequence) {
        let len = cvec_fill_pointer(sequence);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        return Ok(vector_elt(cvec_storage(sequence), index));
    }
    if is_char_seq(sequence) {
        let s = string_content(sequence).unwrap_or_default();
        match s.chars().nth(index) {
            Some(c) => return Ok(BlissVal::from_char(c)),
            None => {
                return Err(BlissError::TypeError {
                    datum: sequence,
                    expected: format!("index {} in bounds (length {})", index, s.chars().count()),
                });
            }
        }
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
    if is_complex_vector(sequence) {
        let len = cvec_fill_pointer(sequence);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        vector_set_elt(cvec_storage(sequence), index, value);
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
    if is_char_seq(sequence) {
        let s: String = collect_elements(sequence)?
            .iter()
            .map(|&c| c.as_char())
            .collect();
        return Ok(crate::streams::make_lisp_string_fresh(&s));
    }
    // A general (non-character) fill-pointer / adjustable vector — char-typed ones
    // are handled by the is_char_seq branch above (bliss-w5t).
    if is_complex_vector(sequence) {
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
    if is_char_seq(sequence) {
        let chars: Vec<char> = string_content(sequence)
            .unwrap_or_default()
            .chars()
            .collect();
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
    } else if is_char_seq(sequence) {
        // REVERSE of a string is a string (ANSI): same element type as input.
        let s: String = elems.iter().map(|&c| c.as_char()).collect();
        Ok(crate::streams::make_lisp_string_fresh(&s))
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

/// Check if a result_type BlissVal indicates a VECTOR type. Matched by symbol
/// NAME (not a hardcoded intern index, which is fragile — the VECTOR symbol does
/// not reliably land at a fixed index, so the old identity check silently failed
/// and CONCATENATE 'VECTOR returned a LIST — bliss-cm0). Accepts the bare type
/// name and the common compound `(vector element-type [size])` / `(simple-vector
/// …)` specifier forms.
fn result_type_is_vector(result_type: BlissVal) -> bool {
    fn name_is_vector(idx: u32) -> bool {
        matches!(
            bliss_compiler::reader::symbol_name(idx).as_deref(),
            Some("VECTOR" | "SIMPLE-VECTOR")
        )
    }
    if result_type.tag() == bliss_rt::value::TAG_SYMBOL {
        return name_is_vector(result_type.as_symbol_index());
    }
    // A compound specifier like (VECTOR T) / (SIMPLE-VECTOR 5): dispatch on the car.
    if result_type.is_cons() {
        let car = unsafe { (*(result_type.as_ptr() as *const ConsCell)).car };
        if car.tag() == bliss_rt::value::TAG_SYMBOL {
            return name_is_vector(car.as_symbol_index());
        }
    }
    false
}

/// Check whether CONCATENATE requested a string result type.
fn result_type_is_string(result_type: BlissVal) -> bool {
    if result_type.tag() != bliss_rt::value::TAG_SYMBOL {
        return false;
    }
    match bliss_compiler::reader::symbol_name(result_type.as_symbol_index()) {
        Some(name) => matches!(
            name.as_str(),
            "STRING" | "SIMPLE-STRING" | "BASE-STRING" | "SIMPLE-BASE-STRING"
        ),
        None => false,
    }
}

/// Concatenate sequences (CL `CONCATENATE`). R5.30.
pub fn concatenate(result_type: BlissVal, sequences: &[BlissVal]) -> Result<BlissVal, BlissError> {
    let mut all_elems = Vec::new();
    for &seq in sequences {
        let elems = collect_elements(seq)?;
        all_elems.extend(elems);
    }
    if result_type_is_string(result_type) {
        let mut out = String::with_capacity(all_elems.len());
        for elem in all_elems {
            if !elem.is_character() {
                return Err(BlissError::TypeError {
                    datum: elem,
                    expected: "character".to_string(),
                });
            }
            out.push(elem.as_char());
        }
        Ok(crate::streams::make_lisp_string(&out))
    } else if result_type_is_vector(result_type) {
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
    if is_list(sequence) {
        Ok(sort_list_in_place(sequence, predicate, key, false))
    } else {
        Ok(sort_vector_in_place(sequence, predicate, key, false))
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
    if is_list(sequence) {
        Ok(sort_list_in_place(sequence, predicate, key, true))
    } else {
        Ok(sort_vector_in_place(sequence, predicate, key, true))
    }
}
