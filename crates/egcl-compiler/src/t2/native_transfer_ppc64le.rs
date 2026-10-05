// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! ELFv2 (ppc64le) machine stubs for the native transfer ABI.
//!
//! The x86-64 stubs in native_transfer.rs define the protocol: a helper
//! veneer that tests only the `NativeOutcome::exit` word and tail-jumps to a
//! cold entry with `(request, value, exit)`, and a landing stub that enters a
//! retained pad from a `(stack_pointer, entry)` packet. POWER has a different
//! nonvolatile set, a fixed linkage area, a link register and a TOC, so it
//! gets its own emitter rather than a target test around x86 byte encodings;
//! keeping the two apart also makes accidental installation of x86 bytes on
//! Power impossible.
//!
//! # What this file owns
//!
//! * [`emit_helper_veneer`] — a callable ELFv2 adapter owning a 64-byte
//!   temporary frame. It saves LR and r2 in the ABI's linkage slots, keeps the
//!   request at a fixed frame offset, calls the [`NativeHelperV2`] through the
//!   count register with an outcome area in the same frame, then loads value
//!   and exit into r4/r5. `Returned` moves the primary to r3, restores the
//!   frame and returns; otherwise it reloads the request into r3, restores the
//!   frame and branches to `cold_entry` with `(request, value, exit)` in
//!   r3–r5. No Rust frame is crossed by the branch.
//! * [`emit_native_landing_stub`] — loads r1 and the target from the packet
//!   in r3, moves the primary from r4 to r3, and branches through CTR. The
//!   source frame stays live; the selected pad owns its retirement.
//!
//! # What is missing
//!
//! There is no ppc64le capture stub: nothing here snapshots nonvolatile
//! registers or calls a preparation routine, so the `SysvTransferCapture` /
//! `SysvCaptureLocation` machinery has no counterpart on this target. The
//! ppc64le native-segment entry therefore admits only bodies that cannot call
//! back into Lisp, cross a control scope, or deopt; everything else uses the
//! checked ABI. The unit test checks only that both stubs are non-empty
//! 4-byte-aligned word sequences; execution is covered by the runtime's
//! ppc64le segment probe.

use egcl_rt::asm::Cc;
use egcl_rt::asm_ppc64le::{frame, Asm};
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

/// Version-two helper ABI shared with the runtime: `(request, outcome*)`.
pub type NativeHelperV2 = unsafe extern "C" fn(*mut u8, *mut NativeOutcome);

/// Emit a callable ELFv2 adapter around a Rust helper.
///
/// The adapter owns only its 64-byte temporary frame. A normal helper outcome
/// returns through the original caller; a transfer outcome restores the caller
/// frame and branches to `cold_entry` with `(request, value, exit)` in r3-r5.
/// No Rust frame is crossed by the branch.
pub fn emit_helper_veneer(helper: NativeHelperV2, cold_entry: *const u8) -> Vec<u8> {
    const FRAME_BYTES: i32 = 64;
    const REQUEST: i32 = 32;
    const OUTCOME: i32 = 48;
    const VALUE: i32 = OUTCOME;
    const EXIT: i32 = OUTCOME + 8;

    let mut a = Asm::new();
    let exceptional = a.label();
    a.move_from_link(0);
    assert!(a.store(0, 1, frame::LINK_SLOT).is_some());
    assert!(a.store(2, 1, frame::TOC_SLOT).is_some());
    assert!(a.store_update(1, 1, -FRAME_BYTES).is_some());
    assert!(a.store(3, 1, REQUEST).is_some());
    a.addi(4, 1, OUTCOME as i16);
    a.imm64(12, helper as usize as u64);
    a.move_to_count(12);
    a.call_count();

    // Keep the three cold-route values in volatile registers while the frame is
    // retired. The successful route moves the primary into r3 after the test.
    assert!(a.load(4, 1, VALUE).is_some());
    assert!(a.load(5, 1, EXIT).is_some());
    a.compare_imm(0, 5, NativeExit::Returned as i16);
    a.branch(Cc::Ne, 0, exceptional);
    a.mov(3, 4);
    restore_frame(&mut a);
    a.ret();

    a.bind(exceptional);
    assert!(a.load(3, 1, REQUEST).is_some());
    restore_frame(&mut a);
    a.imm64(12, cold_entry as usize as u64);
    a.move_to_count(12);
    a.jump_count();
    a.finish().expect("ELFv2 helper veneer has local labels")
}

/// Emit the cold native landing adapter. The packet is `(stack_pointer,
/// entry)`, followed by the primary and exit values in r4/r5. The source
/// native frame stays live; the selected landing pad owns its eventual
/// retirement after receiving the primary in r3.
pub fn emit_native_landing_stub() -> Vec<u8> {
    const PACKET: u8 = 11;
    const TARGET: u8 = 12;
    let mut a = Asm::new();
    a.mov(PACKET, 3);
    assert!(a.load(1, PACKET, 0).is_some());
    assert!(a.load(TARGET, PACKET, 8).is_some());
    a.mov(3, 4);
    a.move_to_count(TARGET);
    a.jump_count();
    a.finish().expect("ELFv2 landing stub has no labels")
}

fn restore_frame(a: &mut Asm) {
    assert!(a.load(12, 1, 0).is_some());
    assert!(a.load(0, 12, frame::LINK_SLOT).is_some());
    a.move_to_link(0);
    assert!(a.load(2, 12, frame::TOC_SLOT).is_some());
    a.mov(1, 12);
}

#[cfg(test)]
mod tests {
    use super::*;

    unsafe extern "C" fn helper(_request: *mut u8, out: *mut NativeOutcome) {
        // The emitter test checks the machine shape; runtime execution is
        // covered by the egcl-rt ppc64le segment probe.
        unsafe {
            out.write(NativeOutcome {
                value: egcl_rt::value::NIL,
                exit: NativeExit::Returned,
            });
        }
    }

    #[test]
    fn helper_and_landing_stubs_are_aligned_elfv2_words() {
        let helper = emit_helper_veneer(helper, std::ptr::dangling());
        let landing = emit_native_landing_stub();
        assert!(!helper.is_empty());
        assert!(!landing.is_empty());
        assert_eq!(helper.len() % 4, 0);
        assert_eq!(landing.len() % 4, 0);
    }
}
