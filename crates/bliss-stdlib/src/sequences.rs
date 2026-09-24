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
    if crate::pathnames::is_registered_string(s) {
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
        let len = bliss_rt::object::simple_string_char_count(ptr);
        if index >= len {
            // An out-of-range index is a TYPE-ERROR, not an internal one. The
            // datum is the INDEX against the valid range so the error is
            // checkable — ansi asserts a type error's datum does not satisfy
            // its own expected-type (ansi ELT.10's string case).
            return Err(BlissError::TypeError {
                datum: BlissVal::from_fixnum(index as i64),
                expected: format!("(integer 0 {})", len.saturating_sub(1)),
            });
        }
        // O(1) in-place store (SBCL model, spec §1.6.3). A CHARACTER string
        // (the default) holds any code point; storing a code point >= 256 into
        // a BASE string is a TYPE-ERROR (base strings never promote).
        if !bliss_rt::object::simple_string_set_char(ptr, index, new_char) {
            return Err(BlissError::TypeError {
                datum: ch,
                expected: "base-char (a character < code point 256) for a base string".into(),
            });
        }
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
    // No registry consultation here, deliberately. This used to ask the
    // pathname string registry first, because a registry-backed string was once
    // a FNV hash sitting behind a heap-object tag, and dereferencing it would
    // fault. That representation is gone: make_string_bv now allocates an
    // ordinary Lisp string (its own comment says so), arena_str registers a real
    // arena allocation, and the remaining registration in the evaluator checks
    // the object header first. The `keyword_hash` sentinels that ARE raw bits
    // carry TAG_SYMBOL (0b101), not TAG_HEAP_OBJECT (0b010), so they never reach
    // this branch.
    //
    // So every registered value is a real object and the header read below is
    // safe — and it gives the same answer the registry did, since a string's
    // type id is not SIMPLE_VECTOR either way. The check was pure cost: a global
    // mutex, a hash and a String clone on EVERY element access, which made
    // (aref v 3) cost ~17x (car c) in a hot loop (bliss-edzd).
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

/// The character (not byte) at `index` of a string sequence, or `None` if `v`
/// is not a string or `index` is out of range. This is the single choke point
/// for character indexing (CHAR/SCHAR/ELT/AREF on a string): the compact-string
/// layout (bliss-qsgq) will make the simple-string case O(1) by swapping only
/// this function's implementation. Today it is O(n) over the UTF-8 storage.
pub fn string_char_at(v: BlissVal, index: usize) -> Option<char> {
    // O(1) for a real simple-string heap object; registry-backed sentinels and
    // fill-pointer char vectors fall back to the decoded content.
    if v.is_heap_object() && !crate::pathnames::is_registered_string(v) {
        let ptr = unsafe { v.as_ptr() };
        let tid = unsafe { (*(ptr as *const ObjectHeader)).type_id() };
        if tid == type_id::SIMPLE_BASE_STRING || tid == type_id::SIMPLE_CHARACTER_STRING {
            return unsafe { bliss_rt::object::simple_string_char_at(ptr, index) };
        }
    }
    string_content(v).and_then(|s| s.chars().nth(index))
}

/// The character (not byte) length of a string sequence, or `None` if `v` is
/// not a string. The other choke point Step B makes O(1).
pub fn string_char_count(v: BlissVal) -> Option<usize> {
    string_content(v).map(|s| s.chars().count())
}

/// UTF-8 content bytes of ANY string sequence — simple, fill-pointer, adjustable
/// or displaced — or `None` for a non-string. Used by hash-table EQUAL/EQUALP and
/// SXHASH content hashing so that e.g. a simple string and a fill-pointer string
/// with the same active characters compare and hash alike.
pub fn string_content_bytes(v: BlissVal) -> Option<Vec<u8>> {
    string_content(v).map(String::into_bytes)
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
/// True iff `v` is the interpreter's closure representation
/// `(BLISS::CLOSURE . <id>)` — structurally a cons, but a FUNCTION, not a list.
///
/// Sequence traversals must never walk one. ansi's CHECK-TYPE-ERROR applies each
/// sequence function to every non-sequence in its universe and asserts the
/// signalled TYPE-ERROR's DATUM is the argument it passed, so walking a closure
/// and blaming its cdr reports the wrong datum.
fn is_closure_cons(v: BlissVal) -> bool {
    if !v.is_cons() {
        return false;
    }
    let cell = unsafe { &*(v.as_ptr() as *const ConsCell) };
    if cell.car.tag() != bliss_rt::value::TAG_SYMBOL || !cell.cdr.is_fixnum() {
        return false;
    }
    matches!(
        bliss_compiler::reader::symbol_name(cell.car.as_symbol_index()).as_deref(),
        Some("BLISS::CLOSURE")
    )
}

/// A closure is a FUNCTION, not a sequence, however cons-shaped it is.
fn reject_function_value(v: BlissVal) -> Result<(), BlissError> {
    if is_closure_cons(v) {
        return Err(BlissError::TypeError {
            datum: v,
            expected: "SEQUENCE".to_string(),
        });
    }
    Ok(())
}

/// A CL sequence must be a PROPER list.
///
/// The datum is the improper TAIL, not the list: ansi's SIGNALS-ERROR asserts a
/// TYPE-ERROR's datum does NOT satisfy its own expected-type, and a dotted list
/// IS `typep` LIST — it is a cons — so blaming the list would be a
/// self-contradicting error. The tail is the thing that is not a list, which is
/// also what SBCL reports.
fn reject_improper_tail(tail: BlissVal) -> Result<(), BlissError> {
    if !tail.is_nil() {
        return Err(BlissError::TypeError {
            datum: tail,
            expected: "LIST".to_string(),
        });
    }
    Ok(())
}

fn collect_elements(sequence: BlissVal) -> Result<Vec<BlissVal>, BlissError> {
    if sequence.is_nil() {
        return Ok(Vec::new());
    }
    // Both guards live HERE, in the one traversal every sequence function shares,
    // rather than in each caller. Putting them in LENGTH alone was net zero on
    // the chapter — it fixed the functions written over LENGTH and broke REVERSE,
    // SORT, REDUCE and the duplicate-removers, which carry their own walks
    // (bliss-hvfe, reverted in b1f8073).
    reject_function_value(sequence)?;
    if sequence.is_cons() {
        let mut elems = Vec::new();
        let mut cur = sequence;
        while cur.is_cons() {
            let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
            elems.push(cell.car);
            cur = cell.cdr;
        }
        reject_improper_tail(cur)?;
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
        let len = cvec_fill_pointer(sequence);
        let mut elems = Vec::with_capacity(len);
        for i in 0..len {
            elems.push(cvec_elt(sequence, i)?);
        }
        return Ok(elems);
    }
    if let Some(len) = bliss_rt::types::bit_vector_len(sequence) {
        let mut elems = Vec::with_capacity(len);
        for i in 0..len {
            elems.push(BlissVal::from_fixnum(
                bliss_rt::types::bit_vector_ref(sequence, i).unwrap_or(0) as i64,
            ));
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

/// Copy the spines of all but the final argument onto the final argument.
/// The final argument is shared unchanged and may be any object (ANSI APPEND).
pub fn append(args: &[BlissVal]) -> Result<BlissVal, BlissError> {
    let Some((&tail, lists)) = args.split_last() else {
        return Ok(NIL);
    };
    bliss_rt::rooted!(result = tail);
    bliss_rt::rooted!(elements = Vec::<BlissVal>::new());
    // No Lisp allocation while traversing: finish reading the source spines
    // before consing. Elements and the final tail remain rooted while copying.
    for &list in lists {
        let mut cursor = list;
        while cursor.is_cons() {
            let cell = unsafe { &*(cursor.as_ptr() as *const ConsCell) };
            elements.push(cell.car);
            cursor = cell.cdr;
        }
        reject_improper_tail(cursor)?;
    }
    for i in (0..elements.len()).rev() {
        *result = alloc_cons(elements[i], *result);
    }
    Ok(*result)
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
            // Value points at the object header. For a small object that is
            // body − 8; a large SIMPLE_VECTOR has a 16-byte header prefix, so
            // subtract the true header offset or the value points into the
            // size-extension word and the vector is misread (bliss-tjru).
            let header_off = bliss_rt::gc::body_header_offset(body_size);
            return BlissVal::from_heap_ptr(body.sub(header_off));
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

/// Build a simple-vector of `len` elements all EQ to `fill`, without first
/// materializing the elements.
///
/// `(make-array n)` — the most common array constructor there is — used to
/// build an n-element LIST with MAKE-LIST and then `(apply #'vector …)` it
/// (boot.lisp). That spent one Lisp-level CONS call per element, each one
/// reaching the interpreter through the slow synthesize-and-evaluate path in
/// `apply_function`, and then spread a 100k-element list as function arguments:
/// `(make-array 100000)` took ~980ms, ~9.8us per element, which is ~11x the
/// cost of the 100k-iteration `(setf (aref a i) i)` loop that fills it
/// (bliss-3o0r). Allocating the storage once in Rust removes the list entirely.
///
/// GC safety: `fill` is rooted across the single `alloc_typed`, which is the
/// only allocation here — the element loop writes an already-rooted immediate
/// into freshly allocated storage and cannot itself collect. Writing the young
/// `fill` into a brand-new object needs no write barrier: the object is in the
/// nursery, so the minor collector scans it regardless.
pub fn build_filled_simple_vector(len: usize, fill: BlissVal) -> BlissVal {
    bliss_rt::rooted!(fill = fill);
    let body_size = 8 + len * 8;
    if let Some(body) = bliss_rt::gc::alloc_typed(body_size, type_id::SIMPLE_VECTOR) {
        unsafe {
            *(body as *mut u64) = len as u64;
            let raw = fill.to_raw();
            for i in 0..len {
                *(body.add(8 + i * 8) as *mut u64) = raw;
            }
            let header_off = bliss_rt::gc::body_header_offset(body_size);
            return BlissVal::from_heap_ptr(body.sub(header_off));
        }
    }
    // OOM fallback: mirror `build_vector`'s leaked-block path so vector
    // allocation never fails.
    let vals = vec![*fill; len];
    build_vector(&vals)
}

/// Build a fresh simple bit-vector from a slice of element values (each a
/// fixnum 0 or 1). Used by SUBSEQ/COPY-SEQ/REVERSE so a bit-vector input yields
/// a bit-vector result (ANSI: the result of these on a bit-vector is a
/// bit-vector), not a general simple-vector (bliss-8z5f). A non-bit element is
/// treated as 1 for any non-zero fixnum; callers only pass values collected from
/// a bit-vector, so every element is already 0/1.
fn build_bit_vector_from_vals(vals: &[BlissVal]) -> BlissVal {
    let bits: Vec<u8> = vals
        .iter()
        .map(|v| if v.is_fixnum() && v.as_fixnum() == 0 { 0 } else { 1 })
        .collect();
    bliss_rt::types::make_bit_vector(&bits)
}

/// Get vector length from a heap-object BlissVal known to be a vector.
#[inline]
fn vector_payload_offset(ptr: *const u8) -> usize {
    // The length word begins the payload, at header + 8 (small) or + 16 (large).
    let header = unsafe { *(ptr as *const ObjectHeader) };
    if header.is_large_object() {
        16
    } else {
        8
    }
}

#[inline]
fn vector_length(v: BlissVal) -> usize {
    let ptr = unsafe { v.as_ptr() };
    let off = vector_payload_offset(ptr);
    unsafe { *(ptr.add(off) as *const u64) as usize }
}

/// Get vector element at index from a heap-object BlissVal.
#[inline]
fn vector_elt(v: BlissVal, idx: usize) -> BlissVal {
    let ptr = unsafe { v.as_ptr() };
    let off = vector_payload_offset(ptr);
    unsafe { *(ptr.add(off + 8 + idx * 8) as *const BlissVal) }
}

/// Set vector element at index.
#[inline]
fn vector_set_elt(v: BlissVal, idx: usize, val: BlissVal) {
    let ptr = unsafe { v.as_ptr() };
    let off = vector_payload_offset(ptr);
    unsafe {
        *(ptr.add(off + 8 + idx * 8) as *mut BlissVal) = val;
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
//   word 3 (+32): element-type tag, a fixnum (1 = CHARACTER, 0 = general T)
//   word 4 (+40): has-user-fill-pointer flag, a fixnum (1 = the user asked for
//                 :fill-pointer, 0 = plain :adjustable array). Word 1 is the
//                 active length either way, but ANSI says only the former
//                 answers T to ARRAY-HAS-FILL-POINTER-P (bliss-0x9y).
// The active length is the fill pointer; `array-total-size` is the storage's
// length; growth (`vector-push-extend`) replaces the storage with a larger one.

/// True if `v` is a COMPLEX_ARRAY (fill-pointer / adjustable vector).
pub fn is_complex_vector(v: BlissVal) -> bool {
    if !v.is_heap_object() {
        return false;
    }
    // Same reasoning as `is_vector`: every registered value is a real object, so
    // the header read is safe and answers identically without a registry
    // lookup (bliss-edzd).
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
/// character, 0/absent = general T). Words 3 and 4 are only present on the
/// 5-word COMPLEX_ARRAYs built by `build_complex_vector`; the readers are safe
/// because every COMPLEX_ARRAY this crate allocates now carries them.
/// The immediate element-type tag stored in COMPLEX_ARRAY word 3:
/// 1 = CHARACTER (string), 2 = BIT (bit vector), 0 = general. `char` wins if
/// both are set (a string is never also a bit vector).
#[inline]
fn element_type_code(element_is_char: bool, element_is_bit: bool) -> i64 {
    if element_is_char {
        1
    } else if element_is_bit {
        2
    } else {
        0
    }
}

/// Whether a complex vector has element-type BIT — a (fill-pointer /
/// adjustable / displaced) bit vector — so it answers BIT-VECTOR-P / TYPEP
/// BIT-VECTOR / ARRAY-ELEMENT-TYPE BIT and compares EQUAL to a simple bit
/// vector of the same bits (bliss-65nx). Tag 2 in body word 3.
#[inline]
pub fn cvec_is_bit(v: BlissVal) -> bool {
    unsafe {
        let tag = *(v.as_ptr().add(32) as *const BlissVal);
        tag.is_fixnum() && tag.as_fixnum() == 2
    }
}

#[inline]
pub fn cvec_is_string(v: BlissVal) -> bool {
    unsafe {
        let tag = *(v.as_ptr().add(32) as *const BlissVal);
        tag.is_fixnum() && tag.as_fixnum() == 1
    }
}
/// Whether a complex vector has a *user* fill pointer — i.e. it was created
/// with a non-NIL `:fill-pointer`. A plain `(make-array n :adjustable t)` is
/// also a COMPLEX_ARRAY and still stores its length in the fill-pointer word,
/// but ANSI says it has no fill pointer, so `ARRAY-HAS-FILL-POINTER-P` (and
/// `FILL-POINTER`/`VECTOR-PUSH`) must tell the two apart. Encoded as an
/// immediate fixnum tag in body word 4 (1 = yes, 0/absent = no). bliss-0x9y.
#[inline]
pub fn cvec_has_fill_pointer(v: BlissVal) -> bool {
    unsafe {
        let tag = *(v.as_ptr().add(40) as *const BlissVal);
        tag.is_fixnum() && tag.as_fixnum() == 1
    }
}
/// Displacement of a complex array (bliss-7o4y): `Some((base, offset, total))`
/// when `v` was built displaced to `base` (`:displaced-to`), where `offset` is
/// the row-major offset into `base` and `total` the displaced array's own
/// total size. `None` for an ordinary complex array. Guarded on the header's
/// recorded size so a legacy 5-word body (e.g. restored from an old image)
/// never reads past its allocation.
#[inline]
pub fn cvec_displacement(v: BlissVal) -> Option<(BlissVal, usize, usize)> {
    unsafe {
        let header = *(v.as_ptr() as *const ObjectHeader);
        if (header.size_units() as usize) < 8 {
            return None;
        }
        let disp = *(v.as_ptr().add(48) as *const BlissVal);
        if !disp.is_fixnum() {
            return None;
        }
        let total = *(v.as_ptr().add(56) as *const BlissVal);
        let total = if total.is_fixnum() {
            total.as_fixnum().max(0) as usize
        } else {
            0
        };
        Some((cvec_storage(v), disp.as_fixnum().max(0) as usize, total))
    }
}

/// Row-major element read on any array `base` a displaced array can target:
/// simple vector, string, complex (possibly itself displaced — chains
/// resolve), bit-vector, or multidimensional array. Indexes the TOTAL-SIZE
/// domain (fill pointers of the base are ignored, per CLHS displacement).
fn array_row_major_elt(base: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    if is_complex_vector(base) {
        if let Some((inner, off, _)) = cvec_displacement(base) {
            return array_row_major_elt(inner, off + index);
        }
        return Ok(vector_elt(cvec_storage(base), index));
    }
    if let Some(storage) = bliss_rt::types::md_array_storage(base) {
        return Ok(vector_elt(storage, index));
    }
    if is_vector(base) {
        return Ok(vector_elt(base, index));
    }
    elt(base, index)
}

/// Row-major element write matching [`array_row_major_elt`].
fn array_row_major_set_elt(
    base: BlissVal,
    index: usize,
    value: BlissVal,
) -> Result<(), BlissError> {
    if is_complex_vector(base) {
        if let Some((inner, off, _)) = cvec_displacement(base) {
            return array_row_major_set_elt(inner, off + index, value);
        }
        vector_set_elt(cvec_storage(base), index, value);
        return Ok(());
    }
    if let Some(storage) = bliss_rt::types::md_array_storage(base) {
        vector_set_elt(storage, index, value);
        return Ok(());
    }
    if is_vector(base) {
        vector_set_elt(base, index, value);
        return Ok(());
    }
    set_elt(base, index, value)
}

/// Total size (row-major element count) of any array value this crate models —
/// the domain a displaced array indexes: simple vector, complex vector (its
/// capacity, ignoring fill pointers), string, bit-vector, or multidimensional
/// array. `None` when `v` is not an array.
pub fn array_total_size(v: BlissVal) -> Option<usize> {
    if is_complex_vector(v) {
        return Some(cvec_capacity(v));
    }
    if let Some(storage) = bliss_rt::types::md_array_storage(v) {
        return Some(vector_length(storage));
    }
    if is_vector(v) {
        return Some(vector_length(v));
    }
    if let Some(n) = bliss_rt::types::bit_vector_len(v) {
        return Some(n);
    }
    string_char_count(v)
}

/// Clear a complex array's displacement words after its storage has been
/// replaced by a grow/adjust: the array now owns word 0 as direct backing
/// storage. No-op on a legacy 5-word body (which cannot be displaced).
#[inline]
fn cvec_clear_displacement(v: BlissVal) {
    unsafe {
        let header = *(v.as_ptr() as *const ObjectHeader);
        if (header.size_units() as usize) >= 8 {
            *(v.as_ptr().add(48) as *mut BlissVal) = NIL;
            *(v.as_ptr().add(56) as *mut BlissVal) = NIL;
        }
    }
}

/// Public displacement-aware element read for the printers (bliss-v9nb): a
/// printer must NOT index a complex array's word 0 raw — for a displaced
/// array that word is the BASE array reference, not element storage, and
/// walking a bit-vector/string base's internal words as BlissVals prints
/// garbage and dereferences near-null junk (universe.lsp's displaced
/// bit-vector segfaulted every printer touch, including error-condition
/// datum rendering). `index` must already be bounds-checked by the caller.
pub fn cvec_element(v: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    cvec_elt(v, index)
}

/// Displacement-aware element read of a complex vector: a displaced array
/// reads through to its base; an ordinary one reads its own storage. `index`
/// must already be bounds-checked by the caller.
#[inline]
fn cvec_elt(v: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    match cvec_displacement(v) {
        Some((base, off, _)) => array_row_major_elt(base, off + index),
        None => Ok(vector_elt(cvec_storage(v), index)),
    }
}

/// Displacement-aware element write of a complex vector.
#[inline]
fn cvec_set_elt(v: BlissVal, index: usize, value: BlissVal) -> Result<(), BlissError> {
    match cvec_displacement(v) {
        Some((base, off, _)) => array_row_major_set_elt(base, off + index, value),
        None => {
            vector_set_elt(cvec_storage(v), index, value);
            Ok(())
        }
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
    let n = cvec_fill_pointer(v);
    let mut s = String::with_capacity(n);
    for i in 0..n {
        let e = cvec_elt(v, i).ok()?;
        if e.is_character() {
            s.push(e.as_char());
        } else {
            s.push('\0');
        }
    }
    Some(s)
}
/// `array-total-size` — the capacity of the backing storage, or a displaced
/// array's own recorded total size (its base's size is not its own).
#[inline]
pub fn cvec_capacity(v: BlissVal) -> usize {
    match cvec_displacement(v) {
        Some((_, _, total)) => total,
        None => vector_length(cvec_storage(v)),
    }
}

/// Build a COMPLEX_ARRAY. `capacity` is the backing array-total-size;
/// `elements` seed positions `0..elements.len()` (rest NIL); `fill_pointer` is
/// the active length; `adjustable` allows later growth. `element_is_char` marks
/// element-type CHARACTER, i.e. a (fill-pointer / adjustable) STRING.
/// `has_fill_pointer` records whether the user actually asked for
/// `:fill-pointer`; a plain `:adjustable` array stores its length in the same
/// word but has no fill pointer per ANSI (bliss-0x9y).
pub fn build_complex_vector(
    elements: &[BlissVal],
    capacity: usize,
    fill_pointer: usize,
    adjustable: bool,
    element_is_char: bool,
    element_is_bit: bool,
    has_fill_pointer: bool,
) -> BlissVal {
    let cap = capacity.max(elements.len());
    let mut store: Vec<BlissVal> = Vec::with_capacity(cap);
    store.extend_from_slice(elements);
    store.resize(cap, NIL);
    let storage = build_vector(&store);
    bliss_rt::rooted!(storage = storage);
    let fp = BlissVal::from_fixnum(fill_pointer.min(cap) as i64);
    let adj = if adjustable { T } else { NIL };
    // Element-type tag: immediate fixnum — 1 = CHARACTER (a string),
    // 2 = BIT (a bit vector), 0 = general (bliss-65nx).
    let elt = BlissVal::from_fixnum(element_type_code(element_is_char, element_is_bit));
    // Has-user-fill-pointer tag: immediate fixnum, 1 = yes, 0 = no.
    let hasfp = BlissVal::from_fixnum(if has_fill_pointer { 1 } else { 0 });
    // Body = [storage-ref | fill-pointer(fixnum) | adjustable(T/NIL) |
    //         element-type(fixnum) | has-fill-pointer(fixnum) |
    //         displaced-offset(NIL or fixnum) | displaced-total-size(NIL or
    //         fixnum)]; only word 0 is a heap reference (the GC's
    //         COMPLEX_ARRAY tracer visits word 0 only), so the extra
    //         immediates are free of tracer changes. For a DISPLACED array
    //         (bliss-7o4y) word 0 is the BASE array reference, word 5 the
    //         row-major offset into it, and word 6 the array's own total size
    //         (underivable from the base); a non-displaced array stores NIL in
    //         words 5-6.
    build_complex_array_body(*storage, fp, adj, elt, hasfp, NIL, NIL)
}

/// Build a COMPLEX_ARRAY displaced to `base` (CLHS `:displaced-to`):
/// element `i` reads and writes `base`'s row-major element `offset + i`.
/// `length` is the displaced array's own total size; `fill_pointer` is the
/// active length (= `length` when `has_fill_pointer` is false). The caller
/// validates `offset + length <= (array-total-size base)`.
pub fn build_displaced_vector(
    base: BlissVal,
    offset: usize,
    length: usize,
    fill_pointer: usize,
    adjustable: bool,
    element_is_char: bool,
    element_is_bit: bool,
    has_fill_pointer: bool,
) -> BlissVal {
    bliss_rt::rooted!(base = base);
    let fp = BlissVal::from_fixnum(fill_pointer.min(length) as i64);
    let adj = if adjustable { T } else { NIL };
    let elt = BlissVal::from_fixnum(element_type_code(element_is_char, element_is_bit));
    let hasfp = BlissVal::from_fixnum(if has_fill_pointer { 1 } else { 0 });
    let disp = BlissVal::from_fixnum(offset as i64);
    let total = BlissVal::from_fixnum(length as i64);
    build_complex_array_body(*base, fp, adj, elt, hasfp, disp, total)
}

/// Allocate the 7-word COMPLEX_ARRAY body (see `build_complex_vector` for the
/// layout). `word0` is the only heap reference and must be rooted by the
/// caller across this call.
fn build_complex_array_body(
    word0: BlissVal,
    fp: BlissVal,
    adj: BlissVal,
    elt: BlissVal,
    hasfp: BlissVal,
    disp: BlissVal,
    total: BlissVal,
) -> BlissVal {
    bliss_rt::rooted!(word0 = word0);
    let body_size = 7 * 8;
    if let Some(body) = bliss_rt::gc::alloc_typed(body_size, type_id::COMPLEX_ARRAY) {
        unsafe {
            *(body as *mut u64) = (*word0).to_raw();
            *(body.add(8) as *mut u64) = fp.to_raw();
            *(body.add(16) as *mut u64) = adj.to_raw();
            *(body.add(24) as *mut u64) = elt.to_raw();
            *(body.add(32) as *mut u64) = hasfp.to_raw();
            *(body.add(40) as *mut u64) = disp.to_raw();
            *(body.add(48) as *mut u64) = total.to_raw();
            return BlissVal::from_heap_ptr(body.sub(8));
        }
    }
    // OOM fallback: a leaked block (header + 7 body words).
    let mut buf: Vec<u64> = Vec::with_capacity(8);
    let header = ObjectHeader::new(type_id::COMPLEX_ARRAY, 8);
    buf.push(header.0);
    buf.push((*word0).to_raw());
    buf.push(fp.to_raw());
    buf.push(adj.to_raw());
    buf.push(elt.to_raw());
    buf.push(hasfp.to_raw());
    buf.push(disp.to_raw());
    buf.push(total.to_raw());
    let ptr = buf.as_mut_ptr() as *mut u8;
    std::mem::forget(buf);
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

/// Build a multidimensional (rank ≥ 2) array: `dims` gives the per-axis
/// dimensions, `fill` seeds every element of the row-major storage. GC-safe: the
/// storage vector is rooted while the dims vector is built, and both are rooted
/// across the MD_ARRAY allocation (bliss-rh0t).
pub fn build_md_array(dims: &[usize], fill: BlissVal) -> BlissVal {
    let total: usize = dims.iter().product();
    // Row-major storage: `total` copies of the fill element.
    bliss_rt::rooted!(fill = fill);
    let store_vec = vec![*fill; total];
    let storage = build_vector(&store_vec);
    bliss_rt::rooted!(storage = storage);
    // Dimensions as a SIMPLE_VECTOR of fixnums.
    let dim_vals: Vec<BlissVal> = dims
        .iter()
        .map(|&d| BlissVal::from_fixnum(d as i64))
        .collect();
    let dims_vec = build_vector(&dim_vals);
    bliss_rt::rooted!(dims_vec = dims_vec);
    let rank = BlissVal::from_fixnum(dims.len() as i64);
    // Body = [storage-ref | dims-ref | rank]; words 0,1 are heap references the
    // GC's MD_ARRAY tracer visits, rank is immediate.
    let body_size = 3 * 8;
    if let Some(body) = bliss_rt::gc::alloc_typed(body_size, type_id::MD_ARRAY) {
        unsafe {
            *(body as *mut u64) = (*storage).to_raw();
            *(body.add(8) as *mut u64) = (*dims_vec).to_raw();
            *(body.add(16) as *mut u64) = rank.to_raw();
            return BlissVal::from_heap_ptr(body.sub(8));
        }
    }
    // OOM fallback: a leaked block (header + 3 body words).
    let mut buf: Vec<u64> = Vec::with_capacity(4);
    let header = ObjectHeader::new(type_id::MD_ARRAY, 4);
    buf.push(header.0);
    buf.push((*storage).to_raw());
    buf.push((*dims_vec).to_raw());
    buf.push(rank.to_raw());
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
    cvec_set_elt(v, fp, value)?;
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
            store.push(cvec_elt(v, i)?);
        }
        store.resize(new_cap, NIL);
        // `build_vector` allocates and can fire a minor GC that relocates the
        // nursery, so BOTH the vector we are about to mutate and the value we
        // still have to store must be rooted across it — an unrooted local
        // would be left pointing at the pre-move address (bliss-ez7w).
        // (`store`'s elements need no rooting here: build_vector roots its own
        // copy of them before allocating.)
        bliss_rt::rooted!(v = v);
        bliss_rt::rooted!(value = value);
        // Allocate FIRST, then re-read `*v`. Writing it as
        // `cvec_set_storage(*v, build_vector(&store))` would reload `*v` as the
        // left argument *before* build_vector runs, handing the setter the very
        // pre-move address the rooting exists to avoid.
        let storage = build_vector(&store);
        cvec_set_storage(*v, storage);
        // The array now owns fresh direct storage — a formerly displaced
        // array stops being displaced (its elements were copied above).
        cvec_clear_displacement(*v);
        vector_set_elt(storage, fp, *value);
        cvec_set_fill_pointer_raw(*v, fp + 1);
        return Ok(BlissVal::from_fixnum(fp as i64));
    }
    cvec_set_elt(v, fp, value)?;
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
    let fp = fill_pointer.unwrap_or(new_size).min(new_size);
    if new_size > cap {
        let mut store: Vec<BlissVal> = Vec::with_capacity(new_size);
        for i in 0..cap {
            store.push(cvec_elt(v, i)?);
        }
        store.resize(new_size, initial_element);
        // `build_vector` allocates and can fire a minor GC that relocates the
        // nursery, so `v` must be rooted across it: otherwise we would write
        // the new storage (and the fill pointer) into `v`'s pre-move address
        // and the surviving array would silently keep its old, smaller
        // storage (bliss-ez7w). `store`'s elements are rooted by build_vector
        // itself, and `initial_element` is not used after the call.
        bliss_rt::rooted!(v = v);
        // Allocate FIRST, then re-read `*v` — see the note in
        // `vector_push_extend` on argument-evaluation order.
        let storage = build_vector(&store);
        cvec_set_storage(*v, storage);
        // Fresh direct storage: a formerly displaced array stops being
        // displaced (ADJUST-ARRAY without :displaced-to, CLHS).
        cvec_clear_displacement(*v);
        cvec_set_fill_pointer_raw(*v, fp);
        return Ok(*v);
    }
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
    let val = cvec_elt(v, fp - 1)?;
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
        //
        // Through the WRITE BARRIER, not a raw store. This relinks EXISTING
        // cells, and after a collection a list can span generations, so a raw
        // store can put a young cell's address into an old cell without the
        // collector knowing — the young cell is then not scanned, is collected
        // or moved, and the old cell's cdr becomes garbage. That is the shape of
        // the corruption in bliss-t53a (a cons whose cdr reads back as
        // Fixnum(0)), and BLISS_GC_VERIFY does not catch it because nothing is
        // pointing INTO the nursery at verification time — the reference was
        // never recorded at all.
        unsafe {
            let next = BlissVal::from_cons_ptr(window[1] as *mut u8);
            bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*window[0]).cdr), next);
        }
    }
    if let Some(&last) = cells.last() {
        unsafe {
            bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*last).cdr), NIL);
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
    reject_function_value(sequence)?;
    if sequence.is_cons() {
        let mut count = 0usize;
        let mut cur = sequence;
        while cur.is_cons() {
            count += 1;
            let cell = unsafe { &*(cur.as_ptr() as *const ConsCell) };
            cur = cell.cdr;
        }
        reject_improper_tail(cur)?;
        return Ok(count);
    }
    if is_vector(sequence) {
        return Ok(vector_length(sequence));
    }
    if is_complex_vector(sequence) {
        // A fill-pointer vector's LENGTH is its fill pointer.
        return Ok(cvec_fill_pointer(sequence));
    }
    if let Some(n) = bliss_rt::types::bit_vector_len(sequence) {
        return Ok(n);
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
        return cvec_elt(sequence, index);
    }
    if is_bit_seq(sequence) {
        return match bliss_rt::types::bit_vector_ref(sequence, index) {
            Some(bit) => Ok(BlissVal::from_fixnum(bit as i64)),
            None => Err(BlissError::TypeError {
                datum: sequence,
                expected: format!(
                    "index {} in bounds (length {})",
                    index,
                    bliss_rt::types::bit_vector_len(sequence).unwrap_or(0)
                ),
            }),
        };
    }
    if is_char_seq(sequence) {
        match string_char_at(sequence, index) {
            Some(c) => return Ok(BlissVal::from_char(c)),
            None => {
                return Err(BlissError::TypeError {
                    datum: sequence,
                    expected: format!(
                        "index {} in bounds (length {})",
                        index,
                        string_char_count(sequence).unwrap_or(0)
                    ),
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
        cvec_set_elt(sequence, index, value)?;
        return Ok(());
    }
    // A simple string is a mutable character vector: (SETF (ELT s i) c) stores a
    // character in place. Fill-pointer / adjustable char vectors are handled by
    // the is_complex_vector branch above; only simple strings reach here (bliss-2pt
    // — SORT/(SETF ELT) on a string previously errored "mutable sequence
    // (vector)"). string_set_char rejects interned literals and does an O(1) store,
    // so no allocation happens across a live BlissVal — GC-safe.
    if is_char_seq(sequence) {
        let len = string_char_count(sequence).unwrap_or(0);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        string_set_char(sequence, index, value)?;
        return Ok(());
    }
    // Bit-vectors store 0/1 in place (bliss-27f5): (SETF (AREF bv i)) /
    // (SETF (SBIT bv i)). No allocation — GC-safe.
    if is_bit_seq(sequence) {
        let len = bliss_rt::types::bit_vector_len(sequence).unwrap_or(0);
        if index >= len {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, len),
            });
        }
        if !value.is_fixnum() || !matches!(value.as_fixnum(), 0 | 1) {
            return Err(BlissError::TypeError {
                datum: value,
                expected: "bit (0 or 1)".to_string(),
            });
        }
        bliss_rt::types::bit_vector_set(sequence, index, value.as_fixnum() as u8);
        return Ok(());
    }
    // Lists are not setf-elt-able
    Err(BlissError::TypeError {
        datum: sequence,
        expected: "mutable sequence (vector)".to_string(),
    })
}

/// Read an array element the way AREF does — like `elt`, except that on a
/// complex (fill-pointer) vector AREF **ignores the fill pointer** and may
/// access any element up to the total allocated size (CLHS AREF: "aref ignores
/// fill pointers"). `elt` bounds against the fill pointer; AREF bounds against
/// the capacity. All other sequence kinds behave identically to `elt`.
pub fn aref(sequence: BlissVal, index: usize) -> Result<BlissVal, BlissError> {
    if is_complex_vector(sequence) {
        let cap = cvec_capacity(sequence);
        if index >= cap {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, cap),
            });
        }
        return cvec_elt(sequence, index);
    }
    elt(sequence, index)
}

/// Store an array element the way `(SETF AREF)` does — like `set_elt`, except
/// that on a complex (fill-pointer) vector AREF ignores the fill pointer and
/// bounds against the total allocated size (see `aref`).
pub fn set_aref(sequence: BlissVal, index: usize, value: BlissVal) -> Result<(), BlissError> {
    if is_complex_vector(sequence) {
        let cap = cvec_capacity(sequence);
        if index >= cap {
            return Err(BlissError::TypeError {
                datum: sequence,
                expected: format!("index {} in bounds (length {})", index, cap),
            });
        }
        cvec_set_elt(sequence, index, value)?;
        return Ok(());
    }
    set_elt(sequence, index, value)
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
    if is_bit_seq(sequence) {
        // COPY-SEQ of a bit-vector is a bit-vector, not a general vector
        // (bliss-8z5f). (This direct entry point is normally shadowed by boot's
        // `(subseq seq 0)`, but keep it correct.)
        let elems = collect_elements(sequence)?;
        return Ok(build_bit_vector_from_vals(&elems));
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
/// Whether `v` is a BIT vector of any kind — simple, or a complex vector
/// (fill-pointer / adjustable / displaced) tagged as holding bits.
///
/// `bliss_rt::types::bit_vector_p` only recognizes the SIMPLE representation, so
/// SUBSEQ, COPY-SEQ and REVERSE of a fill-pointer or displaced bit vector
/// answered a general vector — `(copy-seq <fp bit vector>)` gave #(0 0 1)
/// instead of #*001 (ansi COPY-SEQ.12/13/14).
fn is_bit_seq(v: BlissVal) -> bool {
    bliss_rt::types::bit_vector_p(v) || (is_complex_vector(v) && cvec_is_bit(v))
}

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
        // SUBSEQ (and COPY-SEQ, which is (subseq seq 0)) must return a FRESH,
        // independent, mutable string — not the interned/shared one. make_lisp_string
        // interns by content, so `(copy-seq s)` returned an object EQ to `s` (a no-op)
        // and mutations aliased; `(eq (copy-seq "x") (copy-seq "x"))` was T. Use the
        // fresh constructor so copies have distinct identity and their own storage —
        // this also un-breaks the default-EQL set ops over COPY-SEQ'd strings that
        // ansi-test's CONS chapter exercises (bliss-9kxg).
        return Ok(crate::streams::make_lisp_string_fresh(&sub));
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
    } else if is_bit_seq(sequence) {
        // SUBSEQ of a bit-vector is a bit-vector (and COPY-SEQ = (subseq x 0)),
        // not a general vector (bliss-8z5f).
        Ok(build_bit_vector_from_vals(sub))
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
    } else if is_bit_seq(sequence) {
        // REVERSE of a bit-vector is a bit-vector (bliss-8z5f).
        Ok(build_bit_vector_from_vals(&elems))
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
            // Through the WRITE BARRIER: this reverses EXISTING cells in place,
            // and after a collection a list can span generations, so a raw store
            // can put a young cell's address into an old one without the
            // collector recording it (bliss-t53a).
            let next = unsafe { (*(cur.as_ptr() as *const ConsCell)).cdr };
            unsafe {
                let cell = cur.as_ptr() as *mut ConsCell;
                bliss_rt::gc::store_ref(std::ptr::addr_of_mut!((*cell).cdr), prev);
            }
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
/// Does this sequence result-type specifier designate a STRING?
///
/// Public because COERCE needs the SAME rule (bliss-wzfm). It reduced a type
/// spec to its head symbol, so `(simple-array character (*))` dispatched on
/// SIMPLE-ARRAY and built a general vector -- `#(#\a #\b)` where SBCL answers
/// `"ab"`. Sharing this predicate rather than restating the rule keeps COERCE,
/// CONCATENATE and MAKE-SEQUENCE from drifting; the element-type clause below
/// already exists here for CONCATENATE and was simply out of COERCE's reach.
pub fn result_type_is_string(result_type: BlissVal) -> bool {
    fn name_is_string(idx: u32) -> bool {
        matches!(
            bliss_compiler::reader::symbol_name(idx).as_deref(),
            Some("STRING" | "SIMPLE-STRING" | "BASE-STRING" | "SIMPLE-BASE-STRING")
        )
    }
    if result_type.tag() == bliss_rt::value::TAG_SYMBOL {
        return name_is_string(result_type.as_symbol_index());
    }
    // A COMPOUND specifier — `(STRING 6)`, `(SIMPLE-STRING *)` — designates the
    // same representation as the bare symbol, with an element count that
    // CONCATENATE's result already satisfies by construction. Only the bare
    // symbol was recognized, so `(concatenate '(string 6) "abc" "def")` fell all
    // the way through to the list branch and answered (# # ...) instead of
    // "abcdef" (ansi CONCATENATE.35-40). `result_type_is_vector` right above
    // already dispatches on the car this way.
    if result_type.is_cons() {
        let cell = unsafe { &*(result_type.as_ptr() as *const ConsCell) };
        let car = cell.car;
        if car.tag() != bliss_rt::value::TAG_SYMBOL {
            return false;
        }
        if name_is_string(car.as_symbol_index()) {
            return true;
        }
        // A string can also be named through its ELEMENT TYPE: (vector
        // character), and (array nil (*)) — an element type of NIL holds no
        // elements and is a STRING subtype (CLHS 15.1.2.2), which MAKE-ARRAY
        // already treats that way. Without this, (concatenate '(array nil (*)))
        // answered NIL rather than "" (ansi CONCATENATE.32).
        let head_takes_element_type = matches!(
            bliss_compiler::reader::symbol_name(car.as_symbol_index()).as_deref(),
            Some("VECTOR" | "ARRAY" | "SIMPLE-ARRAY" | "SIMPLE-VECTOR")
        );
        if head_takes_element_type && cell.cdr.is_cons() {
            let elt = unsafe { (*(cell.cdr.as_ptr() as *const ConsCell)).car };
            // NIL is its own immediate, not a TAG_SYMBOL value, so it has to be
            // tested for directly — `(array nil (*))` reaches here with the
            // element type as the NIL immediate.
            if elt.is_nil() {
                return true;
            }
            if elt.tag() == bliss_rt::value::TAG_SYMBOL {
                return matches!(
                    bliss_compiler::reader::symbol_name(elt.as_symbol_index()).as_deref(),
                    Some("CHARACTER" | "BASE-CHAR" | "STANDARD-CHAR")
                );
            }
        }
    }
    false
}

/// Check whether CONCATENATE requested a bit-vector result type.
///
/// Nothing recognized these, so `(concatenate 'bit-vector '(0 1 1))` answered
/// the LIST (0 1 1) instead of #*011 (ansi CONCATENATE.10-15).
/// Does this sequence result-type specifier designate a BIT-VECTOR?
///
/// Public for the same reason as `result_type_is_string`: COERCE reduces a
/// spec to its head symbol, so `(vector bit)` built a general vector -- `#(1 0)`
/// where SBCL answers `#*10` (bliss-h21k). The element-type clause below was
/// already here for MAP; COERCE simply could not reach it.
pub fn result_type_is_bit_vector(result_type: BlissVal) -> bool {
    fn name_is_bit_vector(idx: u32) -> bool {
        matches!(
            bliss_compiler::reader::symbol_name(idx).as_deref(),
            Some("BIT-VECTOR" | "SIMPLE-BIT-VECTOR")
        )
    }
    if result_type.tag() == bliss_rt::value::TAG_SYMBOL {
        return name_is_bit_vector(result_type.as_symbol_index());
    }
    if result_type.is_cons() {
        let cell = unsafe { &*(result_type.as_ptr() as *const ConsCell) };
        let car = cell.car;
        if car.tag() != bliss_rt::value::TAG_SYMBOL {
            return false;
        }
        if name_is_bit_vector(car.as_symbol_index()) {
            return true;
        }
        // `(vector bit)` / `(vector bit 6)` / `(simple-array bit (6))` name a
        // bit vector through their ELEMENT TYPE rather than their head (ansi
        // MAP-BIT-VECTOR.17/23/24).
        let head_takes_element_type = matches!(
            bliss_compiler::reader::symbol_name(car.as_symbol_index()).as_deref(),
            Some("VECTOR" | "ARRAY" | "SIMPLE-ARRAY")
        );
        if head_takes_element_type && cell.cdr.is_cons() {
            let elt = unsafe { (*(cell.cdr.as_ptr() as *const ConsCell)).car };
            if elt.tag() == bliss_rt::value::TAG_SYMBOL {
                return bliss_compiler::reader::symbol_name(elt.as_symbol_index()).as_deref()
                    == Some("BIT");
            }
        }
    }
    false
}

/// Build a sequence of `elems` in the representation `result_type` designates.
///
/// Shared by CONCATENATE and by the interpreter's MAP, which used to carry its
/// OWN inline copy of this classification in cli.rs — and a smaller one, so MAP
/// mishandled every compound specifier, SIMPLE-BASE-STRING, and every
/// bit-vector type, answering a LIST instead (ansi MAP.38-47, MAP.FILL.5).
/// Duplicated classification is exactly what AGENTS.md's architecture rule
/// forbids; one implementation cannot drift from itself.
///
/// A `result_type` this does not recognize builds a list, which is what both
/// callers did before.
/// The LENGTH a compound sequence type specifier declares, or `None` when it
/// leaves it unspecified. The position differs per head: `(vector [et [size]])`
/// puts it third, while `(string [size])` and the other one-parameter vector
/// heads put it second.
fn declared_sequence_length(result_type: BlissVal) -> Option<usize> {
    if !result_type.is_cons() {
        return None;
    }
    let cell = unsafe { &*(result_type.as_ptr() as *const ConsCell) };
    if cell.car.tag() != bliss_rt::value::TAG_SYMBOL {
        return None;
    }
    let head = bliss_compiler::reader::symbol_name(cell.car.as_symbol_index())?;
    let rest = cell.cdr;
    let slot = match head.as_str() {
        "VECTOR" | "ARRAY" | "SIMPLE-ARRAY" => {
            if !rest.is_cons() {
                return None;
            }
            let after_elt = unsafe { (*(rest.as_ptr() as *const ConsCell)).cdr };
            if !after_elt.is_cons() {
                return None;
            }
            unsafe { (*(after_elt.as_ptr() as *const ConsCell)).car }
        }
        "SIMPLE-VECTOR" | "STRING" | "SIMPLE-STRING" | "BASE-STRING"
        | "SIMPLE-BASE-STRING" | "BIT-VECTOR" | "SIMPLE-BIT-VECTOR" => {
            if !rest.is_cons() {
                return None;
            }
            unsafe { (*(rest.as_ptr() as *const ConsCell)).car }
        }
        _ => return None,
    };
    if slot.is_fixnum() && slot.as_fixnum() >= 0 {
        Some(slot.as_fixnum() as usize)
    } else {
        None
    }
}

/// Whether `result_type` names a concrete sequence representation.
///
/// SEQUENCE itself deliberately does NOT: it is a valid type specifier but does
/// not determine a representation, and CLHS makes it an error for CONCATENATE
/// (ansi CONCATENATE.ERROR.1). Neither do FIXNUM, SYMBOL and friends
/// (CONCATENATE.ERROR.2, MAP.ERROR.1).
fn is_sequence_result_type(result_type: BlissVal) -> bool {
    let head = if result_type.tag() == bliss_rt::value::TAG_SYMBOL {
        result_type.as_symbol_index()
    } else if result_type.is_cons() {
        let car = unsafe { (*(result_type.as_ptr() as *const ConsCell)).car };
        if car.tag() != bliss_rt::value::TAG_SYMBOL {
            return false;
        }
        car.as_symbol_index()
    } else {
        return false;
    };
    matches!(
        bliss_compiler::reader::symbol_name(head).as_deref(),
        Some(
            "LIST"
                | "CONS"
                | "NULL"
                | "NIL"
                | "VECTOR"
                | "SIMPLE-VECTOR"
                | "ARRAY"
                | "SIMPLE-ARRAY"
                | "STRING"
                | "SIMPLE-STRING"
                | "BASE-STRING"
                | "SIMPLE-BASE-STRING"
                | "BIT-VECTOR"
                | "SIMPLE-BIT-VECTOR"
        )
    )
}

pub fn build_result_sequence(
    result_type: BlissVal,
    elems: &[BlissVal],
) -> Result<BlissVal, BlissError> {
    if !is_sequence_result_type(result_type) {
        return Err(BlissError::TypeError {
            datum: result_type,
            expected: "sequence type specifier".to_string(),
        });
    }
    // A length the specifier declares must match what was actually produced —
    // `(concatenate '(vector * 3) '(a b c d e))` is a TYPE-ERROR, not a
    // five-element vector (ansi CONCATENATE.ERROR.4, MAP.ERROR.2).
    if let Some(declared) = declared_sequence_length(result_type)
        && declared != elems.len()
    {
        return Err(BlissError::TypeError {
            datum: result_type,
            expected: format!("sequence type specifier matching length {}", elems.len()),
        });
    }
    if result_type_is_string(result_type) {
        let mut out = String::with_capacity(elems.len());
        for &elem in elems {
            if !elem.is_character() {
                return Err(BlissError::TypeError {
                    datum: elem,
                    expected: "character".to_string(),
                });
            }
            out.push(elem.as_char());
        }
        // FRESH, not interned. make_lisp_string interns by content, so two
        // CONCATENATE (or MAP) calls producing the same characters answered the
        // SAME object — `(eq (concatenate 'string "ab" "cd")
        //                    (concatenate 'string "ab" "cd"))` was T. CLHS
        // requires a fresh sequence, and sharing one is worse than a wrong
        // answer: mutating either result corrupts the other, so a test that
        // builds a string this way can break an unrelated later one. SUBSEQ,
        // COPY-SEQ and REVERSE already use the fresh constructor for exactly
        // this reason (bliss-9kxg); this path did not.
        return Ok(crate::streams::make_lisp_string_fresh(&out));
    }
    if result_type_is_bit_vector(result_type) {
        // Every element must be a BIT; the generic builder maps any non-zero to
        // 1, which would silently accept a bad element. Mirror the string
        // branch's type check instead.
        for &elem in elems {
            if !(elem.is_fixnum() && matches!(elem.as_fixnum(), 0 | 1)) {
                return Err(BlissError::TypeError {
                    datum: elem,
                    expected: "bit".to_string(),
                });
            }
        }
        return Ok(build_bit_vector_from_vals(elems));
    }
    if result_type_is_vector(result_type) {
        return Ok(build_vector(elems));
    }
    Ok(build_list(elems))
}

/// Concatenate sequences (CL `CONCATENATE`). R5.30.
pub fn concatenate(result_type: BlissVal, sequences: &[BlissVal]) -> Result<BlissVal, BlissError> {
    let mut all_elems = Vec::new();
    for &seq in sequences {
        let elems = collect_elements(seq)?;
        all_elems.extend(elems);
    }
    build_result_sequence(result_type, &all_elems)
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
