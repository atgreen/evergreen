// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64: x19-x29, LR, the low halves of v8-v15, and FP environment.
//! x18 (platform register) and the carrier's thread pointer are never switched.

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
        "stp x19, x20, [sp, #-176]!",
        "stp x21, x22, [sp, #16]",
        "stp x23, x24, [sp, #32]",
        "stp x25, x26, [sp, #48]",
        "stp x27, x28, [sp, #64]",
        "stp x29, x30, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "mrs x9, fpcr",
        "mrs x10, fpsr",
        "stp x9, x10, [sp, #160]",
        "mov x9, sp",
        "str x9, [x0]",
        "mov sp, x1",
        "ldp x9, x10, [sp, #160]",
        "msr fpcr, x9",
        "msr fpsr, x10",
        "ldp d8, d9, [sp, #96]",
        "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]",
        "ldp d14, d15, [sp, #144]",
        "ldp x21, x22, [sp, #16]",
        "ldp x23, x24, [sp, #32]",
        "ldp x25, x26, [sp, #48]",
        "ldp x27, x28, [sp, #64]",
        "ldp x29, x30, [sp, #80]",
        "ldp x19, x20, [sp], #176",
        "ret",
    );
}

/// Construct a context whose entry must never return.
pub fn make(stack: &mut [u8], entry: extern "C" fn()) -> Context {
    let sp = super::initial_frame(stack, 176, 0);
    unsafe {
        sp.add(88).cast::<usize>().write(entry as usize);
        let fpcr: u64;
        let fpsr: u64;
        core::arch::asm!("mrs {}, fpcr", "mrs {}, fpsr", out(reg) fpcr, out(reg) fpsr,
            options(nomem, nostack, preserves_flags));
        sp.add(160).cast::<u64>().write(fpcr);
        sp.add(168).cast::<u64>().write(fpsr);
    }
    sp
}
