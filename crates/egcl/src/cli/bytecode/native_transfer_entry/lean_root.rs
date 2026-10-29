// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The root entry for an ordinary-ABI body (GitHub #487).
//!
//! An ordinary entry exists only for a scope-free body with fixed, untyped
//! positional parameters, no OSR header and no transfer sites. Its activation
//! has nothing for most of the general `run_activation` to do: no `&optional`,
//! `&rest`, `&key` or captured environment to bind, no declared parameter type
//! to check, no catch, cleanup, handler, restart or special binding to
//! reserve, and no OSR resumption to dispatch. This entry does the remaining
//! steps in the same order with the same guards, so deoptimization, errors,
//! non-local exits and the debugger see exactly what the general entry gives
//! them: a pushed managed frame with the arguments bound, the full capture
//! context, the retained callable identity and the native recursive control.
//!
//! What it saves is the general entry's footprint and its repeated lookups:
//! one host root for everything the activation owns instead of six, execution
//! locals looked up once and reused (including by `native_frames`), both
//! fault-recovery IPs read with one slot lookup and left alone when they are
//! already clear, and an entry poll that tests whether there is anything to
//! service before servicing it.

use super::activation::PooledActivation;
use super::*;

/// Root entries taken, for the structural tests: a counter, not a timing.
#[cfg(test)]
static LEAN_ROOT_ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };

#[cfg(test)]
pub(in crate::cli::bytecode) fn take_lean_root_entries_for_test() -> usize {
    LEAN_ROOT_ENTRIES.with(|count| count.replace(0))
}

/// `EGCL_LEAN_ROOT_ENTRY=0` sends ordinary roots back through the general
/// entry, for comparison. Read once.
fn lean_root_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED
        .get_or_init(|| std::env::var_os("EGCL_LEAN_ROOT_ENTRY").is_none_or(|value| value != "0"))
}

impl TransferCode {
    /// The ordinary entry offset, when a root activation of this code may use
    /// [`Self::run_ordinary_root`].
    pub(in crate::cli::bytecode) fn lean_root_entry(&self) -> Option<usize> {
        // Ordinary emission already requires a scope-free body with fixed
        // tagged parameters; restate the facts this entry relies on.
        self.ordinary_entry.filter(|_| {
            !self.body.variadic
                && !self.body.has_env
                && self.body.min_args == self.body.arity
                && self
                    .body
                    .param_types
                    .iter()
                    .all(|ty| matches!(ty, DeclaredType::Any))
                && self.osr_entries.is_empty()
                && lean_root_enabled()
        })
    }

    /// `run_activation` for a root call of an ordinary entry at `offset`.
    /// See the module documentation for what it may leave out.
    #[inline(never)]
    pub(in crate::cli::bytecode) fn run_ordinary_root(
        &self,
        offset: usize,
        function: EgclVal,
        args: &[EgclVal],
        env: &mut Env,
    ) -> Result<EgclVal, EgclError> {
        #[cfg(test)]
        LEAN_ROOT_ENTRIES.with(|count| count.set(count.get() + 1));
        // Nothing below collects before every value is rooted in `roots`.
        if args.len() != usize::from(self.body.arity) {
            return Err(EgclError::ProgramError(
                "native transfer entry: wrong argument count".into(),
            ));
        }
        // Declared first, so it pops the managed frame last, as in the
        // general entry.
        let mut frame_guard = FrameGuard(None);
        let _catch_guard = CatchScopeGuard {
            base: env.catch_stack.len(),
            env,
        };
        // Preparation reserves Rust storage only; it cannot collect.
        let mut state = PooledActivation::new(self, ActivationState::prepare_reusing(self, env)?);
        let mut children = SegmentActivations::default();
        let depth = NATIVE_DEPTH.with(Cell::as_ptr);
        egcl_rt::rooted!(
            roots = EntryRoots {
                function,
                args: EntryArgs::new(args),
                env,
                state: &mut *state,
                children: &mut children,
                entry: None,
            }
        );
        // The rooted copy stays in place; the collector updates it there.
        let args: &[EgclVal] = unsafe { &*std::ptr::from_ref(roots.args.as_slice()) };
        unsafe {
            *depth += 1;
        }
        let _depth = LeanDepthGuard(depth);
        let stack = egcl_rt::current_stack();
        let frame_function = if egcl_rt::function::is_interpreted_function(roots.function) {
            roots.function
        } else {
            self.deopt_metadata
                .as_ref()
                .and_then(|metadata| metadata.deopt_bodies.keys().next().copied())
                .or(self.installed_symbol)
                .map(EgclVal::from_symbol_index)
                .unwrap_or(NIL)
        };
        let frame = stack
            .push_frame(frame_function, self.code_info, self.slots, FLAG_CALL)
            .ok_or_else(|| {
                EgclError::StackOverflow(
                    egcl_rt::current_fiber_id()
                        .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
                )
            })?;
        frame_guard.0 = Some(frame);
        bind_params(&self.body, frame, args, None);
        let _dynamic_scope_guard = DynamicScopeGuard {
            env,
            scopes: &mut state.dynamic_scopes,
        };
        let mut context = state.context(self, frame, &mut children, std::ptr::null_mut());
        let _scope = BlockScopeGuard {
            env,
            blocks: std::mem::take(&mut env.block_stack),
            tags: std::mem::take(&mut env.tag_stack),
        };
        env.clear_mv();
        let _entry = roots.enter(env, &mut context);
        // An enclosing legacy recovery PC describes its own machine frame,
        // never this one or our Rust helpers. Nearly every entry finds both
        // clear and has nothing to save or restore.
        let _recovery = clear_fault_recovery();
        let execution = egcl_rt::thread::current_execution_context_for_native()
            .expect("native segment has a registered execution");
        // The same safe entry poll as the general entry, skipped only when it
        // would find nothing to service.
        if !entry_poll_idle(unsafe { &*execution }) {
            egcl_rt::thread::begin_current_execution_poll();
            egcl_rt::safepoint::poll_safepoint();
            let signal_error = pending_signal_error_for_current_execution();
            egcl_rt::thread::rearm_current_execution_poll();
            if native_error_pending() {
                egcl_rt::thread::request_current_execution_poll();
            }
            if let Some(error) = signal_error {
                return Err(error);
            }
        }
        // A safepoint may have migrated a fiber: the execution context is
        // per execution and stays put, so `execution` is still this one.
        let _ordinary_segment = super::super::call_table::enter_ordinary_segment();
        let _ordinary_root = if self.needs_callable_identity() {
            Some(
                _ordinary_segment
                    .retain_root(self, roots.function)
                    .ok_or_else(invalid_capture)?,
            )
        } else {
            None
        };
        let control = unsafe { native_frames::initialize_for(&mut context, env, execution, depth) };
        // Keep the existing execution-specific emergency stack reserve, as
        // the general entry does; generated child calls check their own bounds.
        let sp: usize;
        unsafe {
            std::arch::asm!("mov {}, rsp", out(reg) sp, options(nomem, nostack, preserves_flags));
        }
        let overflow_bytes = args
            .len()
            .saturating_sub(3)
            .checked_mul(8)
            .and_then(|size| size.checked_add(15))
            .map(|size| size & !15)
            .ok_or_else(invalid_capture)?;
        let required = self
            .ordinary_stack_required
            .expect("ordinary stack admission bound")
            .checked_add(4096)
            .and_then(|size| size.checked_add(overflow_bytes))
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
        let outcome = unsafe {
            let execution = &*execution;
            execution.with_active_native_control(
                std::pin::Pin::new_unchecked(&mut *control),
                || {
                    #[cfg(test)]
                    if let Some(observe) = ORDINARY_ENTRY_HOOK.with(|hook| hook.take()) {
                        observe();
                    }
                    native_transfer::invoke_ordinary_native_segment(
                        self.code.as_ptr().add(offset),
                        execution,
                        NIL,
                        args,
                        stack,
                    )
                },
            )
        }
        .map(|returned| returned.outcome)
        .map_err(|_| EgclError::Internal("native segment is unavailable".into()))?;
        // No Lisp allocation between the machine return and rooting its value
        // or taking ownership of the execution-local error.
        let primary = outcome.value;
        let error = take_native_error();
        if outcome.exit == NativeExit::Returned
            && error.is_none()
            && context.failure.is_none()
            && context.osr.is_none()
            && context.selected.is_none()
            && state.is_quiescent()
        {
            return Ok(primary);
        }
        egcl_rt::rooted!(error = error);
        // An ordinary body has no OSR header and no transfer site, so the
        // general entry's OSR and selected-transfer exits cannot arise here.
        if context.failure.is_some() || context.osr.is_some() {
            return Err(invalid_capture());
        }
        match outcome.exit {
            NativeExit::Transfer if context.recursive_escape => {
                // Every scope-free generated activation has been retired.
                error.take().ok_or_else(invalid_capture).and_then(Err)
            }
            NativeExit::Transfer => unsafe {
                self.resume_transfer(
                    &mut context,
                    &mut frame_guard.0,
                    env,
                    error.take().ok_or_else(invalid_capture)?,
                )
            },
            _ => Err(invalid_capture()),
        }
    }
}

/// The execution-local state an entry replaces, restored when it ends.
pub(super) struct EntryState {
    env_slot: *const Cell<*mut Env>,
    capture_slot: *const Cell<*mut CaptureContext>,
    env: *mut Env,
    capture: *mut CaptureContext,
    error: Option<EgclError>,
}

impl Drop for EntryState {
    fn drop(&mut self) {
        // Execution-local cells stay at one address for the execution's life,
        // across fiber migration.
        unsafe {
            (*self.env_slot).set(self.env);
            (*self.capture_slot).set(self.capture);
        }
        restore_native_error(self.error.take());
    }
}

/// Every host root of a native entry, linked once: the callable, a copy of the
/// actuals, the caller's Env, the activation state and child ledger, and the
/// enclosing pending error the entry holds aside while it runs.
pub(super) struct EntryRoots {
    pub(super) function: EgclVal,
    pub(super) args: EntryArgs,
    pub(super) env: *mut Env,
    pub(super) state: *mut ActivationState,
    pub(super) children: *mut SegmentActivations,
    pub(super) entry: Option<EntryState>,
}

impl EntryRoots {
    /// Install `env` and `context` as this execution's NATIVE_ENV and
    /// CAPTURE, holding the enclosing values and pending error aside (rooted
    /// here) until the returned guard drops. Nested legacy callees and other
    /// executions must not overwrite this invocation's capture pointer or an
    /// enclosing pending error.
    pub(super) fn enter(&mut self, env: *mut Env, context: *mut CaptureContext) -> EntryRestore {
        let env_slot = NATIVE_ENV.with(std::ptr::from_ref);
        let capture_slot = CAPTURE.with(std::ptr::from_ref);
        self.entry = Some(EntryState {
            env_slot,
            capture_slot,
            env: unsafe { (*env_slot).replace(env) },
            capture: unsafe { (*capture_slot).replace(context) },
            error: take_native_error(),
        });
        EntryRestore(&mut self.entry)
    }
}

impl egcl_rt::gc::TraceHostRoots for EntryRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.function);
        self.args.trace_host_roots(visit);
        // The entry declares the state and child ledger before linking this
        // root and drops them after unlinking it; Env outlives the entry.
        unsafe {
            (*self.env).trace_host_roots(visit);
            (*self.state).trace_host_roots(visit);
            (*self.children).trace_host_roots(visit);
        }
        if let Some(entry) = &mut self.entry {
            entry.error.trace_host_roots(visit);
        }
    }
}

/// Restores what [`EntryRoots::enter`] replaced, at the point the entry
/// declared it. Taking the state out runs no Lisp and cannot collect.
pub(super) struct EntryRestore(*mut Option<EntryState>);

impl Drop for EntryRestore {
    fn drop(&mut self) {
        drop(unsafe { (*self.0).take() });
    }
}

struct LeanDepthGuard(*mut u32);
impl Drop for LeanDepthGuard {
    fn drop(&mut self) {
        unsafe { *self.0 = (*self.0).saturating_sub(1) };
    }
}
