// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned control for generated recursive machine frames. Runtime
//! entry runs once per outer activation; recursive calls visit Rust only when
//! a fast check identifies work or dispatch must select another definition.

use super::*;
use egcl_compiler::t2::emit::{NativeRecursiveCallRequest, NativeRecursiveControl};

pub(super) fn flags() -> Option<egcl_compiler::t2::emit::NativeRecursiveFlags> {
    let (gc_requested, sigusr1_pending) = egcl_rt::safepoint::native_poll_flag_addresses()?;
    Some(egcl_compiler::t2::emit::NativeRecursiveFlags {
        gc_requested: gc_requested as u64,
        sigusr1_pending: sigusr1_pending as u64,
        posted_signal: egcl_rt::thread::posted_pending_signals_address() as u64,
        native_errors: std::ptr::addr_of!(PENDING_NATIVE_ERRORS) as u64,
        signal_epoch: egcl_rt::runtime::process_signal_activity_address() as u64,
        generation: std::ptr::addr_of!(DIRECT_CALL_GEN) as u64,
        instrument_entries: cfg!(test),
        observe_forces_cold: cfg!(test),
    })
}

/// A selected transfer leaves this entire scope-free machine-frame chain.
/// The owning Rust entry still retires its own depth through NativeDepthGuard.
pub(super) fn retire_children(context: &CaptureContext) {
    if let Some(control) = &context.native_recursive {
        NATIVE_DEPTH.with(|depth| depth.set(control.anchor_depth));
    }
}

/// Called before the public entry loads its arguments. The managed anchor
/// already roots those arguments and the exact callable. This does not poll,
/// allocate Lisp objects, or yield. ExecutionLocal boxes and the anchor remain
/// at stable addresses if the running fiber later changes carriers.
pub(super) unsafe extern "C" fn initialize() -> *mut NativeRecursiveControl {
    let context = CAPTURE.with(Cell::get);
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!context.is_null() && !env.is_null());
    let execution = egcl_rt::thread::current_execution_context_for_native()
        .expect("native segment has a registered execution");
    let depth = NATIVE_DEPTH.with(Cell::as_ptr);
    let function = unsafe { (*(*context).frame).function };
    let invoke_count = if unsafe { (*context).recursive_profile } {
        // FunctionData is pinned; its exact owner stays rooted in the anchor.
        egcl_rt::function::invocation_counter_address(function)
            .expect("profiled recursive owner is an interpreted callable")
    } else {
        std::ptr::null()
    };
    let control = NativeRecursiveControl {
        execution,
        depth,
        invoke_count,
        mv_active: unsafe { std::ptr::addr_of!((*env).mv_active) },
        expected_generation: unsafe { (*context).recursive_generation },
        seen_carrier: unsafe { &*execution }.carrier_id(),
        seen_signal_epoch: egcl_rt::runtime::current_seen_signal_activity()
            .expect("native segment has signal activity state"),
        depth_limit: native_depth_cap(),
        anchor_depth: unsafe { *depth },
        deadline_armed: u8::from(egcl_rt::sandbox_cpu_deadline_armed()),
        reserved: [0; 7],
        test_entries: std::ptr::null_mut(),
        test_observer: std::ptr::null(),
        stack_limit: egcl_rt::stack::current_native_stack_limit(),
    };
    unsafe { (*context).native_recursive = Some(control); }
    let control = unsafe { (*context).native_recursive.as_mut().unwrap() };
    #[cfg(test)]
    {
        control.test_entries = RECURSIVE_ENTRIES.with(Cell::as_ptr);
        control.test_observer = root_publication::OBSERVE.with(Cell::as_ptr).cast();
    }
    control
}

/// Refresh carrier snapshots only after a published poll has returned. Copy
/// the carrier's *consumed* epoch, never the global producer epoch: an arrival
/// racing this refresh must still force the next generated check to go cold.
unsafe fn refresh(context: *mut CaptureContext) {
    let control = unsafe { (*context).native_recursive.as_mut().unwrap() };
    control.seen_carrier = unsafe { &*control.execution }.carrier_id();
    control.seen_signal_epoch = egcl_rt::runtime::current_seen_signal_activity()
        .expect("native segment has signal activity state");
    control.deadline_armed = u8::from(egcl_rt::sandbox_cpu_deadline_armed());
    control.expected_generation = unsafe { (*context).recursive_generation };
}

/// Service a failed generated entry check with suspended native roots
/// published. `admitted = 1` permits generated admission after servicing the
/// checks; only generated code commits the child and increments counters.
pub(super) unsafe extern "C" fn slow_call(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    unsafe {
        root_publication::published(image, || {
            let request = &mut *request.cast::<NativeRecursiveCallRequest>();
            request.admitted = 0;
            let context = CAPTURE.with(Cell::get);
            let expected = (*context).native_recursive.as_mut()
                .map(std::ptr::from_mut).expect("native recursive control");
            assert_eq!(request.control, expected);
            assert_eq!(Some(request.call.symbol as u32), (*(*context).owner).recursive_symbol);
            assert_eq!(request.call.nargs, usize::from((*(*context).body).arity));
            #[cfg(test)]
            root_publication::observe();
            poll_or_transfer_unpublished(std::ptr::null_mut(), out);
            if (*out).exit != NativeExit::Returned {
                return;
            }
            let matches = recursive_target_matches(context, request.call.symbol as u32);
            refresh(context);
            let limit = (*context).native_recursive.as_ref().unwrap().stack_limit;
            // The cold request occupies 48 bytes below the caller's body SP.
            // Budget the child's exact prologue and CALL return word just as
            // the generated fast check does; a retry must not bypass that guard.
            let frame_bytes = (*(*context).owner)._native_calls.unwind_recipe()
                .expect("native recursive unwind recipe").frame_bytes() as usize;
            let child_sp = ((*image).caller_sp as usize)
                .checked_add(std::mem::size_of::<NativeRecursiveCallRequest>())
                .and_then(|sp| sp.checked_sub(frame_bytes + 8));
            if !matches || NATIVE_DEPTH.with(Cell::get) >= native_depth_cap()
                || child_sp.is_none_or(|sp| sp as u64 <= limit)
            {
                c2i_call_legacy_v2(std::ptr::from_mut(&mut request.call).cast(), out);
                return;
            }
            c2i_clear_mv();
            request.admitted = 1;
            out.write(NativeOutcome { value: NIL, exit: NativeExit::Returned });
        });
    }
}
