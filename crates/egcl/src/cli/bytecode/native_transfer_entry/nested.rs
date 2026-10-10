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
    pending_arguments: Vec<PendingArguments>,
}
impl TraceHostRoots for SegmentActivations {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for child in &mut self.active {
            child.state.trace_host_roots(visit);
        }
        for arguments in &mut self.pending_arguments {
            arguments.values.trace_host_roots(visit);
        }
    }
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn pending_argument_count() -> usize {
    let capture = CAPTURE.with(Cell::get);
    if capture.is_null() {
        0
    } else {
        unsafe { (*(*capture).children).pending_arguments.len() }
    }
}

/// The context is stable caller-owned stack storage. The values' allocation
/// stays fixed until callable preparation binds a frame or takes Rust ownership.
struct PendingArguments {
    context: *mut egcl_rt::call_table::NativeCallContext,
    values: Vec<EgclVal>,
}

pub(in crate::cli::bytecode) unsafe fn take_call_arguments(
    record: *mut MappedCallRecord,
) -> Option<Vec<EgclVal>> {
    let capture = CAPTURE.with(Cell::get);
    if capture.is_null() {
        return None;
    }
    let context = unsafe { (*record).context };
    let pending = unsafe { &mut (*(*capture).children).pending_arguments };
    let index = pending
        .iter()
        .position(|arguments| arguments.context == context)?;
    let arguments = pending.swap_remove(index);
    unsafe {
        assert_eq!((*context).args, arguments.values.as_ptr().add(1).cast_mut());
        (*context).args = std::ptr::null_mut();
        (*context).nargs = 0;
        (*record).target = 0;
    }
    Some(arguments.values)
}

unsafe fn prepare_apply(record: *mut MappedCallRecord) -> Result<EgclVal, EgclError> {
    let capture = CAPTURE.with(Cell::get);
    if capture.is_null() {
        return Err(invalid_capture());
    }
    let context = unsafe { (*record).context };
    let invocation = unsafe { &mut *context };
    let args = unsafe { std::slice::from_raw_parts(invocation.args, invocation.nargs) };
    let Some(entries) = super::super::native_callable::entries_for(args[0]) else {
        return Ok(NIL);
    };
    // The same list expansion as interpreted APPLY. Only Rust storage allocates
    // here; root it before registering the buffer for the next entry's poll.
    egcl_rt::rooted!(values = args[..args.len() - 1].to_vec());
    values.extend(list_to_vec(args[args.len() - 1]));
    let pending = unsafe { &mut (*(*capture).children).pending_arguments };
    assert!(!pending.iter().any(|arguments| arguments.context == context));
    pending.try_reserve(1).map_err(|_| EgclError::Oom)?;
    let target = values.as_mut_ptr();
    invocation.args = unsafe { target.add(1) };
    invocation.nargs = values.len() - 1;
    pending.push(PendingArguments {
        context,
        values: std::mem::take(&mut *values),
    });
    unsafe {
        (*record).target = target as u64;
        (*record).forward = entries;
    }
    Ok(NIL)
}

struct ChildActivation {
    code: Rc<TransferCode>,
    parent: *mut CaptureContext,
    context: Option<CaptureContext>,
    state: ActivationState,
    frame: Option<*mut Frame>,
    previous_frame: *const Frame,
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
    prepare_call(record, || unsafe {
        let invocation = &mut *(*record).context;
        if call_table::is_builtin_funcall(cell) && invocation.nargs != 0 {
            if let Some(entries) = super::super::native_callable::entries_for(*invocation.args) {
                (*record).target = invocation.args as u64;
                (*record).forward = entries;
                invocation.args = invocation.args.add(1);
                invocation.nargs -= 1;
                return Ok(NIL);
            }
        }
        if call_table::is_builtin_apply(cell) && invocation.nargs >= 2 {
            return prepare_apply(record);
        }
        prepare_child(record, || call_table::mapped_callee(cell))
    });
}

pub(in crate::cli::bytecode) unsafe extern "C" fn prepare_callable(
    target: u64,
    record: *mut MappedCallRecord,
) {
    #[cfg(test)]
    super::super::native_callable::observe_entry();
    #[cfg(test)]
    super::super::native_callable::inject_entry_poll_error();
    prepare_call(record, || unsafe {
        #[cfg(test)]
        super::super::native_callable::collect_at_entry(target)?;
        if super::super::native_callable::outer::owns_record(record) {
            // An outer Rust boundary has no mapped caller activation. Its
            // permanent adapter dispatches the selected callable; a compiled
            // body reached from that Rust dispatcher owns a separate segment.
            return Ok(NIL);
        }
        let invocation = &*(*record).context;
        // The target word is a stable address into caller-scanned storage.
        // Read after polling, then root while code selection may allocate.
        egcl_rt::rooted!(function = *(target as *const EgclVal));
        prepare_child(record, || {
            call_table::mapped_callable(*function, invocation.nargs)
        })
    });
}

fn prepare_call(
    record: *mut MappedCallRecord,
    prepare: impl FnOnce() -> Result<EgclVal, EgclError>,
) {
    // Preparation polls, compiles and allocates on behalf of a caller that is
    // suspended beneath this adapter frame. The record carries that caller's
    // exact geometry, so publish it for the whole extent.
    unsafe { super::root_publication::published_mapped(record, || prepare_call_published(record, prepare)) }
}

fn prepare_call_published(
    record: *mut MappedCallRecord,
    prepare: impl FnOnce() -> Result<EgclVal, EgclError>,
) {
    unsafe {
        (*record).entry = std::ptr::null();
        (*record).activation = std::ptr::null_mut();
        (*record).owner = std::ptr::null_mut();
        (*record).forward = std::ptr::null();
        (*record).outcome = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        // Already published by prepare_call from the adapter record.
        poll_or_transfer_unpublished(std::ptr::null_mut(), &mut (*record).outcome);
        if (*record).outcome.exit != NativeExit::Returned {
            drop(take_call_arguments(record));
            return;
        }
    }
    let result = guard_c2i(prepare);
    if let Err(error) = result {
        NATIVE_ERROR.with(|slot| slot.set_first(error));
        unsafe {
            (*record).outcome.exit = NativeExit::Transfer;
            drop(take_call_arguments(record));
        }
    }
}

unsafe fn prepare_child(
    record: *mut MappedCallRecord,
    select: impl FnOnce() -> Option<call_table::MappedCallee>,
) -> Result<EgclVal, EgclError> {
    let here = std::ptr::from_ref(&record) as usize as u64;
    let limit = egcl_rt::stack::NATIVE_STACK_LIMIT.load(std::sync::atomic::Ordering::Acquire);
    if NATIVE_DEPTH.with(|depth| depth.get() >= native_depth_cap()) || here <= limit {
        return Ok(NIL); // clean legacy depth fallback, not an exceptional outcome
    }
    let parent = CAPTURE.with(Cell::get);
    if parent.is_null() {
        return Err(invalid_capture());
    }
    let Some(mut target) = select() else {
        return Ok(NIL);
    };
    egcl_rt::rooted_ref!(_target = &mut target);
    let code = Rc::clone(&target.code);
    let invocation = unsafe { &*(*record).context };
    if invocation.nargs != usize::from(code.body.arity) {
        return Ok(NIL);
    }
    let args = if invocation.nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(invocation.args, invocation.nargs) }
    };
    validate_declared_args(&code.body, args)?;
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let state = ActivationState::prepare(&code, env)?;
    let children = unsafe { (*parent).children };
    unsafe { (*children).active.try_reserve(1) }.map_err(|_| EgclError::Oom)?;
    let mut child = Box::new(ChildActivation {
        code,
        parent,
        context: None,
        state,
        frame: None,
        previous_frame: egcl_rt::current_stack().fp(),
    });
    let stack = egcl_rt::current_stack();
    let Some(frame) = stack.push_frame(
        target.function,
        child.code.code_info,
        child.code.slots,
        FLAG_CALL,
    ) else {
        return Ok(NIL);
    };
    // Fixed positional slots only. No operation from here through publication
    // can allocate, signal, yield or fail, so no partial frame can escape.
    bind_params(&child.code.body, frame, args, None);
    // The scanned frame now owns independent copies of every argument.
    drop(unsafe { take_call_arguments(record) });
    child.frame = Some(frame);
    child.context = Some(child.state.context(&child.code, frame, children, record));
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
pub(in crate::cli::bytecode) unsafe extern "C" fn finish_nested(record: *mut MappedCallRecord) {
    let owner = unsafe { (*record).owner.cast::<ChildActivation>() };
    let child = unsafe { &mut *owner };
    let context = child.context.as_mut().unwrap();
    assert_eq!(CAPTURE.with(Cell::get), context as *mut CaptureContext);
    let stack = egcl_rt::current_stack();
    let children = context.children;
    let parent = child.parent;
    if let Some(failure) = context.failure.take() {
        unsafe {
            (*parent).failure.get_or_insert(failure);
        }
    }
    // Scope ownership may already have moved to the interpreter. Its handoff
    // clears these ledgers before Lisp runs, preventing duplicate retirement.
    let env = NATIVE_ENV.with(Cell::get);
    drop(DynamicScopeGuard {
        env,
        scopes: &mut child.state.dynamic_scopes,
    });
    drop(CatchScopeGuard {
        env,
        base: child.state.catch_base,
    });
    if let Some(frame) = child.frame.take() {
        assert_eq!(stack.fp(), frame);
        stack.pop_frame();
    }
    assert_eq!(stack.fp(), child.previous_frame);
    CAPTURE.with(|slot| slot.set(parent));
    NATIVE_DEPTH.with(|depth| depth.set(depth.get() - 1));
    let retired = unsafe { (*children).active.pop() }.expect("active child owner");
    assert!(std::ptr::eq(&*retired, owner));
    unsafe {
        (*record).owner = std::ptr::null_mut();
    }
    drop(retired);
}

/// Code owning the PC the adapter saved for this record's caller.
///
/// While a child is live, `CAPTURE` still names the CHILD's activation — the
/// parent is only restored by `finish_nested`, which runs after the resuming
/// Lisp. So a record-based publication must take its owner from the record's
/// own child activation, or it would pair the caller's PC with the callee's
/// code and the walk would reject its first frame.
///
/// # Safety
/// Only valid once preparation has written `record.owner`. The adapter's
/// prologue does not initialize that word, so calling this earlier reads
/// uninitialized stack and may dereference garbage.
pub(super) unsafe fn record_parent_owner(
    record: *mut MappedCallRecord,
) -> Option<*const TransferCode> {
    let child = unsafe { (*record).owner.cast::<ChildActivation>() };
    if child.is_null() {
        return None; // no child selected yet: the caller is CAPTURE's own owner
    }
    let parent = unsafe { (*child).parent };
    if parent.is_null() {
        return None;
    }
    let owner = unsafe { (*parent).owner };
    (!owner.is_null()).then_some(owner)
}

/// The child's machine frames are already gone. Resume its captured logical
/// continuation through Rust before deciding whether the caller must unwind.
pub(in crate::cli::bytecode) unsafe extern "C" fn resume_nested(record: *mut MappedCallRecord) {
    // Cold resumption runs Lisp. The child's machine frames are gone, so the
    // record's saved caller is the right publication for this extent — and the
    // child is still live here, so the owner comes from the record, not CAPTURE.
    unsafe {
        super::root_publication::published_mapped_resuming(record, || {
            resume_nested_published(record)
        })
    }
}

unsafe fn resume_nested_published(record: *mut MappedCallRecord) {
    let owner = unsafe { (*record).owner.cast::<ChildActivation>() };
    egcl_rt::rooted!(
        result = guard_c2i(|| {
            // Retain only stable pointers and an independent code handle across
            // allocating Lisp; the root scanner may mutate the child's state.
            let code = unsafe { Rc::clone(&(*owner).code) };
            let context = unsafe { (*owner).context.as_mut().unwrap() as *mut CaptureContext };
            if unsafe { (*context).failure.is_some() } {
                return Err(invalid_capture());
            }
            let error = NATIVE_ERROR
                .with(|slot| slot.take())
                .ok_or_else(invalid_capture)?;
            // Completed T0 recovery must not replay the child's continuation.
            // Scope-free children likewise have nothing left to reconstruct.
            if unsafe { (*context).recursive_escape } || scope_free_native_body(&code.body, true) {
                return Err(error);
            }
            unsafe {
                code.resume_transfer(
                    context,
                    &mut (*owner).frame,
                    &mut *NATIVE_ENV.with(Cell::get),
                    error,
                )
            }
        })
    );
    unsafe {
        finish_nested(record);
    }
    let outcome = match std::mem::replace(&mut *result, Ok(NIL)) {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe {
        (*record).outcome = outcome;
    }
}
