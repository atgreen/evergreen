// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64 foreign calls (spec §4.7.5.2, R4.44).
//!
//! The x86-64 path JITs one adapter per signature, because SysV placement
//! depends on argument classification in a way that is awkward to do at
//! runtime. AAPCS64 does not need that: integers and pointers fill `X0`–`X7` in
//! order, floats and doubles fill `D0`–`D7` in order, and the remainder goes to
//! the stack in eight-byte slots. One trampoline that loads all sixteen
//! registers from two buffers therefore covers every scalar signature, and
//! there is no A64 encoder to write, review, or keep correct.
//!
//! What this replaces: `legacy.rs` handles non-x86 targets by transmuting to a
//! hardcoded set of eighteen shapes — all-`i32` or all-`u64`, nought to eight
//! arguments. Integers only. A mixed signature like
//! `glUniform3f(GLint, GLfloat, GLfloat, GLfloat)` cannot be expressed there at
//! all, which is why the Android raymarcher could not run on arm64.
//!
//! Scope, deliberately: scalars — integers, pointers, floats, doubles. That is
//! the whole EGL/GLES/libandroid surface the app calls. Aggregates need HFA/HVA
//! classification, which differs materially from SysV and is tracked separately.
//!
//! Self-contained on purpose. The x86 modules share `Scalar`/`Location` through
//! `call.rs`, but that file is under active development for Win64; duplicating
//! forty lines here buys an independent backend with no conflict surface.
//! Consolidating is worth doing once both backends have settled.

use super::AlienType;
use crate::error::EgclError;

const ARGUMENT_REGISTERS: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scalar {
    Void,
    Integer { bits: u8, signed: bool },
    Float,
    Double,
}

impl Scalar {
    fn from_type(ty: &AlienType) -> Result<Self, EgclError> {
        match ty {
            AlienType::Void => Ok(Self::Void),
            AlienType::Int {
                bits: bits @ (8 | 16 | 32 | 64),
                signed,
            } => Ok(Self::Integer {
                bits: *bits,
                signed: *signed,
            }),
            AlienType::Pointer(_) | AlienType::FnPtr { .. } => Ok(Self::Integer {
                bits: 64,
                signed: false,
            }),
            AlienType::Float => Ok(Self::Float),
            AlienType::Double => Ok(Self::Double),
            _ => Err(EgclError::FfiError(format!(
                "AAPCS64 scalar calls do not support {ty:?}"
            ))),
        }
    }

    fn is_floating(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }
}

/// Where one argument goes.
#[derive(Clone, Copy)]
enum Slot {
    Integer(usize),
    Float(usize),
    Stack(usize),
}

/// AAPCS64 placement.
///
/// Variadic note: on Linux and Android a variadic call places arguments in
/// exactly the same registers as a fixed one, so `fixed_count` needs no special
/// handling. Apple's arm64 ABI differs — every variadic argument goes on the
/// stack — so a Darwin port must not reuse this unchanged.
fn classify(arguments: &[Scalar]) -> Result<Vec<Slot>, EgclError> {
    let mut integers = 0usize;
    let mut floats = 0usize;
    let mut stack = 0usize;
    let mut slots = Vec::with_capacity(arguments.len());
    for scalar in arguments {
        if matches!(scalar, Scalar::Void) {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        if scalar.is_floating() {
            if floats < ARGUMENT_REGISTERS {
                slots.push(Slot::Float(floats));
                floats += 1;
                continue;
            }
        } else if integers < ARGUMENT_REGISTERS {
            slots.push(Slot::Integer(integers));
            integers += 1;
            continue;
        }
        slots.push(Slot::Stack(stack));
        stack += 1;
    }
    Ok(slots)
}

// A single-precision argument occupies `S<n>` — the LOW 32 BITS of `V<n>`, as
// an f32 bit pattern, NOT converted to double. The trampoline loads 64 bits per
// register, and the low half is exactly the slot's f32 pattern, so raw slots go
// through untouched. Widening them to f64 here was wrong: the callee reads S<n>
// and got the bottom half of a double.

/// Narrow an integer result to its declared width. AAPCS64 leaves the upper
/// bits of `X0` unspecified for a 32-bit return, so a `GLint` would otherwise
/// arrive with whatever the callee happened to leave above it.
///
/// ZERO-extends, including for signed types, which is what the x86-64 adapter
/// does (`movzx`/`mov eax,eax`) and what `unmarshal_from_c` expects: it masks to
/// the declared width and performs the sign extension itself. Sign-extending
/// here would wash out after that mask, but the two backends would no longer
/// agree on what a raw result means.
fn narrow_result(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits: 64, .. } | Scalar::Void => raw,
        Scalar::Integer { bits, .. } => raw & ((1u64 << bits) - 1),
        // Float and Double results are read from the right register by the
        // matching trampoline declaration and need no further work.
        Scalar::Float | Scalar::Double => raw,
    }
}

/// Call a foreign function with the AAPCS64 scalar ABI.
///
/// # Safety
/// `fn_ptr` must point to a function with exactly this signature, and `args`
/// must hold one raw slot per argument.
pub unsafe fn ffi_call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, EgclError> {
    if fn_ptr.is_null() {
        return Err(EgclError::FfiError("null function pointer".into()));
    }
    if arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "foreign argument count does not match signature".into(),
        ));
    }
    // EGCL_FFI_PROFILE (bliss-1dp): the same four phases the x86-64 path
    // reports, so the two architectures can be compared directly.
    let profile = crate::ffi::ffi_profile_enabled();
    let t0 = if profile {
        Some(std::time::Instant::now())
    } else {
        None
    };

    let result = Scalar::from_type(ret_type)?;
    let scalars = arg_types
        .iter()
        .map(Scalar::from_type)
        .collect::<Result<Vec<_>, _>>()?;
    let slots = classify(&scalars)?;
    let t1 = t0.map(|_| std::time::Instant::now());

    let mut integers = [0u64; ARGUMENT_REGISTERS];
    let mut floats = [0u64; ARGUMENT_REGISTERS];
    let mut stack: Vec<u64> = Vec::new();
    for (slot, raw) in slots.iter().zip(args) {
        let bits = *raw;
        match *slot {
            Slot::Integer(index) => integers[index] = bits,
            Slot::Float(index) => floats[index] = bits,
            Slot::Stack(index) => {
                if stack.len() <= index {
                    stack.resize(index + 1, 0);
                }
                stack[index] = bits;
            }
        }
    }

    // Publish Native state so a collection can proceed while foreign code runs,
    // exactly as the x86-64 path does.
    let foreign_frame = crate::debug_stack::ForeignFrame::enter(fn_ptr)?;
    let state_guard = crate::safepoint::ForeignStateScope::native();
    let t2 = t0.map(|_| std::time::Instant::now());
    // SAFETY: the trampoline reads eight words from each register buffer and
    // `stack.len()` from the stack buffer, all initialised above. The caller
    // vouches for the target's signature.
    let raw = unsafe {
        // One declaration per return class, so Rust reads the result from the
        // register the ABI actually put it in: X0, D0, or S0. A single u64
        // entry point would mangle every float-returning call.
        if matches!(result, Scalar::Float) {
            u64::from(
                call_returning_float(
                    fn_ptr,
                    integers.as_ptr(),
                    floats.as_ptr(),
                    stack.as_ptr(),
                    stack.len(),
                )
                .to_bits(),
            )
        } else if matches!(result, Scalar::Double) {
            call_returning_double(
                fn_ptr,
                integers.as_ptr(),
                floats.as_ptr(),
                stack.as_ptr(),
                stack.len(),
            )
            .to_bits()
        } else {
            call_returning_word(
                fn_ptr,
                integers.as_ptr(),
                floats.as_ptr(),
                stack.as_ptr(),
                stack.len(),
            )
        }
    };
    let t3 = t0.map(|_| std::time::Instant::now());
    drop(state_guard);
    drop(foreign_frame);
    if let (Some(t0), Some(t1), Some(t2), Some(t3)) = (t0, t1, t2, t3) {
        crate::ffi::ffi_profile_record(
            t1.duration_since(t0).as_nanos() as u64,
            t2.duration_since(t1).as_nanos() as u64,
            t3.duration_since(t2).as_nanos() as u64,
            std::time::Instant::now().duration_since(t3).as_nanos() as u64,
        );
    }
    Ok(narrow_result(result, raw))
}

/// Variadic calls, after the C default argument promotions.
///
/// # Safety
/// As [`ffi_call`], and the callee must consume the trailing arguments using
/// their promoted types.
pub unsafe fn ffi_call_variadic(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
) -> Result<u64, EgclError> {
    if fixed_count > arg_types.len() || arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "invalid variadic argument counts".into(),
        ));
    }
    let mut types = arg_types.to_vec();
    let mut values = args.to_vec();
    for index in fixed_count..arg_types.len() {
        match &arg_types[index] {
            AlienType::Float => {
                types[index] = AlienType::Double;
                values[index] = (f32::from_bits(args[index] as u32) as f64).to_bits();
            }
            AlienType::Int { bits, signed } if *bits < 32 => {
                types[index] = AlienType::Int {
                    bits: 32,
                    signed: *signed,
                };
            }
            _ => {}
        }
    }
    // SAFETY: forwarded caller contract; promotion only widens.
    unsafe { ffi_call(fn_ptr, ret_type, &types, &values) }
}

/// The trampoline body. Two declarations share it so that Rust reads the result
/// from the right register: a value returned in `D0` is not in `X0`, and one
/// entry point returning `u64` would silently mangle every float-returning
/// call.
macro_rules! aapcs64_trampoline {
    ($name:ident -> $ret:ty) => {
        /// # Safety
        /// `ints` and `floats` each address eight readable words; `stack`
        /// addresses `stack_words`; `target` is executable and AAPCS64.
        #[cfg(target_arch = "aarch64")]
        #[unsafe(naked)]
        unsafe extern "C" fn $name(
            target: *const (),
            ints: *const u64,
            floats: *const u64,
            stack: *const u64,
            stack_words: usize,
        ) -> $ret {
            core::arch::naked_asm!(
                // X29 anchors the frame so the variable-size outgoing area
                // below needs no separate bookkeeping to unwind.
                "stp x29, x30, [sp, #-16]!",
                "mov x29, sp",
                // Move the incoming pointers clear of X0-X7 before those become
                // the callee's arguments.
                "mov x16, x0",
                "mov x9,  x1",
                "mov x10, x2",
                "mov x11, x3",
                "mov x12, x4",
                // Round only the outgoing allocation to an even word count.
                // Keep X12 as the actual readable input count: padding the
                // destination never authorizes reading a source padding word.
                // AAPCS64 requires SP 16-byte aligned throughout the call.
                "add x13, x12, #1",
                "bic x13, x13, #1",
                "lsl x13, x13, #3",
                "sub sp, sp, x13",
                "mov x14, #0",
                "1:",
                "cmp x14, x12",
                "b.ge 2f",
                "ldr x15, [x11, x14, lsl #3]",
                "str x15, [sp, x14, lsl #3]",
                "add x14, x14, #1",
                "b 1b",
                "2:",
                // Floats first; loading them clobbers nothing still needed.
                "ldp d0, d1, [x10, #0]",
                "ldp d2, d3, [x10, #16]",
                "ldp d4, d5, [x10, #32]",
                "ldp d6, d7, [x10, #48]",
                // Integers last: X0/X1 held our own arguments until now.
                "ldp x0, x1, [x9, #0]",
                "ldp x2, x3, [x9, #16]",
                "ldp x4, x5, [x9, #32]",
                "ldp x6, x7, [x9, #48]",
                "blr x16",
                "mov sp, x29",
                "ldp x29, x30, [sp], #16",
                "ret",
            )
        }

        #[cfg(not(target_arch = "aarch64"))]
        unsafe extern "C" fn $name(
            _target: *const (),
            _ints: *const u64,
            _floats: *const u64,
            _stack: *const u64,
            _stack_words: usize,
        ) -> $ret {
            unreachable!("AAPCS64 trampoline on a non-AArch64 target")
        }
    };
}

aapcs64_trampoline!(call_returning_word -> u64);
aapcs64_trampoline!(call_returning_double -> f64);
aapcs64_trampoline!(call_returning_float -> f32);

#[cfg(all(test, target_arch = "aarch64", unix))]
mod stack_input_tests {
    use super::{call_returning_double, call_returning_float, call_returning_word};
    use crate::syscall;
    use std::mem::{size_of, size_of_val};

    /// Put the last readable argument immediately before an inaccessible page.
    /// Ordinary Vec spare capacity can conceal a one-word assembly overread.
    struct GuardedStack {
        mapping: *mut u8,
        mapping_len: usize,
        words: *const u64,
    }

    impl GuardedStack {
        fn new(values: &[u64]) -> Self {
            let page = syscall::page_size();
            assert!(values.len() <= page / size_of::<u64>());
            // SAFETY: own both anonymous pages until Drop; only the first is
            // readable after protection. The source words lie inside it.
            unsafe {
                let mapping = syscall::mmap(
                    std::ptr::null_mut(),
                    page * 2,
                    syscall::PROT_READ | syscall::PROT_WRITE,
                    syscall::MAP_PRIVATE | syscall::MAP_ANONYMOUS,
                    -1,
                    0,
                )
                .expect("guarded source mapping");
                let mut owned = Self {
                    mapping,
                    mapping_len: page * 2,
                    words: std::ptr::null(),
                };
                syscall::mprotect(mapping.add(page), page, syscall::PROT_NONE)
                    .expect("source guard page");
                let words = mapping.add(page - size_of_val(values)).cast::<u64>();
                std::ptr::copy_nonoverlapping(values.as_ptr(), words, values.len());
                owned.words = words;
                owned
            }
        }
    }

    impl Drop for GuardedStack {
        fn drop(&mut self) {
            // SAFETY: this object uniquely owns the original complete mapping.
            let _ = unsafe { syscall::munmap(self.mapping, self.mapping_len) };
        }
    }

    macro_rules! targets {
        ($nine:ident, $ten:ident, $result:ty) => {
            #[allow(clippy::too_many_arguments, reason = "exercise arguments beyond X0-X7")]
            extern "C" fn $nine(
                a: u64,
                b: u64,
                c: u64,
                d: u64,
                e: u64,
                f: u64,
                g: u64,
                h: u64,
                i: u64,
            ) -> $result {
                (a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 + i * 9) as $result
            }
            #[allow(clippy::too_many_arguments, reason = "exercise arguments beyond X0-X7")]
            extern "C" fn $ten(
                a: u64,
                b: u64,
                c: u64,
                d: u64,
                e: u64,
                f: u64,
                g: u64,
                h: u64,
                i: u64,
                j: u64,
            ) -> $result {
                (a + b * 2 + c * 3 + d * 4 + e * 5 + f * 6 + g * 7 + h * 8 + i * 9 + j * 10)
                    as $result
            }
        };
    }

    targets!(nine_word, ten_word, u64);
    targets!(nine_float, ten_float, f32);
    targets!(nine_double, ten_double, f64);

    #[test]
    fn guarded_odd_and_even_inputs_preserve_all_return_register_classes() {
        let registers = [1, 2, 3, 4, 5, 6, 7, 8];
        let floats = [0; 8];
        for (values, word_target, float_target, double_target, expected) in [
            (
                &[9][..],
                nine_word as *const (),
                nine_float as *const (),
                nine_double as *const (),
                285,
            ),
            (
                &[9, 10][..],
                ten_word as *const (),
                ten_float as *const (),
                ten_double as *const (),
                385,
            ),
        ] {
            let stack = GuardedStack::new(values);
            // SAFETY: each statically compiled C-ABI target takes eight u64
            // register arguments followed by exactly values.len() u64 stack
            // arguments, and has the selected X0/S0/D0 return type. Narrow
            // packed Apple stack arguments are deliberately not under test.
            unsafe {
                assert_eq!(
                    call_returning_word(
                        word_target,
                        registers.as_ptr(),
                        floats.as_ptr(),
                        stack.words,
                        values.len()
                    ),
                    expected
                );
                assert_eq!(
                    call_returning_float(
                        float_target,
                        registers.as_ptr(),
                        floats.as_ptr(),
                        stack.words,
                        values.len()
                    ),
                    expected as f32
                );
                assert_eq!(
                    call_returning_double(
                        double_target,
                        registers.as_ptr(),
                        floats.as_ptr(),
                        stack.words,
                        values.len()
                    ),
                    expected as f64
                );
            }
        }
    }

    #[test]
    fn zero_stack_arguments_do_not_read_the_inaccessible_input_pointer() {
        extern "C" fn answer() -> u64 {
            42
        }
        let stack = GuardedStack::new(&[]);
        let registers = [0; 8];
        // SAFETY: the target has no arguments. The input address deliberately
        // points into PROT_NONE; count zero must cause no source load at all.
        let result = unsafe {
            call_returning_word(
                answer as *const (),
                registers.as_ptr(),
                registers.as_ptr(),
                stack.words,
                0,
            )
        };
        assert_eq!(result, 42);
    }
}
