// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A Rust-to-published-callable boundary with no synthetic Lisp activation.
//! All Lisp words live in explicit host roots. The machine shim retains only
//! stable pointers and registers its exact suspended call geometry.

use super::*;
use egcl_rt::call_table::NativeCallContext;
use egcl_rt::native_transfer::{NativeSegment, current_segment};
use std::cell::Cell;

#[cfg(test)]
#[path = "outer/return_tests.rs"]
mod return_tests;

#[repr(C)]
struct Invocation {
    context: NativeCallContext,
    function: *mut EgclVal,
    entry: usize,
    segment: *mut NativeSegment,
    call_sp: usize,
    return_pc: usize,
    #[cfg(test)]
    caller_segment: usize,
    #[cfg(test)]
    caller_capture: Option<(usize, bool)>,
    #[cfg(test)]
    previous_invocation: *mut Invocation,
}

#[cfg(test)]
pub(in crate::cli::bytecode) type SuspendedCallers = Vec<(usize, Option<(usize, bool)>)>;

#[cfg(test)]
pub(in crate::cli::bytecode) fn suspended_callers_for_test() -> Option<SuspendedCallers> {
    let mut invocation = ACTIVE.with(Cell::get);
    let mut segment = current_segment();
    if invocation.is_null() || segment.is_null() {
        return None;
    }
    if unsafe { (*invocation).segment } != segment {
        return None; // an actual mapped child has its own current capture
    }
    let mut callers = Vec::new();
    loop {
        let current = unsafe { &*invocation };
        assert_eq!(
            current.segment, segment,
            "outer invocation owns this exact segment"
        );
        let previous_segment = unsafe { (*segment).previous() };
        assert_eq!(previous_segment as usize, current.caller_segment);
        callers.push((current.caller_segment, current.caller_capture));
        if current.caller_capture.is_some() || previous_segment.is_null() {
            break;
        }
        // Another Rust adapter can stand between this adapter and mapped code.
        // Follow the saved ACTIVE predecessor, verifying its identity against
        // the actual segment link before reading its captured caller snapshot.
        invocation = current.previous_invocation;
        assert!(
            !invocation.is_null(),
            "suspended outer caller must retain its invocation"
        );
        segment = previous_segment;
    }
    Some(callers)
}

static ACTIVE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut Invocation>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };

struct ActiveInvocation(*mut Invocation);
impl Drop for ActiveInvocation {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(self.0));
    }
}

pub(super) fn invoke(
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(function = function);
    egcl_rt::rooted!(args = args.to_vec());
    let entry = ordinary_entry().ok_or_else(|| {
        EgclError::Internal("ordinary native callable entry is unavailable".into())
    })?;
    let mut invocation = Invocation {
        context: NativeCallContext {
            // Assembly installs its actual segment anchor before calling the
            // adapter. Its cold tail jump supplies this to leave_native_segment.
            request: std::ptr::null_mut(),
            capture: egcl_rt::native_transfer::leave_native_segment as *const u8,
            args: args.as_mut_ptr(),
            nargs: args.len(),
        },
        function: std::ptr::from_mut(&mut *function),
        entry: entry as usize,
        segment: std::ptr::null_mut(),
        call_sp: 0,
        return_pc: 0,
        #[cfg(test)]
        caller_segment: current_segment() as usize,
        #[cfg(test)]
        caller_capture: native_transfer_entry::child_capture_for_test(),
        #[cfg(test)]
        previous_invocation: ACTIVE.with(Cell::get),
    };
    let _active = ActiveInvocation(ACTIVE.with(|active| active.replace(&mut invocation)));
    unsafe { native_transfer_entry::invoke_outer_ordinary(entry, *function, &args, env) }
}

// The ordinary root shim owns the segment's machine geometry. Keep the
// invocation chain for diagnostics while no mapped adapter record is involved.
pub(super) fn observe_ordinary_entry() {
    let invocation = ACTIVE.with(Cell::get);
    assert!(
        !invocation.is_null(),
        "ordinary outer entry owns its invocation"
    );
    unsafe {
        (*invocation).segment = current_segment();
    }
}

/// Recognize only the registered outer adapter record. Matching a context is
/// insufficient: validate its actual segment, return PC and pre-CALL stack
/// position before exempting this rootless machine shim from a Lisp frame walk.
///
/// # Safety
/// `record` must be the live record constructed by a published callable adapter.
pub(in crate::cli::bytecode) unsafe fn owns_record(record: *mut MappedCallRecord) -> bool {
    let invocation = ACTIVE.with(Cell::get);
    if invocation.is_null()
        || unsafe { (*record).context != std::ptr::addr_of_mut!((*invocation).context) }
    {
        return false;
    }
    let invocation = unsafe { &*invocation };
    let segment = current_segment();
    assert!(!segment.is_null());
    assert_eq!(
        invocation.segment, segment,
        "outer record must belong to the active segment"
    );
    let caller = record as usize + egcl_compiler::t2::native_transfer::ADAPTER_FRAME_BYTES;
    assert_eq!(
        unsafe { (caller as *const usize).read() },
        invocation.return_pc
    );
    assert_eq!(caller + 8, invocation.call_sp);
    assert_eq!(invocation.call_sp + 16, unsafe { (*segment).saved_sp });
    assert_eq!(invocation.context.request.cast::<NativeSegment>(), segment);
    true
}
