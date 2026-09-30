// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Physical AAPCS64 probes for the native-transfer boundary.
#![cfg(all(target_arch = "aarch64", unix))]

use egcl_rt::native_transfer::{self, NativeExit, NativeSegment};

#[unsafe(naked)]
unsafe extern "C" fn normal_entry() -> u64 {
    core::arch::naked_asm!("ldr x0, [x0]", "ret");
}

#[unsafe(naked)]
unsafe extern "C" fn transfer_entry() -> u64 {
    core::arch::naked_asm!(
        "mov x0, x2", // anchor
        "mov x1, #42",
        "mov x2, #1", // NativeExit::Transfer
        "b {leave}",
        leave = sym native_transfer::leave_native_segment,
    );
}

#[unsafe(naked)]
unsafe extern "C" fn clobber_entry() -> u64 {
    core::arch::naked_asm!(
        "ldr x0, [x0]",
        "mov x19, #1",
        "mov x20, #2",
        "mov x21, #3",
        "mov x22, #4",
        "mov x23, #5",
        "mov x24, #6",
        "mov x25, #7",
        "mov x26, #8",
        "mov x27, #9",
        "mov x28, #10",
        "fmov d8, x19",
        "fmov d9, x20",
        "fmov d10, x21",
        "fmov d11, x22",
        "fmov d12, x23",
        "fmov d13, x24",
        "fmov d14, x25",
        "fmov d15, x26",
        "ret",
    );
}

extern "C" fn invoke_for_register_probe(entry: *const u8, slots: *mut u64) -> u64 {
    let outcome =
        unsafe { native_transfer::invoke_native_segment(entry, slots, egcl_rt::current_stack()) }
            .expect("aarch64 register probe entry");
    outcome.value.0
}

#[unsafe(naked)]
unsafe extern "C" fn register_probe(entry: *const u8, slots: *mut u64) -> u64 {
    core::arch::naked_asm!(
        // Save the caller's complete AAPCS64 nonvolatile set. The generated
        // body deliberately clobbers every member of it.
        "stp x29, x30, [sp, #-240]!", "stp x19, x20, [sp, #16]",
        "stp x21, x22, [sp, #32]", "stp x23, x24, [sp, #48]",
        "stp x25, x26, [sp, #64]", "stp x27, x28, [sp, #80]",
        "stp d8, d9, [sp, #96]", "stp d10, d11, [sp, #112]",
        "stp d12, d13, [sp, #128]", "stp d14, d15, [sp, #144]",
        "mov x19, #101", "mov x20, #102", "mov x21, #103", "mov x22, #104",
        "mov x23, #105", "mov x24, #106", "mov x25, #107", "mov x26, #108",
        "mov x27, #109", "mov x28, #110",
        "mov x12, #201", "fmov d8, x12", "mov x12, #202", "fmov d9, x12",
        "mov x12, #203", "fmov d10, x12", "mov x12, #204", "fmov d11, x12",
        "mov x12, #205", "fmov d12, x12", "mov x12, #206", "fmov d13, x12",
        "mov x12, #207", "fmov d14, x12", "mov x12, #208", "fmov d15, x12",
        "bl {helper}",
        "mov x10, #1",
        "cmp x19, #101", "b.ne 1f", "cmp x20, #102", "b.ne 1f",
        "cmp x21, #103", "b.ne 1f", "cmp x22, #104", "b.ne 1f",
        "cmp x23, #105", "b.ne 1f", "cmp x24, #106", "b.ne 1f",
        "cmp x25, #107", "b.ne 1f", "cmp x26, #108", "b.ne 1f",
        "cmp x27, #109", "b.ne 1f", "cmp x28, #110", "b.ne 1f",
        "fmov x11, d8", "cmp x11, #201", "b.ne 1f",
        "fmov x11, d9", "cmp x11, #202", "b.ne 1f",
        "fmov x11, d10", "cmp x11, #203", "b.ne 1f",
        "fmov x11, d11", "cmp x11, #204", "b.ne 1f",
        "fmov x11, d12", "cmp x11, #205", "b.ne 1f",
        "fmov x11, d13", "cmp x11, #206", "b.ne 1f",
        "fmov x11, d14", "cmp x11, #207", "b.ne 1f",
        "fmov x11, d15", "cmp x11, #208", "b.ne 1f",
        "b 2f",
        "1:", "mov x10, #0",
        "2:",
        "ldp d8, d9, [sp, #96]", "ldp d10, d11, [sp, #112]",
        "ldp d12, d13, [sp, #128]", "ldp d14, d15, [sp, #144]",
        "ldp x19, x20, [sp, #16]", "ldp x21, x22, [sp, #32]",
        "ldp x23, x24, [sp, #48]", "ldp x25, x26, [sp, #64]",
        "ldp x27, x28, [sp, #80]", "mov x0, x10",
        "ldp x29, x30, [sp], #240", "ret",
        helper = sym invoke_for_register_probe,
    );
}

#[test]
fn aapcs64_normal_and_direct_transfer_restore_the_segment() {
    assert!(native_transfer::is_supported());
    let stack = egcl_rt::current_stack();
    let before = (stack.sp(), stack.fp());
    let mut slots = 0x1234_u64;

    let normal = unsafe {
        native_transfer::invoke_native_segment(normal_entry as *const u8, &mut slots, stack)
    }
    .expect("aarch64 normal entry");
    assert_eq!(normal.exit, NativeExit::Returned);
    assert_eq!(normal.value.0, slots);
    assert!(native_transfer::current_segment().is_null());

    let transfer = unsafe {
        native_transfer::invoke_native_segment(transfer_entry as *const u8, &mut slots, stack)
    }
    .expect("aarch64 transfer entry");
    assert_eq!(transfer.exit, NativeExit::Transfer);
    assert_eq!(transfer.value.0, 42);
    assert!(native_transfer::current_segment().is_null());
    assert_eq!((stack.sp(), stack.fp()), before);
}

#[test]
fn native_segment_layout_matches_aapcs64_offsets() {
    assert_eq!(std::mem::offset_of!(NativeSegment, saved_sp), 0);
    assert_eq!(std::mem::offset_of!(NativeSegment, landing_pc), 8);
    assert_eq!(
        std::mem::offset_of!(egcl_rt::native_transfer::NativeOutcome, value),
        0
    );
    assert_eq!(
        std::mem::offset_of!(egcl_rt::native_transfer::NativeOutcome, exit),
        8
    );
}

#[test]
fn aapcs64_preserves_all_callee_saved_registers() {
    assert!(native_transfer::is_supported());
    let mut slots = 0x1234_u64;
    let value = unsafe { register_probe(clobber_entry as *const u8, &mut slots) };
    assert_eq!(value, 1);
}
