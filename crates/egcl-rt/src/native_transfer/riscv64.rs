// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LP64D native-segment boundary for riscv64.
//!
//! Generated entries receive `(slots, stack, anchor)` in `a0`-`a2`.
//! `enter_riscv64` preserves the LP64D callee-saved registers in a fixed
//! 256-byte frame: ra at 0, s0 at 8, s1-s11 at 16..104, fs0-fs11 at
//! 104..200, the five incoming words at 200..240, and the outcome (value,
//! exit) at 240 and 248. A generated body may return normally or call the
//! cold leave helper, which restores that frame's sp and jumps to the common
//! landing path.

use super::{EgclStack, NativeOutcome, NativeSegment};

const _: () = {
    assert!(std::mem::offset_of!(NativeSegment, saved_sp) == 0);
    assert!(std::mem::offset_of!(NativeSegment, landing_pc) == 8);
    assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
    assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
};

pub(super) fn is_supported() -> bool {
    true
}

pub(super) unsafe fn enter(
    entry: *const u8,
    slots: *mut u64,
    stack: *const EgclStack,
    anchor: *mut NativeSegment,
    out: *mut NativeOutcome,
) {
    // SAFETY: the caller supplies a live generated entry and pinned anchor.
    unsafe { enter_riscv64(entry, slots, stack, anchor, out) }
}

/// Cold transfer exit. The fixed entry frame remains live until its landing
/// path has written the explicit outcome and restored the caller's state.
///
/// # Safety
/// The caller must supply a live native segment and enter through its
/// generated transfer path with the expected LP64D register convention.
#[unsafe(naked)]
pub unsafe extern "C" fn leave_native_segment(
    _anchor: *mut NativeSegment,
    _value: u64,
    _exit: super::NativeExit,
) -> ! {
    core::arch::naked_asm!(
        // a0=anchor, a1=value, a2=NativeExit.
        "ld t0, 0(a0)",
        "sd a1, 240(t0)",
        "sd a2, 248(t0)",
        "ld t1, 8(a0)",
        "mv sp, t0",
        "jr t1",
    );
}

#[unsafe(naked)]
unsafe extern "C" fn enter_riscv64(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: *const EgclStack,
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
) {
    core::arch::naked_asm!(
        // The integrated assembler does not inherit the target's D extension
        // inside a naked function.
        ".option push",
        ".option arch, +d",
        "addi sp, sp, -256",
        "sd ra, 0(sp)",
        "sd s0, 8(sp)",
        "sd s1, 16(sp)",
        "sd s2, 24(sp)",
        "sd s3, 32(sp)",
        "sd s4, 40(sp)",
        "sd s5, 48(sp)",
        "sd s6, 56(sp)",
        "sd s7, 64(sp)",
        "sd s8, 72(sp)",
        "sd s9, 80(sp)",
        "sd s10, 88(sp)",
        "sd s11, 96(sp)",
        "fsd fs0, 104(sp)",
        "fsd fs1, 112(sp)",
        "fsd fs2, 120(sp)",
        "fsd fs3, 128(sp)",
        "fsd fs4, 136(sp)",
        "fsd fs5, 144(sp)",
        "fsd fs6, 152(sp)",
        "fsd fs7, 160(sp)",
        "fsd fs8, 168(sp)",
        "fsd fs9, 176(sp)",
        "fsd fs10, 184(sp)",
        "fsd fs11, 192(sp)",
        "sd a0, 200(sp)",
        "sd a1, 208(sp)",
        "sd a2, 216(sp)",
        "sd a3, 224(sp)",
        "sd a4, 232(sp)",
        // Publish the frame and the common landing address in the anchor.
        "sd sp, 0(a3)",
        "lla t0, 2f",
        "sd t0, 8(a3)",
        // Generated entries use (slots, stack, anchor).
        "ld a0, 208(sp)",
        "ld a1, 216(sp)",
        "ld a2, 224(sp)",
        "ld t0, 200(sp)",
        "jalr ra, t0, 0",
        "sd a0, 240(sp)",
        "sd zero, 248(sp)",
        "j 2f",
        "2:",
        "ld t0, 232(sp)",
        "ld t1, 240(sp)",
        "sd t1, 0(t0)",
        "ld t1, 248(sp)",
        "sd t1, 8(t0)",
        "ld t1, 224(sp)",
        "sd zero, 0(t1)",
        "sd zero, 8(t1)",
        "fld fs0, 104(sp)",
        "fld fs1, 112(sp)",
        "fld fs2, 120(sp)",
        "fld fs3, 128(sp)",
        "fld fs4, 136(sp)",
        "fld fs5, 144(sp)",
        "fld fs6, 152(sp)",
        "fld fs7, 160(sp)",
        "fld fs8, 168(sp)",
        "fld fs9, 176(sp)",
        "fld fs10, 184(sp)",
        "fld fs11, 192(sp)",
        "ld s1, 16(sp)",
        "ld s2, 24(sp)",
        "ld s3, 32(sp)",
        "ld s4, 40(sp)",
        "ld s5, 48(sp)",
        "ld s6, 56(sp)",
        "ld s7, 64(sp)",
        "ld s8, 72(sp)",
        "ld s9, 80(sp)",
        "ld s10, 88(sp)",
        "ld s11, 96(sp)",
        "ld s0, 8(sp)",
        "ld ra, 0(sp)",
        "addi sp, sp, 256",
        "ret",
        ".option pop",
    );
}
