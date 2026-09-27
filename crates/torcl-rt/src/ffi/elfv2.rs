//! ELFv2 (ppc64le) foreign calls.
//!
//! Structured like the AAPCS64 path and for the same reason: placement is simple
//! enough to do at runtime, so one trampoline that loads every argument register
//! from buffers covers all scalar signatures and there is no encoder to keep
//! correct.
//!
//! What this replaces: `legacy.rs`, which handles remaining targets by transmuting
//! to eighteen hardcoded shapes — all-`i32` or all-`u64`, nought to eight
//! arguments, integers only. A mixed signature cannot be expressed there at all,
//! and its own header says not to extend it.
//!
//! WHERE ELFv2 DIFFERS FROM AAPCS64, and it is not a detail. AAPCS64 allocates
//! integer and float registers INDEPENDENTLY: x0–x7 fill in order and d0–d7 fill in
//! order, so `f(int, double, int)` puts the second int in x1. ELFv2 allocates from
//! a single ordered parameter save area, where a floating-point argument takes the
//! next FPR *and consumes its position's GPR without using it*. The same call puts
//! the second int in r5, not r4. Assuming otherwise shifts every argument after the
//! first float — a silent wrong-value bug, not a crash, which is why the register
//! index here comes from the argument's POSITION rather than from a counter.
//!
//! Scope, deliberately: scalars — integers, pointers, floats, doubles. Aggregates
//! need their own classification and are tracked separately.

use super::AlienType;
use crate::error::TorclError;

/// r3–r10 carry the first eight doublewords of the parameter area.
const INTEGER_REGISTERS: usize = 8;
/// f1–f13 carry floating-point arguments.
const FLOAT_REGISTERS: usize = 13;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Scalar {
    Void,
    Integer { bits: u8, signed: bool },
    Float,
    Double,
}

impl Scalar {
    fn from_type(ty: &AlienType) -> Result<Self, TorclError> {
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
            _ => Err(TorclError::FfiError(format!(
                "ELFv2 scalar calls do not support {ty:?}"
            ))),
        }
    }

    fn is_floating(self) -> bool {
        matches!(self, Self::Float | Self::Double)
    }
}

/// Where one argument goes. A float can need two places at once: its FPR, and —
/// for a variadic callee, which reads trailing arguments out of the save area —
/// the GPR for its position as well.
#[derive(Clone, Copy)]
struct Placement {
    integer: Option<usize>,
    float: Option<usize>,
    stack: Option<usize>,
}

/// ELFv2 placement (Power ISA ELF V2 ABI §2.2.3).
///
/// `also_in_save_area` is set for the variadic tail: a variadic callee reads those
/// arguments from the parameter save area, so a float must be written to its GPR or
/// stack slot in addition to its FPR.
fn classify(
    arguments: &[Scalar],
    fixed_count: Option<usize>,
) -> Result<Vec<Placement>, TorclError> {
    let mut floats = 0usize;
    let mut placements = Vec::with_capacity(arguments.len());
    for (position, scalar) in arguments.iter().enumerate() {
        if matches!(scalar, Scalar::Void) {
            return Err(TorclError::FfiError("void is not an argument type".into()));
        }
        // The save-area doubleword is the argument's position, whatever its type:
        // this is the whole difference from AAPCS64.
        let in_register = position < INTEGER_REGISTERS;
        let variadic_tail = fixed_count.is_some_and(|fixed| position >= fixed);
        let mut placement = Placement {
            integer: None,
            float: None,
            stack: None,
        };
        if scalar.is_floating() {
            if floats < FLOAT_REGISTERS {
                placement.float = Some(floats);
                floats += 1;
            }
            // Its position's slot is consumed either way; a variadic callee will
            // read the value from there, so fill it as well as the FPR.
            if placement.float.is_none() || variadic_tail {
                if in_register {
                    placement.integer = Some(position);
                } else {
                    placement.stack = Some(position - INTEGER_REGISTERS);
                }
            }
        } else if in_register {
            placement.integer = Some(position);
        } else {
            placement.stack = Some(position - INTEGER_REGISTERS);
        }
        placements.push(placement);
    }
    Ok(placements)
}

/// Narrow an integer result to its declared width.
///
/// ZERO-extends, including for signed types, matching the x86-64 adapter and
/// AArch64 path: `unmarshal_from_c` masks to the declared width and does the sign
/// extension itself, so sign-extending here would make the backends disagree about
/// what a raw result means even though the final value would be the same.
fn narrow_result(scalar: Scalar, raw: u64) -> u64 {
    match scalar {
        Scalar::Integer { bits: 64, .. } | Scalar::Void => raw,
        Scalar::Integer { bits, .. } => raw & ((1u64 << bits) - 1),
        Scalar::Float | Scalar::Double => raw,
    }
}

/// Call a foreign function with the ELFv2 scalar ABI.
///
/// # Safety
/// `fn_ptr` must point to a function with exactly this signature, and `args` must
/// hold one raw value per argument type.
#[cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]
pub unsafe fn ffi_call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
) -> Result<u64, TorclError> {
    // SAFETY: forwarded unchanged; a fixed call is the variadic form with no tail.
    unsafe { call(fn_ptr, ret_type, arg_types, args, None) }
}

/// Variadic calls, after the C default argument promotions.
///
/// # Safety
/// As [`ffi_call`], and the callee must consume the trailing arguments using their
/// promoted types.
#[cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]
pub unsafe fn ffi_call_variadic(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: usize,
) -> Result<u64, TorclError> {
    if fixed_count > arg_types.len() {
        return Err(TorclError::FfiError(
            "fixed argument count exceeds the argument list".into(),
        ));
    }
    // SAFETY: forwarded unchanged.
    unsafe { call(fn_ptr, ret_type, arg_types, args, Some(fixed_count)) }
}

/// # Safety
/// As [`ffi_call`].
#[cfg(all(target_arch = "powerpc64", target_endian = "little", unix))]
unsafe fn call(
    fn_ptr: *const (),
    ret_type: &AlienType,
    arg_types: &[AlienType],
    args: &[u64],
    fixed_count: Option<usize>,
) -> Result<u64, TorclError> {
    if fn_ptr.is_null() {
        return Err(TorclError::FfiError("null function pointer".into()));
    }
    if arg_types.len() != args.len() {
        return Err(TorclError::FfiError(
            "foreign argument count does not match signature".into(),
        ));
    }
    let result = Scalar::from_type(ret_type)?;
    let scalars = arg_types
        .iter()
        .map(Scalar::from_type)
        .collect::<Result<Vec<_>, _>>()?;
    let placements = classify(&scalars, fixed_count)?;

    let mut integers = [0u64; INTEGER_REGISTERS];
    let mut floats = [0u64; FLOAT_REGISTERS];
    let mut stack: Vec<u64> = Vec::new();
    for (position, (placement, raw)) in placements.iter().zip(args).enumerate() {
        if let Some(index) = placement.integer {
            integers[index] = *raw;
        }
        if let Some(index) = placement.float {
            // POWER floating-point registers hold doubles, and the ABI passes a
            // single-precision argument as its value CONVERTED to double — the
            // adapter loads with `lfd`, and a callee reading a float finds the
            // converted value. This is the mirror image of AAPCS64, where `S<n>`
            // holds the raw f32 pattern and converting is what breaks it.
            floats[index] = match scalars[position] {
                Scalar::Float => f64::from(f32::from_bits(*raw as u32)).to_bits(),
                _ => *raw,
            };
        }
        if let Some(index) = placement.stack {
            if stack.len() <= index {
                stack.resize(index + 1, 0);
            }
            stack[index] = *raw;
        }
    }

    let entry = adapter(stack.len())?;
    // Publish Native state so a collection can proceed while foreign code runs,
    // exactly as the other paths do.
    let state_guard = crate::safepoint::ForeignStateScope::native();
    // SAFETY: the trampoline reads a fixed count from each register buffer and
    // `stack.len()` from the stack buffer, all initialised above. The caller
    // vouches for the target's signature.
    let raw = unsafe {
        // One transmute per return class, so Rust reads the result from the
        // register the ABI actually used: r3, f1 as a double, or f1 as a single.
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
    Ok(narrow_result(result, raw))
}

/// The trampoline is GENERATED, not written as a naked function.
///
/// Rust's inline assembly is not stable for powerpc64 — an attempt is rejected
/// outright with "inline assembly is not stable yet on this architecture" — and the
/// toolchain here is pinned to stable. So this path does what the x86-64 one does
/// and emits its adapter at runtime, through the same assembler the JIT tiers use.
/// That assembler is execution-tested independently, which is the only reason this
/// is a reasonable thing to rely on.
///
/// One adapter per stack-argument count rather than per signature: the register
/// loads are identical for every signature, and only the number of overflow words
/// copied into the parameter save area varies. Zero is much the commonest case and
/// costs no copies at all.
const MAX_STACK_WORDS: usize = 64;

/// Frame: the ELFv2 header, the eight-doubleword register save area a callee may
/// write its register arguments into, and room for the overflow words. Fixed rather
/// than computed at runtime so the adapter needs no dynamic stack arithmetic.
const TRAMPOLINE_FRAME: i32 = 32 + 8 * 8 + 8 * MAX_STACK_WORDS as i32;

static ADAPTERS: std::sync::OnceLock<
    std::sync::Mutex<std::collections::HashMap<usize, &'static crate::jit::JitBuffer>>,
> = std::sync::OnceLock::new();

/// Build the adapter for `stack_words` overflow arguments.
///
/// Entry, per ELFv2: r3 = target, r4 = integer buffer, r5 = float buffer,
/// r6 = overflow buffer, r7 = unused (the count is baked in).
fn build_adapter(stack_words: usize) -> Option<crate::jit::JitBuffer> {
    use crate::asm_ppc64le::Asm;
    let mut asm = Asm::new();
    // Save the link register and TOC in the caller's frame, then claim ours.
    asm.move_from_link(0);
    asm.store(0, 1, 16)?;
    asm.store(2, 1, 24)?;
    asm.store_update(1, 1, -TRAMPOLINE_FRAME)?;
    // Copy the overflow arguments into the parameter save area. That area holds a
    // doubleword for EVERY argument position, not only the overflow ones, so the
    // first overflow argument is position 8 and belongs at 32 + 8*8 — not at 32.
    // Unrolled, because the count is known when this is built.
    for index in 0..stack_words {
        let offset = 8 * index as i32;
        asm.load(11, 6, offset)?;
        asm.store(11, 1, 32 + 8 * INTEGER_REGISTERS as i32 + offset)?;
    }
    // Floating-point arguments, f1–f13.
    for index in 0..FLOAT_REGISTERS {
        asm.load_float(1 + index as u8, 5, 8 * index as i32)?;
    }
    // The target goes in r12 so a callee entered at its global entry point can
    // compute its own TOC.
    asm.mov(12, 3);
    asm.move_to_count(12);
    // Integer arguments last, because r3–r7 held this adapter's own arguments until
    // now. r4 IS the buffer pointer, so its own value is loaded after every other
    // register: loading it earlier destroys the base the rest are read from.
    for index in 0..INTEGER_REGISTERS {
        let register = 3 + index as u8;
        if register == 4 {
            continue;
        }
        asm.load(register, 4, 8 * index as i32)?;
    }
    asm.load(4, 4, 8)?;
    asm.call_count();
    // Unwind through the back chain, so this does not depend on the frame size.
    asm.load(1, 1, 0)?;
    asm.load(2, 1, 24)?;
    asm.load(0, 1, 16)?;
    asm.move_to_link(0);
    asm.ret();
    crate::jit::JitBuffer::new(&asm.finish()?)
}

/// The adapter's entry point for `stack_words`, built once and retained.
fn adapter(stack_words: usize) -> Result<*const u8, TorclError> {
    if stack_words > MAX_STACK_WORDS {
        return Err(TorclError::FfiError(format!(
            "foreign signature needs {stack_words} stack words, more than the {MAX_STACK_WORDS} this adapter reserves"
        )));
    }
    let mut cache = ADAPTERS
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| TorclError::FfiError("foreign adapter cache is poisoned".into()))?;
    if let Some(buffer) = cache.get(&stack_words) {
        return Ok(buffer.as_ptr());
    }
    let buffer = build_adapter(stack_words)
        .ok_or_else(|| TorclError::FfiError("cannot build a foreign call adapter".into()))?;
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

    /// The property that distinguishes ELFv2 from AAPCS64, and the one an
    /// independent-counter implementation would get wrong: a float consumes its
    /// position's GPR slot, so the integer after it lands two registers along.
    #[test]
    fn a_float_consumes_its_positions_integer_register() {
        let arguments = [integer(32), AlienType::Double, integer(32)];
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        let placements = classify(&scalars, None).unwrap();
        assert_eq!(placements[0].integer, Some(0), "first int in r3");
        assert_eq!(placements[1].float, Some(0), "the double in f1");
        assert_eq!(
            placements[1].integer, None,
            "a fixed float does not also fill its GPR"
        );
        assert_eq!(
            placements[2].integer,
            Some(2),
            "the second int is in r5, not r4: the double consumed position 1"
        );
    }

    /// Floats keep filling FPRs in order while integers keep taking their own
    /// positions, so the two counters diverge as soon as any float appears.
    #[test]
    fn floats_fill_their_own_registers_in_order() {
        let arguments = [
            AlienType::Double,
            integer(64),
            AlienType::Double,
            integer(64),
        ];
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        let placements = classify(&scalars, None).unwrap();
        assert_eq!(placements[0].float, Some(0));
        assert_eq!(placements[1].integer, Some(1));
        assert_eq!(placements[2].float, Some(1), "second float in f2");
        assert_eq!(placements[3].integer, Some(3), "and this int in r6");
    }

    /// A variadic callee reads its trailing arguments from the parameter save
    /// area, so a float there must be written to its GPR as well as its FPR.
    #[test]
    fn a_variadic_float_is_placed_twice() {
        let arguments = [integer(32), AlienType::Double];
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        let fixed = classify(&scalars, None).unwrap();
        assert_eq!(fixed[1].integer, None, "a fixed float needs only its FPR");

        let variadic = classify(&scalars, Some(1)).unwrap();
        assert_eq!(variadic[1].float, Some(0));
        assert_eq!(
            variadic[1].integer,
            Some(1),
            "a variadic float also fills its save-area slot"
        );
    }

    #[test]
    fn arguments_beyond_the_registers_go_to_the_save_area() {
        let arguments: Vec<_> = (0..10).map(|_| integer(64)).collect();
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        let placements = classify(&scalars, None).unwrap();
        for position in 0..8 {
            assert_eq!(placements[position].integer, Some(position));
            assert_eq!(placements[position].stack, None);
        }
        assert_eq!(placements[8].integer, None);
        assert_eq!(placements[8].stack, Some(0));
        assert_eq!(placements[9].stack, Some(1));
    }

    #[test]
    fn void_is_refused_as_an_argument() {
        assert!(classify(&[Scalar::Void], None).is_err());
    }

    #[test]
    fn narrow_results_are_zero_extended() {
        // Matching x86-64 and AArch64: unmarshal_from_c does the sign extension.
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
}
