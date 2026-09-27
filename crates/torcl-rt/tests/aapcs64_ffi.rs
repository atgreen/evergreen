//! AAPCS64 scalar foreign calls (spec §4.7.5.2, R4.44).
//!
//! The targets are `extern "C"` functions defined right here: on AArch64 they
//! follow AAPCS64 exactly like any C function, so the suite needs no compiler
//! on the device and can cover shapes libc does not happen to offer — ten
//! integers, ten doubles, and mixtures that spill past the register file.
//!
//! Every case here is one the legacy dispatcher could not express. It transmutes
//! to eighteen hardcoded shapes, all-`i32` or all-`u64`, so a float argument, a
//! float return, or any mixed signature was simply unavailable on this target.
#![cfg(all(target_arch = "aarch64", unix))]

use torcl_rt::ffi::{AlienType, ffi_call};

fn int(bits: u8) -> AlienType {
    AlienType::Int { bits, signed: true }
}

/// Ten integers: X0-X7 fill, then two go to the stack.
extern "C" fn ten_ints(
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

/// Ten doubles: D0-D7 fill, then two go to the stack.
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

/// Mixed classes. The integer and float register files are independent in
/// AAPCS64, so these interleave rather than consuming a shared sequence — the
/// exact thing SysV-shaped reasoning gets wrong.
extern "C" fn mixed(a: i32, x: f64, b: i32, y: f32, c: i64) -> f64 {
    f64::from(a) + x + f64::from(b) * 10.0 + f64::from(y) * 100.0 + (c as f64) * 1000.0
}

/// A float return arrives in D0, not X0.
extern "C" fn halve(x: f32) -> f32 {
    x / 2.0
}

extern "C" fn negate(x: f64) -> f64 {
    -x
}

/// A narrow signed return leaves the upper bits of X0 unspecified.
extern "C" fn narrow(x: i32) -> i32 {
    x - 1
}

extern "C" fn through_pointer(p: *const u64) -> u64 {
    // SAFETY: the test passes the address of a live u64.
    unsafe { *p }
}

fn call(target: usize, ret: AlienType, types: &[AlienType], args: &[u64]) -> u64 {
    // SAFETY: each target below is declared with exactly the signature its
    // caller describes.
    unsafe { ffi_call(target as *const (), &ret, types, args).expect("ffi_call") }
}

#[test]
fn integers_fill_the_register_file_then_spill() {
    let args: Vec<u64> = (1..=10).collect();
    let types = vec![int(64); 10];
    let result = call(ten_ints as usize, int(64), &types, &args);
    // 1 + 2*2 + 3*3 + ... + 10*10
    let expected: i64 = (1..=10).map(|n| n * n).sum();
    assert_eq!(result as i64, expected);
}

#[test]
fn doubles_fill_their_own_register_file_then_spill() {
    let args: Vec<u64> = (1..=10).map(|n| (n as f64).to_bits()).collect();
    let types = vec![AlienType::Double; 10];
    let result = call(ten_doubles as usize, AlienType::Double, &types, &args);
    let expected: f64 = (1..=10).map(|n| (n * n) as f64).sum();
    assert_eq!(f64::from_bits(result), expected);
}

#[test]
fn integer_and_float_registers_are_allocated_independently() {
    let args = vec![7u64, 2.5f64.to_bits(), 3u64, 0.5f32.to_bits() as u64, 4u64];
    let types = vec![
        int(32),
        AlienType::Double,
        int(32),
        AlienType::Float,
        int(64),
    ];
    let result = call(mixed as usize, AlienType::Double, &types, &args);
    assert_eq!(f64::from_bits(result), 7.0 + 2.5 + 30.0 + 50.0 + 4000.0);
}

#[test]
fn a_float_result_comes_back_from_d0() {
    // A Float result is f32 BITS zero-extended, exactly as the x86-64 adapter's
    // `movd eax, xmm0` produces and as unmarshal_from_c reads it back.
    let result = call(
        halve as usize,
        AlienType::Float,
        &[AlienType::Float],
        &[9.0f32.to_bits() as u64],
    );
    assert_eq!(f32::from_bits(result as u32), 4.5);

    let result = call(
        negate as usize,
        AlienType::Double,
        &[AlienType::Double],
        &[1.25f64.to_bits()],
    );
    assert_eq!(f64::from_bits(result), -1.25);
}

#[test]
fn a_narrow_result_carries_exactly_its_declared_width() {
    // The raw contract is zero-extension, matching the x86-64 adapter's movzx.
    // unmarshal_from_c masks to the declared width and sign-extends, so the
    // Lisp-visible value is -1; what must not happen is stray high bits from
    // whatever the callee left in X0.
    let result = call(narrow as usize, int(32), &[int(32)], &[0u64]);
    assert_eq!(result, 0xFFFF_FFFF, "0 - 1, zero-extended from 32 bits");
    assert_eq!(
        torcl_rt::ffi::unmarshal_from_c(result, &int(32))
            .expect("unmarshal")
            .as_fixnum(),
        -1,
        "and -1 once the Lisp layer has interpreted it"
    );
}

#[test]
fn pointers_pass_as_integers() {
    let value: u64 = 0xfeed_face_dead_beef;
    let result = call(
        through_pointer as usize,
        AlienType::Int {
            bits: 64,
            signed: false,
        },
        &[AlienType::Pointer(Box::new(AlienType::Void))],
        &[&value as *const u64 as u64],
    );
    assert_eq!(result, value);
}

#[test]
fn a_mixed_signature_reaches_the_system_math_library() {
    // The shape the Android raymarcher actually needs: a double and an int through
    // one call, against a function this crate did not compile.
    //
    // Reached by name where that works, and otherwise through the linked symbol.
    // The name is not portable — bionic ships `libm.so`, glibc merged libm into libc
    // and keeps a versioned `libm.so.6`, and a cross sysroot's `libm.so` is a linker
    // script rather than a loadable object — so insisting on `dlopen` would make this
    // test about the environment instead of about the ABI.
    unsafe extern "C" {
        fn ldexp(x: f64, n: i32) -> f64;
    }
    let target = ["libm.so", "libm.so.6", "libc.so.6"]
        .into_iter()
        .find_map(|name| {
            let library = torcl_rt::ffi::load_foreign_library(name).ok()?;
            // SAFETY: ldexp(double, int) -> double is its declared signature.
            unsafe { torcl_rt::ffi::foreign_symbol(library, "ldexp") }.ok()
        })
        .map_or(ldexp as usize, |symbol| symbol as usize);
    let result = call(
        target,
        AlienType::Double,
        &[AlienType::Double, int(32)],
        &[3.0f64.to_bits(), 4u64],
    );
    assert_eq!(f64::from_bits(result), 48.0, "3.0 * 2^4");
}
