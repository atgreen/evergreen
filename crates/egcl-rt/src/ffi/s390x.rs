// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! s390x (IBM Z, 64-bit ELF ABI) foreign calls.
//!
//! Structured like the ELFv2 path and for the same reason: scalar placement is
//! simple enough to do at runtime, so one generated trampoline per overflow-word
//! count loads every argument register from buffers and covers all scalar
//! signatures, with nothing signature-specific to encode.
//!
//! What this replaces: `legacy.rs`, which transmuted to eighteen hardcoded
//! integer-only shapes. A double passed through it went into a GENERAL register
//! as its bit pattern, and the callee read whatever f0 happened to hold — so
//! `(sqrt 16d0)` answered 16.0 and `pow` returned a denormal, silently, with no
//! error at any point.
//!
//! Placement, per the zSeries ELF ABI supplement and MEASURED against gcc on a
//! z17 rather than taken from the document alone (`gcc -O1 -S` of a callee shows
//! which register each parameter is read from):
//!
//! - integers and pointers fill r2–r6 in order, with their OWN counter;
//! - floats and doubles fill f0, f2, f4, f6 in order, with their OWN counter —
//!   independent of the integer one, unlike ELFv2, so `f(long, double, long)`
//!   puts the second long in r3;
//! - everything that does not get a register goes to the caller's outgoing area
//!   at 160(r15) and up, one doubleword per argument in declaration order
//!   whatever its class;
//! - a narrower integer is extended to 64 bits by the CALLER — gcc's callee adds
//!   a `short` and an `unsigned char` with a 32-bit `ar` and no extension of its
//!   own — signed types sign-extended, unsigned zero-extended;
//! - a single-precision value lives in the HIGH half of its FPR, and in the
//!   high-address half of its stack doubleword (big endian, right-justified, so
//!   at offset +4);
//! - variadic arguments are passed exactly as fixed ones (a double bound for
//!   `printf` stays in f0), so `fixed_count` only validates;
//! - the result comes back in r2, or in f0 for floating point.
//!
//! Scope, deliberately: scalars — integers, pointers, floats, doubles.
//! Aggregates need their own classification and are tracked separately.

use super::AlienType;
use crate::error::EgclError;

/// r2–r6 carry integer and pointer arguments.
const INTEGER_REGISTERS: usize = 5;
/// f0, f2, f4, f6 carry floating-point arguments.
const FLOAT_REGISTERS: usize = 4;
/// The caller's register save area; outgoing stack arguments start above it.
const OUTGOING_ARGS: i32 = 160;

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
                "s390x scalar calls do not support {ty:?}"
            ))),
        }
    }

    fn is_floating(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }
}

/// Where one argument goes: exactly one of a GPR, an FPR, or a stack doubleword.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Placement {
    Integer(usize),
    Float(usize),
    Stack(usize),
}

/// s390x placement: two independent register counters, one shared overflow area.
fn classify(arguments: &[Scalar]) -> Result<Vec<Placement>, EgclError> {
    let mut integers = 0usize;
    let mut floats = 0usize;
    let mut stack = 0usize;
    let mut placements = Vec::with_capacity(arguments.len());
    for scalar in arguments {
        if matches!(scalar, Scalar::Void) {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        let placement = if scalar.is_floating() {
            if floats < FLOAT_REGISTERS {
                floats += 1;
                Placement::Float(floats - 1)
            } else {
                stack += 1;
                Placement::Stack(stack - 1)
            }
        } else if integers < INTEGER_REGISTERS {
            integers += 1;
            Placement::Integer(integers - 1)
        } else {
            stack += 1;
            Placement::Stack(stack - 1)
        };
        placements.push(placement);
    }
    Ok(placements)
}

/// The 64-bit pattern a GPR or stack doubleword must hold for an integer
/// argument: the caller extends narrow values to the full register.
fn extend_integer(bits: u8, signed: bool, raw: u64) -> u64 {
    if bits >= 64 {
        return raw;
    }
    let shift = 64 - u32::from(bits);
    if signed {
        (((raw << shift) as i64) >> shift) as u64
    } else {
        raw & ((1u64 << bits) - 1)
    }
}

/// The 64-bit pattern an argument's register must hold. An FPR is loaded with
/// `ldy` as a raw doubleword, so a single lands in its high half by shifting.
fn register_bits(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits, signed } => extend_integer(bits, signed, raw),
        Scalar::Float => u64::from(raw as u32) << 32,
        Scalar::Double | Scalar::Void => raw,
    }
}

/// The 64-bit pattern an argument's stack doubleword must hold. Big endian: a
/// single sits in the low-order (high-address) four bytes.
fn stack_bits(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits, signed } => extend_integer(bits, signed, raw),
        Scalar::Float => u64::from(raw as u32),
        Scalar::Double | Scalar::Void => raw,
    }
}

/// Narrow an integer result to its declared width.
///
/// ZERO-extends, including for signed types, matching the x86-64 adapter and
/// the AArch64 and ELFv2 paths: `unmarshal_from_c` masks to the declared width
/// and does the sign extension itself.
fn narrow_result(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits: 64, .. } | Scalar::Void => raw,
        Scalar::Integer { bits, .. } => raw & ((1u64 << bits) - 1),
        Scalar::Float | Scalar::Double => raw,
    }
}

/// Call a foreign function with the s390x scalar ABI.
///
/// # Safety
/// `fn_ptr` must point to a function with exactly this signature, and `args` must
/// hold one raw value per argument type.
#[cfg(all(target_arch = "s390x", unix))]
pub unsafe fn ffi_call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, EgclError> {
    // SAFETY: forwarded unchanged; a fixed call is the variadic form with no tail.
    unsafe { call(fn_ptr, ret_type, arg_types, args, None) }
}

/// Variadic calls, after the C default argument promotions. The ABI passes the
/// tail exactly as it passes fixed arguments, so this only validates the count.
///
/// # Safety
/// As [`ffi_call`], and the callee must consume the trailing arguments using their
/// promoted types.
#[cfg(all(target_arch = "s390x", unix))]
pub unsafe fn ffi_call_variadic(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
) -> Result<u64, EgclError> {
    if fixed_count > arg_types.len() {
        return Err(EgclError::FfiError(
            "fixed argument count exceeds the argument list".into(),
        ));
    }
    // SAFETY: forwarded unchanged.
    unsafe { call(fn_ptr, ret_type, arg_types, args, Some(fixed_count)) }
}

/// # Safety
/// As [`ffi_call`].
#[cfg(all(target_arch = "s390x", unix))]
unsafe fn call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    _fixed_count: Option<usize>,
) -> Result<u64, EgclError> {
    if fn_ptr.is_null() {
        return Err(EgclError::FfiError("null function pointer".into()));
    }
    if arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "foreign argument count does not match signature".into(),
        ));
    }
    let result = Scalar::from_type(ret_type)?;
    let scalars = arg_types
        .iter()
        .map(Scalar::from_type)
        .collect::<Result<Vec<_>, _>>()?;
    let placements = classify(&scalars)?;

    let mut integers = [0u64; INTEGER_REGISTERS];
    let mut floats = [0u64; FLOAT_REGISTERS];
    let mut stack: Vec<u64> = Vec::new();
    for ((placement, scalar), raw) in placements.iter().zip(&scalars).zip(args) {
        match *placement {
            Placement::Integer(index) => integers[index] = register_bits(*scalar, *raw),
            Placement::Float(index) => floats[index] = register_bits(*scalar, *raw),
            Placement::Stack(index) => {
                if stack.len() <= index {
                    stack.resize(index + 1, 0);
                }
                stack[index] = stack_bits(*scalar, *raw);
            }
        }
    }
    let entry = adapter(stack.len())?;

    // Publish Native state so a collection can proceed while foreign code runs,
    // exactly as the other paths do.
    let foreign_frame = crate::debug_stack::ForeignFrame::enter(fn_ptr)?;
    let state_guard = crate::safepoint::ForeignStateScope::native();
    // SAFETY: the trampoline reads a fixed count from each register buffer and
    // `stack.len()` from the stack buffer, all initialised above. The caller
    // vouches for the target's signature.
    let raw = unsafe {
        // One transmute per return class, so Rust reads the result from the
        // register the ABI actually used: r2, f0 as a double, or f0 as a single.
        // The adapter's code is the same either way.
        type Word = unsafe extern "C" fn(*const (), *const u64, *const u64, *const u64) -> u64;
        type Double = unsafe extern "C" fn(*const (), *const u64, *const u64, *const u64) -> f64;
        type Single = unsafe extern "C" fn(*const (), *const u64, *const u64, *const u64) -> f32;
        let ints = integers.as_ptr();
        let fps = floats.as_ptr();
        let overflow = stack.as_ptr();
        match result {
            Scalar::Float => {
                let call: Single = std::mem::transmute(entry);
                u64::from(call(fn_ptr, ints, fps, overflow).to_bits())
            }
            Scalar::Double => {
                let call: Double = std::mem::transmute(entry);
                call(fn_ptr, ints, fps, overflow).to_bits()
            }
            _ => {
                let call: Word = std::mem::transmute(entry);
                call(fn_ptr, ints, fps, overflow)
            }
        }
    };
    drop(state_guard);
    drop(foreign_frame);
    Ok(narrow_result(result, raw))
}

/// The trampoline is GENERATED, through the same assembler the JIT tiers use,
/// rather than written as inline assembly: that assembler is execution-tested
/// independently and its encodings are pinned against llvm-mc.
///
/// One adapter per stack-argument count rather than per signature: the register
/// loads are identical for every signature, and only the number of overflow
/// words copied above the save area varies. Zero is much the commonest case and
/// costs no copies at all.
const MAX_STACK_WORDS: usize = 64;

static ADAPTERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<usize, &'static crate::jit::JitBuffer>>,
> = std::sync::OnceLock::new();

/// Build the adapter for `stack_words` overflow arguments.
///
/// Entry, per the ELF ABI: r2 = target, r3 = integer buffer, r4 = float buffer,
/// r5 = overflow buffer (the count is baked in).
fn build_adapter(stack_words: usize) -> Option<crate::jit::JitBuffer> {
    use crate::asm_s390x::Asm;
    let overflow_bytes = 8 * i32::try_from(stack_words).ok()?;
    let mut asm = Asm::new();
    // Save r6–r15 in the caller's save area and claim a frame with room for the
    // overflow words above our own 160-byte save area.
    asm.prologue_with(overflow_bytes);
    // Overflow arguments, one doubleword each, at 160(r15) and up. Unrolled,
    // because the count is known when this is built.
    for index in 0..stack_words {
        let offset = 8 * index as i32;
        asm.load(1, 5, offset);
        asm.store(1, 15, OUTGOING_ARGS + offset);
    }
    // Floating-point arguments: f0, f2, f4, f6, loaded as raw doublewords.
    for index in 0..FLOAT_REGISTERS {
        asm.load_float(2 * index as u8, 4, 8 * index as i32);
    }
    // The target moves to r1 before r2 is overwritten with the first argument.
    asm.mov(1, 2);
    // Integer arguments last, because r2–r5 held this adapter's own arguments
    // until now. r3 IS the buffer pointer, so its own value is loaded after
    // every other register: loading it earlier destroys the base.
    for index in 0..INTEGER_REGISTERS {
        let register = 2 + index as u8;
        if register == 3 {
            continue;
        }
        asm.load(register, 3, 8 * index as i32);
    }
    asm.load(3, 3, 8);
    asm.call_reg(1);
    // r2 or f0 carries the result through untouched.
    asm.epilogue_with(overflow_bytes);
    crate::jit::JitBuffer::new(&asm.finish()?)
}

/// The adapter's entry point for `stack_words`, built once and retained.
fn adapter(stack_words: usize) -> Result<*const u8, EgclError> {
    if stack_words > MAX_STACK_WORDS {
        return Err(EgclError::FfiError(format!(
            "foreign signature needs {stack_words} stack words, more than the {MAX_STACK_WORDS} this adapter reserves"
        )));
    }
    let mut cache = ADAPTERS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| EgclError::FfiError("foreign adapter cache is poisoned".into()))?;
    if let Some(buffer) = cache.get(&stack_words) {
        return Ok(buffer.as_ptr());
    }
    let buffer = build_adapter(stack_words)
        .ok_or_else(|| EgclError::FfiError("cannot build a foreign call adapter".into()))?;
    // Leaked deliberately: an adapter is reached by raw pointer from calls already
    // in flight, and there are at most MAX_STACK_WORDS + 1 of them.
    let buffer: &'static crate::jit::JitBuffer = Box::leak(Box::new(buffer));
    cache.insert(stack_words, buffer);
    Ok(buffer.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn integer(bits: u8) -> AlienType {
        AlienType::Int { bits, signed: true }
    }

    fn placements(arguments: &[AlienType]) -> Vec<Placement> {
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        classify(&scalars).unwrap()
    }

    /// The property that distinguishes s390x from ELFv2: a double takes an FPR
    /// and leaves the integer counter alone, so the next long still gets r3.
    #[test]
    fn floats_and_integers_count_independently() {
        let placed = placements(&[integer(64), AlienType::Double, integer(64)]);
        assert_eq!(placed[0], Placement::Integer(0), "first long in r2");
        assert_eq!(placed[1], Placement::Float(0), "the double in f0");
        assert_eq!(
            placed[2],
            Placement::Integer(1),
            "the second long in r3: the double consumed no GPR"
        );
    }

    #[test]
    fn four_float_registers_then_the_stack() {
        let placed = placements(&vec![AlienType::Double; 6]);
        assert_eq!(placed[3], Placement::Float(3), "f6 is the last FPR");
        assert_eq!(placed[4], Placement::Stack(0));
        assert_eq!(placed[5], Placement::Stack(1));
    }

    #[test]
    fn five_integer_registers_then_the_stack() {
        let placed = placements(&vec![integer(64); 7]);
        assert_eq!(placed[4], Placement::Integer(4), "r6 is the last GPR");
        assert_eq!(placed[5], Placement::Stack(0));
        assert_eq!(placed[6], Placement::Stack(1));
    }

    /// Overflow doublewords are shared between the classes in declaration
    /// order: the fifth double and the sixth long interleave on the stack.
    #[test]
    fn the_overflow_area_is_shared_in_declaration_order() {
        let mut arguments = vec![AlienType::Double; 4];
        arguments.extend(vec![integer(64); 5]);
        arguments.push(AlienType::Double);
        arguments.push(integer(64));
        arguments.push(AlienType::Double);
        let placed = placements(&arguments);
        assert_eq!(placed[9], Placement::Stack(0), "fifth double");
        assert_eq!(placed[10], Placement::Stack(1), "sixth long");
        assert_eq!(placed[11], Placement::Stack(2), "sixth double");
    }

    #[test]
    fn narrow_integers_are_extended_by_the_caller() {
        let signed = Scalar::Integer {
            bits: 16,
            signed: true,
        };
        let unsigned = Scalar::Integer {
            bits: 8,
            signed: false,
        };
        assert_eq!(
            register_bits(signed, 0xffff),
            u64::MAX,
            "-1i16 sign-extends"
        );
        assert_eq!(register_bits(signed, 0x7fff), 0x7fff);
        assert_eq!(
            register_bits(unsigned, 0xffff_ff80),
            0x80,
            "u8 zero-extends"
        );
        assert_eq!(stack_bits(signed, 0xfffe), u64::MAX - 1);
    }

    /// A single-precision value is in the high half of its FPR but the low
    /// (high-address) half of its big-endian stack doubleword.
    #[test]
    fn singles_sit_high_in_a_register_and_low_on_the_stack() {
        let bits = u64::from(1.5f32.to_bits());
        assert_eq!(register_bits(Scalar::Float, bits), bits << 32);
        assert_eq!(stack_bits(Scalar::Float, bits), bits);
        let double = 2.5f64.to_bits();
        assert_eq!(register_bits(Scalar::Double, double), double);
        assert_eq!(stack_bits(Scalar::Double, double), double);
    }

    #[test]
    fn void_is_refused_as_an_argument() {
        assert!(classify(&[Scalar::Void]).is_err());
    }

    #[test]
    fn narrow_results_are_zero_extended() {
        assert_eq!(
            narrow_result(
                Scalar::Integer {
                    bits: 32,
                    signed: true
                },
                0xffff_ffff_ffff_fff5
            ),
            0xffff_fff5
        );
        assert_eq!(
            narrow_result(
                Scalar::Integer {
                    bits: 64,
                    signed: true
                },
                0xffff_ffff_ffff_fff5
            ),
            0xffff_ffff_ffff_fff5
        );
    }

    /// The adapter's shape is fixed by the count alone; pin the zero-overflow
    /// instruction stream so a change to the register order is deliberate.
    #[test]
    fn the_leaf_adapter_loads_every_register_and_the_base_last() {
        let mut asm = crate::asm_s390x::Asm::new();
        asm.prologue_with(0);
        for index in 0..FLOAT_REGISTERS {
            asm.load_float(2 * index as u8, 4, 8 * index as i32);
        }
        asm.mov(1, 2);
        for (index, register) in [(0, 2u8), (2, 4), (3, 5), (4, 6), (1, 3)] {
            asm.load(register, 3, 8 * index);
        }
        asm.call_reg(1);
        asm.epilogue_with(0);
        let expected = asm.finish().unwrap();
        let built = build_adapter(0).expect("a leaf adapter builds on any host");
        // SAFETY: the buffer holds exactly the bytes finish() produced.
        let actual = unsafe { std::slice::from_raw_parts(built.as_ptr(), expected.len()) };
        assert_eq!(actual, &expected[..]);
    }
}
