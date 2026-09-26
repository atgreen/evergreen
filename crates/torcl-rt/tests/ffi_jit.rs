//! Exercise generated adapters against independently compiled C-ABI functions.
//! R2.11 and the scalar outbound subset of R2.14; this suite does not certify
//! aggregate, variadic, callback, or non-SysV support.
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use torcl_rt::ffi::{AlienType, ffi_call};

fn c_symbol(name: &str) -> *const () {
    use std::{process::Command, sync::OnceLock};
    static LIBRARY: OnceLock<usize> = OnceLock::new();
    let library = *LIBRARY.get_or_init(|| {
        let directory = std::env::temp_dir().join(format!("torcl-ffi-jit-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let output = directory.join("scalars.so");
        let compilation = Command::new("cc")
            .args(["-shared", "-fPIC", "-O2"])
            .arg(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/ffi_scalars.c"
            ))
            .arg("-o")
            .arg(&output)
            .output()
            .expect("C compiler required for ABI oracle");
        assert!(
            compilation.status.success(),
            "{}",
            String::from_utf8_lossy(&compilation.stderr)
        );
        let library = torcl_rt::ffi::load_foreign_library(output.to_str().unwrap()).unwrap();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(directory).unwrap();
        library as usize
    });
    unsafe { torcl_rt::ffi::foreign_symbol(library as *mut (), name) }.unwrap()
}

fn int(bits: u8) -> AlienType {
    AlienType::Int { signed: true, bits }
}

#[test]
fn narrow_arguments_are_extended_to_32_bits_according_to_signedness() {
    for (name, bits, signed, raw, expected) in [
        ("torcl_ffi_i8", 8, true, 0xf9u64, (-7i32) as u32 as u64),
        ("torcl_ffi_u8", 8, false, u64::MAX, 255),
        ("torcl_ffi_i16", 16, true, 0xfff9, (-7i32) as u32 as u64),
        ("torcl_ffi_u16", 16, false, u64::MAX, 65535),
    ] {
        let actual = unsafe {
            ffi_call(
                c_symbol(name),
                &int(32),
                &[AlienType::Int { bits, signed }],
                &[raw],
            )
        }
        .unwrap();
        assert_eq!(actual, expected, "{name}");
    }
}

extern "C" fn mixed(a: i64, b: f64, c: f32, d: i32) -> f64 {
    a as f64 + b * 2.0 + c as f64 * 3.0 + d as f64 * 4.0
}

#[test]
fn mixed_register_classes_and_double_return() {
    for function in [mixed as *const (), c_symbol("torcl_ffi_mixed")] {
        let result = unsafe {
            ffi_call(
                function,
                &AlienType::Double,
                &[int(64), AlienType::Double, AlienType::Float, int(32)],
                &[
                    7,
                    1.25f64.to_bits(),
                    2.5f32.to_bits() as u64,
                    (-3i32) as u64,
                ],
            )
        }
        .unwrap();
        assert_eq!(f64::from_bits(result), 5.0);
    }
}

extern "C" fn floats(a: f32, b: f64) -> f32 {
    a + b as f32
}

#[test]
fn single_float_return() {
    let result = unsafe {
        ffi_call(
            floats as *const (),
            &AlienType::Float,
            &[AlienType::Float, AlienType::Double],
            &[1.25f32.to_bits() as u64, 2.5f64.to_bits()],
        )
    }
    .unwrap();
    assert_eq!(result, 3.75f32.to_bits() as u64);
}

extern "C" fn overflow(
    a: u64,
    b: f64,
    c: u64,
    d: f64,
    e: u64,
    f: f64,
    g: u64,
    h: f64,
    i: u64,
    j: f64,
    k: u64,
    l: f64,
    m: u64,
    n: f64,
    o: u64,
    p: f64,
    q: f64,
    r: u64,
    s: f64,
) -> f64 {
    (a + 2 * c + 3 * e + 4 * g + 5 * i + 6 * k + 7 * m + 8 * o + 9 * r) as f64
        + b
        + 2.0 * d
        + 3.0 * f
        + 4.0 * h
        + 5.0 * j
        + 6.0 * l
        + 7.0 * n
        + 8.0 * p
        + 9.0 * q
        + 10.0 * s
}

#[test]
fn both_register_classes_overflow_to_the_stack_in_argument_order() {
    let types = vec![
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        AlienType::Double,
        int(64),
        AlienType::Double,
    ];
    let args = vec![
        1,
        1.5f64.to_bits(),
        2,
        2.5f64.to_bits(),
        3,
        3.5f64.to_bits(),
        4,
        4.5f64.to_bits(),
        5,
        5.5f64.to_bits(),
        6,
        6.5f64.to_bits(),
        7,
        7.5f64.to_bits(),
        8,
        8.5f64.to_bits(),
        9.5f64.to_bits(),
        9,
        10.5f64.to_bits(),
    ];
    let expected = overflow(
        1, 1.5, 2, 2.5, 3, 3.5, 4, 4.5, 5, 5.5, 6, 6.5, 7, 7.5, 8, 8.5, 9.5, 9, 10.5,
    );
    for function in [overflow as *const (), c_symbol("torcl_ffi_stack")] {
        let result = unsafe { ffi_call(function, &AlienType::Double, &types, &args) }.unwrap();
        assert_eq!(f64::from_bits(result), expected);
    }
}

#[test]
fn pointer_argument_and_void_return() {
    let mut value = 0u64;
    let result = unsafe {
        ffi_call(
            c_symbol("torcl_ffi_store"),
            &AlienType::Void,
            &[AlienType::Pointer(Box::new(int(64))), int(64)],
            &[std::ptr::addr_of_mut!(value) as u64, 1234],
        )
    }
    .unwrap();
    assert_eq!(value, 1234);
    assert_eq!(result, 0);
}

#[test]
fn odd_stack_slot_count_preserves_alignment_and_padding() {
    let result = unsafe {
        ffi_call(
            c_symbol("torcl_ffi_seven"),
            &int(64),
            &vec![int(64); 7],
            &[1, 2, 3, 4, 5, 6, 7],
        )
    }
    .unwrap();
    assert_eq!(result, 140);
}

extern "C" fn narrow() -> i8 {
    -7
}

#[test]
fn narrow_return_has_no_unspecified_upper_bits() {
    let result = unsafe { ffi_call(narrow as *const (), &int(8), &[], &[]) }.unwrap();
    assert_eq!(result, (-7i8) as u8 as u64);
}

extern "C" fn no_args() -> u64 {
    17
}

extern "C" fn reenter(value: u64) -> u64 {
    value + unsafe { ffi_call(no_args as *const (), &int(64), &[], &[]) }.unwrap()
}

#[test]
fn foreign_code_can_reenter_the_adapter_cache_and_restore_native_state() {
    let thread = torcl_rt::current_thread();
    let before = thread.state();
    let result = unsafe { ffi_call(reenter as *const (), &int(64), &[int(64)], &[25]) }.unwrap();
    assert_eq!(result, 42);
    assert_eq!(thread.state(), before);
}

#[test]
fn malformed_signatures_fail_before_entering_foreign_code() {
    for (types, args) in [
        (vec![int(64)], vec![]),
        (vec![], vec![1]),
        (vec![AlienType::Void], vec![0]),
        (vec![int(7)], vec![0]),
    ] {
        assert!(unsafe { ffi_call(no_args as *const (), &int(64), &types, &args) }.is_err());
    }
    assert!(
        unsafe {
            ffi_call(
                no_args as *const (),
                &AlienType::Struct {
                    fields: vec![int(64)],
                    packed: false,
                },
                &[],
                &[],
            )
        }
        .is_err()
    );
}
