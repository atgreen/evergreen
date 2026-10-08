// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LP64D foreign calls for riscv64 (the RISC-V psABI integer calling
//! convention with hardware floating point), the same shape as
//! [`super::aapcs64`]: integers and pointers fill `a0`-`a7`, floats and
//! doubles fill `fa0`-`fa7`, and one trampoline that loads all sixteen
//! registers from two buffers plus a copied stack area covers every scalar
//! signature, so there is no instruction encoder in this path.
//!
//! Where LP64D differs from AAPCS64:
//!
//! - A float whose `fa` registers are exhausted is passed in the next free
//!   *integer* register before it goes to the stack.
//! - Variadic arguments never use `fa` registers: after the C promotions they
//!   follow the integer convention, so a variadic `double` travels in an
//!   `a` register or on the stack. The classifier therefore needs the fixed
//!   argument count.
//! - A `float` in an `fa` register must be NaN-boxed: the upper 32 bits of the
//!   64-bit register are all ones, or the callee reads it as a NaN.
//!
//! Scope, as on AArch64: scalars only. Aggregates (which LP64D flattens into
//! up to two registers by field class) are tracked separately.

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
                "LP64D scalar calls do not support {ty:?}"
            ))),
        }
    }

    fn is_floating(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }
}

/// Where one argument goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Integer(usize),
    Float(usize),
    Stack(usize),
}

/// LP64D placement. Arguments at index `fixed_count` and beyond are variadic
/// and take the integer convention whatever their type.
fn classify(arguments: &[Scalar], fixed_count: usize) -> Result<Vec<Slot>, EgclError> {
    let mut integers = 0usize;
    let mut floats = 0usize;
    let mut stack = 0usize;
    let mut slots = Vec::with_capacity(arguments.len());
    for (index, scalar) in arguments.iter().enumerate() {
        if matches!(scalar, Scalar::Void) {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        if scalar.is_floating() && index < fixed_count && floats < ARGUMENT_REGISTERS {
            slots.push(Slot::Float(floats));
            floats += 1;
        } else if integers < ARGUMENT_REGISTERS {
            slots.push(Slot::Integer(integers));
            integers += 1;
        } else {
            slots.push(Slot::Stack(stack));
            stack += 1;
        }
    }
    Ok(slots)
}

/// The 64-bit pattern an `fa` register must hold for a scalar slot: a double
/// as is, a float NaN-boxed into the low half.
fn float_register_bits(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Float => 0xffff_ffff_0000_0000 | (raw & 0xffff_ffff),
        _ => raw,
    }
}

/// Narrow an integer result to its declared width. LP64D sign-extends a
/// 32-bit result to 64 bits in `a0`, and narrower ones are extended per their
/// type, but the shared contract with the x86-64 adapter and
/// `unmarshal_from_c` is a ZERO-extended raw value of the declared width.
fn narrow_result(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits: 64, .. } | Scalar::Void => raw,
        Scalar::Integer { bits, .. } => raw & ((1u64 << bits) - 1),
        Scalar::Float | Scalar::Double => raw,
    }
}

unsafe fn ffi_call_inner(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
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
    // reports, so the architectures can be compared directly.
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
    let slots = classify(&scalars, fixed_count)?;
    let t1 = t0.map(|_| std::time::Instant::now());

    let mut integers = [0u64; ARGUMENT_REGISTERS];
    let mut floats = [0u64; ARGUMENT_REGISTERS];
    let mut stack: Vec<u64> = Vec::new();
    for ((slot, scalar), raw) in slots.iter().zip(&scalars).zip(args) {
        let bits = *raw;
        match *slot {
            Slot::Integer(index) => integers[index] = bits,
            Slot::Float(index) => floats[index] = float_register_bits(*scalar, bits),
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
        // register the ABI actually put it in: a0 or fa0 (NaN-boxed for f32).
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

/// Call a foreign function with the LP64D scalar ABI.
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
    unsafe { ffi_call_inner(fn_ptr, ret_type, arg_types, args, arg_types.len()) }
}

/// Variadic calls, after the C default argument promotions. The promoted
/// trailing arguments take the integer convention, as the psABI requires.
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
    unsafe { ffi_call_inner(fn_ptr, ret_type, &types, &values, fixed_count) }
}

/// The trampoline body. Three declarations share it so that Rust reads the
/// result from the right register: a value returned in `fa0` is not in `a0`.
macro_rules! lp64d_trampoline {
    ($name:ident -> $ret:ty) => {
        /// # Safety
        /// `ints` and `floats` each address eight readable words; `stack`
        /// addresses `stack_words`; `target` is executable and LP64D.
        #[cfg(target_arch = "riscv64")]
        #[unsafe(naked)]
        unsafe extern "C" fn $name(
            target: *const (),
            ints: *const u64,
            floats: *const u64,
            stack: *const u64,
            stack_words: usize,
        ) -> $ret {
            core::arch::naked_asm!(
                // The integrated assembler does not inherit the target's D
                // extension inside a naked function.
                ".option push",
                ".option arch, +d",
                // s0 anchors the frame so the variable-size outgoing area
                // below needs no separate bookkeeping to unwind.
                "addi sp, sp, -16",
                "sd ra, 8(sp)",
                "sd s0, 0(sp)",
                "addi s0, sp, 16",
                // Move the incoming pointers clear of a0-a7 before those
                // become the callee's arguments.
                "mv t0, a0",
                "mv t1, a1",
                "mv t2, a2",
                "mv t3, a3",
                "mv t4, a4",
                // Reserve an even number of words so sp stays 16-byte aligned
                // at the call, but copy only the words the buffer holds.
                "addi t5, t4, 1",
                "andi t5, t5, -2",
                "slli t5, t5, 3",
                "sub sp, sp, t5",
                "li t6, 0",
                "1:",
                "bge t6, t4, 2f",
                "slli t5, t6, 3",
                "add a5, t3, t5",
                "ld a5, 0(a5)",
                "add a6, sp, t5",
                "sd a5, 0(a6)",
                "addi t6, t6, 1",
                "j 1b",
                "2:",
                // Floats first; loading them clobbers nothing still needed.
                "fld fa0, 0(t2)",
                "fld fa1, 8(t2)",
                "fld fa2, 16(t2)",
                "fld fa3, 24(t2)",
                "fld fa4, 32(t2)",
                "fld fa5, 40(t2)",
                "fld fa6, 48(t2)",
                "fld fa7, 56(t2)",
                // Integers last: a0-a4 held our own arguments until now.
                "ld a0, 0(t1)",
                "ld a1, 8(t1)",
                "ld a2, 16(t1)",
                "ld a3, 24(t1)",
                "ld a4, 32(t1)",
                "ld a5, 40(t1)",
                "ld a6, 48(t1)",
                "ld a7, 56(t1)",
                "jalr ra, t0, 0",
                "addi sp, s0, -16",
                "ld ra, 8(sp)",
                "ld s0, 0(sp)",
                "addi sp, sp, 16",
                "ret",
                ".option pop",
            )
        }

        #[cfg(not(target_arch = "riscv64"))]
        unsafe extern "C" fn $name(
            _target: *const (),
            _ints: *const u64,
            _floats: *const u64,
            _stack: *const u64,
            _stack_words: usize,
        ) -> $ret {
            unreachable!("LP64D trampoline on a non-riscv64 target")
        }
    };
}

lp64d_trampoline!(call_returning_word -> u64);
lp64d_trampoline!(call_returning_double -> f64);
lp64d_trampoline!(call_returning_float -> f32);

#[cfg(test)]
mod tests {
    use super::*;

    const I: Scalar = Scalar::Integer {
        bits: 64,
        signed: true,
    };

    #[test]
    fn floats_spill_into_integer_registers_before_the_stack() {
        // Nine doubles and two integers: fa0-fa7, then the ninth double takes
        // a1 (a0 went to the first integer), the last integer a2.
        let args: Vec<Scalar> = std::iter::once(I)
            .chain(std::iter::repeat_n(Scalar::Double, 9))
            .chain(std::iter::once(I))
            .collect();
        let slots = classify(&args, args.len()).unwrap();
        assert_eq!(slots[0], Slot::Integer(0));
        assert_eq!(
            slots[1..9].to_vec(),
            (0..8).map(Slot::Float).collect::<Vec<_>>()
        );
        assert_eq!(slots[9], Slot::Integer(1));
        assert_eq!(slots[10], Slot::Integer(2));
    }

    #[test]
    fn variadic_floats_take_the_integer_convention() {
        // printf-like: (pointer, double...) with one fixed argument.
        let args = [I, Scalar::Double, Scalar::Double];
        let slots = classify(&args, 1).unwrap();
        assert_eq!(
            slots,
            [Slot::Integer(0), Slot::Integer(1), Slot::Integer(2)]
        );
        // The same call with every argument fixed uses fa registers.
        let slots = classify(&args, 3).unwrap();
        assert_eq!(slots, [Slot::Integer(0), Slot::Float(0), Slot::Float(1)]);
    }

    #[test]
    fn everything_past_the_register_files_goes_to_the_stack_in_order() {
        let args: Vec<Scalar> = std::iter::repeat_n(I, 9)
            .chain(std::iter::repeat_n(Scalar::Float, 9))
            .collect();
        let slots = classify(&args, args.len()).unwrap();
        // The ninth integer and the ninth float both overflow: integer
        // registers are full by then, so both go to the stack, in order.
        assert_eq!(slots[8], Slot::Stack(0));
        assert_eq!(slots[17], Slot::Stack(1));
    }

    #[test]
    fn singles_are_nan_boxed_in_float_registers() {
        let bits = 1.5f32.to_bits() as u64;
        assert_eq!(
            float_register_bits(Scalar::Float, bits),
            0xffff_ffff_0000_0000 | bits
        );
        let double = 1.5f64.to_bits();
        assert_eq!(float_register_bits(Scalar::Double, double), double);
    }
}
