//! Physical AAPCS64 probes for the native-transfer boundary.
#![cfg(all(target_arch = "aarch64", unix))]

use torcl_rt::native_transfer::{self, NativeExit, NativeSegment};

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

#[test]
fn aapcs64_normal_and_direct_transfer_restore_the_segment() {
    assert!(native_transfer::is_supported());
    let stack = torcl_rt::current_stack();
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
        std::mem::offset_of!(torcl_rt::native_transfer::NativeOutcome, value),
        0
    );
    assert_eq!(
        std::mem::offset_of!(torcl_rt::native_transfer::NativeOutcome, exit),
        8
    );
}
