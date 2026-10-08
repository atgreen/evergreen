// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution tests for the s390x foreign-call path: call real C functions on
//! IBM Z and check the values that come back.
//!
//! The unit tests in `ffi/s390x.rs` pin argument PLACEMENT. These prove the
//! generated trampoline delivers arguments where placement says they go — the
//! legacy path this replaces returned plausible wrong numbers for every
//! floating-point signature without a single test noticing.

#![cfg(all(target_arch = "s390x", unix))]

use egcl_rt::ffi::{AlienType, ffi_call, ffi_call_variadic};

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
fn integers_fill_five_registers_and_then_the_stack() {
    let types: Vec<_> = (0..10).map(|_| int(64)).collect();
    let args: Vec<u64> = (1..=10).collect();
    let expected: i64 = (1..=10).map(|n| n * n).sum();
    assert_eq!(
        call(ten_integers as *const (), &int(64), &types, &args) as i64,
        expected,
        "the sixth to tenth arguments come from 160(r15) upward"
    );
}

extern "C" fn mixed(a: i64, b: f64, c: i64, d: f64, e: i64) -> f64 {
    a as f64 + b * 10.0 + c as f64 * 100.0 + d * 1000.0 + e as f64 * 10000.0
}

#[test]
fn floats_and_integers_count_independently() {
    // The property that distinguishes s390x from ELFv2: `c` arrives in r3, not
    // r4, because the double before it took f0 and no GPR.
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
fn doubles_fill_four_registers_and_then_the_stack() {
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
    // A single lives in the high half of its FPR; widening it to double, or
    // leaving it in the low half, both read as garbage.
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

extern "C" fn nine_singles(
    a: f32,
    b: f32,
    c: f32,
    d: f32,
    e: f32,
    f: f32,
    g: f32,
    h: f32,
    i: f32,
) -> f32 {
    a + b * 2.0 + c * 3.0 + d * 4.0 + e * 5.0 + f * 6.0 + g * 7.0 + h * 8.0 + i * 9.0
}

#[test]
fn singles_on_the_stack_are_right_justified() {
    // Big endian: the callee reads a stack single from offset +4 of its
    // doubleword. Five of the nine land there.
    let types: Vec<_> = (0..9).map(|_| AlienType::Float).collect();
    let args: Vec<u64> = (1..=9).map(|n| u64::from((n as f32).to_bits())).collect();
    let expected: f32 = (1..=9).map(|n| (n * n) as f32).sum();
    let raw = call(nine_singles as *const (), &AlienType::Float, &types, &args);
    assert_eq!(f32::from_bits(raw as u32), expected);
}

extern "C" fn narrow_arguments(a: i16, b: u8, c: i32, d: u32) -> i64 {
    i64::from(a) + i64::from(b) + i64::from(c) + i64::from(d)
}

#[test]
fn narrow_arguments_are_extended_by_the_caller() {
    // gcc's callee uses the register as-is, so the sign extension of -1i16 and
    // the zero extension of 250u8 are this side's job.
    let types = [
        int(16),
        AlienType::Int {
            bits: 8,
            signed: false,
        },
        int(32),
        AlienType::Int {
            bits: 32,
            signed: false,
        },
    ];
    let args = [
        (-1i16) as u16 as u64,
        250u64,
        (-1000i32) as u32 as u64,
        u64::from(u32::MAX),
    ];
    let raw = call(narrow_arguments as *const (), &int(64), &types, &args);
    assert_eq!(raw as i64, -1 + 250 - 1000 + i64::from(u32::MAX));
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
fn real_libc_functions_answer_correctly() {
    // Functions this crate did not compile, so the ABI is genuinely being tested
    // rather than a convention shared with the caller. These three are exactly
    // the probe that exposed the legacy path: it answered 16.0, a denormal, and
    // 1.5000000000002272.
    unsafe extern "C" {
        fn sqrt(x: f64) -> f64;
        fn pow(x: f64, y: f64) -> f64;
        fn ldexp(x: f64, n: i32) -> f64;
    }
    let raw = call(
        sqrt as *const (),
        &AlienType::Double,
        &[AlienType::Double],
        &[16.0f64.to_bits()],
    );
    assert_eq!(f64::from_bits(raw), 4.0);
    let raw = call(
        pow as *const (),
        &AlienType::Double,
        &[AlienType::Double, AlienType::Double],
        &[2.0f64.to_bits(), 10.0f64.to_bits()],
    );
    assert_eq!(f64::from_bits(raw), 1024.0);
    let raw = call(
        ldexp as *const (),
        &AlienType::Double,
        &[AlienType::Double, int(32)],
        &[1.5f64.to_bits(), 4u64],
    );
    assert_eq!(f64::from_bits(raw), 24.0, "1.5 * 2^4");
}

#[test]
fn a_variadic_double_is_read_from_its_register() {
    // snprintf(buf, size, "%d %.2f %ld", 7, 2.5, 123456789012): the tail is
    // passed exactly as fixed arguments would be, so the double stays in f0 and
    // the long after it takes r6.
    unsafe extern "C" {
        fn snprintf(buf: *mut u8, size: usize, fmt: *const u8, ...) -> i32;
    }
    let mut buffer = [0u8; 64];
    let format = c"%d %.2f %ld";
    let types = [
        AlienType::Pointer(Box::new(AlienType::Void)),
        AlienType::Int {
            bits: 64,
            signed: false,
        },
        AlienType::Pointer(Box::new(AlienType::Void)),
        int(32),
        AlienType::Double,
        int(64),
    ];
    let args = [
        buffer.as_mut_ptr() as u64,
        buffer.len() as u64,
        format.as_ptr() as u64,
        7u64,
        2.5f64.to_bits(),
        123_456_789_012u64,
    ];
    // SAFETY: the signature above is snprintf's, with the tail promoted.
    let written = unsafe { ffi_call_variadic(snprintf as *const (), &int(32), &types, &args, 3) }
        .expect("variadic calls are supported");
    let text = std::str::from_utf8(&buffer[..written as usize]).unwrap();
    assert_eq!(text, "7 2.50 123456789012");
}

#[test]
fn a_null_target_is_refused_rather_than_called() {
    // SAFETY: a null target is rejected before any call is made.
    let outcome = unsafe { ffi_call(std::ptr::null(), &int(64), &[], &[]) };
    assert!(outcome.is_err());
}
