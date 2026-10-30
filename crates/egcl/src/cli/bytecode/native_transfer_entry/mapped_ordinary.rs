// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reusable ordinary call state below a published mapped activation.
//!
//! The mapped caller remains published for the entire call. The existing
//! ordinary assembly shim owns a *new per-call segment and cold landing*, but
//! no managed callee frame is pushed. All ordinary frames therefore reconstruct
//! as native children. In particular, their escape cannot discard the mapped
//! parent's catches, bindings or Rust publication frame.
use super::*;
use egcl_rt::gc::TraceHostRoots;

pub(super) type Lanes = Vec<std::pin::Pin<Box<Bridge>>>;

/// This box belongs to one live mapped activation, not to a global cache or a
/// code pool. Its self-referential context points only into separately boxed
/// state/children, which never move while the parent is rooted.
pub(super) struct Bridge {
    code: Rc<TransferCode>,
    body: super::super::ActiveBytecodeRoot,
    state: Option<Box<ActivationState>>,
    children: Box<SegmentActivations>,
    context: Box<CaptureContext>,
    function: EgclVal,
    args: EntryArgs,
    instance: u32,
    busy: bool,
}

impl TraceHostRoots for Bridge {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        self.body.trace_host_roots(visit);
        if let Some(state) = &mut self.state {
            state.trace_host_roots(visit);
        }
        self.children.trace_host_roots(visit);
        visit(&mut self.function);
        self.args.trace_host_roots(visit);
        if self.busy {
            super::super::ActiveCallableRoot(self.instance).trace_host_roots(visit);
        }
    }
}

impl Bridge {
    fn new(
        code: Rc<TransferCode>,
        env: &mut Env,
        frame: *mut Frame,
    ) -> Result<Box<Self>, EgclError> {
        // Rust-only reservations, on the mapped caller's published side.
        let mut state = ActivationState::prepare_reusing(&code, env)?;
        let mut children = Box::<SegmentActivations>::default();
        let mut context =
            Box::new(state.context(&code, frame, &mut *children, std::ptr::null_mut()));
        context.ordinary_bridge = true;
        // Borrow the REAL mapped parent frame only as the logical/debug
        // attachment. No managed callee frame is created or enriched; the
        // explicit bridge marker always selects native-child reconstruction.
        context.activation = std::ptr::null_mut();
        let body = super::super::ActiveBytecodeRoot::new(&code.body);
        #[cfg(test)]
        PREPARES.with(|count| count.set(count.get() + 1));
        Ok(Box::new(Self {
            code,
            body,
            state: Some(state),
            children,
            context,
            function: NIL,
            args: EntryArgs::new(&[]),
            instance: u32::MAX,
            busy: false,
        }))
    }
}

impl Drop for Bridge {
    fn drop(&mut self) {
        // The parent has retired the lane; no context can still reference its
        // ledgers. Return only quiescent buffers to the existing code pool.
        if let Some(state) = self.state.take() {
            state.recycle(&self.code);
        }
    }
}

#[cfg(test)]
thread_local! {
    static ENTRIES: Cell<usize> = const { Cell::new(0) };
    static PREPARES: Cell<usize> = const { Cell::new(0) };
}
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_entries_for_test() -> usize {
    ENTRIES.with(|count| count.replace(0))
}
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_prepares_for_test() -> usize {
    PREPARES.with(|count| count.replace(0))
}

/// Restore execution pointers and depth even when a checked Rust helper panics.
/// The parent roots the bridge throughout; this guard is not another root list.
struct Restore {
    capture_slot: *const Cell<*mut CaptureContext>,
    previous: *mut CaptureContext,
    bridge: *mut Bridge,
    depth: *mut u32,
    old_depth: u32,
}
impl Drop for Restore {
    fn drop(&mut self) {
        unsafe {
            (*self.capture_slot).set(self.previous);
            *self.depth = self.old_depth;
            (*self.bridge).busy = false;
            (*self.bridge).function = NIL;
            (*self.bridge).args = EntryArgs::new(&[]);
            (*self.bridge).state.as_mut().unwrap().env_frame = None;
        }
    }
}

/// The caller has already performed lean_native_target's exact current-target,
/// arity, depth and entry-cohort checks. It keeps published_mapped live around
/// this function. None means no state changed and the old root entry must run.
pub(super) unsafe fn call(
    code: &Rc<TransferCode>,
    offset: usize,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Option<Result<EgclVal, EgclError>> {
    let parent = CAPTURE.with(Cell::get);
    if parent.is_null() || !current_poll_idle() || native_error_pending() {
        return None;
    }
    // Only a managed mapped activation owns this reusable lane. An ordinary
    // activation or a compatibility reentry keeps its authoritative old path.
    if unsafe { (*parent).ordinary_bridge || (*(*parent).owner).ordinary_entry.is_some() }
        || !code.body.self_call_sites.is_empty()
    {
        return None;
    }
    let slot = unsafe { (*parent).mapped_ordinary };
    if slot.is_null() || args.len() != usize::from(code.body.arity) {
        return None;
    }
    if code.native_callable_slot.is_some() && !code.accepts_explicit_callable(function) {
        return None;
    }
    let execution = egcl_rt::thread::current_execution_context_for_native()?;
    // No Rust borrow of the GC-scanned cache survives an allocating callee.
    const LANES: usize = 4;
    let cached = unsafe {
        (&*slot)
            .iter()
            .position(|bridge| Rc::ptr_eq(&bridge.code, code))
    };
    let index = match cached {
        Some(index) => {
            if unsafe { (&*slot)[index].busy } {
                return None;
            }
            index
        }
        None => {
            if unsafe { (&*slot).len() } == LANES {
                return None;
            }
            let frame = unsafe { (*parent).frame };
            let bridge = match Bridge::new(Rc::clone(code), env, frame) {
                Ok(bridge) => bridge,
                Err(error) => return Some(Err(error)),
            };
            let index = unsafe { (&*slot).len() };
            unsafe { (&mut *slot).push(Box::into_pin(bridge)) };
            index
        }
    };
    let bridge = unsafe { (&mut *slot)[index].as_mut().get_mut() as *mut Bridge };
    // OSR may replace/grow the parent managed extent between calls.
    unsafe { (*bridge).context.frame = (*parent).frame };
    Some(unsafe { run(bridge, offset, function, args, env, execution) })
}

unsafe fn run(
    bridge: *mut Bridge,
    offset: usize,
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
    execution: *const egcl_rt::exec_context::LispExecutionContext,
) -> Result<EgclVal, EgclError> {
    // Keep an immutable code lease independently of the GC-scanned bridge.
    let code = unsafe { Rc::clone(&(*bridge).code) };
    // Caller publication owns input words; copy while no Lisp can allocate.
    unsafe {
        (*bridge).function = function;
        (*bridge).args = EntryArgs::new(args);
        (*bridge).instance = if code.native_callable_slot.is_some()
            && egcl_rt::function::is_interpreted_function(function)
        {
            egcl_rt::function::definition_index(function)
        } else {
            code.installed_symbol.unwrap_or(u32::MAX)
        };
        (*bridge).busy = true;
        (*bridge).context.recursive_escape = false;
        (*bridge).state.as_mut().unwrap().env_frame = if code.native_callable_slot.is_some() {
            closure_captured_env(function)
        } else {
            None
        };
    }
    let capture_slot = CAPTURE.with(std::ptr::from_ref);
    let context = unsafe { std::ptr::from_mut(&mut *(*bridge).context) };
    let previous = unsafe { (*capture_slot).replace(context) };
    let depth = NATIVE_DEPTH.with(Cell::as_ptr);
    let old_depth = unsafe { *depth };
    let _restore = Restore {
        capture_slot,
        previous,
        bridge,
        depth,
        old_depth,
    };
    unsafe { *depth += 1 };
    let control = if unsafe { (*context).native_recursive.is_none() } {
        unsafe { native_frames::initialize_for(context, env, execution, depth) }
    } else {
        unsafe { (*context).native_recursive.as_mut().unwrap() as *mut _ }
    };
    unsafe {
        (*control).anchor_depth = *depth;
    }
    // A reused control's signal/carrier snapshots deliberately stay stale after
    // migration or notification: ordinary admission services the normal cold
    // poll before refreshing them. Never acknowledge producer epochs here.
    let sp: usize;
    unsafe {
        std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags))
    };
    let overflow = args
        .len()
        .saturating_sub(3)
        .checked_mul(8)
        .and_then(|n| n.checked_add(15))
        .map(|n| n & !15)
        .ok_or_else(invalid_capture)?;
    let required = code
        .ordinary_stack_required
        .ok_or_else(invalid_capture)?
        .checked_add(4096)
        .and_then(|n| n.checked_add(overflow))
        .ok_or_else(invalid_capture)?;
    if sp
        .checked_sub(required)
        .is_none_or(|bottom| bottom as u64 <= unsafe { (*control).stack_limit })
    {
        return Err(EgclError::StackOverflow(
            egcl_rt::current_fiber_id()
                .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
        ));
    }
    let _catch_guard = CatchScopeGuard {
        base: env.catch_stack.len(),
        env,
    };
    let _scope = BlockScopeGuard {
        env,
        blocks: std::mem::take(&mut env.block_stack),
        tags: std::mem::take(&mut env.tag_stack),
    };
    env.clear_mv();
    let directory = super::super::call_table::enter_ordinary_segment();
    let _identity = if code.needs_callable_identity() && code.native_callable_slot.is_none() {
        Some(
            directory
                .retain_root(&code, unsafe { (*bridge).function })
                .ok_or_else(invalid_capture)?,
        )
    } else {
        None
    };
    let _recovery = clear_fault_recovery();
    let outcome = unsafe {
        (&*execution).with_active_native_control(
            std::pin::Pin::new_unchecked(&mut *control),
            || {
                #[cfg(test)]
                {
                    ENTRIES.with(|count| count.set(count.get() + 1));
                    if let Some(observe) = ORDINARY_ENTRY_HOOK.with(|hook| hook.take()) {
                        observe();
                    }
                }
                native_transfer::invoke_ordinary_native_segment(
                    code.code.as_ptr().add(offset),
                    &*execution,
                    if code.native_callable_slot.is_some() {
                        (*bridge).function
                    } else {
                        NIL
                    },
                    (*bridge).args.as_slice(),
                    egcl_rt::current_stack(),
                )
            },
        )
    }
    .map_err(|_| EgclError::Internal("ordinary mapped bridge segment is unavailable".into()))?
    .outcome;
    let error = take_native_error();
    if outcome.exit == NativeExit::Returned
        && error.is_none()
        && unsafe { (*bridge).state.as_ref().unwrap().is_quiescent() }
    {
        return Ok(outcome.value);
    }
    error.ok_or_else(invalid_capture).and_then(Err)
}
