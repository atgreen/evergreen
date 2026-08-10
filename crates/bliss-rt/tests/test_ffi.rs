use bliss_rt::ffi::*;
use bliss_rt::value::NIL;

#[test]
fn alien_type_variants_constructible() {
    let _ = AlienType::Void;
    let _ = AlienType::Int { signed: true, bits: 32 };
    let _ = AlienType::Float;
    let _ = AlienType::Double;
    let _ = AlienType::Pointer(Box::new(AlienType::Void));
    let _ = AlienType::Struct { fields: vec![AlienType::Float], packed: false };
    let _ = AlienType::Union { variants: vec![AlienType::Int { signed: true, bits: 32 }] };
    let _ = AlienType::FnPtr {
        ret: Box::new(AlienType::Void),
        args: vec![AlienType::Int { signed: true, bits: 32 }],
        variadic: true,
    };
}

#[test]
fn alien_type_equality() {
    assert_eq!(AlienType::Void, AlienType::Void);
    assert_ne!(AlienType::Float, AlienType::Double);
    assert_eq!(
        AlienType::Int { signed: true, bits: 32 },
        AlienType::Int { signed: true, bits: 32 }
    );
    assert_ne!(
        AlienType::Int { signed: true, bits: 32 },
        AlienType::Int { signed: false, bits: 32 }
    );
}

#[test]
fn alien_type_sizes() {
    assert_eq!(AlienType::Void.size(), 0);
    assert_eq!(AlienType::Int { signed: true, bits: 8 }.size(), 1);
    assert_eq!(AlienType::Int { signed: false, bits: 16 }.size(), 2);
    assert_eq!(AlienType::Int { signed: true, bits: 32 }.size(), 4);
    assert_eq!(AlienType::Int { signed: false, bits: 64 }.size(), 8);
    assert_eq!(AlienType::Float.size(), 4);
    assert_eq!(AlienType::Double.size(), 8);
    assert_eq!(AlienType::Pointer(Box::new(AlienType::Void)).size(), std::mem::size_of::<*const ()>());
}

#[test]
fn alien_type_alignments() {
    assert_eq!(AlienType::Int { signed: true, bits: 8 }.alignment(), 1);
    assert_eq!(AlienType::Int { signed: true, bits: 32 }.alignment(), 4);
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
