// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LP64D: ra, s0-s11, fs0-fs11 and the floating-point CSR. The thread
//! pointer (tp) and global pointer (gp) belong to the carrier and are never
//! switched.
//!
//! Frame (208 bytes, 16-byte aligned): ra at 0, s0-s11 at 8..104, fs0-fs11 at
//! 104..200, fcsr at 200.

use super::Context;

/// Save the current stack and resume another live context.
///
/// # Safety
/// See [`super::swap`]: both contexts and their stacks must remain live.
#[inline]
pub unsafe fn swap(from: *mut Context, to: Context) {
    unsafe { context_swap(from, to) }
}

#[unsafe(naked)]
unsafe extern "C" fn context_swap(_from: *mut Context, _to: Context) {
    core::arch::naked_asm!(
        // The integrated assembler does not inherit the target's D extension
        // inside a naked function; enable it for this block.
        ".option push",
        ".option arch, +d",
        "addi sp, sp, -208",
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
        "csrr t0, fcsr",
        "sd t0, 200(sp)",
        "sd sp, 0(a0)",
        "mv sp, a1",
        "ld t0, 200(sp)",
        "csrw fcsr, t0",
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
        "ld ra, 0(sp)",
        "ld s0, 8(sp)",
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
        "addi sp, sp, 208",
        "ret",
        ".option pop",
    );
}

/// Construct a context whose entry must never return.
pub fn make(stack: &mut [u8], entry: extern "C" fn()) -> Context {
    let sp = super::initial_frame(stack, 208, 0);
    unsafe {
        // The first swap into this context `ret`s through the saved ra.
        sp.cast::<usize>().write(entry as usize);
        let fcsr: u64;
        core::arch::asm!("csrr {}, fcsr", out(reg) fcsr, options(nomem, nostack));
        sp.add(200).cast::<u64>().write(fcsr);
    }
    sp
}
