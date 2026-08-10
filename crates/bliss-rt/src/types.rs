//! CL type lattice and type predicates.
//!
//! Maps the CL type hierarchy onto BlissVal tag + ObjectHeader type_id.
//! See §1.16–§1.17 of the spec.

use crate::value::BlissVal;

/// Extract the `type_id` from a heap object's header.
///
/// # Safety
/// Caller must ensure `v` is a heap-object-tagged `BlissVal` (tag `010`).
pub unsafe fn type_id_of(v: BlissVal) -> u8 {
    unimplemented!("type_id_of")
}

// ── Primary type predicates (tag-only, O(1)) ───────────────────────

/// `FIXNUMP` — tag `000`.
pub fn fixnump(v: BlissVal) -> bool {
    unimplemented!("fixnump")
}

/// `CONSP` — tag `001`.
pub fn consp(v: BlissVal) -> bool {
    unimplemented!("consp")
}

/// `CHARACTERP` — tag `011`.
pub fn characterp(v: BlissVal) -> bool {
    unimplemented!("characterp")
}

/// `SINGLE-FLOAT-P` — tag `100`.
pub fn single_float_p(v: BlissVal) -> bool {
    unimplemented!("single_float_p")
}

/// `SYMBOLP` — tag `101` or special NIL/T.
pub fn symbolp(v: BlissVal) -> bool {
    unimplemented!("symbolp")
}

/// `FUNCTIONP` — tag `110`.
pub fn functionp(v: BlissVal) -> bool {
    unimplemented!("functionp")
}

/// `NULLP` — exactly NIL.
pub fn nullp(v: BlissVal) -> bool {
    unimplemented!("nullp")
}

/// `LISTP` — cons or NIL.
pub fn listp(v: BlissVal) -> bool {
    unimplemented!("listp")
}

/// `HEAP-OBJECT-P` — tag `010`.
pub fn heap_object_p(v: BlissVal) -> bool {
    unimplemented!("heap_object_p")
}

// ── Secondary type predicates (require header load) ────────────────

/// `STRINGP` — simple-base-string, simple-character-string, or complex string.
pub fn stringp(v: BlissVal) -> bool {
    unimplemented!("stringp")
}

/// `VECTORP` — rank-1 array of any kind.
pub fn vectorp(v: BlissVal) -> bool {
    unimplemented!("vectorp")
}

/// `ARRAYP` — any array type.
pub fn arrayp(v: BlissVal) -> bool {
    unimplemented!("arrayp")
}

/// `BIT-VECTOR-P` — rank-1 array with BIT element type.
pub fn bit_vector_p(v: BlissVal) -> bool {
    unimplemented!("bit_vector_p")
}

/// `NUMBERP` — fixnum, single-float, or heap numeric types.
pub fn numberp(v: BlissVal) -> bool {
    unimplemented!("numberp")
}

/// `INTEGERP` — fixnum or bignum.
pub fn integerp(v: BlissVal) -> bool {
    unimplemented!("integerp")
}

/// `RATIONALP` — integer or ratio.
pub fn rationalp(v: BlissVal) -> bool {
    unimplemented!("rationalp")
}

/// `REALP` — rational or float.
pub fn realp(v: BlissVal) -> bool {
    unimplemented!("realp")
}

/// `FLOATP` — single-float or double-float.
pub fn floatp(v: BlissVal) -> bool {
    unimplemented!("floatp")
}

/// `COMPLEXP` — complex number.
pub fn complexp(v: BlissVal) -> bool {
    unimplemented!("complexp")
}

/// `PACKAGEP` — package object.
pub fn packagep(v: BlissVal) -> bool {
    unimplemented!("packagep")
}

/// `HASH-TABLE-P` — hash table object.
pub fn hash_table_p(v: BlissVal) -> bool {
    unimplemented!("hash_table_p")
}

/// `STREAMP` — stream object.
pub fn streamp(v: BlissVal) -> bool {
    unimplemented!("streamp")
}

/// `PATHNAMEP` — pathname object.
pub fn pathnamep(v: BlissVal) -> bool {
    unimplemented!("pathnamep")
}

/// `READTABLEP` — readtable object.
pub fn readtablep(v: BlissVal) -> bool {
    unimplemented!("readtablep")
}

// ── TYPEP dispatch ─────────────────────────────────────────────────

/// General `TYPEP` dispatch. Type specifier is represented as a BlissVal
/// (a symbol or compound type form).
pub fn typep(value: BlissVal, type_specifier: BlissVal) -> bool {
    unimplemented!("typep")
}

/// `SUBTYPEP` — determine subtype relationship between two type specifiers.
/// Returns `(subtype-p, valid-p)`.
pub fn subtypep(type1: BlissVal, type2: BlissVal) -> (bool, bool) {
    unimplemented!("subtypep")
}
