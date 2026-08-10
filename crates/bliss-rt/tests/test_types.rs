//! Comprehensive tests for type predicates in bliss-rt.
//!
//! Tests all primary predicates (tag-only, O(1)) and secondary predicates
//! (requiring heap object header inspection) defined in crates/bliss-rt/src/types.rs.
//! Red phase: all tests expected to fail before implementation.

use bliss_rt::value::*;
use bliss_rt::types::*;
use bliss_rt::object::*;

// ── Helpers ────────────────────────────────────────────────────────

fn mk_fix(n: i64) -> BlissVal { BlissVal::from_fixnum(n) }
fn mk_chr(c: char) -> BlissVal { BlissVal::from_char(c) }
fn mk_flt(f: f32) -> BlissVal { BlissVal::from_single_float(f) }
fn mk_sym(i: u32) -> BlissVal { BlissVal::from_symbol_index(i) }

unsafe fn mk_heap(storage: &mut [u64; 2], tid: u8) -> BlissVal {
    let header = ObjectHeader::new(tid, 1);
    storage[0] = header.0;
    storage[1] = 0;
    BlissVal::from_heap_ptr(storage.as_mut_ptr() as *mut u8)
}

unsafe fn mk_cons(cell: &mut ConsCell) -> BlissVal {
    BlissVal::from_cons_ptr(cell as *mut ConsCell as *mut u8)
}

unsafe fn mk_func(storage: &mut [u64; 8]) -> BlissVal {
    BlissVal::from_function_ptr(storage.as_mut_ptr() as *mut u8)
}

// ═══════════════════════════════════════════════════════════════════
// fixnump
// ═══════════════════════════════════════════════════════════════════

#[test]
fn fixnump_true_cases() {
    assert!(fixnump(mk_fix(0)));
    assert!(fixnump(mk_fix(42)));
    assert!(fixnump(mk_fix(-1)));
}

#[test]
fn fixnump_false_cases() {
    assert!(!fixnump(mk_chr('A')));
    assert!(!fixnump(NIL));
    assert!(!fixnump(mk_flt(1.0)));
    assert!(!fixnump(mk_sym(0)));
}

// ═══════════════════════════════════════════════════════════════════
// consp
// ═══════════════════════════════════════════════════════════════════

#[test]
fn consp_true_for_cons() {
    unsafe {
        let mut cell = ConsCell { car: NIL, cdr: NIL };
        assert!(consp(mk_cons(&mut cell)));
    }
}

#[test]
fn consp_false_cases() {
    assert!(!consp(NIL));
    assert!(!consp(mk_fix(0)));
    assert!(!consp(mk_sym(5)));
}

// ═══════════════════════════════════════════════════════════════════
// characterp
// ═══════════════════════════════════════════════════════════════════

#[test]
fn characterp_true_cases() {
    assert!(characterp(mk_chr('A')));
    assert!(characterp(mk_chr('λ')));
}

#[test]
fn characterp_false_cases() {
    assert!(!characterp(mk_fix(65)));
    assert!(!characterp(NIL));
}

// ═══════════════════════════════════════════════════════════════════
// single_float_p
// ═══════════════════════════════════════════════════════════════════

#[test]
fn single_float_p_true_cases() {
    assert!(single_float_p(mk_flt(3.14)));
    assert!(single_float_p(mk_flt(0.0)));
    assert!(single_float_p(mk_flt(-1.5)));
}

#[test]
fn single_float_p_false_cases() {
    assert!(!single_float_p(mk_fix(3)));
    assert!(!single_float_p(NIL));
}

// ═══════════════════════════════════════════════════════════════════
// symbolp — tag 101 or special NIL/T
// ═══════════════════════════════════════════════════════════════════

#[test]
fn symbolp_true_for_symbol_index() {
    assert!(symbolp(mk_sym(42)));
}

#[test]
fn symbolp_true_for_nil() {
    assert!(symbolp(NIL));
}

#[test]
fn symbolp_true_for_t() {
    assert!(symbolp(T));
}

#[test]
fn symbolp_false_cases() {
    assert!(!symbolp(mk_fix(0)));
    assert!(!symbolp(mk_chr('A')));
    assert!(!symbolp(mk_flt(1.0)));
}

// ═══════════════════════════════════════════════════════════════════
// functionp
// ═══════════════════════════════════════════════════════════════════

#[test]
fn functionp_true_for_function() {
    unsafe {
        let mut s = [0u64; 8];
        assert!(functionp(mk_func(&mut s)));
    }
}

#[test]
fn functionp_false_cases() {
    assert!(!functionp(mk_fix(0)));
    assert!(!functionp(NIL));
    assert!(!functionp(mk_sym(0)));
}

/// Heap objects with function-related type_ids (FUNCTION_INTERPRETED,
/// COMPILED_FUNCTION, CLOSURE) are heap-tagged (010), not function-tagged (110).
/// functionp checks the tag, so these heap objects should NOT satisfy functionp
/// unless the implementation also inspects the heap type_id. This test documents
/// the expected behavior: tag-based functionp returns false for heap function objects.
#[test]
fn functionp_false_for_heap_function_types() {
    unsafe {
        let mut s1 = [0u64; 2];
        let interp = mk_heap(&mut s1, type_id::FUNCTION_INTERPRETED);
        // A heap object with FUNCTION_INTERPRETED type_id has heap tag (010),
        // not function tag (110), so tag-based functionp returns false.
        assert!(!functionp(interp),
            "heap FUNCTION_INTERPRETED should not satisfy tag-based functionp");

        let mut s2 = [0u64; 2];
        let compiled = mk_heap(&mut s2, type_id::COMPILED_FUNCTION);
        assert!(!functionp(compiled),
            "heap COMPILED_FUNCTION should not satisfy tag-based functionp");

        let mut s3 = [0u64; 2];
        let closure = mk_heap(&mut s3, type_id::CLOSURE);
        assert!(!functionp(closure),
            "heap CLOSURE should not satisfy tag-based functionp");
    }
}

// ═══════════════════════════════════════════════════════════════════
// nullp — exactly NIL
// ═══════════════════════════════════════════════════════════════════

#[test]
fn nullp_true_for_nil() {
    assert!(nullp(NIL));
}

#[test]
fn nullp_false_for_t() {
    assert!(!nullp(T));
}

#[test]
fn nullp_false_cases() {
    assert!(!nullp(mk_fix(0)));
    assert!(!nullp(mk_sym(0)));
    unsafe {
        let mut cell = ConsCell { car: NIL, cdr: NIL };
        assert!(!nullp(mk_cons(&mut cell)));
    }
}

// ═══════════════════════════════════════════════════════════════════
// listp — cons or NIL
// ═══════════════════════════════════════════════════════════════════

#[test]
fn listp_true_for_nil() {
    assert!(listp(NIL));
}

#[test]
fn listp_true_for_cons() {
    unsafe {
        let mut cell = ConsCell { car: mk_fix(1), cdr: NIL };
        assert!(listp(mk_cons(&mut cell)));
    }
}

#[test]
fn listp_false_for_t() {
    // T is special-tagged (tag 111) like NIL, but listp must return false for T.
    // This ensures listp doesn't accidentally return true for all special-tagged values.
    assert!(!listp(T));
}

#[test]
fn listp_false_cases() {
    assert!(!listp(mk_fix(0)));
    assert!(!listp(mk_sym(42)));
    assert!(!listp(mk_chr('x')));
    assert!(!listp(mk_flt(1.0)));
}

// ═══════════════════════════════════════════════════════════════════
// heap_object_p — tag 010
// ═══════════════════════════════════════════════════════════════════

#[test]
fn heap_object_p_true_for_heap() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(heap_object_p(mk_heap(&mut s, type_id::SIMPLE_VECTOR)));
    }
}

#[test]
fn heap_object_p_false_cases() {
    assert!(!heap_object_p(mk_fix(0)));
    assert!(!heap_object_p(NIL));
    unsafe {
        let mut cell = ConsCell { car: NIL, cdr: NIL };
        assert!(!heap_object_p(mk_cons(&mut cell)));
    }
}

// ═══════════════════════════════════════════════════════════════════
// stringp — simple-base-string, simple-character-string, complex string
// ═══════════════════════════════════════════════════════════════════

#[test]
fn stringp_true_cases() {
    unsafe {
        let mut s1 = [0u64; 2];
        assert!(stringp(mk_heap(&mut s1, type_id::SIMPLE_BASE_STRING)));
        let mut s2 = [0u64; 2];
        assert!(stringp(mk_heap(&mut s2, type_id::SIMPLE_CHARACTER_STRING)));
    }
}

#[test]
fn stringp_false_cases() {
    assert!(!stringp(mk_fix(0)));
    unsafe {
        let mut s = [0u64; 2];
        assert!(!stringp(mk_heap(&mut s, type_id::SIMPLE_VECTOR)));
    }
}

// ═══════════════════════════════════════════════════════════════════
// vectorp — rank-1 array
// ═══════════════════════════════════════════════════════════════════

#[test]
fn vectorp_true_cases() {
    unsafe {
        let mut s1 = [0u64; 2];
        assert!(vectorp(mk_heap(&mut s1, type_id::SIMPLE_VECTOR)));
        // Strings are vectors
        let mut s2 = [0u64; 2];
        assert!(vectorp(mk_heap(&mut s2, type_id::SIMPLE_BASE_STRING)));
    }
}

#[test]
fn vectorp_false_for_fixnum() {
    assert!(!vectorp(mk_fix(1)));
}

// ═══════════════════════════════════════════════════════════════════
// arrayp — any array type
// ═══════════════════════════════════════════════════════════════════

#[test]
fn arrayp_true_cases() {
    unsafe {
        let mut s1 = [0u64; 2];
        assert!(arrayp(mk_heap(&mut s1, type_id::SIMPLE_ARRAY)));
        let mut s2 = [0u64; 2];
        assert!(arrayp(mk_heap(&mut s2, type_id::COMPLEX_ARRAY)));
        // Vectors are arrays
        let mut s3 = [0u64; 2];
        assert!(arrayp(mk_heap(&mut s3, type_id::SIMPLE_VECTOR)));
    }
}

#[test]
fn arrayp_false_cases() {
    assert!(!arrayp(mk_fix(0)));
    unsafe {
        let mut s = [0u64; 2];
        assert!(!arrayp(mk_heap(&mut s, type_id::HASH_TABLE)));
    }
}

// ═══════════════════════════════════════════════════════════════════
// bit_vector_p
// ═══════════════════════════════════════════════════════════════════

/// A bit vector is a SIMPLE_ARRAY whose element type is BIT.
/// Since there is no dedicated BIT_VECTOR type_id, the implementation must
/// inspect both the type_id (SIMPLE_ARRAY) and the element-type tag
/// (ElementTypeTag::Bit) stored in the array header. We construct a heap
/// object that encodes SIMPLE_ARRAY with a Bit element-type byte at the
/// expected position (first byte after the ObjectHeader).
#[test]
fn bit_vector_p_true_for_simple_bit_array() {
    unsafe {
        // Layout: [ObjectHeader(SIMPLE_ARRAY)][element_type_tag = Bit, ...]
        let mut storage = [0u64; 4];
        let header = ObjectHeader::new(type_id::SIMPLE_ARRAY, 3);
        storage[0] = header.0;
        // Place ElementTypeTag::Bit (1) as the first byte of the second word,
        // which is where the array element-type tag is expected.
        storage[1] = ElementTypeTag::Bit as u64;
        let v = BlissVal::from_heap_ptr(storage.as_mut_ptr() as *mut u8);
        assert!(bit_vector_p(v), "SIMPLE_ARRAY with Bit element type should be bit_vector_p");
    }
}

#[test]
fn bit_vector_p_false_cases() {
    assert!(!bit_vector_p(mk_fix(0)));
    assert!(!bit_vector_p(NIL));
    unsafe {
        let mut s = [0u64; 2];
        assert!(!bit_vector_p(mk_heap(&mut s, type_id::SIMPLE_BASE_STRING)));
        // A SIMPLE_ARRAY with General element type is not a bit vector
        let mut s2 = [0u64; 4];
        let header = ObjectHeader::new(type_id::SIMPLE_ARRAY, 3);
        s2[0] = header.0;
        s2[1] = ElementTypeTag::General as u64;
        let v = BlissVal::from_heap_ptr(s2.as_mut_ptr() as *mut u8);
        assert!(!bit_vector_p(v), "SIMPLE_ARRAY with General element type should NOT be bit_vector_p");
    }
}

// ═══════════════════════════════════════════════════════════════════
// packagep, hash_table_p, streamp, pathnamep, readtablep
// ═══════════════════════════════════════════════════════════════════

#[test]
fn packagep_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(packagep(mk_heap(&mut s, type_id::PACKAGE)));
    }
}

#[test]
fn packagep_false() {
    assert!(!packagep(mk_fix(0)));
    assert!(!packagep(mk_sym(0)));
}

#[test]
fn hash_table_p_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(hash_table_p(mk_heap(&mut s, type_id::HASH_TABLE)));
    }
}

#[test]
fn hash_table_p_false() {
    assert!(!hash_table_p(mk_fix(0)));
    assert!(!hash_table_p(NIL));
}

#[test]
fn streamp_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(streamp(mk_heap(&mut s, type_id::STREAM)));
    }
}

#[test]
fn streamp_false() {
    assert!(!streamp(mk_fix(0)));
    assert!(!streamp(NIL));
}

#[test]
fn pathnamep_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(pathnamep(mk_heap(&mut s, type_id::PATHNAME)));
    }
}

#[test]
fn pathnamep_false() {
    assert!(!pathnamep(mk_fix(0)));
    unsafe {
        let mut s = [0u64; 2];
        assert!(!pathnamep(mk_heap(&mut s, type_id::STREAM)));
    }
}

#[test]
fn readtablep_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(readtablep(mk_heap(&mut s, type_id::READTABLE)));
    }
}

#[test]
fn readtablep_false() {
    assert!(!readtablep(mk_fix(0)));
    assert!(!readtablep(NIL));
}

// ═══════════════════════════════════════════════════════════════════
// complexp
// ═══════════════════════════════════════════════════════════════════

#[test]
fn complexp_true() {
    unsafe {
        let mut s = [0u64; 2];
        assert!(complexp(mk_heap(&mut s, type_id::COMPLEX)));
    }
}

#[test]
fn complexp_false() {
    assert!(!complexp(mk_fix(0)));
    assert!(!complexp(mk_flt(1.0)));
}

// ═══════════════════════════════════════════════════════════════════
// Numeric hierarchy — fixnum
// ═══════════════════════════════════════════════════════════════════

#[test]
fn fixnum_numeric_hierarchy() {
    let v = mk_fix(42);
    assert!(numberp(v),   "fixnum must be numberp");
    assert!(integerp(v),  "fixnum must be integerp");
    assert!(rationalp(v), "fixnum must be rationalp");
    assert!(realp(v),     "fixnum must be realp");
    assert!(!floatp(v),   "fixnum must NOT be floatp");
    assert!(!complexp(v), "fixnum must NOT be complexp");
}

// ═══════════════════════════════════════════════════════════════════
// Numeric hierarchy — single-float
// ═══════════════════════════════════════════════════════════════════

#[test]
fn single_float_numeric_hierarchy() {
    let v = mk_flt(3.14);
    assert!(numberp(v),    "single-float must be numberp");
    assert!(realp(v),      "single-float must be realp");
    assert!(floatp(v),     "single-float must be floatp");
    assert!(!integerp(v),  "single-float must NOT be integerp");
    assert!(!rationalp(v), "single-float must NOT be rationalp");
    assert!(!complexp(v),  "single-float must NOT be complexp");
}

// ═══════════════════════════════════════════════════════════════════
// Numeric hierarchy — heap numeric types
// ═══════════════════════════════════════════════════════════════════

#[test]
fn bignum_numeric_hierarchy() {
    unsafe {
        let mut s = [0u64; 2];
        let v = mk_heap(&mut s, type_id::BIGNUM);
        assert!(numberp(v),  "bignum must be numberp");
        assert!(integerp(v), "bignum must be integerp");
        assert!(rationalp(v),"bignum must be rationalp");
    }
}

#[test]
fn ratio_numeric_hierarchy() {
    unsafe {
        let mut s = [0u64; 2];
        let v = mk_heap(&mut s, type_id::RATIO);
        assert!(rationalp(v), "ratio must be rationalp");
        assert!(!integerp(v), "ratio must NOT be integerp");
    }
}

#[test]
fn double_float_numeric_hierarchy() {
    unsafe {
        let mut s = [0u64; 2];
        let v = mk_heap(&mut s, type_id::DOUBLE_FLOAT);
        assert!(numberp(v),   "double-float must be numberp");
        assert!(realp(v),     "double-float must be realp");
        assert!(floatp(v),    "double-float must be floatp");
        assert!(!integerp(v), "double-float must NOT be integerp");
    }
}

#[test]
fn complex_numeric_hierarchy() {
    unsafe {
        let mut s = [0u64; 2];
        let v = mk_heap(&mut s, type_id::COMPLEX);
        assert!(numberp(v), "complex must be numberp");
        assert!(!realp(v),  "complex must NOT be realp");
    }
}

// ═══════════════════════════════════════════════════════════════════
// type_id_of — verify dispatch to correct type_id
// ═══════════════════════════════════════════════════════════════════

#[test]
fn type_id_of_dispatches_correctly() {
    unsafe {
        let cases: &[(u8, &str)] = &[
            (type_id::SIMPLE_VECTOR, "simple_vector"),
            (type_id::PACKAGE, "package"),
            (type_id::HASH_TABLE, "hash_table"),
            (type_id::STREAM, "stream"),
            (type_id::READTABLE, "readtable"),
        ];
        for &(tid, label) in cases {
            let mut s = [0u64; 2];
            let v = mk_heap(&mut s, tid);
            assert_eq!(type_id_of(v), tid, "type_id_of mismatch for {}", label);
        }
    }
}

// ═══════════════════════════════════════════════════════════════════
// typep and subtypep — verify signatures exist and are callable
// ═══════════════════════════════════════════════════════════════════

#[test]
fn typep_with_fixnum_returns_meaningful_result() {
    // typep must be callable and return a bool without panicking.
    // A fixnum checked against a type specifier for fixnum (represented as a symbol)
    // should return true. We use mk_sym(0) as a stand-in for the FIXNUM type symbol;
    // once the symbol table is bootstrapped, this should map to the real FIXNUM symbol.
    let result: bool = typep(mk_fix(42), mk_sym(0));
    // A fixnum checked against its own type specifier should return true.
    assert!(result, "typep of a fixnum against the fixnum type specifier should return true");

    // Verify that typep returns false for an obviously wrong type:
    // A fixnum should not satisfy a cons type predicate.
    // (Assuming mk_sym(1) maps to a different type specifier than fixnum.)
    let wrong_type_result = typep(mk_fix(42), mk_sym(1));
    assert!(!wrong_type_result,
        "typep of a fixnum against a non-fixnum type specifier should return false");
}

#[test]
fn subtypep_returns_meaningful_result() {
    // subtypep must be callable and return (bool, bool) without panicking.
    let (subtype_p, valid_p) = subtypep(mk_sym(0), mk_sym(0));
    // A type is always a subtype of itself, so (true, true) is expected
    // when both specifiers refer to the same type.
    assert!(subtype_p, "a type should be a subtype of itself");
    assert!(valid_p, "subtypep should return valid=true for known types");
}

// ═══════════════════════════════════════════════════════════════════
// Cross-cutting: non-numeric types should not satisfy numeric predicates
// ═══════════════════════════════════════════════════════════════════

#[test]
fn nil_is_not_numeric() {
    assert!(!numberp(NIL));
    assert!(!integerp(NIL));
    assert!(!rationalp(NIL));
    assert!(!realp(NIL));
    assert!(!floatp(NIL));
}

#[test]
fn character_is_not_numeric() {
    let c = mk_chr('0');
    assert!(!numberp(c));
    assert!(!integerp(c));
}

#[test]
fn symbol_is_not_numeric() {
    assert!(!numberp(mk_sym(0)));
}

#[test]
fn cons_is_not_numeric() {
    unsafe {
        let mut cell = ConsCell { car: NIL, cdr: NIL };
        assert!(!numberp(mk_cons(&mut cell)));
    }
}
