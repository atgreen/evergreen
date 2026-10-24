// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `EQL`, `EQUAL` and `EQUALP` (CLHS 5.3).
//!
//! The one definition shared by the evaluator's builtins and by EQL/EQUAL/
//! EQUALP hash tables. They used to have separate copies that had drifted: the
//! table's EQUALP could not see that `1` and `1d0`, a fill-pointer string and a
//! simple one, or two structures with EQUALP slots are EQUALP, so `GETHASH`
//! missed keys the predicate called equal. A table's hash function must agree
//! with its test, and that is only maintainable when there is one test.
//!
//! Nothing here allocates Lisp objects -- strings and bignum magnitudes are
//! copied into Rust memory -- so these are safe to call from a hash-table probe
//! and from list walkers holding unrooted locals.

use crate::numbers::float_exact_rational;
use egcl_rt::bignum::{BigRat, as_bigrat, bigint_from_val, bigrat_cmp, ratio_parts_val};
use egcl_rt::object::{ConsCell, type_id};
use egcl_rt::value::EgclVal;
use std::cmp::Ordering;

/// The heap type of a boxed number, or `None` for anything else.
fn heap_number_type(v: EgclVal) -> Option<u8> {
    egcl_rt::gc::heap_object_type_id(v).filter(|t| {
        matches!(
            *t,
            type_id::BIGNUM | type_id::RATIO | type_id::COMPLEX | type_id::DOUBLE_FLOAT
        )
    })
}

/// NUMBERP, without dereferencing a registry sentinel.
pub fn numberp(v: EgclVal) -> bool {
    v.is_fixnum() || v.is_single_float() || heap_number_type(v).is_some()
}

/// A number's real and imaginary parts; a real number's imaginary part is 0.
pub(crate) fn number_parts(v: EgclVal) -> (EgclVal, EgclVal) {
    match (
        egcl_rt::types::complex_realpart(v),
        egcl_rt::types::complex_imagpart(v),
    ) {
        (Some(re), Some(im)) => (re, im),
        _ => (v, EgclVal::from_fixnum(0)),
    }
}

/// A float's value, or `None` for a rational.
pub(crate) fn float_value(v: EgclVal) -> Option<f64> {
    if v.is_single_float() {
        Some(f64::from(v.as_single_float()))
    } else if v.is_double_float() {
        Some(v.as_double_float())
    } else {
        None
    }
}

/// The exact value of a real: a rational as itself, a finite float decoded
/// from its bits. `None` for an infinity, a NaN, or a non-real.
pub(crate) fn exact_rational(v: EgclVal) -> Option<BigRat> {
    match float_value(v) {
        Some(f) => f.is_finite().then(|| float_exact_rational(f)),
        None => as_bigrat(v),
    }
}

/// `=` on two reals, exactly: a float equals a rational only when it denotes
/// that rational, however large (CLHS 12.1.4.1). Infinities and NaNs compare
/// as floats, so a NaN equals nothing.
fn real_equal(a: EgclVal, b: EgclVal) -> bool {
    if a.is_fixnum() && b.is_fixnum() {
        return a == b;
    }
    if let (Some(x), Some(y)) = (float_value(a), float_value(b)) {
        return x == y;
    }
    match (exact_rational(a), exact_rational(b)) {
        (Some(x), Some(y)) => bigrat_cmp(&x, &y) == Ordering::Equal,
        _ => false,
    }
}

/// `=` on two numbers: a complex equals a real when its imaginary part is
/// zero, and two complexes compare part by part.
pub fn number_equal(a: EgclVal, b: EgclVal) -> bool {
    let (ar, ai) = number_parts(a);
    let (br, bi) = number_parts(b);
    real_equal(ar, br) && real_equal(ai, bi)
}

/// `EQL`: identity, or two numbers of the same type and value. Floats compare
/// by representation, so `0d0` and `-0d0` are distinct and a NaN is EQL to an
/// identical NaN -- `=` would say the reverse of both.
pub fn eql(a: EgclVal, b: EgclVal) -> bool {
    if a == b {
        return true;
    }
    let (Some(ta), Some(tb)) = (heap_number_type(a), heap_number_type(b)) else {
        return false;
    };
    if ta != tb {
        return false;
    }
    match ta {
        type_id::DOUBLE_FLOAT => a.as_double_float().to_bits() == b.as_double_float().to_bits(),
        type_id::BIGNUM => match (bigint_from_val(a), bigint_from_val(b)) {
            (Some(x), Some(y)) => x.sign == y.sign && x.mag == y.mag,
            _ => false,
        },
        type_id::RATIO => match (ratio_parts_val(a), ratio_parts_val(b)) {
            (Some((an, ad)), Some((bn, bd))) => eql(an, bn) && eql(ad, bd),
            _ => false,
        },
        type_id::COMPLEX => {
            let (ar, ai) = number_parts(a);
            let (br, bi) = number_parts(b);
            eql(ar, br) && eql(ai, bi)
        }
        _ => false,
    }
}

/// The characters of any string -- simple, fill-pointer, adjustable,
/// displaced, or a registry-backed sentinel -- or `None` for a non-string. A
/// pathname is never a string, even when its namestring is registered.
/// The object pointer of `v` when it is a real SIMPLE-BASE-STRING or
/// SIMPLE-CHARACTER-STRING, for comparing or hashing it in place. A
/// registry-backed sentinel (whose bits are not a pointer) is never one.
pub(crate) fn simple_string_ptr(v: EgclVal) -> Option<*const u8> {
    if !v.is_heap_object() || crate::pathnames::is_registered_string(v) {
        return None;
    }
    // Safety: not a sentinel, so a heap-tagged value points at a live header.
    let ptr = unsafe { v.as_ptr() } as *const u8;
    let tid = unsafe { (*(ptr as *const egcl_rt::object::ObjectHeader)).type_id() };
    (tid == egcl_rt::object::type_id::SIMPLE_BASE_STRING
        || tid == egcl_rt::object::type_id::SIMPLE_CHARACTER_STRING)
        .then_some(ptr)
}

pub(crate) fn string_text(v: EgclVal) -> Option<String> {
    if crate::pathnames::is_pathname(v) {
        return None;
    }
    if let Some(s) = crate::pathnames::registered_string(v) {
        return Some(s);
    }
    crate::sequences::string_content(v)
}

/// The active length of a bit vector, simple or complex.
pub(crate) fn bit_vector_length(v: EgclVal) -> Option<usize> {
    if let Some(n) = egcl_rt::types::bit_vector_len(v) {
        return Some(n);
    }
    (crate::sequences::is_complex_vector(v) && crate::sequences::cvec_is_bit(v))
        .then(|| crate::sequences::cvec_fill_pointer(v))
}

/// Bit `i` of a bit vector, simple or complex (whose bits are fixnums).
pub(crate) fn bit_vector_bit(v: EgclVal, i: usize) -> u8 {
    if let Some(bit) = egcl_rt::types::bit_vector_ref(v, i) {
        return bit;
    }
    crate::sequences::cvec_element(v, i)
        .ok()
        .filter(|e| e.is_fixnum())
        .map(|e| (e.as_fixnum() & 1) as u8)
        .unwrap_or(0)
}

/// VECTORP for EQUALP: any rank-1 array, including a packed double-float
/// vector and a registry-backed string (which is a string, so a vector, even
/// though it is not a heap pointer to read a header from). Never a pathname.
pub(crate) fn vectorp(v: EgclVal) -> bool {
    if crate::pathnames::is_pathname(v) {
        return false;
    }
    if crate::pathnames::registered_string(v).is_some() {
        return true;
    }
    egcl_rt::types::vectorp(v) || crate::sequences::is_double_float_array(v)
}

/// The DEFSTRUCT class of `v`, if `v` is a structure instance. A general
/// standard object is EQUALP only to itself.
pub(crate) fn structure_class(v: EgclVal) -> Option<EgclVal> {
    if !crate::clos::is_instance(v) {
        return None;
    }
    let class = crate::clos::class_of(v);
    crate::clos::is_structure_class(class).then_some(class)
}

/// Walk two lists in step, comparing cars with `element` and the final
/// non-cons tails with `atom`. Iterative along the cdr chain, so a long list
/// cannot overflow the stack.
fn lists_equal(mut a: EgclVal, mut b: EgclVal, element: fn(EgclVal, EgclVal) -> bool) -> bool {
    while a.is_cons() && b.is_cons() {
        if a == b {
            return true;
        }
        // Safety: both are conses.
        let (ca, cb) = unsafe {
            (
                &*(a.as_ptr() as *const ConsCell),
                &*(b.as_ptr() as *const ConsCell),
            )
        };
        if !element(ca.car, cb.car) {
            return false;
        }
        a = ca.cdr;
        b = cb.cdr;
    }
    element(a, b)
}

/// `EQUAL`: EQL, except that conses, strings and bit vectors compare by
/// contents and pathnames by components. Every other array is EQUAL only to
/// itself.
pub fn equal(a: EgclVal, b: EgclVal) -> bool {
    if a == b {
        return true;
    }
    if a.is_cons() && b.is_cons() {
        return lists_equal(a, b, equal);
    }
    if numberp(a) && numberp(b) {
        return eql(a, b);
    }
    if !a.is_heap_object() || !b.is_heap_object() {
        return false;
    }
    // Two simple strings, the usual EQUAL hash-table key: compare in place.
    // Neither is a pathname, so the general path below would reach the same
    // character comparison after two pathname-registry lookups and decoding
    // both into fresh `String`s (bliss-a2bso).
    if let (Some(x), Some(y)) = (simple_string_ptr(a), simple_string_ptr(b)) {
        // Safety: both are live simple strings; nothing here allocates.
        return unsafe { egcl_rt::object::simple_strings_eq(x, y) };
    }
    if crate::pathnames::is_pathname(a) || crate::pathnames::is_pathname(b) {
        return crate::pathnames::is_pathname(a)
            && crate::pathnames::is_pathname(b)
            && crate::pathnames::pathnames_equal(a, b);
    }
    if let (Some(x), Some(y)) = (bit_vector_length(a), bit_vector_length(b)) {
        return x == y && (0..x).all(|i| bit_vector_bit(a, i) == bit_vector_bit(b, i));
    }
    match (string_text(a), string_text(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// Compare a packed double-float element with any Lisp number, unboxed.
fn raw_double_equalp(raw: f64, other: EgclVal) -> bool {
    if !numberp(other) {
        return false;
    }
    let (re, im) = number_parts(other);
    let real_matches = match float_value(re) {
        Some(f) => raw == f,
        None if raw.is_finite() => as_bigrat(re)
            .is_some_and(|r| bigrat_cmp(&float_exact_rational(raw), &r) == Ordering::Equal),
        None => false,
    };
    real_matches && real_equal(im, EgclVal::from_fixnum(0))
}

/// Element `i` of two vectors under EQUALP, reading packed doubles unboxed so
/// the comparison never allocates.
fn vector_elements_equalp(a: EgclVal, b: EgclVal, i: usize) -> bool {
    use crate::sequences::{double_float_array_element, elt};
    match (
        double_float_array_element(a, i),
        double_float_array_element(b, i),
    ) {
        (Some(x), Some(y)) => x == y,
        (Some(x), None) => elt(b, i).is_ok_and(|y| raw_double_equalp(x, y)),
        (None, Some(y)) => elt(a, i).is_ok_and(|x| raw_double_equalp(y, x)),
        (None, None) => matches!((elt(a, i), elt(b, i)), (Ok(x), Ok(y)) if equalp(x, y)),
    }
}

/// `EQUALP`: EQUAL, extended. Numbers compare with `=`; characters and
/// strings ignore case; any two arrays compare by dimensions and EQUALP
/// elements whatever their element types (so a string is EQUALP to a general
/// vector of the same characters); structures of one type compare slot by
/// slot; hash tables by test, count and EQUALP values.
///
/// Case is folded with the Unicode 1:1 mapping CHAR-EQUAL uses (see
/// `characters::char_upcase`).
pub fn equalp(a: EgclVal, b: EgclVal) -> bool {
    if a == b {
        return true;
    }
    if a.is_cons() && b.is_cons() {
        return lists_equal(a, b, equalp);
    }
    if numberp(a) && numberp(b) {
        return number_equal(a, b);
    }
    if a.is_character() && b.is_character() {
        return crate::characters::char_equal(a.as_char(), b.as_char());
    }
    if !a.is_heap_object() || !b.is_heap_object() {
        return false;
    }
    if crate::pathnames::is_pathname(a) || crate::pathnames::is_pathname(b) {
        return crate::pathnames::is_pathname(a)
            && crate::pathnames::is_pathname(b)
            && crate::pathnames::pathnames_equal(a, b);
    }
    if let (Some(x), Some(y)) = (string_text(a), string_text(b)) {
        return x.chars().count() == y.chars().count()
            && x.chars()
                .zip(y.chars())
                .all(|(p, q)| crate::characters::char_equal(p, q));
    }
    if vectorp(a) && vectorp(b) {
        let (Ok(n), Ok(m)) = (crate::sequences::length(a), crate::sequences::length(b)) else {
            return false;
        };
        return n == m && (0..n).all(|i| vector_elements_equalp(a, b, i));
    }
    if egcl_rt::types::md_array_p(a) && egcl_rt::types::md_array_p(b) {
        return match (
            egcl_rt::types::md_array_dims(a),
            egcl_rt::types::md_array_dims(b),
            egcl_rt::types::md_array_storage(a),
            egcl_rt::types::md_array_storage(b),
        ) {
            (Some(da), Some(db), Some(sa), Some(sb)) => equalp(da, db) && equalp(sa, sb),
            _ => false,
        };
    }
    if let (Some(ca), Some(cb)) = (structure_class(a), structure_class(b)) {
        if ca != cb {
            return false;
        }
        return crate::clos::effective_slots(ca).into_iter().all(|slot| {
            matches!(
                (crate::clos::slot_value(a, slot), crate::clos::slot_value(b, slot)),
                (Ok(x), Ok(y)) if equalp(x, y)
            )
        });
    }
    if crate::hashtable::hash_table_p(a) && crate::hashtable::hash_table_p(b) {
        return hash_tables_equalp(a, b);
    }
    false
}

/// Two hash tables are EQUALP when they have the same test and count and each
/// key of one maps, in the other, to an EQUALP value (CLHS EQUALP).
fn hash_tables_equalp(a: EgclVal, b: EgclVal) -> bool {
    use crate::hashtable::{gethash, hash_table_count, hash_table_entries, hash_table_test};
    let same_shape = matches!(
        (hash_table_test(a), hash_table_test(b)),
        (Ok(x), Ok(y)) if x == y
    ) && matches!(
        (hash_table_count(a), hash_table_count(b)),
        (Ok(x), Ok(y)) if x == y
    );
    same_shape
        && hash_table_entries(a).is_ok_and(|entries| {
            entries.into_iter().all(|(key, value)| {
                matches!(
                    gethash(key, b, egcl_rt::value::NIL),
                    Ok((other, true)) if equalp(value, other)
                )
            })
        })
}
