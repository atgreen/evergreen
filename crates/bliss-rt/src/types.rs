//! CL type lattice and type predicates.
//!
//! Maps the CL type hierarchy onto BlissVal tag + ObjectHeader type_id.
//! See §1.16–§1.17 of the spec.

use crate::object::{ElementTypeTag, ObjectHeader, type_id};
use crate::value::BlissVal;

/// Extract the `type_id` from a heap object's header.
///
/// # Safety
/// Caller must ensure `v` is a heap-object-tagged `BlissVal` (tag `010`).
pub unsafe fn type_id_of(v: BlissVal) -> u8 {
    let ptr = unsafe { v.as_ptr() } as *const ObjectHeader;
    unsafe { (*ptr).type_id() }
}

/// Helper: if v is a heap object, return its type_id; otherwise None.
fn heap_type_id(v: BlissVal) -> Option<u8> {
    if v.is_heap_object() {
        Some(unsafe { type_id_of(v) })
    } else {
        None
    }
}

// ── Primary type predicates (tag-only, O(1)) ───────────────────────

/// `FIXNUMP` — tag `000`.
pub fn fixnump(v: BlissVal) -> bool {
    v.is_fixnum()
}

/// `CONSP` — tag `001`.
pub fn consp(v: BlissVal) -> bool {
    v.is_cons()
}

/// `CHARACTERP` — tag `011`.
pub fn characterp(v: BlissVal) -> bool {
    v.is_character()
}

/// `SINGLE-FLOAT-P` — tag `100`.
pub fn single_float_p(v: BlissVal) -> bool {
    v.is_single_float()
}

/// `SYMBOLP` — tag `101` or special NIL/T.
pub fn symbolp(v: BlissVal) -> bool {
    v.is_symbol()
}

/// `FUNCTIONP` — tag `110`.
pub fn functionp(v: BlissVal) -> bool {
    v.is_function()
}

/// `NULLP` — exactly NIL.
pub fn nullp(v: BlissVal) -> bool {
    v.is_nil()
}

/// `LISTP` — cons or NIL.
pub fn listp(v: BlissVal) -> bool {
    v.is_list()
}

/// `HEAP-OBJECT-P` — tag `010`.
pub fn heap_object_p(v: BlissVal) -> bool {
    v.is_heap_object()
}

// ── Secondary type predicates (require header load) ────────────────

/// `STRINGP` — simple-base-string, simple-character-string, or complex string.
pub fn stringp(v: BlissVal) -> bool {
    matches!(
        heap_type_id(v),
        Some(type_id::SIMPLE_BASE_STRING) | Some(type_id::SIMPLE_CHARACTER_STRING)
    )
}

/// `VECTORP` — rank-1 array of any kind.
/// Includes simple-vector, simple-array, simple-base-string,
/// simple-character-string, complex-array.
pub fn vectorp(v: BlissVal) -> bool {
    matches!(
        heap_type_id(v),
        Some(type_id::SIMPLE_VECTOR)
            | Some(type_id::SIMPLE_ARRAY)
            | Some(type_id::SIMPLE_BASE_STRING)
            | Some(type_id::SIMPLE_CHARACTER_STRING)
            | Some(type_id::COMPLEX_ARRAY)
    )
}

/// `ARRAYP` — any array type.
pub fn arrayp(v: BlissVal) -> bool {
    vectorp(v)
}

/// `BIT-VECTOR-P` — rank-1 array with BIT element type.
/// Checks for SIMPLE_ARRAY with ElementTypeTag::Bit in the element-type
/// position (first byte of the word after the ObjectHeader).
pub fn bit_vector_p(v: BlissVal) -> bool {
    match heap_type_id(v) {
        Some(type_id::SIMPLE_ARRAY) => {
            // Read the element-type tag from the first byte after the header
            let ptr = unsafe { v.as_ptr() };
            let elem_type_ptr = unsafe { ptr.add(8) }; // skip 8-byte ObjectHeader
            let elem_byte = unsafe { *(elem_type_ptr as *const u8) };
            elem_byte == ElementTypeTag::Bit as u8
        }
        _ => false,
    }
}

/// Length of a bit-vector (the count of bits), or `None` if `v` is not a
/// bit-vector. Layout (see the reader's `alloc_bit_vector`): ObjectHeader(8) +
/// element-type tag word(8) + length:u64(8) + LSB-first packed bits.
pub fn bit_vector_len(v: BlissVal) -> Option<usize> {
    if !bit_vector_p(v) {
        return None;
    }
    let ptr = unsafe { v.as_ptr() };
    Some(unsafe { *(ptr.add(16) as *const u64) } as usize)
}

/// Bit `i` (0 or 1) of a bit-vector, or `None` if `v` is not a bit-vector or `i`
/// is out of bounds.
pub fn bit_vector_ref(v: BlissVal, i: usize) -> Option<u8> {
    let len = bit_vector_len(v)?;
    if i >= len {
        return None;
    }
    let ptr = unsafe { v.as_ptr() };
    let byte = unsafe { *ptr.add(24 + i / 8) };
    Some((byte >> (i % 8)) & 1)
}

/// Store bit `i` of a bit-vector in place (bliss-27f5). Returns `false` if
/// `v` is not a bit-vector or `i` is out of bounds. `bit` is stored as 0 for
/// zero, 1 otherwise. No allocation — GC-safe anywhere.
pub fn bit_vector_set(v: BlissVal, i: usize, bit: u8) -> bool {
    let Some(len) = bit_vector_len(v) else {
        return false;
    };
    if i >= len {
        return false;
    }
    let ptr = unsafe { v.as_ptr() as *mut u8 };
    unsafe {
        let byte = ptr.add(24 + i / 8);
        if bit != 0 {
            *byte |= 1 << (i % 8);
        } else {
            *byte &= !(1 << (i % 8));
        }
    }
    true
}

/// Build a fresh simple bit-vector from a slice of 0/1 bytes (any non-zero byte
/// stores a 1). Mirrors the reader's `#*` allocation exactly (same
/// `SIMPLE_ARRAY`/`ElementTypeTag::Bit` layout) so reader- and runtime-produced
/// bit-vectors are indistinguishable. Layout: ObjectHeader(8) + element-type
/// tag byte + padding(7) + length:u64(8) + LSB-first packed bits.
///
/// Bit-vectors are small objects (never the large-object path), so the standard
/// 8-byte body-header offset applies and `from_heap_ptr` on the write base is
/// correct. GC-safe: no live BlissVal is held across the single allocation.
pub fn make_bit_vector(bits: &[u8]) -> BlissVal {
    let hdr = core::mem::size_of::<ObjectHeader>();
    let data_bytes = bits.len().div_ceil(8);
    // body = element-type word(8) + length(8) + packed data
    let body_size = (8 + 8 + data_bytes).max(1);
    let body = match crate::gc::alloc_typed(body_size, type_id::SIMPLE_ARRAY) {
        Some(b) => b,
        None => std::alloc::handle_alloc_error(
            std::alloc::Layout::from_size_align(hdr + body_size, hdr).unwrap(),
        ),
    };
    unsafe {
        // `body` is the payload start (offset hdr past the object header).
        *body = ElementTypeTag::Bit as u8;
        *(body.add(8) as *mut u64) = bits.len() as u64;
        for (i, &b) in bits.iter().enumerate() {
            if b != 0 {
                *body.add(16 + i / 8) |= 1 << (i % 8);
            }
        }
        BlissVal::from_heap_ptr(body.sub(hdr))
    }
}

/// `NUMBERP` — fixnum, single-float, or heap numeric types.
pub fn numberp(v: BlissVal) -> bool {
    if v.is_fixnum() || v.is_single_float() {
        return true;
    }
    matches!(
        heap_type_id(v),
        Some(type_id::BIGNUM)
            | Some(type_id::RATIO)
            | Some(type_id::COMPLEX)
            | Some(type_id::DOUBLE_FLOAT)
    )
}

/// `INTEGERP` — fixnum or bignum.
pub fn integerp(v: BlissVal) -> bool {
    if v.is_fixnum() {
        return true;
    }
    heap_type_id(v) == Some(type_id::BIGNUM)
}

/// `RATIONALP` — integer or ratio.
pub fn rationalp(v: BlissVal) -> bool {
    if integerp(v) {
        return true;
    }
    heap_type_id(v) == Some(type_id::RATIO)
}

/// `REALP` — rational or float.
pub fn realp(v: BlissVal) -> bool {
    if rationalp(v) || v.is_single_float() {
        return true;
    }
    heap_type_id(v) == Some(type_id::DOUBLE_FLOAT)
}

/// `FLOATP` — single-float or double-float.
pub fn floatp(v: BlissVal) -> bool {
    if v.is_single_float() {
        return true;
    }
    heap_type_id(v) == Some(type_id::DOUBLE_FLOAT)
}

/// `COMPLEXP` — complex number.
pub fn complexp(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::COMPLEX)
}

/// The real part of a COMPLEX heap object, or `None` if `v` is not complex.
/// Layout (ComplexData): ObjectHeader(8) + realpart@8 + imagpart@16.
pub fn complex_realpart(v: BlissVal) -> Option<BlissVal> {
    if !complexp(v) {
        return None;
    }
    Some(unsafe { *(v.as_ptr().add(8) as *const BlissVal) })
}

/// The imaginary part of a COMPLEX heap object, or `None` if `v` is not complex.
pub fn complex_imagpart(v: BlissVal) -> Option<BlissVal> {
    if !complexp(v) {
        return None;
    }
    Some(unsafe { *(v.as_ptr().add(16) as *const BlissVal) })
}

/// `MD-ARRAY-P` — a multidimensional (rank ≥ 2) array. Layout (MdArrayData):
/// ObjectHeader(8) + storage@8 + dims@16 + rank@24.
pub fn md_array_p(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::MD_ARRAY)
}

/// The row-major storage SIMPLE_VECTOR of a multidimensional array, or `None`.
pub fn md_array_storage(v: BlissVal) -> Option<BlissVal> {
    if !md_array_p(v) {
        return None;
    }
    Some(unsafe { *(v.as_ptr().add(8) as *const BlissVal) })
}

/// The dimensions SIMPLE_VECTOR (of fixnums) of a multidimensional array.
pub fn md_array_dims(v: BlissVal) -> Option<BlissVal> {
    if !md_array_p(v) {
        return None;
    }
    Some(unsafe { *(v.as_ptr().add(16) as *const BlissVal) })
}

/// The rank (a fixnum) of a multidimensional array.
pub fn md_array_rank(v: BlissVal) -> Option<BlissVal> {
    if !md_array_p(v) {
        return None;
    }
    Some(unsafe { *(v.as_ptr().add(24) as *const BlissVal) })
}

/// `PACKAGEP` — package object.
pub fn packagep(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::PACKAGE)
}

/// `HASH-TABLE-P` — hash table object.
pub fn hash_table_p(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::HASH_TABLE)
}

/// `STREAMP` — stream object.
pub fn streamp(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::STREAM)
}

/// `PATHNAMEP` — pathname object.
pub fn pathnamep(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::PATHNAME)
}

/// `READTABLEP` — readtable object.
pub fn readtablep(v: BlissVal) -> bool {
    heap_type_id(v) == Some(type_id::READTABLE)
}

// ── TYPEP dispatch ─────────────────────────────────────────────────

/// General `TYPEP` dispatch. Type specifier is represented as a BlissVal
/// (a symbol or compound type form).
///
/// Currently implements a minimal bootstrap version that recognizes
/// symbol indices as type specifiers. Full implementation requires the
/// symbol table to be bootstrapped.
pub fn typep(value: BlissVal, type_specifier: BlissVal) -> bool {
    // Bootstrap: use symbol index as a type discriminator.
    // Index 0 = FIXNUM type, and we check if value matches.
    if type_specifier.tag() == crate::value::TAG_SYMBOL {
        let idx = type_specifier.as_symbol_index();
        match idx {
            0 => fixnump(value),
            1 => consp(value),
            2 => characterp(value),
            3 => single_float_p(value),
            4 => symbolp(value),
            5 => functionp(value),
            6 => nullp(value),
            7 => listp(value),
            8 => numberp(value),
            9 => integerp(value),
            10 => stringp(value),
            _ => false,
        }
    } else {
        false
    }
}

/// `SUBTYPEP` — determine subtype relationship between two type specifiers.
/// Returns `(subtype-p, valid-p)`.
///
/// Bootstrap implementation: a type is always a subtype of itself.
/// Full implementation requires the type lattice from the symbol table.
pub fn subtypep(type1: BlissVal, type2: BlissVal) -> (bool, bool) {
    // Bootstrap: a type is a subtype of itself.
    if type1 == type2 {
        (true, true)
    } else {
        (false, false)
    }
}
