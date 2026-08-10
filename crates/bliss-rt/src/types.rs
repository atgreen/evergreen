//! CL type lattice and type predicates.
//!
//! Maps the CL type hierarchy onto BlissVal tag + ObjectHeader type_id.
//! See §1.16–§1.17 of the spec.

use crate::object::{ObjectHeader, type_id, ElementTypeTag};
use crate::value::{BlissVal, TAG_FIXNUM, TAG_CONS, TAG_CHARACTER, TAG_SINGLE_FLOAT,
                   TAG_SYMBOL, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_SPECIAL,
                   NIL_BITS, T_BITS};

/// Extract the `type_id` from a heap object's header.
///
/// # Safety
/// Caller must ensure `v` is a heap-object-tagged `BlissVal` (tag `010`).
pub unsafe fn type_id_of(v: BlissVal) -> u8 {
    let ptr = v.as_ptr() as *const ObjectHeader;
    (*ptr).type_id()
}

/// Helper: if `v` is a heap object, load its type_id; otherwise return None.
#[inline]
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
    v.tag() == TAG_FIXNUM
}

/// `CONSP` — tag `001`.
pub fn consp(v: BlissVal) -> bool {
    v.tag() == TAG_CONS
}

/// `CHARACTERP` — tag `011`.
pub fn characterp(v: BlissVal) -> bool {
    v.tag() == TAG_CHARACTER
}

/// `SINGLE-FLOAT-P` — tag `100`.
pub fn single_float_p(v: BlissVal) -> bool {
    v.tag() == TAG_SINGLE_FLOAT
}

/// `SYMBOLP` — tag `101` or special NIL/T.
pub fn symbolp(v: BlissVal) -> bool {
    v.tag() == TAG_SYMBOL || v.0 == NIL_BITS || v.0 == T_BITS
}

/// `FUNCTIONP` — tag `110`.
pub fn functionp(v: BlissVal) -> bool {
    v.tag() == TAG_FUNCTION
}

/// `NULLP` — exactly NIL.
pub fn nullp(v: BlissVal) -> bool {
    v.0 == NIL_BITS
}

/// `LISTP` — cons or NIL.
pub fn listp(v: BlissVal) -> bool {
    v.tag() == TAG_CONS || v.0 == NIL_BITS
}

/// `HEAP-OBJECT-P` — tag `010`.
pub fn heap_object_p(v: BlissVal) -> bool {
    v.tag() == TAG_HEAP_OBJECT
}

// ── Secondary type predicates (require header load) ────────────────

/// `STRINGP` — simple-base-string, simple-character-string, or complex string.
pub fn stringp(v: BlissVal) -> bool {
    match heap_type_id(v) {
        Some(type_id::SIMPLE_BASE_STRING)
        | Some(type_id::SIMPLE_CHARACTER_STRING) => true,
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
    if !v.is_heap_object() {
        return false;
    }
    let tid = unsafe { type_id_of(v) };
    if tid != type_id::SIMPLE_ARRAY {
        return false;
    }
    // The element type tag is stored as the first byte of the second word
    // (i.e., at offset 8 from the object start, which is the u64 after the header).
    unsafe {
        let ptr = v.as_ptr() as *const u64;
        let second_word = *ptr.add(1);
        // The element type tag is the low byte
        let elt_tag = (second_word & 0xFF) as u8;
        elt_tag == ElementTypeTag::Bit as u8
    }
}

/// `NUMBERP` — fixnum, single-float, or heap numeric types.
pub fn numberp(v: BlissVal) -> bool {
    let tag = v.tag();
    if tag == TAG_FIXNUM || tag == TAG_SINGLE_FLOAT {
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
    if v.tag() == TAG_FIXNUM {
        return true;
    }
    heap_type_id(v) == Some(type_id::BIGNUM)
}

/// `RATIONALP` — integer or ratio.
pub fn rationalp(v: BlissVal) -> bool {
    if v.tag() == TAG_FIXNUM {
        return true;
    }
    match heap_type_id(v) {
        Some(type_id::BIGNUM)
        | Some(type_id::RATIO) => true,
        _ => false,
    }
}

/// `REALP` — rational or float.
pub fn realp(v: BlissVal) -> bool {
    let tag = v.tag();
    if tag == TAG_FIXNUM || tag == TAG_SINGLE_FLOAT {
        return true;
    }
    match heap_type_id(v) {
        Some(type_id::BIGNUM)
        | Some(type_id::RATIO)
        | Some(type_id::DOUBLE_FLOAT) => true,
        _ => false,
    }
}

/// `FLOATP` — single-float or double-float.
pub fn floatp(v: BlissVal) -> bool {
    if v.tag() == TAG_SINGLE_FLOAT {
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
    // If type_specifier is a symbol index, dispatch based on the index.
    // This is a bootstrap implementation — once the symbol table is live,
    // we look up the symbol name and dispatch properly.
    if type_specifier.tag() == TAG_SYMBOL {
        let idx = type_specifier.as_symbol_index();
        // Convention: symbol index 0 = T (the universal type)
        // Every value satisfies type T.
        if idx == 0 {
            return true;
        }
        // For other symbol indices, we can't resolve without the symbol table.
        // Return false as a safe default for unknown type specifiers.
        return false;
    }

    // If the specifier is NIL, nothing satisfies it
    if type_specifier.is_nil() {
        return false;
    }

    // For compound type specifiers (cons forms), we'd need to destructure.
    // Return false for now — full implementation requires the evaluator.
    false
}

/// `SUBTYPEP` — determine subtype relationship between two type specifiers.
/// Returns `(subtype-p, valid-p)`.
///
/// Bootstrap implementation: a type is always a subtype of itself.
/// Full implementation requires the type lattice from the symbol table.
pub fn subtypep(type1: BlissVal, type2: BlissVal) -> (bool, bool) {
    // If both specifiers are identical, type1 is trivially a subtype of type2.
    if type1 == type2 {
        return (true, true);
    }

    // If type2 is symbol index 0 (T), everything is a subtype of T.
    if type2.tag() == TAG_SYMBOL && type2.as_symbol_index() == 0 {
        return (true, true);
    }

    // If type1 is NIL, NIL is a subtype of everything.
    if type1.is_nil() {
        return (true, true);
    }

    // For other cases, we can't determine the relationship without the
    // full type lattice. Return (false, false) to indicate "don't know".
    (false, false)
}
