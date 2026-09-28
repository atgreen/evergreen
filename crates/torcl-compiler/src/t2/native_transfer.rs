//! SysV machine adapters for the native transfer ABI. These adapters are not
//! installed by the legacy T2 pipeline; call-site capture and landing metadata
//! must be supplied by the new ABI's installer.

use torcl_rt::asm::{Asm, Cc};
use torcl_rt::native_transfer::{NativeExit, NativeOutcome};

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
    pub value: torcl_rt::value::TorclVal,
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
