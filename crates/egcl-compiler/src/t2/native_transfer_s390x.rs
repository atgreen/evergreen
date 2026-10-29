// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! System Z (s390x ELF64) machine stubs for the native transfer ABI.
//!
//! The x86-64 stubs in native_transfer.rs define the protocol and explain it:
//! a helper veneer that tests only the [`NativeOutcome::exit`] word and
//! tail-jumps to a cold entry with `(request, value, exit)`, a capture stub
//! that snapshots the still-live caller frame and calls Rust preparation, and
//! a landing stub that re-enters retained native code from a
//! `(stack_pointer, entry)` packet. This file is the System Z emitter for the
//! same three stubs. It is a machine interface, not an admission check: the
//! installer still owns retention, call-site coverage and the root protocol.
//!
//! # Why a separate emitter
//!
//! z/Architecture differs from SysV x86-64 in every detail the protocol
//! touches. The return address is a register (r14), not a stack word, so the
//! veneer must hand it to the capture stub rather than leave it on the stack.
//! Each frame owns a 160-byte linkage area that a callee writes through, so a
//! temporary frame is 160 bytes larger than its payload. The callee-saved GPRs
//! are r6-r13 -- eight, not six -- and they are saved into the *caller's*
//! frame at fixed offsets (register rN at `8*(N-6)+48`), which is why the
//! veneer parks r14 at 112(r15) before it claims its own frame. There is no
//! indirect-branch landing-pad instruction to place at a pad, and no
//! shadow-stack interaction to preserve.
//!
//! # What this file owns
//!
//! * [`emit_helper_veneer`] -- the callable adapter `(request) -> primary` a
//!   generated caller invokes instead of the raw helper. It calls a
//!   [`NativeHelperV2`] with an out-parameter [`NativeOutcome`] in its own
//!   frame and tests only the `exit` word. `Returned` leaves the primary in
//!   r2 and returns; anything else removes the veneer frame, restores the
//!   caller's return address into r14 and branches to the cold entry with
//!   `(request, value, exit)` in r2/r3/r4, leaving the caller's frame intact.
//! * [`emit_capture_stub`] -- the cold entry. It writes an
//!   [`S390xTransferCapture`] in its own frame (request, value, exit, the
//!   eight callee-saved GPRs r6-r13, the caller's r15 at the call, and the
//!   return PC the veneer preserved in r14), calls a
//!   [`NativeTransferPrepare`] with a pointer to it, reloads the
//!   possibly-updated image and branches to the dispatch address with the
//!   same three-register convention. Only the stub's own frame is removed.
//! * [`emit_native_landing_stub`] -- the tail adapter from
//!   `(landing_packet, primary, exit)` to a landing pad: it loads r15 and the
//!   entry from an [`S390xNativeLanding`] packet, moves the primary into r2
//!   and branches.
//! * [`S390xCaptureLocation`] -- a checked recipe for reading or writing one
//!   value's home through the capture image: an index into `preserved`, or a
//!   byte offset from `caller_sp` for a frame slot.
//!
//! # Contract
//!
//! Volatile GPRs (r0-r5, r14) and every floating-point register are clobbered
//! by the time the capture runs, so a value that must survive a throwing call
//! needs a call-preserved home -- r6-r13 or a frame slot. The capture image is
//! stack-resident and temporary: it registers no GC roots, so preparation must
//! publish the payload and every mapped root before it allocates or yields,
//! must update native homes for anything the collector moved, and must not
//! retain the image by address. The source frame stays live until the
//! dispatcher retires it. Lisp errors inside helpers and preparation are
//! reported through the outcome, never by panic.

use egcl_rt::asm_s390x::Asm;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

/// Version-two helper: a request pointer plus an explicit outcome
/// destination, the same signature the x86-64 and ppc64le stubs call. The
/// helper must initialize both outcome fields, return normally (including on
/// Lisp errors), and root request/payload references across allocation.
pub type NativeHelperV2 = unsafe extern "C" fn(*mut u8, *mut NativeOutcome);

/// Rust preparation called by the capture stub with a pointer to the image it
/// just wrote. It returns normally; the stub then reloads the image, so
/// preparation may rewrite any home the collector moved.
pub type NativeTransferPrepare = unsafe extern "C" fn(*mut S390xTransferCapture);

/// Stack-resident image written by [`emit_capture_stub`] after a helper takes
/// the cold route. Volatile registers are already clobbered: exception-live
/// values must have call-preserved homes. This image registers no GC roots.
#[repr(C)]
pub struct S390xTransferCapture {
    pub request: *mut u8,
    pub value: egcl_rt::value::EgclVal,
    pub exit: NativeExit,
    /// Hardware r6, r7, r8, r9, r10, r11, r12, r13, in that order.
    /// Preparation may update these words; the stub reloads them before it
    /// dispatches.
    pub preserved: [u64; 8],
    /// The caller's r15 at the call that threw -- the base its frame slots are
    /// addressed from, including any temporary stack adjustment at the call.
    pub caller_sp: *const u64,
    /// The call site's return address, which the veneer preserved in r14.
    pub return_pc: *const u8,
}

/// Execution-owned packet consumed by [`emit_native_landing_stub`] after Rust
/// preparation returns. This is a machine interface, not an admission check:
/// the dispatcher must validate both fields against retained code/frame maps.
#[repr(C)]
pub struct S390xNativeLanding {
    /// The landing frame's body r15, after discarding the completed helper's
    /// temporary areas. The frame must still be live in the current segment.
    pub stack_pointer: *mut u64,
    /// A landing pad in retained native code. Its live-value homes must
    /// already be populated, including any write moving GC required.
    pub entry: *const u8,
}

/// The lowest and highest callee-saved GPR the capture image preserves.
const FIRST_PRESERVED: u8 = 6;
const PRESERVED_COUNT: usize = 8;

/// Every frame must give its callees a 160-byte linkage area.
const LINKAGE_BYTES: i32 = 160;

/// Where the ABI parks r14 in the *caller's* frame: `8 * (14 - 6) + 48`.
const RETURN_ADDRESS_SLOT: i32 = 112;

/// A home reachable through [`S390xTransferCapture`]: either one of the eight
/// preserved GPRs, or a doubleword at a signed offset from `caller_sp`.
/// Construction is checked, so a recipe can never name r0-r5, r14 or r15 --
/// registers the capture cannot speak for -- or a misaligned frame word.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum S390xCaptureLocation {
    /// Index into `preserved`: 0 is r6, 7 is r13.
    Preserved(u8),
    /// Byte offset from `caller_sp`, a multiple of eight.
    FrameSlot(i32),
}

impl S390xCaptureLocation {
    /// A hardware register number, accepted only for r6-r13.
    pub fn register(number: u8) -> Option<Self> {
        let index = number.checked_sub(FIRST_PRESERVED)?;
        (usize::from(index) < PRESERVED_COUNT).then_some(Self::Preserved(index))
    }

    /// A doubleword frame slot at `offset` from the caller's r15.
    pub fn frame_slot(offset: i32) -> Option<Self> {
        (offset % 8 == 0).then_some(Self::FrameSlot(offset))
    }

    /// Read this home out of a capture image.
    ///
    /// # Safety
    /// `capture` must be the live image the stub wrote, and a `FrameSlot`
    /// recipe must name a doubleword inside the still-live caller frame that
    /// the emitted code actually reserved.
    pub unsafe fn read(self, capture: &S390xTransferCapture) -> u64 {
        match self {
            Self::Preserved(index) => capture.preserved[usize::from(index)],
            // SAFETY: the caller guarantees the slot is live and reserved.
            Self::FrameSlot(offset) => unsafe {
                capture.caller_sp.byte_offset(offset as isize).read()
            },
        }
    }

    /// Write this home through a capture image, for a value the collector
    /// moved while preparation ran.
    ///
    /// # Safety
    /// As [`Self::read`], and the written word must be a value the dispatcher
    /// has already published as a root.
    pub unsafe fn write(self, capture: &mut S390xTransferCapture, word: u64) {
        match self {
            Self::Preserved(index) => capture.preserved[usize::from(index)] = word,
            // SAFETY: the caller guarantees the slot is live and reserved.
            Self::FrameSlot(offset) => unsafe {
                capture
                    .caller_sp
                    .cast_mut()
                    .byte_offset(offset as isize)
                    .write(word)
            },
        }
    }
}

/// Emit a callable adapter with the C signature `(request) -> primary`.
///
/// Only the Rust helper's `exit` word is tested, never the Lisp value, so a
/// generated caller needs no successful-return transfer test of its own. On
/// `Transfer` or `Deopt` the adapter removes its own frame, restores the
/// caller's return address into r14 and branches to `cold_entry` with
/// `(request, value, exit)` in r2/r3/r4. The caller's frame and return
/// address stay live for the capture; no Rust frame is crossed by the branch,
/// and no helper, allocation or safepoint runs between reading the outcome
/// and entering the cold route. The outcome slot is not itself a GC root.
pub fn emit_helper_veneer(helper: NativeHelperV2, cold_entry: *const u8) -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<NativeOutcome>() == 16);
        assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
        assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
        assert!(NativeExit::Returned as u64 == 0);
    }
    // Above the linkage area: the saved request, then the outcome the helper
    // fills in. 32 bytes keeps the outcome doubleword-aligned with room for
    // the request below it.
    const REQUEST: i32 = LINKAGE_BYTES;
    const OUTCOME: i32 = LINKAGE_BYTES + 16;
    const FRAME_BYTES: i32 = LINKAGE_BYTES + 32;

    let mut a = Asm::new();
    let exceptional = a.label();
    a.store(14, 15, RETURN_ADDRESS_SLOT);
    a.address(15, 15, -FRAME_BYTES);
    a.store(2, 15, REQUEST);
    a.address(3, 15, OUTCOME);
    a.imm64(1, helper as usize as u64);
    a.call_reg(1);
    // Hold the three cold-route values in volatile registers while the frame
    // is retired; the successful route moves the primary into r2 afterwards.
    a.load(4, 15, OUTCOME);
    a.load(5, 15, OUTCOME + 8);
    a.load(2, 15, REQUEST);
    a.compare_imm(5, NativeExit::Returned as i16);
    a.branch(6, exceptional);
    a.mov(2, 4);
    a.address(15, 15, FRAME_BYTES);
    a.load(14, 15, RETURN_ADDRESS_SLOT);
    a.ret();

    a.bind(exceptional);
    a.mov(3, 4);
    a.mov(4, 5);
    a.address(15, 15, FRAME_BYTES);
    // The cold route needs the call site's return address, which on this
    // target lives in a register rather than on the stack.
    a.load(14, 15, RETURN_ADDRESS_SLOT);
    a.imm64(1, cold_entry as usize as u64);
    a.jump_reg(1);
    a.finish().expect("System Z helper veneer has local labels")
}

/// Emit the cold entry a veneer branches to with `(request, value, exit)` in
/// r2/r3/r4, the call site's return address in r14 and the caller's r15 still
/// addressing its live frame.
///
/// The stub writes an [`S390xTransferCapture`] in its own frame, calls
/// `prepare` with it, reloads the possibly-updated image and branches to
/// `dispatch` with the same three registers. Preparation returns normally
/// before any frame is adjusted, so no Rust frame is ever jumped over, and
/// only this stub's temporary frame is removed.
pub fn emit_capture_stub(prepare: NativeTransferPrepare, dispatch: *const u8) -> Vec<u8> {
    use std::mem::{offset_of, size_of};
    const {
        assert!(size_of::<S390xTransferCapture>() == 104);
        assert!(offset_of!(S390xTransferCapture, request) == 0);
        assert!(offset_of!(S390xTransferCapture, value) == 8);
        assert!(offset_of!(S390xTransferCapture, exit) == 16);
    }
    const IMAGE: i32 = LINKAGE_BYTES;
    let frame_bytes = LINKAGE_BYTES + size_of::<S390xTransferCapture>() as i32;
    let at = |field: usize| IMAGE + field as i32;
    let preserved = |index: usize| at(offset_of!(S390xTransferCapture, preserved) + index * 8);

    let mut a = Asm::new();
    // r15 still addresses the caller's frame, so record it before claiming
    // one: every FrameSlot recipe is an offset from exactly this value.
    a.mov(0, 15);
    a.address(15, 15, -frame_bytes);
    a.store(2, 15, at(offset_of!(S390xTransferCapture, request)));
    a.store(3, 15, at(offset_of!(S390xTransferCapture, value)));
    a.store(4, 15, at(offset_of!(S390xTransferCapture, exit)));
    for index in 0..PRESERVED_COUNT {
        a.store(FIRST_PRESERVED + index as u8, 15, preserved(index));
    }
    a.store(0, 15, at(offset_of!(S390xTransferCapture, caller_sp)));
    a.store(14, 15, at(offset_of!(S390xTransferCapture, return_pc)));
    a.address(2, 15, IMAGE);
    a.imm64(1, prepare as usize as u64);
    a.call_reg(1);
    // Preparation has returned, so reload everything it may have rewritten
    // for a moved object before the dispatcher sees it.
    for index in 0..PRESERVED_COUNT {
        a.load(FIRST_PRESERVED + index as u8, 15, preserved(index));
    }
    a.load(2, 15, at(offset_of!(S390xTransferCapture, request)));
    a.load(3, 15, at(offset_of!(S390xTransferCapture, value)));
    a.load(4, 15, at(offset_of!(S390xTransferCapture, exit)));
    a.address(15, 15, frame_bytes);
    a.imm64(1, dispatch as usize as u64);
    a.jump_reg(1);
    a.finish()
        .expect("System Z capture stub has no unresolved labels")
}

/// Emit the tail adapter from `(landing_packet, primary, exit)` in r2/r3/r4
/// to a landing pad.
///
/// Pair it with [`emit_capture_stub`]: preparation returns normally and that
/// stub restores the updated callee-saved registers before entering this
/// adapter. The source frame remains live and the pad owns its retirement.
/// The packet must occupy stable storage until dispatch, independently of the
/// temporary capture frame. This adapter neither searches handlers nor
/// establishes roots, validates targets, or supplies unwind metadata: the
/// installer must enforce those contracts before any of these bytes run. A
/// transfer cursor may ignore the primary on cleanup entry.
pub fn emit_native_landing_stub() -> Vec<u8> {
    const {
        assert!(std::mem::size_of::<S390xNativeLanding>() == 16);
        assert!(std::mem::offset_of!(S390xNativeLanding, stack_pointer) == 0);
        assert!(std::mem::offset_of!(S390xNativeLanding, entry) == 8);
    }
    let mut a = Asm::new();
    // Read both packet words before r15 moves: the packet may itself live in
    // the frame the new stack pointer discards.
    a.load(1, 2, 8);
    a.load(15, 2, 0);
    a.mov(2, 3);
    a.jump_reg(1);
    a.finish().expect("System Z landing stub has no labels")
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::value::EgclVal;

    #[test]
    fn capture_locations_reject_homes_the_image_cannot_speak_for() {
        assert_eq!(
            S390xCaptureLocation::register(6),
            Some(S390xCaptureLocation::Preserved(0))
        );
        assert_eq!(
            S390xCaptureLocation::register(13),
            Some(S390xCaptureLocation::Preserved(7))
        );
        // Volatile, the stack pointer, and the link register are clobbered or
        // owned by the stub itself by the time the capture runs.
        for number in [0, 1, 2, 3, 4, 5, 14, 15] {
            assert_eq!(S390xCaptureLocation::register(number), None, "r{number}");
        }
        assert_eq!(
            S390xCaptureLocation::frame_slot(160),
            Some(S390xCaptureLocation::FrameSlot(160))
        );
        assert_eq!(S390xCaptureLocation::frame_slot(164), None);
    }

    unsafe extern "C" fn returning_helper(request: *mut u8, out: *mut NativeOutcome) {
        // SAFETY: the veneer passes its own frame slot as the destination.
        unsafe {
            out.write(NativeOutcome {
                value: EgclVal(request as u64 + 1),
                exit: NativeExit::Returned,
            });
        }
    }

    unsafe extern "C" fn no_prepare(_capture: *mut S390xTransferCapture) {}

    #[test]
    fn every_stub_is_a_nonempty_sequence_of_whole_system_z_instructions() {
        let stubs = [
            emit_helper_veneer(returning_helper, std::ptr::dangling()),
            emit_capture_stub(no_prepare, std::ptr::dangling()),
            emit_native_landing_stub(),
        ];
        for stub in stubs {
            assert!(!stub.is_empty());
            // Every z/Architecture instruction is 2, 4 or 6 bytes long.
            assert_eq!(stub.len() % 2, 0);
        }
    }

    /// The whole cold route on real hardware: a generated caller fills the
    /// callee-saved registers, calls the veneer, and the helper throws. The
    /// veneer must reach the capture stub, the capture must describe the
    /// caller exactly, preparation must be able to rewrite a home for an
    /// object the collector moved, and the landing stub must resume the
    /// caller at a pad with that updated home in place.
    #[cfg(all(target_arch = "s390x", unix))]
    #[test]
    fn a_throwing_helper_captures_the_caller_and_lands_in_its_pad() {
        use std::cell::Cell;

        const REQUEST: u64 = 0x5eed_0001;
        const THROWN: u64 = 0x5eed_0002;
        const RELOCATED: u64 = 0x5eed_0003;
        // What the caller leaves in r6; preparation replaces it, standing in
        // for a value the collector moved while it ran.
        const ORIGINAL_R6: u64 = 0x5eed_0004;

        thread_local! {
            static PAD: Cell<*const u8> = const { Cell::new(std::ptr::null()) };
            static LANDING: Cell<*const u8> = const { Cell::new(std::ptr::null()) };
            static PACKET: Cell<S390xNativeLanding> = const {
                Cell::new(S390xNativeLanding {
                    stack_pointer: std::ptr::null_mut(),
                    entry: std::ptr::null(),
                })
            };
            static SEEN: Cell<(u64, u64, u64, u64, usize)> = const { Cell::new((0, 0, 0, 0, 0)) };
        }

        unsafe extern "C" fn throwing_helper(request: *mut u8, out: *mut NativeOutcome) {
            // SAFETY: the veneer owns the destination in its own frame.
            unsafe {
                out.write(NativeOutcome {
                    value: EgclVal(THROWN),
                    exit: NativeExit::Transfer,
                });
            }
            assert_eq!(request as u64, REQUEST);
        }

        unsafe extern "C" fn prepare(capture: *mut S390xTransferCapture) {
            // SAFETY: the stub passes the image it has just written, and it
            // stays live until this returns.
            let capture = unsafe { &mut *capture };
            SEEN.with(|seen| {
                seen.set((
                    capture.request as u64,
                    capture.value.0,
                    capture.exit as u64,
                    capture.preserved[0],
                    capture.caller_sp as usize,
                ))
            });
            // Rewrite r6's home the way a moving collection would, and point
            // the landing packet at the caller's still-live frame.
            let home = S390xCaptureLocation::register(6).expect("r6 is preserved");
            // SAFETY: r6 is a preserved register of the live image.
            unsafe { home.write(capture, RELOCATED) };
            PACKET.with(|packet| {
                packet.set(S390xNativeLanding {
                    stack_pointer: capture.caller_sp.cast_mut(),
                    entry: PAD.with(Cell::get),
                })
            });
        }

        let landing = egcl_rt::jit::JitBuffer::new(&emit_native_landing_stub()).unwrap();
        LANDING.with(|slot| slot.set(landing.as_ptr()));

        // Trampoline: `(request, value, exit)` in r2/r3/r4 -> `(packet,
        // primary, exit)`. It reads the packet address from a baked-in
        // pointer, which is what a real dispatcher publishes per call site.
        let packet_address = PACKET.with(|packet| packet.as_ptr() as u64);
        let trampoline = {
            let mut a = Asm::new();
            // The primary already sits in r3, where the landing stub wants
            // it; only the packet address has to be supplied.
            a.imm64(2, packet_address);
            a.imm64(1, landing.as_ptr() as u64);
            a.jump_reg(1);
            egcl_rt::jit::JitBuffer::new(&a.finish().unwrap()).unwrap()
        };
        let capture =
            egcl_rt::jit::JitBuffer::new(&emit_capture_stub(prepare, trampoline.as_ptr())).unwrap();
        let veneer =
            egcl_rt::jit::JitBuffer::new(&emit_helper_veneer(throwing_helper, capture.as_ptr()))
                .unwrap();

        // The generated caller. It owns r6-r13 across the call, so r6 is a
        // legal exception-live home; the pad returns whatever r6 holds.
        let (caller_code, pad_offset) = {
            let mut a = Asm::new();
            a.prologue();
            a.imm64(6, ORIGINAL_R6);
            a.imm64(2, REQUEST);
            a.imm64(1, veneer.as_ptr() as u64);
            a.call_reg(1);
            // Normal return: report that the cold route never ran.
            a.imm64(2, 0);
            a.epilogue();
            let pad = a.here();
            a.mov(2, 6);
            a.epilogue();
            (a.finish().unwrap(), pad)
        };
        let caller = egcl_rt::jit::JitBuffer::new(&caller_code).unwrap();
        // SAFETY: the pad lies inside the buffer just installed.
        PAD.with(|slot| slot.set(unsafe { caller.as_ptr().add(pad_offset) }));

        // SAFETY: the buffer holds the complete C-ABI entry emitted above.
        let entry: extern "C" fn() -> u64 = unsafe { std::mem::transmute(caller.as_ptr()) };
        let result = entry();

        let (request, value, exit, preserved_r6, caller_sp) = SEEN.with(Cell::get);
        assert_eq!(request, REQUEST, "the capture kept the request");
        assert_eq!(value, THROWN, "the capture kept the thrown value");
        assert_eq!(exit, NativeExit::Transfer as u64);
        assert_eq!(preserved_r6, ORIGINAL_R6, "r6 survived into the capture");
        assert_ne!(caller_sp, 0, "the capture recorded the caller's frame");
        assert_eq!(
            result, RELOCATED,
            "the pad must observe the home preparation rewrote"
        );
    }
}
