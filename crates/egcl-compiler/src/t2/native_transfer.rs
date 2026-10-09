// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! SysV x86-64 machine stubs for the native transfer ABI.
//!
//! # The transfer ABI in one paragraph
//!
//! Under the segment ABI a successful native-to-native call returns its value
//! directly, with no pending-error test afterwards. An escaping error or
//! non-local exit instead leaves the Rust helper through a *cold route*: the
//! helper returns normally with a `NativeOutcome` whose `exit` is `Transfer`
//! or `Deopt`, a veneer tests that word and tail-jumps to a capture stub, the
//! stub snapshots the still-live caller frame and calls Rust preparation,
//! preparation consults the compiler's per-call-site maps (transfer_map.rs,
//! transfer_sites.rs) and the control-scope model (control_scope.rs,
//! native_unwind.rs) to select cleanup and landing pads, and a landing stub
//! finally re-enters retained native code at the chosen pad. No Rust or
//! foreign frame is ever jumped over; every Rust call returns before assembly
//! adjusts a generated frame.
//!
//! # What this file owns
//!
//! The x86-64 stubs and the capture image they share:
//!
//! * [`emit_helper_veneer`] — the callable adapter `(request) -> primary` a
//!   generated caller invokes instead of the raw helper. It calls a
//!   [`NativeHelperV2`] with an out-parameter [`NativeOutcome`], and only the
//!   `exit` word is tested (never the Lisp value). `Returned` returns the
//!   primary in RAX; anything else removes the veneer's 24-byte frame and
//!   tail-jumps to the cold entry with `(request, value, exit)` in
//!   RDI/RSI/RDX, leaving the caller's frame and return address intact.
//! * [`emit_legacy_cell_veneer`] — a migration adapter from mapped callers to
//!   existing CallCell slice entries. It brackets legacy fault recovery and
//!   converts the legacy pending-error state into the same cold capture route.
//! * [`emit_capture_stub`] — the cold entry. It writes a
//!   [`SysvTransferCapture`] on its own stack (request, value, exit, the six
//!   callee-saved GPRs RBX/RBP/R12–R15, the caller's RSP above the return
//!   address, and the return PC), calls a [`NativeTransferPrepare`] with a
//!   pointer to it, then reloads the possibly-updated image and tail-jumps to
//!   the dispatch address with the same three-register convention. Only the
//!   stub's temporary frame is removed.
//! * [`emit_native_landing_stub`] — the tail adapter from
//!   `(landing_packet, primary, exit)` to a landing pad: it loads RSP and the
//!   entry from a [`SysvNativeLanding`] packet, moves the primary to RAX and
//!   jumps. Every pad begins with `ENDBR64`.
//! * [`SysvCaptureLocation`] — a checked recipe for reading or writing one
//!   value's home through the capture image: a callee-saved register index
//!   into `preserved`, or a byte offset from `caller_sp` for a frame slot,
//!   including any temporary stack adjustment present at the CALL.
//!
//! # Contract
//!
//! Caller-saved GPRs and all XMM registers are clobbered by the time the
//! capture runs, so any value that must survive a throwing call needs a
//! call-preserved home; transfer-map lowering and the framed emitter's home
//! selection guarantee that. The capture image is stack-resident and
//! temporary: it registers no GC roots itself, so preparation must publish the
//! payload and every mapped root before allocating or yielding, must update
//! native homes for anything the collector moved, and must not retain the
//! image by address. The source frame stays live until the dispatcher retires
//! it. Lisp errors inside helpers and preparation are reported through the
//! outcome, never by panic.
//!
//! These stubs are machine interfaces, not admission checks. The installer
//! (the runtime's native_transfer_entry module) must retain both branch
//! targets, validate every physical capture recipe against the emitted frame,
//! gate on the platform's segment and control-flow-hardening support, and
//! publish complete call-site coverage before any of this code runs. Layout
//! assumptions (`NativeOutcome` is 16 bytes, `SysvTransferCapture` is 88,
//! `Returned == 0`) are enforced by `const` assertions.
//!
//! The ppc64le equivalents are in native_transfer_ppc64le.rs; AArch64 and
//! s390x have no transfer stubs yet and stay on the checked ABI.

mod mapped_call;
pub use mapped_call::{
    ADAPTER_FRAME_BYTES, MappedCallRecord, emit_published_call_entry, emit_published_call_veneer,
};

use egcl_rt::asm::{Asm, Cc};
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

/// Version-two helper: a request pointer plus an explicit outcome destination.
/// The helper must initialize both outcome fields, return normally (including
/// on Lisp errors), and root request/payload references across allocation. It
/// must not jump across its Rust frame. The request layout belongs to the helper.
/// The request itself must occupy stable, nonmoving storage that remains alive
/// until either the normal return or the cold route finishes consuming it.
pub type NativeHelperV2 = unsafe extern "C" fn(*mut u8, *mut NativeOutcome);

/// Stack-resident image captured after a SysV helper returns. Caller-saved
/// GPRs and all XMM registers are already clobbered: exception-live values must
/// have call-preserved homes. This image does not itself register GC roots.
#[repr(C)]
pub struct SysvTransferCapture {
    pub request: *mut u8,
    pub value: egcl_rt::value::EgclVal,
    pub exit: NativeExit,
    /// Hardware RBX, RBP, R12, R13, R14, R15, in that order. Preparation may
    /// update these words after GC; the stub reloads them before dispatch.
    pub preserved: [u64; 6],
    /// Caller RSP immediately before CALL, above the saved return address.
    pub caller_sp: *const u64,
    pub return_pc: *const u8,
}

/// Execution-owned packet consumed by the native landing adapter after Rust
/// preparation returns. This is a machine interface, not an admission check.
/// The dispatcher must validate both fields against retained code/frame maps.
#[repr(C)]
pub struct SysvNativeLanding {
    /// The landing frame's normal body RSP, after discarding the completed
    /// helper's return address and temporary request area. The frame must still
    /// be live in the current segment; no Rust/foreign frame may be crossed.
    pub stack_pointer: *mut u64,
    /// An ENDBR64 landing pad in retained native code. Its live-value homes must
    /// already have been populated, including writes required by moving GC.
    pub entry: *const u8,
}

/// Tail adapter from `(landing_packet, primary, exit)` to a native landing pad.
/// Pair with `emit_capture_stub`: preparation returns normally and that stub
/// restores the updated nonvolatile registers before entering this adapter.
/// The source frame remains live. The landing pad owns its eventual retirement.
///
/// The packet must occupy stable storage until dispatch, independently of the
/// temporary capture frame. Publish all pending transfer/cleanup roots before
/// selecting this route. This adapter neither searches handlers nor establishes
/// roots, validates targets, or supplies platform unwind metadata. The installer
/// must enforce those contracts and the segment capability gate before use.
/// Caller-saved registers are scratch; RAX receives the primary and RDI retains
/// the packet pointer. A transfer cursor may ignore the primary on cleanup entry.
pub fn emit_native_landing_stub() -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<SysvNativeLanding>() == 16);
        assert!(std::mem::offset_of!(SysvNativeLanding, stack_pointer) == 0);
        assert!(std::mem::offset_of!(SysvNativeLanding, entry) == 8);
    }
    vec![
        0xf3, 0x0f, 0x1e, 0xfa, // endbr64
        0x48, 0x89, 0xf0, // mov rax, rsi
        0x4c, 0x8b, 0x5f, 0x08, // mov r11, [rdi + 8]
        0x48, 0x8b, 0x27, // mov rsp, [rdi]
        0x41, 0xff, 0xe3, // jmp r11
    ]
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureHome {
    Preserved(u8),
    StackOffset(u32),
}

/// A checked physical access recipe. Construct it from the emitter's final
/// home, not a transient allocator location. No lookup or allocation is needed
/// to read/write the word once the recipe has been selected for a call site.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SysvCaptureLocation(CaptureHome);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureLocationError {
    UnavailableRegister(u8),
    StackSlotOutsideFrame(u32),
    UnalignedStackAdjustment,
    OffsetOverflow,
}

impl SysvCaptureLocation {
    /// `call_stack_adjust` is the temporary stack space below the body's normal
    /// RSP at this CALL. Include it so a spill is addressed from captured SP.
    pub fn for_home(
        home: crate::t2::x64_frame::ValueHome,
        stack_slots: u32,
        call_stack_adjust: u32,
    ) -> Result<Self, CaptureLocationError> {
        use crate::t2::x64_frame::ValueHome;
        use CaptureLocationError::*;
        if call_stack_adjust % 8 != 0 {
            return Err(UnalignedStackAdjustment);
        }
        Ok(Self(match home {
            ValueHome::Reg(register) => {
                let index = [3, 5, 12, 13, 14, 15]
                    .iter()
                    .position(|r| *r == register)
                    .ok_or(UnavailableRegister(register))?;
                CaptureHome::Preserved(index as u8)
            }
            ValueHome::Stack(slot) => {
                if slot >= stack_slots {
                    return Err(StackSlotOutsideFrame(slot));
                }
                let offset = slot
                    .checked_mul(8)
                    .and_then(|n| n.checked_add(call_stack_adjust))
                    .filter(|n| *n <= i32::MAX as u32)
                    .ok_or(OffsetOverflow)?;
                CaptureHome::StackOffset(offset)
            }
        }))
    }

    /// # Safety
    /// The capture must describe the still-live frame for which this recipe was
    /// constructed. Its declared stack slots and call adjustment must match the
    /// actual frame allocation. Do not allocate/yield while copying raw roots.
    pub unsafe fn read(self, capture: &SysvTransferCapture) -> u64 {
        match self.0 {
            CaptureHome::Preserved(index) => capture.preserved[index as usize],
            CaptureHome::StackOffset(offset) => unsafe {
                capture.caller_sp.add(offset as usize / 8).read()
            },
        }
    }

    /// # Safety
    /// Same live-frame contract as `read`; stack homes must also be writable.
    /// `word` must use the home's original machine representation. In particular
    /// do not write a reboxed Lisp float over an unboxed native float slot.
    pub unsafe fn write(self, capture: &mut SysvTransferCapture, word: u64) {
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

/// Preparation must publish the payload and mapped register/stack roots before
/// allocating or yielding, and return normally. It must update all native homes
/// needed by subsequent native cleanup/targets after relocation. The capture
/// image is temporary and must not be retained by address after dispatch. The
/// source frame remains live for the dispatcher, which owns its eventual
/// retirement and any longer-lived cleanup cursor. Lisp errors use an outcome,
/// not panic.
pub type NativeTransferPrepare = unsafe extern "C" fn(*mut SysvTransferCapture);

/// Emit the cold entry for a helper veneer. Capture the caller before invoking
/// Rust preparation, then reload the updated image and tail-dispatch using the
/// same `(request, value, exit)` convention. Only this stub's temporary frame is
/// removed; the caller and its return address remain available to the unwinder.
/// The installer must retain both targets, validate physical capture recipes,
/// and provide platform capability/unwind metadata before publishing this code.
pub fn emit_capture_stub(prepare: NativeTransferPrepare, dispatch: *const u8) -> Vec<u8> {
    use std::mem::{offset_of, size_of};
    const SIZE: u8 = size_of::<SysvTransferCapture>() as u8;
    const {
        assert!(size_of::<SysvTransferCapture>() == 88);
    }
    let mut a = Asm::new();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // endbr64
    a.extend_from_slice(&[0x48, 0x83, 0xec, SIZE]); // align RSP for Rust CALL
    for (reg, offset) in [
        (7, offset_of!(SysvTransferCapture, request)),
        (6, offset_of!(SysvTransferCapture, value)),
        (2, offset_of!(SysvTransferCapture, exit)),
    ] {
        capture_stack_word(&mut a, false, reg, offset);
    }
    let preserved = offset_of!(SysvTransferCapture, preserved);
    for (i, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, false, reg, preserved + i * 8);
    }
    a.extend_from_slice(&[0x48, 0x8d, 0x44, 0x24, SIZE + 8]); // caller SP
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, caller_sp));
    capture_stack_word(&mut a, true, 0, SIZE as usize); // return PC
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, return_pc));
    a.extend_from_slice(&[0x48, 0x89, 0xe7]); // rdi = capture image
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(prepare as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]); // all Rust frames return before dispatch
    for (i, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, true, reg, preserved + i * 8);
    }
    for (reg, offset) in [
        (7, offset_of!(SysvTransferCapture, request)),
        (6, offset_of!(SysvTransferCapture, value)),
        (2, offset_of!(SysvTransferCapture, exit)),
    ] {
        capture_stack_word(&mut a, true, reg, offset);
    }
    a.extend_from_slice(&[0x48, 0x83, 0xc4, SIZE]);
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(dispatch as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xe0]);
    a.finish().expect("capture stub has no unresolved labels")
}

fn capture_stack_word(a: &mut Asm, load: bool, reg: u8, offset: usize) {
    let displacement = i8::try_from(offset).expect("capture image fits disp8") as u8;
    a.extend_from_slice(&[
        0x48 | ((reg >> 3) << 2), // REX.W and high register bit
        if load { 0x8b } else { 0x89 },
        0x44 | ((reg & 7) << 3),
        0x24,
        displacement,
    ]);
}

/// Emit a callable adapter with SysV signature `(request) -> primary_value`.
/// Only the Rust-helper return is tested. A generated caller calls this adapter
/// normally and needs no successful-return transfer test of its own.
///
/// On Transfer or Deopt, tail-jump to `cold_entry` with `(request, value, exit)`
/// in RDI/RSI/RDX, after removing the adapter's temporary stack frame. The
/// caller's frame and return address remain live for capture. The cold entry
/// must be generated code/assembly using that convention and must not return to
/// the successful continuation. It must publish roots before allocating and
/// capture all outstanding cleanup/frame obligations before discarding frames.
/// No helper, allocation or safepoint occurs between reading the outcome and
/// entering that cold route. The outcome stack slot is not itself a GC root.
///
/// Before executing these bytes, the installer must retain both targets, check
/// platform transfer support, and supply the call-site maps and root protocol.
pub fn emit_helper_veneer(helper: NativeHelperV2, cold_entry: *const u8) -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<NativeOutcome>() == 16);
        assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
        assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
        assert!(NativeExit::Returned as u64 == 0);
    }
    let mut a = Asm::new();
    let exceptional = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // endbr64
                                                    // 16 bytes for NativeOutcome and 8 for request; align RSP before Rust CALL.
    a.extend_from_slice(&[0x48, 0x83, 0xec, 24]); // sub rsp,24
    a.extend_from_slice(&[0x48, 0x89, 0x7c, 0x24, 16]); // mov [rsp+16],rdi
    a.extend_from_slice(&[0x48, 0x89, 0xe6]); // mov rsi,rsp (out)
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,helper
    a.extend_from_slice(&(helper as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]); // call rax; Rust has returned before branching
    a.extend_from_slice(&[0x48, 0x8b, 0x04, 0x24]); // mov rax,[rsp] (value)
    a.extend_from_slice(&[0x48, 0x8b, 0x54, 0x24, 8]); // mov rdx,[rsp+8] (exit)
    a.extend_from_slice(&[0x48, 0x8b, 0x7c, 0x24, 16]); // mov rdi,[rsp+16]
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 24]); // add rsp,24
    a.extend_from_slice(&[0x48, 0x85, 0xd2]); // test rdx,rdx (never test Lisp value)
    a.jcc(Cc::Ne, exceptional);
    a.extend_from_slice(&[0xc3]); // ret: successful primary in rax
    a.bind(exceptional);
    a.extend_from_slice(&[0x48, 0x89, 0xc6]); // mov rsi,rax
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,cold_entry
    a.extend_from_slice(&(cold_entry as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xe0]); // jmp rax
    a.finish().expect("local helper veneer label")
}

/// A helper receiving a writable caller image captured before entering Rust.
/// The runtime must publish that image before polling or allocating, and end
/// publication before returning. The image and outcome overlap; do not hold a
/// Rust reference to the image across writes through the outcome pointer.
pub type NativePublishedHelper =
    unsafe extern "C" fn(*mut u8, *mut NativeOutcome, *mut SysvTransferCapture);

/// Capture the suspended Lisp caller before a collecting Rust boundary.
/// The callback owns publication for its entire dynamic extent, including Rust
/// reentry into Lisp. Returning assembly restores the possibly relocated save
/// words before either normal return or cold capture. Direct native calls need
/// no additional callbacks. Code owners must retain both callback targets.
pub fn emit_published_helper_veneer(
    helper: NativePublishedHelper,
    cold_entry: *const u8,
) -> Vec<u8> {
    use std::mem::{offset_of, size_of};
    const SIZE: u8 = size_of::<SysvTransferCapture>() as u8;
    const {
        assert!(size_of::<SysvTransferCapture>() == 88);
        assert!(size_of::<NativeOutcome>() == 16);
        assert!(offset_of!(SysvTransferCapture, exit) == offset_of!(SysvTransferCapture, value) + 8);
        assert!(offset_of!(NativeOutcome, value) == 0);
        assert!(offset_of!(NativeOutcome, exit) == 8);
        assert!(NativeExit::Returned as u64 == 0);
    }
    let mut a = Asm::new();
    let exceptional = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa, 0x48, 0x83, 0xec, SIZE]);
    capture_stack_word(&mut a, false, 7, offset_of!(SysvTransferCapture, request));
    let preserved = offset_of!(SysvTransferCapture, preserved);
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, false, reg, preserved + index * 8);
    }
    a.extend_from_slice(&[0x48, 0x8d, 0x44, 0x24, SIZE + 8]);
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, caller_sp));
    capture_stack_word(&mut a, true, 0, SIZE as usize);
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, return_pc));
    // Initialize every field, including the enum, before Rust may borrow it.
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&egcl_rt::value::NIL.to_raw().to_le_bytes());
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, value));
    a.extend_from_slice(&[0x31, 0xc0]);
    capture_stack_word(&mut a, false, 0, offset_of!(SysvTransferCapture, exit));
    a.extend_from_slice(&[0x48, 0x8d, 0x74, 0x24, offset_of!(SysvTransferCapture, value) as u8]);
    a.extend_from_slice(&[0x48, 0x89, 0xe2]); // rdx=image; rdi=request; rsi=out
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(helper as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]);
    for (index, reg) in [3, 5, 12, 13, 14, 15].into_iter().enumerate() {
        capture_stack_word(&mut a, true, reg, preserved + index * 8);
    }
    for (reg, offset) in [
        (0, offset_of!(SysvTransferCapture, value)),
        (2, offset_of!(SysvTransferCapture, exit)),
        (7, offset_of!(SysvTransferCapture, request)),
    ] {
        capture_stack_word(&mut a, true, reg, offset);
    }
    a.extend_from_slice(&[0x48, 0x83, 0xc4, SIZE, 0x48, 0x85, 0xd2]);
    a.jcc(Cc::Ne, exceptional);
    a.extend_from_slice(&[0xc3]);
    a.bind(exceptional);
    a.extend_from_slice(&[0x48, 0x89, 0xc6, 0x48, 0xb8]);
    a.extend_from_slice(&(cold_entry as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xe0]);
    a.finish().expect("local published helper veneer label")
}

/// Bridge a mapped call to a legacy CallCell slice entry. Load the current entry
/// on every invocation so definition replacement and invalidation stay visible.
/// The recovery toggle enables legacy fault handling only during the cell call;
/// mapped continuation/capture must retain disabled recovery. Both callbacks
/// must be nonallocating and leave multiple values untouched.
/// All legacy/Rust frames have returned before capture; restore this adapter's
/// stack so `cold_entry` sees the original mapped caller's return address.
/// The installer must retain the cell, its entry storage, and all code targets.
pub fn emit_legacy_cell_veneer(
    cell: u64,
    slice_entry: u64,
    recovery_toggle: extern "C" fn(u64),
    pending_error: extern "C" fn() -> u64,
    cold_entry: *const u8,
) -> Vec<u8> {
    const {
        assert!(std::mem::offset_of!(super::emit::TransferCallRequest, nargs) == 8);
        assert!(std::mem::offset_of!(super::emit::TransferCallRequest, args) == 16);
    }
    let mut a = Asm::new();
    let exceptional = a.label();
    a.extend_from_slice(&[0xf3, 0x0f, 0x1e, 0xfa]); // endbr64
    a.extend_from_slice(&[0x48, 0x83, 0xec, 24]); // sub rsp,24 (aligned calls)
    a.extend_from_slice(&[0x48, 0x89, 0x7c, 0x24, 16]); // save request
    a.extend_from_slice(&[0xbf, 1, 0, 0, 0]); // enable legacy fault recovery
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(recovery_toggle as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]);
    a.extend_from_slice(&[0x48, 0x8b, 0x7c, 0x24, 16]); // restore request
    a.extend_from_slice(&[0x48, 0x8b, 0x77, 8]); // mov rsi,[rdi+8] (nargs)
    a.extend_from_slice(&[0x48, 0x8b, 0x57, 16]); // mov rdx,[rdi+16] (args)
    a.extend_from_slice(&[0x48, 0xbf]); // mov rdi,cell
    a.extend_from_slice(&cell.to_le_bytes());
    a.extend_from_slice(&[0x31, 0xc9]); // xor ecx,ecx (reserved profile token)
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,slice_entry
    a.extend_from_slice(&slice_entry.to_le_bytes());
    a.extend_from_slice(&[0xff, 0x10]); // call [rax] (current atomic entry)
    a.extend_from_slice(&[0x48, 0x89, 0x04, 0x24]); // save primary
    a.extend_from_slice(&[0x31, 0xff]); // disable recovery for mapped caller
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(recovery_toggle as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]);
    a.extend_from_slice(&[0x48, 0xb8]); // mov rax,pending_error
    a.extend_from_slice(&(pending_error as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xd0]); // call rax (no GC or MV mutation)
    a.extend_from_slice(&[0x48, 0x85, 0xc0]); // test rax,rax (status only)
    a.jcc(Cc::Ne, exceptional);
    a.extend_from_slice(&[0x48, 0x8b, 0x04, 0x24]); // restore primary
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 24]);
    a.extend_from_slice(&[0xc3]);
    a.bind(exceptional);
    a.extend_from_slice(&[0x48, 0x8b, 0x7c, 0x24, 16]); // restore request
    a.extend_from_slice(&[0x48, 0x8b, 0x34, 0x24]); // mov rsi,[rsp] (value)
    a.extend_from_slice(&[0xba]); // mov edx,Transfer
    a.extend_from_slice(&(NativeExit::Transfer as u32).to_le_bytes());
    a.extend_from_slice(&[0x48, 0x83, 0xc4, 24]); // discard adapter frame
    a.extend_from_slice(&[0x48, 0xb8]);
    a.extend_from_slice(&(cold_entry as usize as u64).to_le_bytes());
    a.extend_from_slice(&[0xff, 0xe0]); // tail-enter capture
    a.finish().expect("local CallCell veneer label")
}
