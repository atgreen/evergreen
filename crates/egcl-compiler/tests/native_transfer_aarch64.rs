#![cfg(all(target_arch = "aarch64", target_os = "linux"))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution tests for the AAPCS64 native-transfer stubs.
//!
//! These RUN the emitted code rather than checking that it is non-empty. The
//! ppc64le stub test does the latter and leans on a separate runtime probe;
//! here the three stubs are small enough to drive directly, and the facts
//! worth proving are exactly the ones a byte-level test cannot see: that the
//! cold route reaches the capture stub with the caller's frame intact, that
//! the capture image names the right SP and return address on an
//! architecture with no pushed return address, and that a word preparation
//! writes into `preserved` is really back in its hardware register by the
//! time the dispatcher runs.

use egcl_compiler::t2::native_transfer_aarch64::{
    Aapcs64CaptureLocation, Aapcs64NativeLanding, Aapcs64TransferCapture, emit_capture_stub,
    emit_helper_veneer, emit_native_landing_stub,
};
use egcl_rt::asm::Asm;
use egcl_rt::asm::a64::{self, LR, SP};
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
use egcl_rt::value::{EgclVal, NIL};
use std::sync::atomic::{AtomicU64, Ordering};

/// Assemble `words` into executable memory and leak it: the returned pointer
/// is branched to after this returns, so a dropped mapping would unmap the
/// code out from under it.
fn jit(words: &[u32]) -> *const u8 {
    let mut a = Asm::new();
    a.words(words);
    let code = a.finish().expect("no unresolved labels");
    JitBuffer::new(&code).expect("executable mapping").leak()
}

fn jit_bytes(code: &[u8]) -> *const u8 {
    JitBuffer::new(code).expect("executable mapping").leak()
}

#[repr(C)]
struct Request {
    value: EgclVal,
    exit: NativeExit,
}

unsafe extern "C" fn helper(request: *mut u8, out: *mut NativeOutcome) {
    let request = unsafe { &*request.cast::<Request>() };
    unsafe {
        out.write(NativeOutcome {
            value: request.value,
            exit: request.exit,
        });
    }
}

static COLD_REQUEST: AtomicU64 = AtomicU64::new(0);
static COLD_VALUE: AtomicU64 = AtomicU64::new(0);
static COLD_EXIT: AtomicU64 = AtomicU64::new(0);

extern "C" fn cold_route(request: *mut u8, value: u64, exit: u64) -> u64 {
    COLD_REQUEST.store(request as u64, Ordering::SeqCst);
    COLD_VALUE.store(value, Ordering::SeqCst);
    COLD_EXIT.store(exit, Ordering::SeqCst);
    0xfeed_face
}

#[test]
fn helper_veneer_returns_normally_and_tails_to_the_cold_route() {
    let veneer = jit_bytes(&emit_helper_veneer(helper, cold_route as *const u8));
    let call: unsafe extern "C" fn(*mut u8) -> u64 = unsafe { std::mem::transmute(veneer) };

    // A Returned outcome hands back the primary, and never reaches the cold
    // route. NIL is the interesting value here precisely because it is not
    // zero: a veneer that tested the Lisp value instead of the exit word
    // would pass this case and fail on a NIL result.
    let mut normal = Request {
        value: NIL,
        exit: NativeExit::Returned,
    };
    COLD_EXIT.store(0xdead, Ordering::SeqCst);
    assert_eq!(unsafe { call((&mut normal as *mut Request).cast()) }, NIL.0);
    assert_eq!(
        COLD_EXIT.load(Ordering::SeqCst),
        0xdead,
        "a normal return must not reach the cold route"
    );

    // A fixnum zero primary with a Transfer exit: the mirror case. The value
    // IS zero here, so a veneer that branched on the value would take the
    // normal return and lose the transfer.
    let mut transfer = Request {
        value: EgclVal::from_fixnum(0),
        exit: NativeExit::Transfer,
    };
    let request = (&mut transfer as *mut Request).cast::<u8>();
    assert_eq!(unsafe { call(request) }, 0xfeed_face);
    assert_eq!(COLD_REQUEST.load(Ordering::SeqCst), request as u64);
    assert_eq!(COLD_VALUE.load(Ordering::SeqCst), EgclVal::from_fixnum(0).0);
    assert_eq!(
        COLD_EXIT.load(Ordering::SeqCst),
        NativeExit::Transfer as u64
    );

    let mut deopt = Request {
        value: EgclVal::from_fixnum(41),
        exit: NativeExit::Deopt,
    };
    assert_eq!(
        unsafe { call((&mut deopt as *mut Request).cast()) },
        0xfeed_face
    );
    assert_eq!(COLD_EXIT.load(Ordering::SeqCst), NativeExit::Deopt as u64);
}

static SEEN_REQUEST: AtomicU64 = AtomicU64::new(0);
static SEEN_VALUE: AtomicU64 = AtomicU64::new(0);
static SEEN_EXIT: AtomicU64 = AtomicU64::new(0);
static SEEN_X19: AtomicU64 = AtomicU64::new(0);
static SEEN_SP: AtomicU64 = AtomicU64::new(0);
static SEEN_RETURN_PC: AtomicU64 = AtomicU64::new(0);
/// What preparation writes back into x19's slot, to prove the reload.
const REPLACED_X19: u64 = 0x5555_0000_aaaa_1111;

unsafe extern "C" fn prepare(capture: *mut Aapcs64TransferCapture) {
    let capture = unsafe { &mut *capture };
    SEEN_REQUEST.store(capture.request as u64, Ordering::SeqCst);
    SEEN_VALUE.store(capture.value.0, Ordering::SeqCst);
    SEEN_EXIT.store(capture.exit as u64, Ordering::SeqCst);
    SEEN_SP.store(capture.caller_sp as u64, Ordering::SeqCst);
    SEEN_RETURN_PC.store(capture.return_pc as u64, Ordering::SeqCst);

    // Read x19 through the documented recipe rather than by index, so the
    // test covers the mapping from a hardware register to its slot.
    let x19 = Aapcs64CaptureLocation::for_register(19).expect("x19 is callee-saved");
    SEEN_X19.store(unsafe { x19.read(capture) }, Ordering::SeqCst);
    // A collection during preparation would relocate values and rewrite their
    // homes; this stands in for that write.
    unsafe { x19.write(capture, REPLACED_X19) };
}

#[test]
fn the_capture_stub_images_the_caller_and_reloads_what_preparation_wrote() {
    // dispatch: hand x19 back, so the caller observes whatever the stub
    // reloaded into the hardware register from the image.
    let dispatch = jit(&[a64::bti_c(), a64::mov(0, 19), a64::ret()]);
    let stub = jit_bytes(&emit_capture_stub(prepare, dispatch));

    // A trampoline, because the facts under test are about registers Rust
    // owns. It plants a sentinel in x19, calls the stub with the cold route's
    // (request, value, exit) convention, then restores x19 before returning
    // so this thread's Rust frames stay intact.
    //
    //   x0 = request, x1 = x19 sentinel, x2 = stub
    const SENTINEL: u64 = 0x1234_5678_9abc_def0;
    let trampoline = jit(&[
        a64::bti_c(),
        a64::stp_pre(29, LR, SP, -32).unwrap(),
        a64::mov_from_sp(29),
        a64::str_imm(19, SP, 16).unwrap(), // save Rust's x19
        a64::mov(16, 2),                   // the stub, before x2 is overwritten
        a64::mov(19, 1),                   // the sentinel
        a64::mov(1, 0),                    // value := request, an arbitrary nonzero word
        a64::movz(2, NativeExit::Transfer as u16, 0),
        a64::blr(16),
        a64::ldr_imm(19, SP, 16).unwrap(), // restore Rust's x19
        a64::ldp_post(29, LR, SP, 32).unwrap(),
        a64::ret(),
    ]);
    let run: unsafe extern "C" fn(u64, u64, u64) -> u64 =
        unsafe { std::mem::transmute(trampoline) };

    let request = 0xabcd_u64;
    let returned = unsafe { run(request, SENTINEL, stub as u64) };

    assert_eq!(SEEN_REQUEST.load(Ordering::SeqCst), request);
    assert_eq!(SEEN_VALUE.load(Ordering::SeqCst), request);
    assert_eq!(
        SEEN_EXIT.load(Ordering::SeqCst),
        NativeExit::Transfer as u64
    );
    assert_eq!(
        SEEN_X19.load(Ordering::SeqCst),
        SENTINEL,
        "the image must carry the caller's callee-saved registers"
    );
    assert_eq!(
        returned, REPLACED_X19,
        "the stub must reload the registers preparation rewrote before dispatching"
    );

    // caller_sp must be the trampoline's SP at the BLR -- that is, inside its
    // own 32-byte frame, below the Rust stack it was called from. The exact
    // value is not predictable, but it must be a plausible live stack address
    // and, crucially, 16-byte aligned: the "+8 for a pushed return address"
    // that SysV needs and AArch64 does not would show up here as a misaligned
    // SP one slot off.
    let caller_sp = SEEN_SP.load(Ordering::SeqCst);
    assert_ne!(caller_sp, 0, "caller_sp was never written");
    assert_eq!(caller_sp % 16, 0, "caller_sp must be 16-byte aligned");

    // return_pc is the trampoline's BLR return address, which lies inside the
    // trampoline's own code.
    let return_pc = SEEN_RETURN_PC.load(Ordering::SeqCst);
    assert!(
        return_pc > trampoline as u64 && return_pc < trampoline as u64 + 4096,
        "return_pc {return_pc:#x} must point into the calling stub, not at {:#x}",
        trampoline as u64
    );
    assert_eq!(
        return_pc % 4,
        0,
        "an AArch64 return address is word-aligned"
    );
}

#[test]
fn the_landing_stub_enters_a_pad_with_the_primary_and_the_packet_sp() {
    // The pad: discard the trampoline's frame and return the primary it was
    // handed in x0. x30 still holds the Rust return address, because a
    // branch to the landing stub never wrote it.
    let pad = jit(&[a64::bti_c(), a64::add_imm(SP, SP, 16).unwrap(), a64::ret()]);
    let landing = jit_bytes(&emit_native_landing_stub());

    // x0 = packet, x1 = primary, x2 = landing stub. The trampoline fills in
    // the packet's stack_pointer with its own live SP, since a landing frame
    // has to be one that is actually still on the stack.
    let trampoline = jit(&[
        a64::bti_c(),
        a64::stp_pre(29, LR, SP, -16).unwrap(),
        a64::mov_from_sp(29),
        a64::mov_from_sp(3),
        a64::str_imm(3, 0, 0).unwrap(), // packet.stack_pointer := SP
        a64::mov(16, 2),
        a64::br(16),
    ]);
    let run: unsafe extern "C" fn(*mut Aapcs64NativeLanding, u64, u64) -> u64 =
        unsafe { std::mem::transmute(trampoline) };

    let mut packet = Aapcs64NativeLanding {
        stack_pointer: std::ptr::null_mut(),
        entry: pad,
    };
    let primary = EgclVal::from_fixnum(1234).0;
    assert_eq!(
        unsafe { run(&mut packet, primary, landing as u64) },
        primary
    );
    assert!(
        !packet.stack_pointer.is_null(),
        "the trampoline should have recorded its SP"
    );
}

#[test]
fn a_caller_saved_register_has_no_capture_home() {
    // x0-x18 are caller-saved: by the time the capture runs they are already
    // clobbered, so a recipe naming one would read a lie rather than fail.
    for register in [0u8, 1, 8, 15, 16, 17, 18] {
        assert!(
            Aapcs64CaptureLocation::for_register(register).is_err(),
            "x{register} is caller-saved and must not resolve to a capture home"
        );
    }
    for register in 19u8..=28 {
        assert!(
            Aapcs64CaptureLocation::for_register(register).is_ok(),
            "x{register} is callee-saved and must resolve"
        );
    }
    // x29 is the frame pointer and x30 the link register: neither is a value
    // home the allocator can hand out.
    assert!(Aapcs64CaptureLocation::for_register(29).is_err());
    assert!(Aapcs64CaptureLocation::for_register(30).is_err());
}

#[test]
fn a_stack_home_must_lie_inside_the_declared_frame() {
    use egcl_compiler::t2::native_transfer_aarch64::CaptureLocationError::*;
    assert_eq!(
        Aapcs64CaptureLocation::for_stack_slot(4, 4, 0),
        Err(StackSlotOutsideFrame(4))
    );
    assert_eq!(
        Aapcs64CaptureLocation::for_stack_slot(0, 4, 4),
        Err(UnalignedStackAdjustment)
    );
    assert!(Aapcs64CaptureLocation::for_stack_slot(3, 4, 16).is_ok());
}
