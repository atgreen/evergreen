// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::native_transfer::MappedCallRecord;
use egcl_rt::gc::TraceHostRoots;

/// The outer segment roots this container throughout machine execution. Boxes
/// keep context addresses stable when another child is prepared or retired.
#[derive(Default)]
pub(super) struct SegmentActivations {
    #[allow(clippy::vec_box)] // generated records and CAPTURE retain these addresses
    active: Vec<Box<ChildActivation>>,
}
impl TraceHostRoots for SegmentActivations {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for child in &mut self.active {
            child.snapshots.trace_host_roots(visit);
            child.cleanups.trace_host_roots(visit);
            child.prepared_handler.trace_host_roots(visit);
            child.prepared_catch.trace_host_roots(visit);
            child.restart_templates.trace_host_roots(visit);
        }
    }
}

struct ChildActivation {
    code: Rc<TransferCode>,
    parent: *mut CaptureContext,
    context: Option<CaptureContext>,
    snapshots: Vec<SysvSiteSnapshot>,
    cleanups: Vec<SavedCleanup>,
    catches: Vec<SavedCatch>,
    handlers: Vec<SavedHandler>,
    handler_binds: Vec<SavedHandlerBind>,
    restart_cases: Vec<SavedRestartCase>,
    restart_templates: RestartFunctionTemplates,
    dynamic_scopes: Vec<DynamicScope>,
    cluster_frames: Vec<(u32, *mut Frame)>,
    prepared_handler: Option<PreparedHandler>,
    prepared_catch: Option<PreparedCatch>,
}

#[cfg(test)]
static NESTED_ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_nested_entries() -> usize {
    NESTED_ENTRIES.with(|count| count.replace(0))
}

#[cfg(test)]
static FAIL_NEXT_CAPTURE: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };
#[cfg(test)]
pub(super) fn fail_next_capture() {
    FAIL_NEXT_CAPTURE.with(|flag| flag.set(true));
}
#[cfg(test)]
pub(super) fn take_capture_failure() -> bool {
    FAIL_NEXT_CAPTURE.with(|flag| flag.replace(false))
}

/// Called only by the explicit mapped-call veneer. A live outer segment alone
/// is not evidence that an arbitrary Rust/legacy caller may use this entry.
pub(super) unsafe extern "C" fn prepare_nested(cell: u64, record: *mut MappedCallRecord) {
    unsafe {
        (*record).entry = std::ptr::null();
        (*record).activation = std::ptr::null_mut();
        (*record).owner = std::ptr::null_mut();
        (*record).outcome = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        poll_or_transfer(std::ptr::null_mut(), &mut (*record).outcome);
        if (*record).outcome.exit != NativeExit::Returned {
            return;
        }
    }
    let result = guard_c2i(|| unsafe { prepare_child(cell, record) });
    if let Err(error) = result {
        NATIVE_ERROR.with(|slot| slot.set_first(error));
        unsafe {
            (*record).outcome.exit = NativeExit::Transfer;
        }
    }
}

unsafe fn prepare_child(cell: u64, record: *mut MappedCallRecord) -> Result<EgclVal, EgclError> {
    let here = std::ptr::from_ref(&record) as usize as u64;
    let limit = egcl_rt::stack::NATIVE_STACK_LIMIT.load(std::sync::atomic::Ordering::Acquire);
    if NATIVE_DEPTH.with(|depth| depth.get() >= native_depth_cap()) || here <= limit {
        return Ok(NIL); // clean legacy depth fallback, not an exceptional outcome
    }
    let parent = CAPTURE.with(Cell::get);
    if parent.is_null() {
        return Err(invalid_capture());
    }
    let Some(mut target) = (unsafe { call_table::mapped_callee(cell) }) else {
        return Ok(NIL);
    };
    egcl_rt::rooted_ref!(_target = &mut target);
    let code = Rc::clone(&target.code);
    let request = unsafe { &*(*record).request };
    if request.nargs != usize::from(code.body.arity) {
        return Ok(NIL);
    }
    let args = if request.nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(request.args, request.nargs) }
    };
    validate_declared_args(&code.body, args)?;
    let snapshots = code
        .sites
        .sites()
        .map(|site| site.reserve_snapshot())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| invalid_capture())?;
    let children = unsafe { (*parent).children };
    // No borrow of the scanner's container spans compilation or Lisp work.
    unsafe { (*children).active.try_reserve(1) }.map_err(|_| EgclError::Oom)?;
    let mut child = Box::new(ChildActivation {
        code,
        parent,
        context: None,
        snapshots,
        cleanups: Vec::new(),
        catches: Vec::new(),
        handlers: Vec::new(),
        handler_binds: Vec::new(),
        restart_cases: Vec::new(),
        restart_templates: RestartFunctionTemplates(Vec::new()),
        dynamic_scopes: Vec::new(),
        cluster_frames: Vec::new(),
        prepared_handler: None,
        prepared_catch: None,
    });
    let stack = egcl_rt::current_stack();
    let Some(frame) = stack.push_frame(
        target.function,
        std::ptr::null(),
        child.code.slots,
        FLAG_CALL,
    ) else {
        return Ok(NIL);
    };
    // Fixed positional slots only. No operation from here through publication
    // can allocate, signal, yield or fail, so no partial frame can escape.
    bind_params(&child.code.body, frame, args, None);
    child.context = Some(CaptureContext {
        children,
        nested: record,
        owner: Rc::as_ptr(&child.code),
        completed_deopt: false,
        recursive_enabled: false,
        recursive_generation: 0,
        recursive: std::ptr::null_mut(),
        recursive_escape: false,
        handlers: &mut child.handlers,
        handler_binds: &mut child.handler_binds,
        restart_cases: &mut child.restart_cases,
        restart_templates: &child.restart_templates,
        dynamic_scopes: &mut child.dynamic_scopes,
        cluster_frames: &mut child.cluster_frames,
        prepared_handler: &mut child.prepared_handler,
        #[cfg(test)]
        unavailable_catch: None,
        #[cfg(test)]
        unavailable_handler: None,
        prepared_catch: &mut child.prepared_catch,
        frame,
        body: child.code.body.as_ref(),
        catches: &mut child.catches,
        completed_cleanup: None,
        landing_stub: child.code.landing.as_ptr(),
        landing: SysvNativeLanding {
            stack_pointer: std::ptr::null_mut(),
            entry: std::ptr::null(),
        },
        dispatch: DispatchPacket {
            entry: std::ptr::null(),
            request: std::ptr::null_mut(),
        },
        cleanups: &mut child.cleanups,
        cleanup_depths: &child.code.cleanup_depths,
        code_base: child.code.code.as_ptr() as usize,
        sites: &child.code.sites,
        snapshots: child.snapshots.as_mut_ptr().cast(),
        activation: unsafe { frame.add(1).cast() },
        slots: usize::from(child.code.slots),
        selected: None,
        failure: None,
    });
    let context = child.context.as_mut().unwrap() as *mut CaptureContext;
    let owner = &mut *child as *mut ChildActivation;
    unsafe {
        (*record).entry = child.code.code.as_ptr();
        (*record).activation = frame.add(1).cast();
        (*record).owner = owner.cast();
        (*children).active.push(child);
    }
    CAPTURE.with(|slot| slot.set(context));
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
    if !profiling_disabled() {
        egcl_rt::function::record_invocation(target.function);
    }
    c2i_clear_mv();
    #[cfg(test)]
    NESTED_ENTRIES.with(|count| count.set(count.get() + 1));
    Ok(NIL)
}

/// Both normal and cold returns execute this callback from caller-owned code.
/// Pop the actual child frame (whose previous fp includes any caller clusters),
/// restore the parent context, then release the child's code and root owners.
pub(super) unsafe extern "C" fn finish_nested(record: *mut MappedCallRecord) {
    let owner = unsafe { (*record).owner.cast::<ChildActivation>() };
    let child = unsafe { &mut *owner };
    let context = child.context.as_mut().unwrap();
    assert_eq!(CAPTURE.with(Cell::get), context as *mut CaptureContext);
    let stack = egcl_rt::current_stack();
    assert_eq!(stack.fp(), context.frame);
    let children = context.children;
    let parent = child.parent;
    if let Some(failure) = context.failure.take() {
        unsafe {
            (*parent).failure.get_or_insert(failure);
        }
    }
    stack.pop_frame();
    CAPTURE.with(|slot| slot.set(parent));
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() - 1));
    let retired = unsafe { (*children).active.pop() }.expect("active child owner");
    assert!(std::ptr::eq(&*retired, owner));
    unsafe {
        (*record).owner = std::ptr::null_mut();
    }
    drop(retired);
}
