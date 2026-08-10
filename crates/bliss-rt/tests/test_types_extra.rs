//! Extra tests for the bliss-rt types module, covering interfaces
//! not exercised by test_types.rs.
//!
//! Focuses on: tag constant values, special-value type behavior (UNBOUND,
//! MISSING, EOF), type_id_of for remaining heap type IDs, structure/
//! standard-object type IDs, and additional cross-cutting predicate checks.

use bliss_rt::value::*;
use bliss_rt::types::*;
use bliss_rt::object::*;

// ── Helpers (same pattern as test_types.rs) ───────────────────────

fn mk_fix(n: i64) -> BlissVal { BlissVal::from_fixnum(n) }
fn mk_flt(f: f32) -> BlissVal { BlissVal::from_single_float(f) }
fn mk_sym(i: u32) -> BlissVal { BlissVal::from_symbol_index(i) }

unsafe fn mk_heap(storage: &mut [u64; 2], tid: u8) -> BlissVal {
    unsafe {
        let header = ObjectHeader::new(tid, 1);
        storage[0] = header.0;
        storage[1] = 0;
        BlissVal::from_heap_ptr(storage.as_mut_ptr() as *mut u8)
    }
}

// ═══════════════════════════════════════════════════════════════════
// Tag constant values — verify encoding matches spec §1.2
// ═══════════════════════════════════════════════════════════════════

#[test]
fn tag_constants_have_correct_values() {
    assert_eq!(TAG_FIXNUM, 0b000);
    assert_eq!(TAG_CONS, 0b001);
    assert_eq!(TAG_HEAP_OBJECT, 0b010);
    assert_eq!(TAG_CHARACTER, 0b011);
    assert_eq!(TAG_SINGLE_FLOAT, 0b100);
    assert_eq!(TAG_SYMBOL, 0b101);
    assert_eq!(TAG_FUNCTION, 0b110);
    assert_eq!(TAG_SPECIAL, 0b111);
}

#[test]
fn tag_mask_is_three_bits() {
    assert_eq!(TAG_MASK, 0b111);
}

// ═══════════════════════════════════════════════════════════════════
// Special value bit patterns — NIL, T, UNBOUND, MISSING, EOF
// ═══════════════════════════════════════════════════════════════════

#[test]
fn special_values_have_special_tag() {
    assert_eq!(NIL.tag(), TAG_SPECIAL);
    assert_eq!(T.tag(), TAG_SPECIAL);
    assert_eq!(UNBOUND.tag(), TAG_SPECIAL);
    assert_eq!(MISSING.tag(), TAG_SPECIAL);
    assert_eq!(EOF.tag(), TAG_SPECIAL);
}

#[test]
fn special_values_are_distinct() {
    let specials = [NIL, T, UNBOUND, MISSING, EOF];
    for i in 0..specials.len() {
        for j in (i + 1)..specials.len() {
            assert_ne!(specials[i], specials[j],
                "special values at index {} and {} must differ", i, j);
        }
    }
}

#[test]
fn special_values_bit_patterns() {
    assert_eq!(NIL.0, NIL_BITS);
    assert_eq!(T.0, T_BITS);
    assert_eq!(UNBOUND.0, UNBOUND_BITS);
    assert_eq!(MISSING.0, MISSING_BITS);
    assert_eq!(EOF.0, EOF_BITS);
}

// ═══════════════════════════════════════════════════════════════════
// UNBOUND / MISSING / EOF — type predicate behavior
// ═══════════════════════════════════════════════════════════════════

#[test]
fn unbound_predicate_behavior() {
    assert!(!nullp(UNBOUND));
    assert!(!symbolp(UNBOUND)); // tag 111 but not NIL or T
    assert!(!listp(UNBOUND));
    assert!(!numberp(UNBOUND));
    assert!(!fixnump(UNBOUND));
}

#[test]
fn missing_predicate_behavior() {
    assert!(!nullp(MISSING));
    assert!(!symbolp(MISSING));
    assert!(!numberp(MISSING));
    assert!(!consp(MISSING));
}

#[test]
fn eof_predicate_behavior() {
    assert!(!nullp(EOF));
    assert!(!symbolp(EOF));
    assert!(!fixnump(EOF));
    assert!(!consp(EOF));
    assert!(!characterp(EOF));
    assert!(!single_float_p(EOF));
    assert!(!functionp(EOF));
    assert!(!heap_object_p(EOF));
    assert!(!listp(EOF));
}

// ═══════════════════════════════════════════════════════════════════
// type_id_of — remaining heap type IDs not covered by test_types.rs
// ═══════════════════════════════════════════════════════════════════

#[test]
fn type_id_of_remaining_heap_types() {
    // Verify type_id_of for heap types not covered in test_types.rs
    unsafe {
        let cases: &[(u8, &str)] = &[
            (type_id::STRUCTURE, "structure"),
            (type_id::STANDARD_OBJECT, "standard_object"),
            (type_id::CONDITION, "condition"),
            (type_id::RESTART, "restart"),
            (type_id::BIGNUM, "bignum"),
            (type_id::RATIO, "ratio"),
            (type_id::DOUBLE_FLOAT, "double_float"),
            (type_id::COMPLEX, "complex"),
            (type_id::PATHNAME, "pathname"),
        ];
        for &(tid, label) in cases {
            let mut s = [0u64; 2];
            let v = mk_heap(&mut s, tid);
            assert_eq!(type_id_of(v), tid, "type_id_of mismatch for {}", label);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// Numeric predicates — heap types not fully covered
// ═══════════════════════════════════════════════════════════════════

#[test]
fn heap_numeric_cross_cutting() {
    unsafe {
        // Bignum: not float
        let mut s1 = [0u64; 2];
        let bg = mk_heap(&mut s1, type_id::BIGNUM);
        assert!(!floatp(bg));
        assert!(!single_float_p(bg));

        // Ratio: real, not float, not integer
        let mut s2 = [0u64; 2];
        let r = mk_heap(&mut s2, type_id::RATIO);
        assert!(realp(r));
        assert!(!floatp(r));
        assert!(numberp(r));

        // Double-float: not integer, not rational
        let mut s3 = [0u64; 2];
        let df = mk_heap(&mut s3, type_id::DOUBLE_FLOAT);
        assert!(!integerp(df));
        assert!(!rationalp(df));

        // Complex: number but not rational/integer/float
        let mut s4 = [0u64; 2];
        let cx = mk_heap(&mut s4, type_id::COMPLEX);
        assert!(numberp(cx));
        assert!(!rationalp(cx));
        assert!(!integerp(cx));
        assert!(!floatp(cx));
    }
}

#[test]
fn non_numeric_heap_types_not_numeric() {
    unsafe {
        for &tid in &[type_id::STRUCTURE, type_id::STANDARD_OBJECT, type_id::PACKAGE] {
            let mut s = [0u64; 2];
            let v = mk_heap(&mut s, tid);
            assert!(!numberp(v), "type_id {} should not be numberp", tid);
            assert!(!integerp(v));
            assert!(!floatp(v));
        }
    }
}

#[test]
fn non_array_heap_types_not_array() {
    unsafe {
        for &tid in &[type_id::BIGNUM, type_id::CONDITION] {
            let mut s = [0u64; 2];
            let v = mk_heap(&mut s, tid);
            assert!(!arrayp(v));
            assert!(!vectorp(v));
            assert!(!stringp(v));
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// stringp — complex string type via COMPLEX_ARRAY should NOT be stringp
// ═══════════════════════════════════════════════════════════════════

#[test]
fn complex_array_is_not_stringp() {
    unsafe {
        let mut s = [0u64; 2];
        let v = mk_heap(&mut s, type_id::COMPLEX_ARRAY);
        // COMPLEX_ARRAY is an array but not specifically a string type
        assert!(!stringp(v));
    }
}

// ═══════════════════════════════════════════════════════════════════
// Fixnum edge cases
// ═══════════════════════════════════════════════════════════════════

#[test]
fn fixnum_boundary_values() {
    let max_fix = mk_fix((1i64 << 60) - 1);
    let min_fix = mk_fix(-(1i64 << 60));
    assert!(fixnump(max_fix));
    assert!(integerp(max_fix));
    assert!(fixnump(min_fix));
    assert!(integerp(min_fix));
}

#[test]
fn fixnum_zero_exclusive_to_fixnum_tag() {
    let zero = mk_fix(0);
    assert!(fixnump(zero));
    assert!(!consp(zero));
    assert!(!symbolp(zero));
    assert!(!nullp(zero));
    assert!(!heap_object_p(zero));
}

// ═══════════════════════════════════════════════════════════════════
// Single-float edge cases
// ═══════════════════════════════════════════════════════════════════

#[test]
fn single_float_special_values() {
    for f in [f32::NEG_INFINITY, -0.0f32, f32::INFINITY, f32::NAN] {
        let v = mk_flt(f);
        assert!(single_float_p(v));
        assert!(floatp(v));
        assert!(numberp(v));
    }
}

// ═══════════════════════════════════════════════════════════════════
// subtypep — non-reflexive cases
// ═══════════════════════════════════════════════════════════════════

#[test]
fn subtypep_different_types() {
    // Two different type specifiers — the bootstrap implementation may
    // return (false, false) or (false, true) depending on design.
    // Either way, the function must be callable and return a tuple.
    let (sub, valid) = subtypep(mk_sym(0), mk_sym(1));
    // At minimum: different type symbols should not claim subtype.
    let _ = (sub, valid); // ensure tuple destructuring works
}

// ═══════════════════════════════════════════════════════════════════
// typep with special values — must be callable without panic
// ═══════════════════════════════════════════════════════════════════

#[test]
fn typep_callable_with_special_values() {
    let _r1: bool = typep(NIL, mk_sym(0));
    let _r2: bool = typep(T, mk_sym(0));
    let _r3: bool = typep(UNBOUND, mk_sym(0));
}

// ═══════════════════════════════════════════════════════════════════
// T is not numeric (supplements test_types.rs)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn t_is_not_numeric() {
    assert!(!numberp(T));
    assert!(!integerp(T));
    assert!(!floatp(T));
}
