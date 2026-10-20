// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64 machine stubs for the native transfer ABI.
//!
//! The x86-64 stubs in native_transfer.rs define the protocol; read that
//! module's header first. In one paragraph: under the segment ABI a
//! successful native-to-native call returns its value directly, with no
//! pending-error test afterwards. An escaping error or non-local exit instead
//! leaves the Rust helper through a *cold route* -- the helper returns
//! normally with a [`NativeOutcome`] whose `exit` is `Transfer` or `Deopt`, a
//! veneer tests that one word and tail-jumps to a capture stub, the stub
//! snapshots the still-live caller frame and calls Rust preparation, and a
//! landing stub re-enters retained native code at the pad preparation chose.
//! No Rust or foreign frame is ever jumped over.
//!
//! AArch64 gets its own emitter rather than a target test around x86 byte
//! encodings, for the reason ppc64le does: a different callee-saved set, a
//! link register instead of a pushed return address, and a different
//! branch-target marker. Keeping them apart also makes accidental
//! installation of x86 bytes on AArch64 impossible.
//!
//! # What this file owns
//!
//! * [`Aapcs64TransferCapture`] -- the capture image, and
//!   [`Aapcs64CaptureLocation`], the checked recipe for reading or writing one
//!   value's home through it.
//! * [`emit_helper_veneer`] -- the callable adapter `(request) -> primary` a
//!   generated caller invokes instead of the raw helper.
//! * [`emit_capture_stub`] -- the cold entry, which writes the capture image,
//!   calls preparation, reloads the possibly-updated image and tail-dispatches.
//! * [`emit_native_landing_stub`] -- the tail adapter from
//!   `(landing_packet, primary)` to a landing pad.
//!
//! # Two ways AArch64 differs from SysV, both easy to get silently wrong
//!
//! **There is no return address on the stack.** `BL` writes the return
//! address to x30. So the SP captured at the cold entry *is already* the
//! caller's SP at the call: where the x86 stub computes `caller_sp` as
//! `rsp + frame + 8` to step over the pushed return address, this one adds
//! nothing, and `return_pc` comes from the saved x30 rather than from memory.
//! Carrying the x86 `+8` across would make every stack-homed capture recipe
//! read one slot off -- which is a plausible-looking wrong value, not a crash.
//!
//! **The marker is `BTI c`, not `ENDBR64`,** and it is a hint instruction, so
//! it costs nothing where branch-target identification is not enabled.
//!
//! # What this file does NOT own, and why the capture has no FP registers
//!
//! AAPCS64 preserves only the LOW 64 bits of v8-v15, so `allocate_framed_a64`
//! deliberately excludes that whole range from its register pool
//! (regalloc.rs, the AArch64 pool is x19-x27 plus v16-v30). No value can
//! therefore have an exception-live home in a callee-saved FP register, and
//! the capture image has no place to put one. This matches the x86 contract,
//! where every XMM register is caller-saved and the same conclusion follows:
//! a value that must survive a throwing call needs a call-preserved home, and
//! on this backend those are exactly x19-x28.
//!
//! # Contract
//!
//! Caller-saved registers are clobbered by the time the capture runs. The
//! capture image is stack-resident and temporary: it registers no GC roots
//! itself, so preparation must publish the payload and every mapped root
//! before allocating or yielding, must update native homes for anything the
//! collector moved, and must not retain the image by address. The source
//! frame stays live until the dispatcher retires it. Lisp errors inside
//! helpers and preparation are reported through the outcome, never by panic.
//!
//! These stubs are machine interfaces, not admission checks. The installer
//! must retain both branch targets, validate every physical capture recipe
//! against the emitted frame, gate on the platform's segment and
//! control-flow-hardening support, and publish complete call-site coverage
//! before any of this code runs.

use egcl_rt::asm::a64::{self, LR, SP};
use egcl_rt::asm::{Asm, Cc};
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

/// Scratch for an address or a loaded word. IP0/IP1 are reserved by the
/// architecture for exactly this, so no value can be live in them across a
/// call, which is what makes them safe to use in a tail-jumping stub.
const SCRATCH: a64::Reg = 16;
const SCRATCH2: a64::Reg = 17;

/// Hardware x19..x28, the AAPCS64 callee-saved GPRs, in that order. This is
/// the order [`Aapcs64TransferCapture::preserved`] stores them in, and the
/// index [`Aapcs64CaptureLocation`] resolves a register home to.
const PRESERVED: [a64::Reg; 10] = [19, 20, 21, 22, 23, 24, 25, 26, 27, 28];

/// Version-two helper ABI shared with the runtime: `(request, outcome*)`. The
/// helper must initialize both outcome fields, return normally (including on
/// Lisp errors), and root request/payload references across allocation. It
/// must not jump across its Rust frame. The request layout belongs to the
/// helper, and must occupy stable, nonmoving storage that stays alive until
/// either the normal return or the cold route finishes consuming it.
pub type NativeHelperV2 = unsafe extern "C" fn(*mut u8, *mut NativeOutcome);

/// Preparation must publish the payload and mapped register/stack roots before
/// allocating or yielding, and return normally. It must update all native
/// homes needed by subsequent native cleanup/targets after relocation. The
/// capture image is temporary and must not be retained by address after
/// dispatch. Lisp errors use an outcome, not a panic.
pub type NativeTransferPrepare = unsafe extern "C" fn(*mut Aapcs64TransferCapture);

/// Stack-resident image captured after an AAPCS64 helper returns.
///
/// Caller-saved registers are already clobbered: exception-live values must
/// have call-preserved homes. This image does not itself register GC roots.
#[repr(C)]
pub struct Aapcs64TransferCapture {
    pub request: *mut u8,
    pub value: egcl_rt::value::EgclVal,
    pub exit: NativeExit,
    /// Hardware x19..x28, in that order. Preparation may update these words
    /// after GC; the stub reloads them before dispatch.
    pub preserved: [u64; 10],
    /// Caller SP at the call. AArch64 pushes no return address, so this is
    /// the SP the cold entry observed, with no adjustment (see the header).
    pub caller_sp: *const u64,
    /// The caller's x30 at the call.
    pub return_pc: *const u8,
}

/// Execution-owned packet consumed by the landing adapter after preparation
/// returns. A machine interface, not an admission check: the dispatcher must
/// validate both fields against retained code and frame maps.
#[repr(C)]
pub struct Aapcs64NativeLanding {
    /// The landing frame's normal body SP. The frame must still be live in the
    /// current segment; no Rust or foreign frame may be crossed.
    pub stack_pointer: *mut u64,
    /// A `BTI c` landing pad in retained native code. Its live-value homes
    /// must already have been populated, including writes required by a
    /// moving collection.
    pub entry: *const u8,
}

/// Load a 64-bit absolute address into `register`.
fn load_address(a: &mut Asm, register: a64::Reg, address: u64) {
    let mut words = Vec::new();
    a64::mov_imm64(register, address, &mut words);
    a.words(&words);
}

/// Emit a callable AAPCS64 adapter around a Rust helper.
///
/// `(request in x0) -> primary in x0`. The adapter owns only its own 48-byte
/// frame. A `Returned` outcome returns the primary normally; anything else
/// removes the frame -- restoring the caller's x30 and SP, so the caller's
/// frame and return address are untouched -- and tail-jumps to `cold_entry`
/// with `(request, value, exit)` in x0-x2. No Rust frame is crossed by the
/// branch: the helper has already returned.
///
/// Only the `exit` word is ever tested. The Lisp value is never inspected,
/// because any bit pattern is a legal tagged value.
pub fn emit_helper_veneer(helper: NativeHelperV2, cold_entry: *const u8) -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<NativeOutcome>() == 16);
        assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
        assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
        assert!(NativeExit::Returned as u64 == 0);
    }
    // 16 bytes of frame record, 16 for the NativeOutcome, 8 for the saved
    // request, rounded to the 16-byte stack alignment AAPCS64 requires at a
    // call.
    const FRAME: i32 = 48;
    const OUTCOME: u64 = 16;
    const REQUEST: u64 = 32;

    let mut a = Asm::new();
    let exceptional = a.label();
    a.word(a64::bti_c());
    a.word(a64::stp_pre(29, LR, SP, -FRAME).expect("48 is a pair offset"));
    a.word(a64::mov_from_sp(29));
    a.word(a64::str_imm(0, SP, REQUEST).expect("32 is a scaled offset"));
    a.word(a64::add_imm(1, SP, OUTCOME).expect("16 is an add immediate"));
    load_address(&mut a, SCRATCH, helper as usize as u64);
    a.word(a64::blr(SCRATCH));
    a.word(a64::ldr_imm(2, SP, OUTCOME).expect("16 is a scaled offset"));
    a.word(a64::ldr_imm(3, SP, OUTCOME + 8).expect("24 is a scaled offset"));
    a.word(a64::ldr_imm(0, SP, REQUEST).expect("32 is a scaled offset"));
    a.word(a64::ldp_post(29, LR, SP, FRAME).expect("48 is a pair offset"));
    // Test the exit word, never the Lisp value.
    a.word(a64::cmp_imm(3, 0).expect("0 is an add immediate"));
    a.jcc(Cc::Ne, exceptional);
    a.word(a64::mov(0, 2));
    a.word(a64::ret());
    a.bind(exceptional);
    // (request, value, exit) in x0-x2. x0 already holds the request; move
    // value before exit, or the exit would overwrite the value's source.
    a.word(a64::mov(1, 2));
    a.word(a64::mov(2, 3));
    load_address(&mut a, SCRATCH, cold_entry as usize as u64);
    a.word(a64::br(SCRATCH));
    a.finish().expect("local helper veneer label")
}

/// Emit the cold entry for a helper veneer.
///
/// Entered with `(request, value, exit)` in x0-x2, with x30 and SP exactly as
/// the original caller left them at its call. Captures that caller, invokes
/// Rust preparation, then reloads the updated image and tail-dispatches with
/// the same three-register convention. Only this stub's own frame is removed;
/// the caller and its return address remain available to the unwinder.
pub fn emit_capture_stub(prepare: NativeTransferPrepare, dispatch: *const u8) -> Vec<u8> {
    use std::mem::{offset_of, size_of};
    const {
        assert!(size_of::<Aapcs64TransferCapture>() == 120);
        assert!(offset_of!(Aapcs64TransferCapture, request) == 0);
        assert!(offset_of!(Aapcs64TransferCapture, value) == 8);
        assert!(offset_of!(Aapcs64TransferCapture, exit) == 16);
    }
    // The image rounded up to the 16-byte stack alignment a call requires.
    const IMAGE: i32 = ((size_of::<Aapcs64TransferCapture>() as i32) + 15) & !15;
    let request = offset_of!(Aapcs64TransferCapture, request) as u64;
    let value = offset_of!(Aapcs64TransferCapture, value) as u64;
    let exit = offset_of!(Aapcs64TransferCapture, exit) as u64;
    let preserved = offset_of!(Aapcs64TransferCapture, preserved) as u64;
    let caller_sp = offset_of!(Aapcs64TransferCapture, caller_sp) as u64;
    let return_pc = offset_of!(Aapcs64TransferCapture, return_pc) as u64;

    let mut a = Asm::new();
    a.word(a64::bti_c());
    a.word(a64::stp_pre(29, LR, SP, -16).expect("16 is a pair offset"));
    a.word(a64::mov_from_sp(29));
    a.word(a64::sub_imm(SP, SP, IMAGE as u64).expect("the image fits an add immediate"));
    for (register, offset) in [(0, request), (1, value), (2, exit)] {
        a.word(a64::str_imm(register, SP, offset).expect("a header offset is scaled"));
    }
    for (index, register) in PRESERVED.into_iter().enumerate() {
        let offset = preserved + index as u64 * 8;
        a.word(a64::str_imm(register, SP, offset).expect("a preserved offset is scaled"));
    }
    // The SP this stub was entered with IS the caller's SP at the call: no
    // return address was pushed. It sits one frame record above our x29.
    a.word(a64::add_imm(SCRATCH2, 29, 16).expect("16 is an add immediate"));
    a.word(a64::str_imm(SCRATCH2, SP, caller_sp).expect("a header offset is scaled"));
    // The caller's x30, which BL left as the return address, saved by our stp.
    a.word(a64::ldr_imm(SCRATCH2, 29, 8).expect("8 is a scaled offset"));
    a.word(a64::str_imm(SCRATCH2, SP, return_pc).expect("a header offset is scaled"));
    a.word(a64::mov_from_sp(0));
    load_address(&mut a, SCRATCH, prepare as usize as u64);
    // Every Rust frame returns before the dispatch branch below.
    a.word(a64::blr(SCRATCH));
    for (index, register) in PRESERVED.into_iter().enumerate() {
        let offset = preserved + index as u64 * 8;
        a.word(a64::ldr_imm(register, SP, offset).expect("a preserved offset is scaled"));
    }
    for (register, offset) in [(0, request), (1, value), (2, exit)] {
        a.word(a64::ldr_imm(register, SP, offset).expect("a header offset is scaled"));
    }
    a.word(a64::add_imm(SP, SP, IMAGE as u64).expect("the image fits an add immediate"));
    a.word(a64::ldp_post(29, LR, SP, 16).expect("16 is a pair offset"));
    load_address(&mut a, SCRATCH, dispatch as usize as u64);
    a.word(a64::br(SCRATCH));
    a.finish().expect("capture stub has no unresolved labels")
}

/// Tail adapter from `(landing_packet in x0, primary in x1)` to a landing pad.
///
/// Pair with [`emit_capture_stub`]: preparation returns normally and that stub
/// restores the updated callee-saved registers before entering this adapter.
/// The source frame remains live; the selected pad owns its retirement.
///
/// The packet must occupy stable storage until dispatch, independently of the
/// temporary capture frame. Publish all pending transfer and cleanup roots
/// before selecting this route. This adapter neither searches handlers nor
/// establishes roots, validates targets, or supplies unwind metadata; the
/// installer must enforce those contracts and the segment capability gate.
/// A transfer cursor may ignore the primary on cleanup entry.
pub fn emit_native_landing_stub() -> Vec<u8> {
    use std::mem::{offset_of, size_of};
    const {
        assert!(size_of::<Aapcs64NativeLanding>() == 16);
        assert!(offset_of!(Aapcs64NativeLanding, stack_pointer) == 0);
        assert!(offset_of!(Aapcs64NativeLanding, entry) == 8);
    }
    let mut a = Asm::new();
    a.word(a64::bti_c());
    // Read both packet fields before x0 is overwritten with the primary.
    a.word(a64::ldr_imm(SCRATCH, 0, 8).expect("8 is a scaled offset"));
    a.word(a64::ldr_imm(SCRATCH2, 0, 0).expect("0 is a scaled offset"));
    a.word(a64::mov(0, 1));
    a.word(a64::mov_to_sp(SCRATCH2));
    a.word(a64::br(SCRATCH));
    a.finish().expect("landing stub has no unresolved labels")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureHome {
    Preserved(u8),
    StackOffset(u32),
}

/// A checked physical access recipe. Construct it from the emitter's final
/// home, not a transient allocator location. No lookup or allocation is needed
/// to read or write the word once the recipe has been selected for a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aapcs64CaptureLocation(CaptureHome);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureLocationError {
    UnavailableRegister(u8),
    StackSlotOutsideFrame(u32),
    UnalignedStackAdjustment,
    OffsetOverflow,
}

impl Aapcs64CaptureLocation {
    /// Resolve one value's home to a recipe against the capture image.
    ///
    /// `register` must be one of x19-x28; anything else is caller-saved and
    /// cannot hold an exception-live value. `call_stack_adjust` is the
    /// temporary stack space below the body's normal SP at this call, included
    /// so a spill is addressed from the captured SP.
    pub fn for_register(register: u8) -> Result<Self, CaptureLocationError> {
        let index = PRESERVED
            .iter()
            .position(|r| *r == register)
            .ok_or(CaptureLocationError::UnavailableRegister(register))?;
        Ok(Self(CaptureHome::Preserved(index as u8)))
    }

    /// A frame slot home, `slot` counted in eight-byte words from the body's
    /// normal SP.
    pub fn for_stack_slot(
        slot: u32,
        stack_slots: u32,
        call_stack_adjust: u32,
    ) -> Result<Self, CaptureLocationError> {
        use CaptureLocationError::*;
        if call_stack_adjust % 8 != 0 {
            return Err(UnalignedStackAdjustment);
        }
        if slot >= stack_slots {
            return Err(StackSlotOutsideFrame(slot));
        }
        let offset = slot
            .checked_mul(8)
            .and_then(|n| n.checked_add(call_stack_adjust))
            .filter(|n| *n <= i32::MAX as u32)
            .ok_or(OffsetOverflow)?;
        Ok(Self(CaptureHome::StackOffset(offset)))
    }

    /// # Safety
    /// The capture must describe the still-live frame for which this recipe
    /// was constructed. Its declared stack slots and call adjustment must
    /// match the actual frame allocation. Do not allocate or yield while
    /// copying raw roots.
    pub unsafe fn read(self, capture: &Aapcs64TransferCapture) -> u64 {
        match self.0 {
            CaptureHome::Preserved(index) => capture.preserved[index as usize],
            CaptureHome::StackOffset(offset) => unsafe {
                capture.caller_sp.add(offset as usize / 8).read()
            },
        }
    }

    /// # Safety
    /// Same live-frame contract as [`Self::read`]; stack homes must also be
    /// writable. `word` must use the home's original machine representation:
    /// in particular do not write a reboxed Lisp float over an unboxed native
    /// float slot.
    pub unsafe fn write(self, capture: &mut Aapcs64TransferCapture, word: u64) {
        match self.0 {
            CaptureHome::Preserved(index) => capture.preserved[index as usize] = word,
            CaptureHome::StackOffset(offset) => unsafe {
                capture
                    .caller_sp
                    .cast_mut()
                    .add(offset as usize / 8)
                    .write(word)
            },
        }
    }
}
