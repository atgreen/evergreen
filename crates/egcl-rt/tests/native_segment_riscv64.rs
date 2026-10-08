// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Physical LP64D probes for the native-transfer boundary on riscv64.
#![cfg(all(target_arch = "riscv64", unix))]

use egcl_rt::native_transfer::{self, NativeExit, NativeSegment};

#[unsafe(naked)]
unsafe extern "C" fn normal_entry() -> u64 {
    core::arch::naked_asm!("ld a0, 0(a0)", "ret");
}

#[unsafe(naked)]
unsafe extern "C" fn transfer_entry() -> u64 {
    core::arch::naked_asm!(
        "mv a0, a2", // anchor
        "li a1, 42",
        "li a2, 1", // NativeExit::Transfer
        "tail {leave}",
        leave = sym native_transfer::leave_native_segment,
    );
}

/// Clobbers every LP64D callee-saved register the boundary must preserve.
#[unsafe(naked)]
unsafe extern "C" fn clobber_entry() -> u64 {
    core::arch::naked_asm!(
        ".option push",
        ".option arch, +d",
        "ld a0, 0(a0)",
        "li s1, 1",
        "li s2, 2",
        "li s3, 3",
        "li s4, 4",
        "li s5, 5",
        "li s6, 6",
        "li s7, 7",
        "li s8, 8",
        "li s9, 9",
        "li s10, 10",
        "li s11, 11",
        "fmv.d.x fs0, s1",
        "fmv.d.x fs1, s2",
        "fmv.d.x fs2, s3",
        "fmv.d.x fs3, s4",
        "fmv.d.x fs4, s5",
        "fmv.d.x fs5, s6",
        "fmv.d.x fs6, s7",
        "fmv.d.x fs7, s8",
        "fmv.d.x fs8, s9",
        "fmv.d.x fs9, s10",
        "fmv.d.x fs10, s11",
        "fmv.d.x fs11, s1",
        "ret",
        ".option pop",
    );
}

extern "C" fn invoke_for_register_probe(entry: *const u8, slots: *mut u64) -> u64 {
    let outcome =
        unsafe { native_transfer::invoke_native_segment(entry, slots, egcl_rt::current_stack()) }
            .expect("riscv64 register probe entry");
    outcome.value.0
}

/// Sets the complete callee-saved set to known values, enters the boundary
/// (whose body clobbers all of them), and returns 1 only if every one came
/// back intact.
#[unsafe(naked)]
unsafe extern "C" fn register_probe(entry: *const u8, slots: *mut u64) -> u64 {
    core::arch::naked_asm!(
        ".option push",
        ".option arch, +d",
        "addi sp, sp, -208",
        "sd ra, 0(sp)",
        "sd s0, 8(sp)",
        "sd s1, 16(sp)", "sd s2, 24(sp)", "sd s3, 32(sp)", "sd s4, 40(sp)",
        "sd s5, 48(sp)", "sd s6, 56(sp)", "sd s7, 64(sp)", "sd s8, 72(sp)",
        "sd s9, 80(sp)", "sd s10, 88(sp)", "sd s11, 96(sp)",
        "fsd fs0, 104(sp)", "fsd fs1, 112(sp)", "fsd fs2, 120(sp)", "fsd fs3, 128(sp)",
        "fsd fs4, 136(sp)", "fsd fs5, 144(sp)", "fsd fs6, 152(sp)", "fsd fs7, 160(sp)",
        "fsd fs8, 168(sp)", "fsd fs9, 176(sp)", "fsd fs10, 184(sp)", "fsd fs11, 192(sp)",
        "li s1, 101", "li s2, 102", "li s3, 103", "li s4, 104", "li s5, 105",
        "li s6, 106", "li s7, 107", "li s8, 108", "li s9, 109", "li s10, 110",
        "li s11, 111",
        "li t0, 201", "fmv.d.x fs0, t0", "li t0, 202", "fmv.d.x fs1, t0",
        "li t0, 203", "fmv.d.x fs2, t0", "li t0, 204", "fmv.d.x fs3, t0",
        "li t0, 205", "fmv.d.x fs4, t0", "li t0, 206", "fmv.d.x fs5, t0",
        "li t0, 207", "fmv.d.x fs6, t0", "li t0, 208", "fmv.d.x fs7, t0",
        "li t0, 209", "fmv.d.x fs8, t0", "li t0, 210", "fmv.d.x fs9, t0",
        "li t0, 211", "fmv.d.x fs10, t0", "li t0, 212", "fmv.d.x fs11, t0",
        "call {helper}",
        "li t1, 1",
        "li t0, 101", "bne s1, t0, 1f", "li t0, 102", "bne s2, t0, 1f",
        "li t0, 103", "bne s3, t0, 1f", "li t0, 104", "bne s4, t0, 1f",
        "li t0, 105", "bne s5, t0, 1f", "li t0, 106", "bne s6, t0, 1f",
        "li t0, 107", "bne s7, t0, 1f", "li t0, 108", "bne s8, t0, 1f",
        "li t0, 109", "bne s9, t0, 1f", "li t0, 110", "bne s10, t0, 1f",
        "li t0, 111", "bne s11, t0, 1f",
        "fmv.x.d t2, fs0", "li t0, 201", "bne t2, t0, 1f",
        "fmv.x.d t2, fs1", "li t0, 202", "bne t2, t0, 1f",
        "fmv.x.d t2, fs2", "li t0, 203", "bne t2, t0, 1f",
        "fmv.x.d t2, fs3", "li t0, 204", "bne t2, t0, 1f",
        "fmv.x.d t2, fs4", "li t0, 205", "bne t2, t0, 1f",
        "fmv.x.d t2, fs5", "li t0, 206", "bne t2, t0, 1f",
        "fmv.x.d t2, fs6", "li t0, 207", "bne t2, t0, 1f",
        "fmv.x.d t2, fs7", "li t0, 208", "bne t2, t0, 1f",
        "fmv.x.d t2, fs8", "li t0, 209", "bne t2, t0, 1f",
        "fmv.x.d t2, fs9", "li t0, 210", "bne t2, t0, 1f",
        "fmv.x.d t2, fs10", "li t0, 211", "bne t2, t0, 1f",
        "fmv.x.d t2, fs11", "li t0, 212", "bne t2, t0, 1f",
        "j 2f",
        "1:",
        "li t1, 0",
        "2:",
        "fld fs0, 104(sp)", "fld fs1, 112(sp)", "fld fs2, 120(sp)", "fld fs3, 128(sp)",
        "fld fs4, 136(sp)", "fld fs5, 144(sp)", "fld fs6, 152(sp)", "fld fs7, 160(sp)",
        "fld fs8, 168(sp)", "fld fs9, 176(sp)", "fld fs10, 184(sp)", "fld fs11, 192(sp)",
        "ld s1, 16(sp)", "ld s2, 24(sp)", "ld s3, 32(sp)", "ld s4, 40(sp)",
        "ld s5, 48(sp)", "ld s6, 56(sp)", "ld s7, 64(sp)", "ld s8, 72(sp)",
        "ld s9, 80(sp)", "ld s10, 88(sp)", "ld s11, 96(sp)",
        "ld s0, 8(sp)",
        "ld ra, 0(sp)",
        "mv a0, t1",
        "addi sp, sp, 208",
        "ret",
        ".option pop",
        helper = sym invoke_for_register_probe,
    );
}

#[test]
fn lp64d_normal_and_direct_transfer_restore_the_segment() {
    assert!(native_transfer::is_supported());
    let stack = egcl_rt::current_stack();
    let before = (stack.sp(), stack.fp());
    let mut slots = 0x1234_u64;

    let normal = unsafe {
        native_transfer::invoke_native_segment(normal_entry as *const u8, &mut slots, stack)
    }
    .expect("riscv64 normal entry");
    assert_eq!(normal.exit, NativeExit::Returned);
    assert_eq!(normal.value.0, slots);
    assert!(native_transfer::current_segment().is_null());

    let transfer = unsafe {
        native_transfer::invoke_native_segment(transfer_entry as *const u8, &mut slots, stack)
    }
    .expect("riscv64 transfer entry");
    assert_eq!(transfer.exit, NativeExit::Transfer);
    assert_eq!(transfer.value.0, 42);
    assert!(native_transfer::current_segment().is_null());
    assert_eq!((stack.sp(), stack.fp()), before);
}

#[test]
fn native_segment_layout_matches_lp64d_offsets() {
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
fn lp64d_preserves_all_callee_saved_registers() {
    assert!(native_transfer::is_supported());
    let mut slots = 0x1234_u64;
    let value = unsafe { register_probe(clobber_entry as *const u8, &mut slots) };
    assert_eq!(value, 1);
}
