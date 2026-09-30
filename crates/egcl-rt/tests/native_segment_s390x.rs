// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Physical ELF64 s390x probes for the native-transfer boundary.
#![cfg(all(target_arch = "s390x", unix))]

use egcl_rt::native_transfer::{self, NativeExit, NativeSegment};

unsafe extern "C" {
    fn s390x_probe_normal() -> u64;
    fn s390x_probe_transfer() -> u64;
    fn s390x_probe_registers() -> u64;
    fn s390x_probe_clobber() -> u64;
}

#[unsafe(no_mangle)]
extern "C" fn s390x_probe_call() -> u64 {
    let mut slots = 0x1234_u64;
    let outcome = unsafe {
        native_transfer::invoke_native_segment(
            s390x_probe_clobber as *const u8,
            &mut slots,
            egcl_rt::current_stack(),
        )
    }
    .expect("s390x register probe entry");
    outcome.value.0
}

#[test]
fn s390x_normal_and_direct_transfer_restore_the_segment() {
    assert!(native_transfer::is_supported());
    let stack = egcl_rt::current_stack();
    let before = (stack.sp(), stack.fp());
    let mut slots = 0x1234_u64;

    let normal = unsafe {
        native_transfer::invoke_native_segment(s390x_probe_normal as *const u8, &mut slots, stack)
    }
    .expect("s390x normal entry");
    assert_eq!(normal.exit, NativeExit::Returned);
    assert_eq!(normal.value.0, slots);
    assert!(native_transfer::current_segment().is_null());

    let transfer = unsafe {
        native_transfer::invoke_native_segment(s390x_probe_transfer as *const u8, &mut slots, stack)
    }
    .expect("s390x transfer entry");
    assert_eq!(transfer.exit, NativeExit::Transfer);
    assert_eq!(transfer.value.0, 42);
    assert!(native_transfer::current_segment().is_null());
    assert_eq!((stack.sp(), stack.fp()), before);
}

#[test]
fn native_segment_layout_matches_s390x_offsets() {
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
fn s390x_preserves_the_elf64_callee_saved_gprs() {
    assert!(native_transfer::is_supported());
    assert_eq!(unsafe { s390x_probe_registers() }, 1);
}
