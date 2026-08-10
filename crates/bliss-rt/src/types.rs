//! CL type lattice and type predicates.
//!
//! Maps the CL type hierarchy onto BlissVal tag + ObjectHeader type_id.
//! See §1.16–§1.17 of the spec.

use crate::object::{ObjectHeader, type_id, ElementTypeTag};
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
    match heap_type_id(v) {
        Some(type_id::SIMPLE_BASE_STRING) | Some(type_id::SIMPLE_CHARACTER_STRING) => true,
        _ => false,
    }
}

/// `VECTORP` — rank-1 array of any kind.
/// Includes simple-vector, simple-array, simple-base-string,
/// simple-character-string, complex-array.
pub fn vectorp(v: BlissVal) -> bool {
    match heap_type_id(v) {
        Some(type_id::SIMPLE_VECTOR)
        | Some(type_id::SIMPLE_ARRAY)
        | Some(type_id::SIMPLE_BASE_STRING)
        | Some(type_id::SIMPLE_CHARACTER_STRING)
        | Some(type_id::COMPLEX_ARRAY) => true,
        _ => false,
    }
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

/// `NUMBERP` — fixnum, single-float, or heap numeric types.
pub fn numberp(v: BlissVal) -> bool {
    if v.is_fixnum() || v.is_single_float() {
        return true;
    }
    match heap_type_id(v) {
        Some(type_id::BIGNUM)
        | Some(type_id::RATIO)
        | Some(type_id::COMPLEX)
        | Some(type_id::DOUBLE_FLOAT) => true,
        _ => false,
    }
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
