// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native inbound scalar adapters for s390x: the entry C code calls when it has
//! been handed a Lisp closure as a function pointer.
//!
//! Same contract as the x86-64 module (`ffi/callback.rs`), so `managed_callback`
//! compiles unchanged on both: preserve the C ABI, convert the arguments to raw
//! u64 slots, hand them to a dispatcher together with a stable context, and put
//! the dispatcher's raw result where the C caller expects it. Closure rooting,
//! runtime transitions and error containment belong to the dispatcher, not to
//! the generated machine-code boundary.
//!
//! Placement is the outbound classification from `ffi/s390x.rs` read in reverse:
//! integers arrive in r2–r6, floating point in f0/f2/f4/f6, the rest at
//! 160(r15) of the CALLER's frame. A single sits in the high half of its FPR and
//! in the low-order half of its stack doubleword; the dispatcher wants the raw
//! f32 bits in the low 32 bits of the slot, which is also where it returns them
//! for a single result.

use super::{
    AlienType,
    s390x::{Placement, Scalar, classify},
};
use crate::{EgclError, asm_s390x::Asm, jit::JitBuffer};

/// A dispatcher receives the stable opaque context and one raw u64 slot per
/// declared argument. It returns raw result bits and MUST NOT unwind into C.
pub type Dispatcher = unsafe extern "C" fn(*mut (), *const u64) -> u64;

/// The caller's register save area; its outgoing stack arguments start above it,
/// and so do this adapter's slots in its own frame.
const SAVE_AREA: i32 = 160;

/// Owns one distinct W^X C-entry mapping. Foreign users must retire the entry
/// before dropping this owner; GC never controls this native lifetime.
pub struct CallbackAdapter {
    code: JitBuffer,
}

impl CallbackAdapter {
    pub fn new(
        result: &AlienType,
        arguments: &[AlienType],
        context: *mut (),
        dispatcher: Dispatcher,
    ) -> Result<Self, EgclError> {
        let result = Scalar::from_type(result)?;
        let arguments = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<Vec<_>, _>>()?;
        let placements = classify(&arguments)?;
        let code = emit(result, &arguments, &placements, context, dispatcher)
            .and_then(|code| JitBuffer::new(&code))
            .ok_or_else(|| EgclError::FfiError("cannot allocate callback adapter".into()))?;
        Ok(Self { code })
    }

    /// The address has the declared foreign signature, not `Dispatcher`'s ABI.
    /// Calling it requires a live adapter and context, matching signature, and
    /// a dispatcher that obeys its own memory and no-unwind contracts.
    pub fn as_fn_ptr(&self) -> *const () {
        self.code.as_ptr().cast()
    }
}

/// The adapter's instruction stream. Separate from the mapping so a test can
/// pin it byte for byte.
fn emit(
    result: Scalar,
    arguments: &[Scalar],
    placements: &[Placement],
    context: *mut (),
    dispatcher: Dispatcher,
) -> Option<Vec<u8>> {
    let slot_bytes = 8 * i32::try_from(arguments.len()).ok()?;
    let mut asm = Asm::new();
    // Save r6–r15 and claim a frame: the dispatcher's save area, then one
    // doubleword per argument. The incoming stack arguments are then found
    // above both this frame and the caller's own save area.
    asm.prologue_with(slot_bytes);
    let incoming = SAVE_AREA + slot_bytes + SAVE_AREA;
    for (index, (placement, scalar)) in placements.iter().zip(arguments).enumerate() {
        // r1 is the only scratch register until every argument register has
        // been captured; r2–r6 still hold arguments.
        match *placement {
            Placement::Integer(register) => asm.mov(1, 2 + register as u8),
            Placement::Float(register) => {
                asm.store_float_bits(1, 2 * register as u8);
                if matches!(scalar, Scalar::Float) {
                    // The single is in the high half of the FPR.
                    asm.shift_right_logical(1, 1, 32);
                }
            }
            Placement::Stack(word) => asm.load(1, 15, incoming + 8 * word as i32),
        }
        // The ABI has the caller extend narrow integers, but the high bits of a
        // stack single's doubleword are whatever the caller left there.
        // Normalize every slot to its declared width, as x86-64 does; Lisp
        // signedness is applied by the dispatcher.
        match scalar {
            Scalar::Integer {
                bits: bits @ (8 | 16 | 32),
                ..
            } => asm.zero_extend(1, 1, *bits),
            Scalar::Float => asm.zero_extend(1, 1, 32),
            _ => {}
        }
        asm.store(1, 15, SAVE_AREA + 8 * index as i32);
    }
    // The dispatcher uses the platform C ABI too: r2 = context, r3 = slots.
    asm.imm64(2, context as usize as u64);
    asm.address(3, 15, SAVE_AREA);
    asm.imm64(1, dispatcher as usize as u64);
    asm.call_reg(1);
    // Put the raw result where the C caller reads this signature's result.
    match result {
        Scalar::Float => {
            // The f32 bits are in the low half of r2; f0 wants them high.
            asm.shift_left(2, 2, 32);
            asm.load_float_bits(0, 2);
        }
        Scalar::Double => asm.load_float_bits(0, 2),
        Scalar::Integer {
            bits: bits @ (8 | 16 | 32),
            signed: true,
        } => asm.sign_extend(2, 2, bits),
        Scalar::Integer {
            bits: bits @ (8 | 16 | 32),
            signed: false,
        } => asm.zero_extend(2, 2, bits),
        Scalar::Void => asm.imm64(2, 0),
        Scalar::Integer { .. } => {}
    }
    asm.epilogue_with(slot_bytes);
    asm.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn dispatcher(_: *mut (), _: *const u64) -> u64 {
        0
    }

    /// `float f(long, float, long x4, long, float x3, float)`: every placement
    /// occurs — five longs in r2–r6 and a sixth on the stack, four singles in
    /// f0–f6 and a fifth on the stack. Pinned byte for byte so a change to the
    /// capture sequence is deliberate; the expected stream is written with the
    /// assembler's own helpers, each pinned against llvm-mc.
    #[test]
    fn the_capture_sequence_reads_every_placement_from_its_abi_location() {
        let long = AlienType::Int {
            bits: 64,
            signed: true,
        };
        let f = AlienType::Float;
        let arguments = [
            long.clone(),
            f.clone(),
            long.clone(),
            long.clone(),
            long.clone(),
            long.clone(),
            long.clone(),
            f.clone(),
            f.clone(),
            f.clone(),
            f.clone(),
        ];
        let scalars: Vec<_> = arguments
            .iter()
            .map(Scalar::from_type)
            .collect::<Result<_, _>>()
            .unwrap();
        let placements = classify(&scalars).unwrap();
        let context = 0x1234_5678usize as *mut ();
        let actual = emit(Scalar::Float, &scalars, &placements, context, dispatcher).unwrap();

        let slot_bytes = 8 * 11;
        let incoming = 160 + slot_bytes + 160;
        let mut a = Asm::new();
        a.prologue_with(slot_bytes);
        let capture_long = |a: &mut Asm, register: u8, slot: i32| {
            a.mov(1, register);
            a.store(1, 15, slot);
        };
        let capture_single = |a: &mut Asm, fpr: u8, slot: i32| {
            a.store_float_bits(1, fpr);
            a.shift_right_logical(1, 1, 32);
            a.zero_extend(1, 1, 32);
            a.store(1, 15, slot);
        };
        capture_long(&mut a, 2, 160);
        capture_single(&mut a, 0, 168);
        capture_long(&mut a, 3, 176);
        capture_long(&mut a, 4, 184);
        capture_long(&mut a, 5, 192);
        capture_long(&mut a, 6, 200);
        // The sixth long: first incoming stack word.
        a.load(1, 15, incoming);
        a.store(1, 15, 208);
        capture_single(&mut a, 2, 216);
        capture_single(&mut a, 4, 224);
        capture_single(&mut a, 6, 232);
        // The fifth single: second incoming stack word, low half.
        a.load(1, 15, incoming + 8);
        a.zero_extend(1, 1, 32);
        a.store(1, 15, 240);
        a.imm64(2, 0x1234_5678);
        a.address(3, 15, 160);
        a.imm64(1, dispatcher as Dispatcher as usize as u64);
        a.call_reg(1);
        a.shift_left(2, 2, 32);
        a.load_float_bits(0, 2);
        a.epilogue_with(slot_bytes);
        assert_eq!(actual, a.finish().unwrap());
    }

    #[test]
    fn void_and_aggregate_signatures_are_refused() {
        assert!(
            CallbackAdapter::new(
                &AlienType::Void,
                &[AlienType::Void],
                std::ptr::null_mut(),
                dispatcher
            )
            .is_err()
        );
        assert!(
            CallbackAdapter::new(
                &AlienType::Struct {
                    fields: vec![AlienType::Float],
                    packed: false
                },
                &[],
                std::ptr::null_mut(),
                dispatcher
            )
            .is_err()
        );
    }
}
