// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LP64D scalar foreign calls on riscv64.
//!
//! The targets are `extern "C"` functions defined right here: on riscv64 they
//! follow the psABI exactly like any C function, so the suite needs no
//! compiler on the device and can cover shapes libc does not happen to offer.
//! Every case is one the legacy dispatcher could not express (it transmutes to
//! eighteen all-integer shapes), plus the two places LP64D departs from
//! AAPCS64: floats overflowing into integer registers, and variadic doubles
//! travelling in integer registers.
#![cfg(all(target_arch = "riscv64", unix))]

use egcl_rt::ffi::{AlienType, ffi_call, ffi_call_variadic};

fn int(bits: u8) -> AlienType {
    AlienType::Int { bits, signed: true }
}

/// Ten integers: a0-a7 fill, then two go to the stack.
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

/// Ten doubles: fa0-fa7 fill, then two go to INTEGER registers a0 and a1.
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

/// Nine floats after eight integers: fa0-fa7 take eight, the ninth float
/// finds the integer registers full too and lands on the stack.
#[allow(clippy::too_many_arguments)]
extern "C" fn ints_then_floats(
    a: i64,
    b: i64,
    c: i64,
    d: i64,
    e: i64,
    f: i64,
    g: i64,
    h: i64,
    x0: f32,
    x1: f32,
    x2: f32,
    x3: f32,
    x4: f32,
    x5: f32,
    x6: f32,
    x7: f32,
    x8: f32,
) -> f64 {
    (a + b + c + d + e + f + g + h) as f64
        + f64::from(x0 + x1 + x2 + x3 + x4 + x5 + x6 + x7) * 10.0
        + f64::from(x8) * 1000.0
}

/// Mixed classes interleave across the two independent register files.
extern "C" fn mixed(a: i32, x: f64, b: i32, y: f32, c: i64) -> f64 {
    f64::from(a) + x + f64::from(b) * 10.0 + f64::from(y) * 100.0 + (c as f64) * 1000.0
}

/// A float return arrives NaN-boxed in fa0, not in a0.
extern "C" fn halve(x: f32) -> f32 {
    x / 2.0
}

extern "C" fn negate(x: f64) -> f64 {
    -x
}

/// LP64D sign-extends a 32-bit result into a0; the raw contract narrows it.
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
    let expected: i64 = (1..=10).map(|n| n * n).sum();
    assert_eq!(result as i64, expected);
}

#[test]
fn doubles_overflow_into_integer_registers() {
    let args: Vec<u64> = (1..=10).map(|n| (n as f64).to_bits()).collect();
    let types = vec![AlienType::Double; 10];
    let result = call(ten_doubles as usize, AlienType::Double, &types, &args);
    let expected: f64 = (1..=10).map(|n| (n * n) as f64).sum();
    assert_eq!(f64::from_bits(result), expected);
}

#[test]
fn floats_past_both_register_files_reach_the_stack() {
    let mut args: Vec<u64> = (1..=8).collect();
    args.extend((1..=9).map(|n| (n as f32).to_bits() as u64));
    let mut types = vec![int(64); 8];
    types.extend(std::iter::repeat_n(AlienType::Float, 9));
    let result = call(ints_then_floats as usize, AlienType::Double, &types, &args);
    assert_eq!(f64::from_bits(result), 36.0 + 360.0 + 9000.0);
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
fn a_float_result_comes_back_from_fa0() {
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
    let result = call(narrow as usize, int(32), &[int(32)], &[0u64]);
    assert_eq!(result, 0xFFFF_FFFF, "0 - 1, zero-extended from 32 bits");
    assert_eq!(
        egcl_rt::ffi::unmarshal_from_c(result, &int(32))
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
    unsafe extern "C" {
        fn ldexp(x: f64, n: i32) -> f64;
    }
    let target = ["libm.so", "libm.so.6", "libc.so.6"]
        .into_iter()
        .find_map(|name| {
            let library = egcl_rt::ffi::load_foreign_library(name).ok()?;
            // SAFETY: ldexp(double, int) -> double is its declared signature.
            unsafe { egcl_rt::ffi::foreign_symbol(library, "ldexp") }.ok()
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

#[test]
fn variadic_doubles_travel_in_integer_registers() {
    // snprintf(buf, n, "%d %.2f %g %s", int, double, float->double, char*):
    // the variadic double and promoted float must NOT use fa registers, or
    // libc reads garbage from a0-a7. Exercised against the real libc.
    unsafe extern "C" {
        fn snprintf(buffer: *mut u8, size: usize, format: *const u8, ...) -> i32;
    }
    let mut buffer = [0u8; 64];
    let format = b"%d %.2f %g %s\0";
    let text = b"ok\0";
    let types = [
        AlienType::Pointer(Box::new(AlienType::Int {
            bits: 8,
            signed: false,
        })),
        AlienType::Int {
            bits: 64,
            signed: false,
        },
        AlienType::Pointer(Box::new(AlienType::Int {
            bits: 8,
            signed: false,
        })),
        int(32),
        AlienType::Double,
        AlienType::Float,
        AlienType::Pointer(Box::new(AlienType::Int {
            bits: 8,
            signed: false,
        })),
    ];
    let args = [
        buffer.as_mut_ptr() as u64,
        buffer.len() as u64,
        format.as_ptr() as u64,
        42u64,
        2.5f64.to_bits(),
        0.75f32.to_bits() as u64,
        text.as_ptr() as u64,
    ];
    // SAFETY: snprintf's fixed signature is (char*, size_t, const char*, ...)
    // and the trailing values match the format's conversions after promotion.
    let written = unsafe {
        ffi_call_variadic(snprintf as *const (), &int(32), &types, &args, 3).expect("snprintf")
    };
    let end = buffer.iter().position(|&b| b == 0).unwrap();
    assert_eq!(
        std::str::from_utf8(&buffer[..end]).unwrap(),
        "42 2.50 0.75 ok"
    );
    assert_eq!(written as usize, end);
}
