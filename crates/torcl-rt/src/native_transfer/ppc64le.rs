//! ELFv2 ppc64le native-segment boundary.
//!
//! This is the machine half of the transfer ABI.  The compiler-side transfer
//! entry remains x86-only for now; keeping this boundary independently tested
//! lets the Power port validate the dangerous part first: a generated callee
//! may return normally or jump through [`leave_native_segment`] without
//! corrupting ELFv2 nonvolatile state.

use super::{NativeOutcome, NativeSegment, TorclStack};

const _: () = {
    assert!(std::mem::offset_of!(NativeSegment, saved_sp) == 0);
    assert!(std::mem::offset_of!(NativeSegment, landing_pc) == 8);
    assert!(std::mem::offset_of!(NativeOutcome, value) == 0);
    assert!(std::mem::offset_of!(NativeOutcome, exit) == 8);
};

/// ELFv2 has no x86-style shadow-stack policy to query.  The adapter's safety
/// contract is established by the ABI probe below and by the generated-code
/// admission checks; unsupported hardening is therefore not a reason to fall
/// back on this target.
pub(super) fn is_supported() -> bool {
    true
}

/// Enter/leave use one fixed 512-byte ELFv2 frame.  The frame is deliberately
/// larger than the current generated code needs: its stable slots let the
/// landing path restore every nonvolatile GPR/FPR without knowing which tier
/// was active, and leave room for future CR/XER state additions.
pub(super) unsafe fn enter(
    entry: *const u8,
    slots: *mut u64,
    stack: *const TorclStack,
    anchor: *mut NativeSegment,
    out: *mut NativeOutcome,
) {
    unsafe { enter_ppc(entry, slots, stack, anchor, out) }
}

#[allow(improper_ctypes)]
unsafe extern "C" {
    fn enter_ppc(
        entry: *const u8,
        slots: *mut u64,
        stack: *const TorclStack,
        anchor: *mut NativeSegment,
        out: *mut NativeOutcome,
    );
}

/// Cold transfer exit.  `anchor`, `value`, and `exit` arrive in r3-r5 under
/// ELFv2.  The saved entry frame receives the result, restores the entry SP,
/// and lands in the common epilogue in `enter_ppc`.
pub unsafe fn leave_native_segment(
    anchor: *mut NativeSegment,
    value: u64,
    exit: super::NativeExit,
) -> ! {
    unsafe { leave_ppc(anchor, value, exit as u64) }
}

#[allow(improper_ctypes)]
unsafe extern "C" {
    fn leave_ppc(anchor: *mut NativeSegment, value: u64, exit: u64) -> !;
}

// Rust stable inline assembly does not yet accept POWER targets.
// The ELFv2 implementation lives in the sibling `ppc64le.S` file and is compiled by
// build.rs for this target; the ABI contract remains in the declarations above.
