//! C-ABI FFI tests: require a dynamic loader, so they run only with the
//! `c-ffi` feature (bliss-bca.5). Marshalling is also covered by spec_runtime_core.
#![cfg(feature = "c-ffi")]

use bliss_rt::ffi::*;
use bliss_rt::value::{BlissVal, NIL};

#[test]
fn alien_type_variants_constructible() {
    let _ = AlienType::Void;
    let _ = AlienType::Int {
        signed: true,
        bits: 32,
    };
    let _ = AlienType::Float;
    let _ = AlienType::Double;
    let _ = AlienType::Pointer(Box::new(AlienType::Void));
    let _ = AlienType::Struct {
        fields: vec![AlienType::Float],
        packed: false,
    };
    let _ = AlienType::Union {
        variants: vec![AlienType::Int {
            signed: true,
            bits: 32,
        }],
    };
    let _ = AlienType::FnPtr {
        ret: Box::new(AlienType::Void),
        args: vec![AlienType::Int {
            signed: true,
            bits: 32,
        }],
        variadic: true,
    };
}

#[test]
fn alien_type_equality() {
    assert_eq!(AlienType::Void, AlienType::Void);
    assert_ne!(AlienType::Float, AlienType::Double);
    assert_eq!(
        AlienType::Int {
            signed: true,
            bits: 32
        },
        AlienType::Int {
            signed: true,
            bits: 32
        }
    );
    assert_ne!(
        AlienType::Int {
            signed: true,
            bits: 32
        },
        AlienType::Int {
            signed: false,
            bits: 32
        }
    );
}

#[test]
fn alien_type_sizes() {
    assert_eq!(AlienType::Void.size(), 0);
    assert_eq!(
        AlienType::Int {
            signed: true,
            bits: 8
        }
        .size(),
        1
    );
    assert_eq!(
        AlienType::Int {
            signed: false,
            bits: 16
        }
        .size(),
        2
    );
    assert_eq!(
        AlienType::Int {
            signed: true,
            bits: 32
        }
        .size(),
        4
    );
    assert_eq!(
        AlienType::Int {
            signed: false,
            bits: 64
        }
        .size(),
        8
    );
    assert_eq!(AlienType::Float.size(), 4);
    assert_eq!(AlienType::Double.size(), 8);
    assert_eq!(
        AlienType::Pointer(Box::new(AlienType::Void)).size(),
        std::mem::size_of::<*const ()>()
    );
}

#[test]
fn alien_type_alignments() {
    assert_eq!(
        AlienType::Int {
            signed: true,
            bits: 8
        }
        .alignment(),
        1
    );
    assert_eq!(
        AlienType::Int {
            signed: true,
            bits: 32
        }
        .alignment(),
        4
    );
    assert_eq!(AlienType::Float.alignment(), 4);
    assert_eq!(AlienType::Double.alignment(), 8);
}

#[test]
fn marshal_nil_to_pointer_is_null() {
    let result = marshal_to_c(NIL, &AlienType::Pointer(Box::new(AlienType::Void)));
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), 0);
}

#[test]
fn unmarshal_void_returns_nil() {
    let result = unmarshal_from_c(0, &AlienType::Void);
    assert!(result.is_ok());
    assert_eq!(result.unwrap(), NIL);
}

#[test]
fn callback_creation_and_fn_ptr() {
    let cb = Callback::new(NIL, AlienType::Void, vec![]).unwrap();
    assert!(!cb.as_fn_ptr().is_null());
}

#[test]
fn load_nonexistent_library_fails() {
    assert!(load_foreign_library("nonexistent_xyz_12345.so").is_err());
}

#[test]
fn foreign_symbol_null_library_fails() {
    unsafe {
        assert!(foreign_symbol(std::ptr::null_mut(), "puts").is_err());
    }
}

// ── Issue #4: ffi_call tests ──────────────────────────────────────

#[test]
fn ffi_call_null_fn_ptr_fails() {
    unsafe {
        let result = ffi_call(std::ptr::null(), &AlienType::Void, &[], &[]);
        assert!(result.is_err());
    }
}

#[test]
fn ffi_call_with_known_c_function() {
    // Load libc and call abs(-42) which should return 42
    let lib = load_foreign_library("libc.so.6")
        .or_else(|_| load_foreign_library("libSystem.B.dylib"))
        .or_else(|_| load_foreign_library("libc.so"))
        .expect("should be able to load libc on any supported platform");
    unsafe {
        let abs_fn = foreign_symbol(lib, "abs").expect("libc should export 'abs'");
        let result = ffi_call(
            abs_fn,
            &AlienType::Int {
                signed: true,
                bits: 32,
            },
            &[AlienType::Int {
                signed: true,
                bits: 32,
            }],
            &[(-42i32 as u32) as u64],
        );
        assert!(result.is_ok(), "ffi_call to abs should succeed");
        assert_eq!(result.unwrap() as i32, 42, "abs(-42) should return 42");
    }
}

// ── Issue #6: Callback::Drop test ─────────────────────────────────

#[test]
fn callback_drop_does_not_panic() {
    let cb = Callback::new(NIL, AlienType::Void, vec![]).expect("Callback::new should succeed");
    drop(cb); // explicitly drop to exercise Drop impl
    // If we reach here without panic, the Drop impl is correct.
}

// ── Issue #8: marshal/unmarshal non-trivial cases ─────────────────

#[test]
fn marshal_fixnum_to_int32() {
    let val = BlissVal::from_fixnum(42);
    let result = marshal_to_c(
        val,
        &AlienType::Int {
            signed: true,
            bits: 32,
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap() as i32, 42);
}

#[test]
fn marshal_fixnum_negative_to_int32() {
    let val = BlissVal::from_fixnum(-7);
    let result = marshal_to_c(
        val,
        &AlienType::Int {
            signed: true,
            bits: 32,
        },
    );
    assert!(result.is_ok());
    assert_eq!(result.unwrap() as i32, -7);
}

#[test]
fn unmarshal_int32_to_fixnum() {
    let result = unmarshal_from_c(
        42,
        &AlienType::Int {
            signed: true,
            bits: 32,
        },
    );
    assert!(result.is_ok());
    // The result should be a fixnum representing 42
    let val = result.unwrap();
    assert_ne!(val, NIL);
}

#[test]
fn marshal_single_float_to_float() {
    let val = BlissVal::from_single_float(std::f32::consts::PI);
    let result = marshal_to_c(val, &AlienType::Float);
    assert!(result.is_ok());
    let bits = result.unwrap() as u32;
    let f = f32::from_bits(bits);
    assert!((f - std::f32::consts::PI).abs() < 0.01);
}

#[test]
fn unmarshal_double_to_blissval() {
    let bits = f64::to_bits(std::f64::consts::E);
    let result = unmarshal_from_c(bits, &AlienType::Double);
    assert!(result.is_ok());
}

// ── Issue #9: struct/union size and alignment tests ───────────────

#[test]
fn alien_type_struct_size_with_padding() {
    // Struct { Int32, Double } — on most platforms:
    // Int32 = 4 bytes, then 4 bytes padding for Double alignment, then Double = 8 bytes
    // Total: 16 bytes
    let s = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            AlienType::Double,
        ],
        packed: false,
    };
    // Size must be at least sum of field sizes (12) and account for alignment
    assert!(s.size() >= 12);
    // With standard C layout, size should be 16 (4 + 4 pad + 8)
    assert_eq!(s.size(), 16);
}

#[test]
fn alien_type_struct_alignment_is_max_field() {
    let s = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            AlienType::Double,
        ],
        packed: false,
    };
    // Alignment should be max of field alignments = alignment of Double = 8
    assert_eq!(s.alignment(), 8);
}

#[test]
fn alien_type_packed_struct_no_padding() {
    let s = AlienType::Struct {
        fields: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            AlienType::Double,
        ],
        packed: true,
    };
    // Packed struct: no padding, size = 4 + 8 = 12
    assert_eq!(s.size(), 12);
}

#[test]
fn alien_type_union_size_is_largest_variant() {
    let u = AlienType::Union {
        variants: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            }, // 4 bytes
            AlienType::Double, // 8 bytes
            AlienType::Int {
                signed: false,
                bits: 8,
            }, // 1 byte
        ],
    };
    // Union size should be the largest variant = 8 (Double)
    assert_eq!(u.size(), 8);
}

#[test]
fn alien_type_union_alignment_is_max_variant() {
    let u = AlienType::Union {
        variants: vec![
            AlienType::Int {
                signed: true,
                bits: 32,
            },
            AlienType::Double,
        ],
    };
    assert_eq!(u.alignment(), 8);
}