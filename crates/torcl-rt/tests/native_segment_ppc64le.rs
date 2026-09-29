//! ELFv2 physical segment probes for the ppc64le native-transfer boundary.
#![cfg(all(
    target_arch = "powerpc64",
    target_endian = "little",
    target_os = "linux"
))]

use torcl_rt::native_transfer::{self, NativeExit, NativeSegment};

unsafe extern "C" {
    fn ppc_probe_normal() -> u64;
    fn ppc_probe_transfer() -> u64;
}

#[test]
fn elfv2_normal_and_direct_transfer_restore_the_segment() {
    assert!(native_transfer::is_supported());
    let stack = torcl_rt::current_stack();
    let before = (stack.sp(), stack.fp());
    let mut slots = 0x1234_u64;

    let normal = unsafe {
        native_transfer::invoke_native_segment(ppc_probe_normal as *const u8, &mut slots, stack)
    }
    .expect("ppc64le normal entry");
    assert_eq!(normal.exit, NativeExit::Returned);
    assert_eq!(normal.value.0, slots);
    assert!(native_transfer::current_segment().is_null());

    let transfer = unsafe {
        native_transfer::invoke_native_segment(ppc_probe_transfer as *const u8, &mut slots, stack)
    }
    .expect("ppc64le transfer entry");
    assert_eq!(transfer.exit, NativeExit::Transfer);
    assert_eq!(transfer.value.0, 42);
    assert!(native_transfer::current_segment().is_null());
    assert_eq!((stack.sp(), stack.fp()), before);
}

#[test]
fn native_segment_layout_matches_elfv2_offsets() {
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
