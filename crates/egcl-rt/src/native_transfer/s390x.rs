//! s390x ELF64 native-segment boundary.
//!
//! The generated entry receives `(slots, stack, anchor)` in r2-r4. The fixed
//! frame saves the ELF64 callee-saved GPRs r6-r13 and FP registers f8-f15.

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
    unsafe { enter_s390x(entry, slots, stack, anchor, out) }
}

/// Cold transfer exit. No Rust frame may be crossed by this path.
#[unsafe(naked)]
pub unsafe extern "C" fn leave_native_segment(
    _anchor: *mut NativeSegment,
    _value: u64,
    _exit: super::NativeExit,
) -> ! {
    core::arch::naked_asm!("b {leave}", leave = sym leave_s390x);
}

#[allow(improper_ctypes)]
unsafe extern "C" {
    fn enter_s390x(
        entry: *const u8,
        slots: *mut u64,
        stack: *const EgclStack,
        anchor: *mut NativeSegment,
        out: *mut NativeOutcome,
    );
    fn leave_s390x(anchor: *mut NativeSegment, value: u64, exit: u64) -> !;
}
