//! Comprehensive tests for `BlissVal` — the 64-bit tagged value type.

use bliss_rt::object::{ObjectHeader, type_id};
use bliss_rt::value::*;

// 61-bit signed fixnum range
const FIXNUM_MAX: i64 = (1_i64 << 60) - 1;
const FIXNUM_MIN: i64 = -(1_i64 << 60);

// ── Special constants: exact bit patterns ──────────────────────────

#[test]
fn special_constants_have_correct_bits() {
    assert_eq!(NIL.to_raw(), NIL_BITS);
    assert_eq!(T.to_raw(), T_BITS);
    assert_eq!(UNBOUND.to_raw(), UNBOUND_BITS);
    assert_eq!(MISSING.to_raw(), MISSING_BITS);
    assert_eq!(EOF.to_raw(), EOF_BITS);
    assert_eq!(NIL_BITS, 0x0000_0000_0000_0007);
    assert_eq!(T_BITS, 0x0000_0000_0000_000F);
    assert_eq!(UNBOUND_BITS, 0x0000_0000_0000_0017);
    assert_eq!(MISSING_BITS, 0x0000_0000_0000_001F);
    assert_eq!(EOF_BITS, 0x0000_0000_0000_0027);
}

#[test]
fn special_payloads_are_sequential() {
    assert_eq!(NIL_BITS, TAG_SPECIAL);
    assert_eq!(T_BITS, (1 << 3) | TAG_SPECIAL);
    assert_eq!(UNBOUND_BITS, (2 << 3) | TAG_SPECIAL);
    assert_eq!(MISSING_BITS, (3 << 3) | TAG_SPECIAL);
    assert_eq!(EOF_BITS, (4 << 3) | TAG_SPECIAL);
}

#[test]
fn all_specials_have_special_tag() {
    for &s in &[NIL, T, UNBOUND, MISSING, EOF] {
        assert_eq!(s.tag(), TAG_SPECIAL);
    }
}

#[test]
fn special_constants_are_distinct() {
    let specials = [NIL, T, UNBOUND, MISSING, EOF];
    for i in 0..specials.len() {
        for j in (i + 1)..specials.len() {
            assert_ne!(specials[i], specials[j]);
        }
    }
}

// ── NIL predicate behavior ─────────────────────────────────────────

#[test]
fn nil_predicates() {
    assert!(NIL.is_nil());
    assert!(NIL.is_list());
    assert!(NIL.is_symbol()); // CL: NIL is a symbol
    assert!(!NIL.is_fixnum());
    assert!(!NIL.is_cons());
    assert!(!NIL.is_heap_object());
    assert!(!NIL.is_character());
    assert!(!NIL.is_single_float());
    assert!(!NIL.is_function());
}

// ── T predicate behavior ───────────────────────────────────────────

#[test]
fn t_predicates() {
    assert!(!T.is_nil());
    assert!(T.is_symbol()); // CL: T is a symbol
    assert!(!T.is_fixnum());
    assert!(!T.is_list());
    assert!(!T.is_cons());
}

// ── Fixnum constructors and extractors ─────────────────────────────

#[test]
fn fixnum_zero() {
    let v = BlissVal::from_fixnum(0);
    assert_eq!(v.tag(), TAG_FIXNUM);
    assert!(v.is_fixnum());
    assert_eq!(v.as_fixnum(), 0);
}

#[test]
fn fixnum_positive_and_negative() {
    for &n in &[1_i64, -1, 42, -42, 1000000, -1000000] {
        let v = BlissVal::from_fixnum(n);
        assert!(v.is_fixnum());
        assert_eq!(v.as_fixnum(), n, "round-trip failed for {}", n);
    }
}

#[test]
fn fixnum_max_value() {
    let v = BlissVal::from_fixnum(FIXNUM_MAX);
    assert!(v.is_fixnum());
    assert_eq!(v.as_fixnum(), FIXNUM_MAX);
}

#[test]
fn fixnum_min_value() {
    let v = BlissVal::from_fixnum(FIXNUM_MIN);
    assert!(v.is_fixnum());
    assert_eq!(v.as_fixnum(), FIXNUM_MIN);
}

#[test]
fn fixnum_is_not_other_types() {
    let v = BlissVal::from_fixnum(42);
    assert!(!v.is_cons());
    assert!(!v.is_heap_object());
    assert!(!v.is_character());
    assert!(!v.is_single_float());
    assert!(!v.is_symbol());
    assert!(!v.is_function());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

// ── Character constructors and extractors ──────────────────────────

#[test]
fn char_ascii() {
    let v = BlissVal::from_char('A');
    assert_eq!(v.tag(), TAG_CHARACTER);
    assert!(v.is_character());
    assert_eq!(v.as_char(), 'A');
}

#[test]
fn char_null_and_space() {
    let v0 = BlissVal::from_char('\0');
    assert!(v0.is_character());
    assert_eq!(v0.as_char(), '\0');
    let vs = BlissVal::from_char(' ');
    assert_eq!(vs.as_char(), ' ');
}

#[test]
fn char_bmp_and_supplementary() {
    // BMP CJK
    let v1 = BlissVal::from_char('漢');
    assert!(v1.is_character());
    assert_eq!(v1.as_char(), '漢');
    // Supplementary plane emoji (U+1F600)
    let v2 = BlissVal::from_char('😀');
    assert!(v2.is_character());
    assert_eq!(v2.as_char(), '😀');
}

#[test]
fn char_max_unicode() {
    let c = char::from_u32(0x10FFFF).unwrap();
    let v = BlissVal::from_char(c);
    assert!(v.is_character());
    assert_eq!(v.as_char(), c);
}

#[test]
fn char_round_trip_various() {
    for c in ['a', 'Z', '0', '\n', '\t', 'λ', '中', '🎉', '\u{FEFF}'] {
        let v = BlissVal::from_char(c);
        assert_eq!(v.as_char(), c, "round-trip failed for {:?}", c);
    }
}

#[test]
fn char_is_not_other_types() {
    let v = BlissVal::from_char('x');
    assert!(!v.is_fixnum());
    assert!(!v.is_cons());
    assert!(!v.is_heap_object());
    assert!(!v.is_single_float());
    assert!(!v.is_symbol());
    assert!(!v.is_function());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

#[test]
fn string_accessor_decodes_runtime_string_layout() {
    let bytes = "hello, bliss".as_bytes();
    let total = 16 + bytes.len();
    let padded = (total + 7) & !7;
    let mut storage = vec![0_u8; padded];
    unsafe {
        *(storage.as_mut_ptr() as *mut ObjectHeader) =
            ObjectHeader::new(type_id::SIMPLE_BASE_STRING, (padded / 8) as u16);
        *((storage.as_mut_ptr() as *mut u64).add(1)) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), storage.as_mut_ptr().add(16), bytes.len());
        let value = BlissVal::from_heap_ptr(storage.as_mut_ptr());
        assert!(value.is_string());
        assert_eq!(value.as_string(), "hello, bliss");
    }
}

// ── Single-float constructors and extractors ───────────────────────

#[test]
fn single_float_zero_and_one() {
    let v0 = BlissVal::from_single_float(0.0_f32);
    assert_eq!(v0.tag(), TAG_SINGLE_FLOAT);
    assert!(v0.is_single_float());
    assert_eq!(v0.as_single_float(), 0.0_f32);
    let v1 = BlissVal::from_single_float(1.0_f32);
    assert_eq!(v1.as_single_float(), 1.0_f32);
}

#[test]
fn single_float_negative() {
    let v = BlissVal::from_single_float(-std::f32::consts::PI);
    assert!(v.is_single_float());
    assert_eq!(v.as_single_float(), -std::f32::consts::PI);
}

#[test]
fn single_float_negative_zero() {
    let v = BlissVal::from_single_float(-0.0_f32);
    assert!(v.is_single_float());
    let extracted = v.as_single_float();
    assert!(
        extracted.is_sign_negative(),
        "negative zero must preserve sign"
    );
    assert_eq!(extracted.to_bits(), (-0.0_f32).to_bits());
}

#[test]
fn single_float_special_values() {
    let inf = BlissVal::from_single_float(f32::INFINITY);
    assert_eq!(inf.as_single_float(), f32::INFINITY);
    let ninf = BlissVal::from_single_float(f32::NEG_INFINITY);
    assert_eq!(ninf.as_single_float(), f32::NEG_INFINITY);
    let nan = BlissVal::from_single_float(f32::NAN);
    assert!(nan.as_single_float().is_nan());
    let max = BlissVal::from_single_float(f32::MAX);
    assert_eq!(max.as_single_float(), f32::MAX);
    let minp = BlissVal::from_single_float(f32::MIN_POSITIVE);
    assert_eq!(minp.as_single_float(), f32::MIN_POSITIVE);
}

#[test]
fn single_float_is_not_other_types() {
    let v = BlissVal::from_single_float(1.0);
    assert!(!v.is_fixnum());
    assert!(!v.is_cons());
    assert!(!v.is_heap_object());
    assert!(!v.is_character());
    assert!(!v.is_symbol());
    assert!(!v.is_function());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

#[test]
fn single_float_encoding_in_upper_bits() {
    let f: f32 = 1.0;
    let v = BlissVal::from_single_float(f);
    let raw = v.to_raw();
    let upper_32 = (raw >> 32) as u32;
    assert_eq!(upper_32, f.to_bits(), "f32 bits should be in bits 63:32");
    assert_eq!(raw & TAG_MASK, TAG_SINGLE_FLOAT);
}

// ── Symbol index constructors and extractors ───────────────────────

#[test]
fn symbol_index_round_trip() {
    for &idx in &[0_u32, 1, 42, 1000, 65535, u32::MAX / 2, u32::MAX] {
        let v = BlissVal::from_symbol_index(idx);
        assert_eq!(v.tag(), TAG_SYMBOL);
        assert!(v.is_symbol());
        assert_eq!(
            v.as_symbol_index(),
            idx,
            "round-trip failed for idx={}",
            idx
        );
    }
}

#[test]
fn symbol_is_not_other_types() {
    let v = BlissVal::from_symbol_index(5);
    assert!(!v.is_fixnum());
    assert!(!v.is_cons());
    assert!(!v.is_heap_object());
    assert!(!v.is_character());
    assert!(!v.is_single_float());
    assert!(!v.is_function());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

// ── Cons pointer ───────────────────────────────────────────────────

#[test]
fn cons_ptr_tag_and_predicates() {
    let aligned: u64 = 0x1000;
    let v = unsafe { BlissVal::from_cons_ptr(aligned as *mut u8) };
    assert_eq!(v.tag(), TAG_CONS);
    assert!(v.is_cons());
    assert!(v.is_list()); // cons is a list
    assert!(!v.is_nil());
}

#[test]
fn cons_ptr_round_trip() {
    let aligned: u64 = 0x7FFF_FFFF_FFF8;
    let v = unsafe { BlissVal::from_cons_ptr(aligned as *mut u8) };
    let extracted = unsafe { v.as_ptr() };
    assert_eq!(extracted as u64, aligned);
}

#[test]
fn cons_is_not_other_types() {
    let v = unsafe { BlissVal::from_cons_ptr(0x2000 as *mut u8) };
    assert!(!v.is_fixnum());
    assert!(!v.is_heap_object());
    assert!(!v.is_character());
    assert!(!v.is_single_float());
    assert!(!v.is_symbol());
    assert!(!v.is_function());
}

// ── Heap object pointer ────────────────────────────────────────────

#[test]
fn heap_ptr_tag_and_round_trip() {
    let aligned: u64 = 0x1000;
    let v = unsafe { BlissVal::from_heap_ptr(aligned as *mut u8) };
    assert_eq!(v.tag(), TAG_HEAP_OBJECT);
    assert!(v.is_heap_object());
    let extracted = unsafe { v.as_ptr() };
    assert_eq!(extracted as u64, aligned);
}

#[test]
fn heap_obj_is_not_other_types() {
    let v = unsafe { BlissVal::from_heap_ptr(0x2000 as *mut u8) };
    assert!(!v.is_fixnum());
    assert!(!v.is_cons());
    assert!(!v.is_character());
    assert!(!v.is_single_float());
    assert!(!v.is_symbol());
    assert!(!v.is_function());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

// ── Function pointer ───────────────────────────────────────────────

#[test]
fn function_ptr_tag_and_round_trip() {
    let aligned: u64 = 0x1000;
    let v = unsafe { BlissVal::from_function_ptr(aligned as *mut u8) };
    assert_eq!(v.tag(), TAG_FUNCTION);
    assert!(v.is_function());
    let extracted = unsafe { v.as_ptr() };
    assert_eq!(extracted as u64, aligned);
}

#[test]
fn function_is_not_other_types() {
    let v = unsafe { BlissVal::from_function_ptr(0x2000 as *mut u8) };
    assert!(!v.is_fixnum());
    assert!(!v.is_cons());
    assert!(!v.is_heap_object());
    assert!(!v.is_character());
    assert!(!v.is_single_float());
    assert!(!v.is_symbol());
    assert!(!v.is_nil());
    assert!(!v.is_list());
}

// ── Tag exhaustiveness: each tag 0-7 maps to correct predicate ────

#[test]
fn tag_exhaustiveness() {
    let samples: [(BlissVal, u64); 8] = [
        (BlissVal::from_fixnum(0), 0),
        (unsafe { BlissVal::from_cons_ptr(0x1000 as *mut u8) }, 1),
        (unsafe { BlissVal::from_heap_ptr(0x1000 as *mut u8) }, 2),
        (BlissVal::from_char('x'), 3),
        (BlissVal::from_single_float(1.0), 4),
        (BlissVal::from_symbol_index(0), 5),
        (unsafe { BlissVal::from_function_ptr(0x1000 as *mut u8) }, 6),
        (NIL, 7),
    ];
    for (val, expected_tag) in &samples {
        assert_eq!(
            val.tag(),
            *expected_tag,
            "tag mismatch for expected_tag={}",
            expected_tag
        );
    }
}

// ── Raw FFI round-trips ────────────────────────────────────────────

#[test]
fn raw_round_trip_fixnum() {
    let v = BlissVal::from_fixnum(99);
    let v2 = BlissVal::from_raw(v.to_raw());
    assert_eq!(v2.as_fixnum(), 99);
}

#[test]
fn raw_round_trip_char() {
    let v = BlissVal::from_char('λ');
    let v2 = BlissVal::from_raw(v.to_raw());
    assert_eq!(v2.as_char(), 'λ');
}

#[test]
fn raw_round_trip_single_float() {
    let v = BlissVal::from_single_float(std::f32::consts::E);
    let v2 = BlissVal::from_raw(v.to_raw());
    assert_eq!(v2.as_single_float(), std::f32::consts::E);
}

#[test]
fn raw_round_trip_symbol() {
    let v = BlissVal::from_symbol_index(12345);
    let v2 = BlissVal::from_raw(v.to_raw());
    assert_eq!(v2.as_symbol_index(), 12345);
}

#[test]
fn raw_round_trip_specials() {
    assert_eq!(BlissVal::from_raw(NIL.to_raw()), NIL);
    assert_eq!(BlissVal::from_raw(T.to_raw()), T);
    assert_eq!(BlissVal::from_raw(UNBOUND.to_raw()), UNBOUND);
    assert_eq!(BlissVal::from_raw(MISSING.to_raw()), MISSING);
    assert_eq!(BlissVal::from_raw(EOF.to_raw()), EOF);
}

#[test]
fn raw_round_trip_pointers() {
    for &addr in &[0x3000_u64, 0x5000, 0x7000] {
        let vc = unsafe { BlissVal::from_cons_ptr(addr as *mut u8) };
        assert_eq!(
            unsafe { BlissVal::from_raw(vc.to_raw()).as_ptr() } as u64,
            addr
        );
        let vh = unsafe { BlissVal::from_heap_ptr(addr as *mut u8) };
        assert_eq!(
            unsafe { BlissVal::from_raw(vh.to_raw()).as_ptr() } as u64,
            addr
        );
        let vf = unsafe { BlissVal::from_function_ptr(addr as *mut u8) };
        assert_eq!(
            unsafe { BlissVal::from_raw(vf.to_raw()).as_ptr() } as u64,
            addr
        );
    }
}

// ── Pointer alignment preservation ─────────────────────────────────

#[test]
fn pointer_alignment_preservation() {
    for &addr in &[0x8_u64, 0x10, 0x100, 0x1_0000, 0x1_0000_0000, 0xFFFF_FFF8] {
        let vc = unsafe { BlissVal::from_cons_ptr(addr as *mut u8) };
        assert_eq!(unsafe { vc.as_ptr() } as u64, addr);
        let vh = unsafe { BlissVal::from_heap_ptr(addr as *mut u8) };
        assert_eq!(unsafe { vh.as_ptr() } as u64, addr);
        let vf = unsafe { BlissVal::from_function_ptr(addr as *mut u8) };
        assert_eq!(unsafe { vf.as_ptr() } as u64, addr);
    }
}

// ── Equality ───────────────────────────────────────────────────────

#[test]
fn same_fixnum_values_are_equal() {
    assert_eq!(BlissVal::from_fixnum(42), BlissVal::from_fixnum(42));
}

#[test]
fn different_fixnum_values_are_not_equal() {
    assert_ne!(BlissVal::from_fixnum(1), BlissVal::from_fixnum(2));
}

#[test]
fn nil_equals_nil_and_differs_from_t() {
    assert_eq!(NIL, NIL);
    assert_ne!(NIL, T);
}

// ── Copy semantics ─────────────────────────────────────────────────

#[test]
fn blissval_is_copy() {
    let a = BlissVal::from_fixnum(10);
    let b = a; // copy
    assert_eq!(a.as_fixnum(), 10);
    assert_eq!(b.as_fixnum(), 10);
}

// ── Hash consistency ───────────────────────────────────────────────

#[test]
fn equal_values_have_equal_hashes() {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let a = BlissVal::from_fixnum(42);
    let b = BlissVal::from_fixnum(42);
    let mut ha = DefaultHasher::new();
    a.hash(&mut ha);
    let mut hb = DefaultHasher::new();
    b.hash(&mut hb);
    assert_eq!(ha.finish(), hb.finish());
}

// ── from_raw / to_raw identity ─────────────────────────────────────

#[test]
fn to_raw_returns_inner_u64() {
    let raw = 0xDEAD_BEEF_CAFE_BABEu64;
    assert_eq!(BlissVal::from_raw(raw).to_raw(), raw);
}

#[test]
fn from_raw_to_raw_identity() {
    for &bits in &[0u64, 1, u64::MAX, 0x5555_5555_5555_5555, NIL_BITS, T_BITS] {
        assert_eq!(BlissVal::from_raw(bits).to_raw(), bits);
    }
}
