// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! AAPCS64 native-segment boundary.
//!
//! Generated entries receive `(slots, stack, anchor)` in `x0`–`x2`.
//! `enter_aarch64` preserves the AAPCS64 callee-saved registers in a fixed
//! frame. A generated body may return normally or call the cold leave helper.

use super::{NativeOutcome, NativeSegment, EgclStack};

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
    unsafe { enter_aarch64(entry, slots, stack, anchor, out) }
}

/// Cold transfer exit. The fixed entry frame remains live until its landing
/// path has written the explicit outcome and restored the caller's state.
///
/// # Safety
/// The caller must supply a live native segment and enter through its
/// generated transfer path with the expected AArch64 register convention.
#[unsafe(naked)]
pub unsafe extern "C" fn leave_native_segment(
    _anchor: *mut NativeSegment,
    _value: u64,
    _exit: super::NativeExit,
) -> ! {
    core::arch::naked_asm!(
        // x0=anchor, x1=value, x2=NativeExit.
        "ldr x9, [x0, #0]",
        "str x1, [x9, #216]",
        "str x2, [x9, #224]",
        "ldr x10, [x0, #8]",
        "mov sp, x9",
        "br x10",
    );
}

#[unsafe(naked)]
unsafe extern "C" fn enter_aarch64(
    _entry: *const u8,
    _slots: *mut u64,
    _stack: *const EgclStack,
    _anchor: *mut NativeSegment,
    _out: *mut NativeOutcome,
) {
    core::arch::naked_asm!(
        // 240 bytes, 16-byte aligned. The fixed frame holds x19-x28, d8-d15,
        // incoming pointers, and the eventual outcome.
        "stp x29, x30, [sp, #-240]!",
        "mov x29, sp",
        "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]",
        "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]",
        "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]",
        "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]",
        "stp d14, d15, [sp, #144]",
        "str x0, [sp, #176]",
        "str x1, [sp, #184]",
        "str x2, [sp, #192]",
        "str x3, [sp, #200]",
        "str x4, [sp, #208]",
        "mov x9, sp",
        "str x9, [x3, #0]",
        "adr x9, 2f",
        "str x9, [x3, #8]",
        // Generated entries use (slots, stack, anchor).
        "ldr x0, [sp, #184]",
        "ldr x1, [sp, #192]",
        "ldr x2, [sp, #200]",
        "ldr x16, [sp, #176]",
        "blr x16",
        "str x0, [sp, #216]",
        "mov x9, #0",
        "str x9, [sp, #224]",
        "b 2f",
        "2:",
        "ldr x9, [sp, #208]",
        "ldr x10, [sp, #216]",
        "str x10, [x9, #0]",
        "ldr x10, [sp, #224]",
        "str x10, [x9, #8]",
        "ldr x11, [sp, #200]",
        "str xzr, [x11, #0]",
        "str xzr, [x11, #8]",
        "ldp d8, d9, [sp, #96]",
        "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]",
        "ldp d14, d15, [sp, #144]",
        "ldp x19, x20, [sp, #16]",
        "ldp x21, x22, [sp, #32]",
        "ldp x23, x24, [sp, #48]",
        "ldp x25, x26, [sp, #64]",
        "ldp x27, x28, [sp, #80]",
        "ldp x29, x30, [sp], #240",
        "ret",
    );
}
