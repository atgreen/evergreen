// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64 foreign calls (spec §4.7.5.2, R4.44).
//!
//! The x86-64 path JITs one adapter per signature, because SysV placement
//! depends on argument classification in a way that is awkward to do at
//! runtime. AAPCS64 does not need that: integers and pointers fill `X0`–`X7` in
//! order, floats and doubles fill `D0`–`D7` in order, and remaining scalars spill
//! to the stack. Generic AAPCS64 uses eight-byte slots; Apple's fixed spills
//! pack at natural alignment and its anonymous arguments use stack slots.
//! One trampoline loads all sixteen registers from two buffers, covering the
//! supported scalar signatures without executable adapter allocation, and
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

/// Apple retains the scalar register files but changes stack placement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Abi {
    Aapcs64,
    Apple,
}

impl Abi {
    fn native() -> Self {
        if cfg!(target_vendor = "apple") {
            Self::Apple
        } else {
            Self::Aapcs64
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Slot {
    Integer(usize),
    Float(usize),
    Stack { offset: usize, width: usize },
}

struct Layout {
    slots: Vec<Slot>,
    stack_bytes: usize,
}

impl Scalar {
    fn size(self) -> usize {
        match self {
            Self::Void => 0,
            Self::Integer { bits, .. } => usize::from(bits) / 8,
            Self::Float => 4,
            Self::Double => 8,
        }
    }
}

fn aligned(value: usize, alignment: usize) -> Result<usize, EgclError> {
    value
        .checked_add(alignment - 1)
        .map(|n| n & !(alignment - 1))
        .ok_or_else(|| EgclError::FfiError("foreign stack layout overflow".into()))
}

/// Classify already-promoted scalar arguments. Apple anonymous arguments go
/// directly to eight-byte stack slots; fixed spills occupy their natural size.
/// Generic AAPCS64 preserves its existing eight-byte spill representation.
fn classify(
    arguments: &[Scalar],
    abi: Abi,
    fixed_count: Option<usize>,
) -> Result<Layout, EgclError> {
    if fixed_count.is_some_and(|count| count > arguments.len()) {
        return Err(EgclError::FfiError(
            "invalid variadic argument counts".into(),
        ));
    }
    let mut integers = 0usize;
    let mut floats = 0usize;
    let mut stack_bytes = 0usize;
    let mut slots = Vec::with_capacity(arguments.len());
    for (index, scalar) in arguments.iter().enumerate() {
        if matches!(scalar, Scalar::Void) {
            return Err(EgclError::FfiError("void is not an argument type".into()));
        }
        let anonymous = abi == Abi::Apple && fixed_count.is_some_and(|count| index >= count);
        if !anonymous {
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
        }
        let (alignment, occupied, width) = if abi == Abi::Aapcs64 {
            (8, 8, 8)
        } else if anonymous {
            (8, 8, scalar.size())
        } else {
            let size = scalar.size();
            (size, size, size)
        };
        let offset = aligned(stack_bytes, alignment)?;
        stack_bytes = offset
            .checked_add(occupied)
            .ok_or_else(|| EgclError::FfiError("foreign stack layout overflow".into()))?;
        slots.push(Slot::Stack { offset, width });
    }
    Ok(Layout { slots, stack_bytes })
}

/// Apple requires the caller to extend narrow integer register arguments to
/// 32 bits. Stack fields still occupy their declared natural width.
fn narrow_integer_bits(raw: u64, bits: u8, signed: bool) -> u64 {
    let shift = 32 - u32::from(bits);
    let low = (raw as u32) << shift;
    u64::from(if signed {
        ((low as i32) >> shift) as u32
    } else {
        low >> shift
    })
}

struct Arguments {
    integers: [u64; ARGUMENT_REGISTERS],
    floats: [u64; ARGUMENT_REGISTERS],
    stack: Vec<u64>,
}

fn pack_arguments(scalars: &[Scalar], args: &[u64], layout: &Layout, abi: Abi) -> Arguments {
    let mut packed = Arguments {
        integers: [0; ARGUMENT_REGISTERS],
        floats: [0; ARGUMENT_REGISTERS],
        // Every copied word, including the final partial word, is initialized.
        stack: vec![0; layout.stack_bytes.div_ceil(8)],
    };
    for ((slot, scalar), raw) in layout.slots.iter().zip(scalars).zip(args) {
        match *slot {
            Slot::Integer(index) => {
                packed.integers[index] = match *scalar {
                    Scalar::Integer { bits, signed } if abi == Abi::Apple && bits < 32 => {
                        narrow_integer_bits(*raw, bits, signed)
                    }
                    _ => *raw,
                };
            }
            Slot::Float(index) => packed.floats[index] = *raw,
            Slot::Stack { offset, width } => {
                if abi == Abi::Aapcs64 {
                    // Preserve the generic path's single native-word store.
                    packed.stack[offset / 8] = *raw;
                } else {
                    // Natural alignment and widths <= 8 ensure a scalar never
                    // straddles a word. Apple ARM64 stack data is little-endian.
                    let start = offset % 8;
                    let mut word = packed.stack[offset / 8].to_ne_bytes();
                    word[start..start + width].copy_from_slice(&raw.to_le_bytes()[..width]);
                    packed.stack[offset / 8] = u64::from_ne_bytes(word);
                }
            }
        }
    }
    packed
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
    // SAFETY: unchanged caller signature contract; no anonymous arguments.
    unsafe { ffi_call_impl(fn_ptr, ret_type, arg_types, args, None) }
}

unsafe fn ffi_call_impl(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: Option<usize>,
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
    let abi = Abi::native();
    let layout = classify(&scalars, abi, fixed_count)?;
    let t1 = t0.map(|_| std::time::Instant::now());

    let Arguments {
        integers,
        floats,
        stack,
    } = pack_arguments(&scalars, args, &layout, abi);

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
    let (types, values) = promote_variadic(arg_types, args, fixed_count)?;
    // SAFETY: default promotions only widen the caller's argument values.
    unsafe { ffi_call_impl(fn_ptr, ret_type, &types, &values, Some(fixed_count)) }
}

fn promote_variadic(
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
) -> Result<(Vec<AlienType>, Vec<u64>), EgclError> {
    if fixed_count > arg_types.len() || arg_types.len() != args.len() {
        return Err(EgclError::FfiError(
            "invalid variadic argument counts".into(),
        ));
    }
    let mut types = arg_types.to_vec();
    let mut values = args.to_vec();
    for index in fixed_count..arg_types.len() {
        // Reject unsupported widths rather than promoting an invalid type.
        Scalar::from_type(&arg_types[index])?;
        match arg_types[index] {
            AlienType::Float => {
                types[index] = AlienType::Double;
                values[index] = f64::from(f32::from_bits(args[index] as u32)).to_bits();
            }
            AlienType::Int {
                bits: bits @ (8 | 16),
                signed,
            } => {
                // Every u8/u16 value fits a C int on the supported targets.
                types[index] = AlienType::Int {
                    bits: 32,
                    signed: true,
                };
                values[index] = narrow_integer_bits(args[index], bits, signed);
            }
            _ => {}
        }
    }
    Ok((types, values))
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

#[cfg(test)]
mod layout_tests {
    use super::*;

    const U64: Scalar = Scalar::Integer {
        bits: 64,
        signed: false,
    };
    const U8: Scalar = Scalar::Integer {
        bits: 8,
        signed: false,
    };
    const I8: Scalar = Scalar::Integer {
        bits: 8,
        signed: true,
    };
    const U16: Scalar = Scalar::Integer {
        bits: 16,
        signed: false,
    };
    const U32: Scalar = Scalar::Integer {
        bits: 32,
        signed: false,
    };

    fn bytes(words: &[u64]) -> Vec<u8> {
        words.iter().flat_map(|word| word.to_ne_bytes()).collect()
    }

    #[test]
    fn generic_register_files_and_eight_byte_spills_are_unchanged() {
        let scalars = [U64; 10]
            .into_iter()
            .chain([Scalar::Float; 10])
            .collect::<Vec<_>>();
        for fixed in [None, Some(3)] {
            let layout = classify(&scalars, Abi::Aapcs64, fixed).expect("layout");
            assert_eq!(
                &layout.slots[..8],
                &(0..8).map(Slot::Integer).collect::<Vec<_>>()
            );
            assert_eq!(
                &layout.slots[10..18],
                &(0..8).map(Slot::Float).collect::<Vec<_>>()
            );
            for (index, offset) in [(8, 0), (9, 8), (18, 16), (19, 24)] {
                assert_eq!(layout.slots[index], Slot::Stack { offset, width: 8 });
            }
            let values = (1..=20).collect::<Vec<u64>>();
            let packed = pack_arguments(&scalars, &values, &layout, Abi::Aapcs64);
            assert_eq!(packed.stack, [9, 10, 19, 20]);
            assert_eq!(layout.stack_bytes, 32);
        }
    }

    #[test]
    fn apple_fixed_narrow_spills_use_natural_width_and_alignment() {
        let scalars = [U64; 8]
            .into_iter()
            .chain([U8, U8, U16, U32, U8, U64])
            .collect::<Vec<_>>();
        let layout = classify(&scalars, Abi::Apple, None).expect("layout");
        for (index, offset, width) in [
            (8, 0, 1),
            (9, 1, 1),
            (10, 2, 2),
            (11, 4, 4),
            (12, 8, 1),
            (13, 16, 8),
        ] {
            assert_eq!(layout.slots[index], Slot::Stack { offset, width });
        }
        let mut values = vec![0; 8];
        values.extend([0xa1, 0xb2, 0xc3d4, 0xe5f60718, 0x29, 0x3132333435363738]);
        let packed = pack_arguments(&scalars, &values, &layout, Abi::Apple);
        assert_eq!(
            bytes(&packed.stack),
            [
                0xa1, 0xb2, 0xd4, 0xc3, 0x18, 0x07, 0xf6, 0xe5, 0x29, 0, 0, 0, 0, 0, 0, 0, 0x38,
                0x37, 0x36, 0x35, 0x34, 0x33, 0x32, 0x31
            ]
        );
    }

    #[test]
    fn apple_float_spills_pack_two_single_floats_into_one_word() {
        let scalars = [Scalar::Float; 10];
        let layout = classify(&scalars, Abi::Apple, None).expect("layout");
        assert_eq!(
            layout.slots[8],
            Slot::Stack {
                offset: 0,
                width: 4
            }
        );
        assert_eq!(
            layout.slots[9],
            Slot::Stack {
                offset: 4,
                width: 4
            }
        );
        let mut values = vec![0; 8];
        values.extend([u64::from(1.5f32.to_bits()), u64::from((-2.25f32).to_bits())]);
        let packed = pack_arguments(&scalars, &values, &layout, Abi::Apple);
        assert_eq!(
            bytes(&packed.stack),
            [1.5f32.to_le_bytes(), (-2.25f32).to_le_bytes()].concat()
        );
    }

    #[test]
    fn apple_mixed_spills_share_one_cursor_and_initialize_final_padding() {
        let scalars = [U64; 8]
            .into_iter()
            .chain([Scalar::Double; 8])
            .chain([U8, Scalar::Double, U16, Scalar::Float, U8])
            .collect::<Vec<_>>();
        let layout = classify(&scalars, Abi::Apple, None).expect("layout");
        for (index, offset, width) in [
            (16, 0, 1),
            (17, 8, 8),
            (18, 16, 2),
            (19, 20, 4),
            (20, 24, 1),
        ] {
            assert_eq!(layout.slots[index], Slot::Stack { offset, width });
        }
        assert_eq!(layout.stack_bytes, 25);
        let packed = pack_arguments(&scalars, &[u64::MAX; 21], &layout, Abi::Apple);
        let mut expected = vec![0; 32];
        for (offset, width) in [(0, 1), (8, 8), (16, 2), (20, 4), (24, 1)] {
            expected[offset..offset + width].fill(0xff);
        }
        assert_eq!(bytes(&packed.stack), expected);
    }

    #[test]
    fn apple_anonymous_arguments_bypass_available_registers() {
        let scalars = [U64, Scalar::Double, U32, Scalar::Double];
        let layout = classify(&scalars, Abi::Apple, Some(2)).expect("layout");
        assert_eq!(
            layout.slots,
            [
                Slot::Integer(0),
                Slot::Float(0),
                Slot::Stack {
                    offset: 0,
                    width: 4
                },
                Slot::Stack {
                    offset: 8,
                    width: 8
                }
            ]
        );
        assert_eq!(layout.stack_bytes, 16);
        let scalars = [U64; 8]
            .into_iter()
            .chain([U8, U32, Scalar::Double])
            .collect::<Vec<_>>();
        let layout = classify(&scalars, Abi::Apple, Some(9)).expect("layout");
        assert_eq!(
            &layout.slots[8..],
            &[
                Slot::Stack {
                    offset: 0,
                    width: 1
                },
                Slot::Stack {
                    offset: 8,
                    width: 4
                },
                Slot::Stack {
                    offset: 16,
                    width: 8
                }
            ]
        );
    }

    #[test]
    fn apple_narrow_registers_are_extended_by_declared_signedness() {
        let scalars = [
            I8,
            U8,
            Scalar::Integer {
                bits: 16,
                signed: true,
            },
            U16,
        ];
        let values = [0xdeadbeef80, 0xdeadbeef80, 0xdeadbeef8001, 0xdeadbeef8001];
        let layout = classify(&scalars, Abi::Apple, None).expect("layout");
        assert_eq!(
            pack_arguments(&scalars, &values, &layout, Abi::Apple).integers[..4],
            [0xffffff80, 0x80, 0xffff8001, 0x8001]
        );
        assert_eq!(
            pack_arguments(&scalars, &values, &layout, Abi::Aapcs64).integers[..4],
            values
        );
    }

    #[test]
    fn default_promotions_transform_bits_and_leave_fixed_arguments_untouched() {
        let types = [
            AlienType::Int {
                bits: 8,
                signed: true,
            },
            AlienType::Int {
                bits: 8,
                signed: true,
            },
            AlienType::Int {
                bits: 16,
                signed: true,
            },
            AlienType::Int {
                bits: 8,
                signed: false,
            },
            AlienType::Int {
                bits: 16,
                signed: false,
            },
            AlienType::Float,
        ];
        let values = [
            0x80,
            0x80,
            0x8001,
            0xdeadbeefff,
            0xdeadbeefffff,
            u64::from(1.5f32.to_bits()),
        ];
        let (promoted, raw) = promote_variadic(&types, &values, 1).expect("promotions");
        assert!(matches!(
            promoted[0],
            AlienType::Int {
                bits: 8,
                signed: true
            }
        ));
        for ty in &promoted[1..5] {
            assert!(matches!(
                ty,
                AlienType::Int {
                    bits: 32,
                    signed: true
                }
            ));
        }
        assert!(matches!(promoted[5], AlienType::Double));
        assert_eq!(
            raw,
            [0x80, 0xffffff80, 0xffff8001, 255, 65535, 1.5f64.to_bits()]
        );
    }

    #[test]
    fn invalid_counts_void_and_unsupported_widths_are_rejected() {
        assert!(classify(&[U64], Abi::Apple, Some(2)).is_err());
        assert!(classify(&[Scalar::Void], Abi::Apple, None).is_err());
        assert!(promote_variadic(&[], &[], 1).is_err());
        assert!(promote_variadic(&[AlienType::Float], &[], 0).is_err());
        assert!(
            promote_variadic(
                &[AlienType::Int {
                    bits: 7,
                    signed: false
                }],
                &[0],
                0
            )
            .is_err()
        );
    }
}
