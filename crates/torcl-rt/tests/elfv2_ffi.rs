//! Execution tests for the ELFv2 foreign-call path: call real C functions on
//! ppc64le and check the values that come back.
//!
//! The unit tests in `ffi/elfv2.rs` pin argument PLACEMENT, which is where ELFv2
//! differs from AAPCS64. These prove the trampoline actually delivers arguments
//! where placement says they go — the distinction that mattered on AArch64, where
//! a single-precision argument was being widened to double and every encoder test
//! was green.

#![cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]

use torcl_rt::ffi::{AlienType, ffi_call};

fn int(bits: u8) -> AlienType {
    AlienType::Int { bits, signed: true }
}

fn call(target: *const (), ret: &AlienType, types: &[AlienType], args: &[u64]) -> u64 {
    // SAFETY: each caller below passes the signature of the `extern "C"` function
    // it names.
    unsafe { ffi_call(target, ret, types, args) }.expect("the call must be supported")
}

extern "C" fn ten_integers(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
    i: i64,
    j: i64,
) -> i64 {
    a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 + i * 9 + j * 10
}

#[test]
fn integers_fill_the_registers_and_then_the_save_area() {
    let types: Vec<_> = (0..10).map(|_| int(64)).collect();
    let args: Vec<u64> = (1..=10).collect();
    let expected: i64 = (1..=10).map(|n| n * n).sum();
    assert_eq!(
        call(ten_integers as *const (), &int(64), &types, &args) as i64,
        expected,
        "the ninth and tenth arguments come from the parameter save area"
    );
}

extern "C" fn mixed(a: i64, b: f64, c: i64, d: f64, e: i64) -> f64 {
    a as f64 + b * 10.0 + c as f64 * 100.0 + d * 1000.0 + e as f64 * 10000.0
}

#[test]
fn a_float_consumes_its_positions_integer_register() {
    // The property that distinguishes ELFv2 from AAPCS64. If the integer registers
    // were allocated with their own counter, `c` would arrive in r4 instead of r5
    // and every argument after the first double would be wrong — silently, with a
    // plausible-looking number rather than a crash.
    let types = [
        int(64),
        AlienType::Double,
        int(64),
        AlienType::Double,
        int(64),
    ];
    let args = [1u64, 2.0f64.to_bits(), 3u64, 4.0f64.to_bits(), 5u64];
    let raw = call(mixed as *const (), &AlienType::Double, &types, &args);
    assert_eq!(f64::from_bits(raw), 1.0 + 20.0 + 300.0 + 4000.0 + 50000.0);
}

extern "C" fn ten_doubles(
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
    i: f64,
    j: f64,
) -> f64 {
    a + b * 2.0 + c * 3.0 + d * 4.0 + e * 5.0 + f * 6.0 + g * 7.0 + h * 8.0 + i * 9.0 + j * 10.0
}

#[test]
fn doubles_fill_their_own_thirteen_registers() {
    let types: Vec<_> = (0..10).map(|_| AlienType::Double).collect();
    let args: Vec<u64> = (1..=10).map(|n| (n as f64).to_bits()).collect();
    let expected: f64 = (1..=10).map(|n| (n * n) as f64).sum();
    let raw = call(ten_doubles as *const (), &AlienType::Double, &types, &args);
    assert_eq!(f64::from_bits(raw), expected);
}

extern "C" fn single_precision(a: f32, b: f32) -> f32 {
    a * 2.0 + b
}

#[test]
fn single_precision_stays_single() {
    // The AArch64 equivalent was wrong twice: the argument was widened to double,
    // and the result read from the wrong register.
    let types = [AlienType::Float, AlienType::Float];
    let args = [u64::from(1.5f32.to_bits()), u64::from(0.25f32.to_bits())];
    let raw = call(
        single_precision as *const (),
        &AlienType::Float,
        &types,
        &args,
    );
    assert_eq!(f32::from_bits(raw as u32), 3.25);
}

extern "C" fn narrow_return(value: i64) -> i32 {
    value as i32
}

#[test]
fn a_narrow_result_is_masked_to_its_declared_width() {
    let raw = call(
        narrow_return as *const (),
        &int(32),
        &[int(64)],
        &[(-11i64) as u64],
    );
    assert_eq!(
        raw, 0xffff_fff5,
        "zero-extended to 32 bits; unmarshal_from_c does the sign extension"
    );
}

extern "C" fn pointer_identity(pointer: *const u64) -> *const u64 {
    pointer
}

#[test]
fn pointers_survive_the_round_trip() {
    let value = 0x1234_5678_9abc_def0u64;
    let pointer = &value as *const u64;
    let raw = call(
        pointer_identity as *const (),
        &AlienType::Pointer(Box::new(AlienType::Void)),
        &[AlienType::Pointer(Box::new(AlienType::Void))],
        &[pointer as u64],
    );
    assert_eq!(raw, pointer as u64);
    // SAFETY: the pointer round-tripped unchanged and `value` is still alive.
    assert_eq!(unsafe { *(raw as *const u64) }, value);
}

#[test]
fn a_real_libc_function_answers_correctly() {
    // ldexp(x, n) = x * 2^n — a mixed signature against a function this crate did
    // not compile, so the ABI is genuinely being tested rather than a convention
    // shared with the caller.
    unsafe extern "C" {
        fn ldexp(x: f64, n: i32) -> f64;
    }
    let raw = call(
        ldexp as *const (),
        &AlienType::Double,
        &[AlienType::Double, int(32)],
        &[3.5f64.to_bits(), 4u64],
    );
    assert_eq!(f64::from_bits(raw), 56.0, "3.5 * 2^4");
}

#[test]
fn a_null_target_is_refused_rather_than_called() {
    let outcome = unsafe { ffi_call(std::ptr::null(), &int(64), &[], &[]) };
    assert!(outcome.is_err());
}
