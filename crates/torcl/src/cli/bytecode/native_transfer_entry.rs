//! Opt-in runtime entry for the tagged Invoke emitter. Normal installation is
//! still gated on complete helper/poll/scope coverage. Own code and definitions,
//! capture into rooted snapshots, enter native cleanup for selected throws, and
//! retain bytecode *unwinding* as the fallback.
#![allow(dead_code)]

use super::*;
use std::cell::Cell;
use torcl_compiler::control_scope::{Ownership, ScopeKind};
use torcl_compiler::native_unwind::{NativeUnwindStep, SelectedTarget, next_unwind_step};
use torcl_compiler::t2::native_transfer::{
    SysvNativeLanding, SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
    emit_native_landing_stub,
};
use torcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable, TransferSiteError};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer::{self, NativeExit};

/// Only constructible through the host-specific emitter. Retaining this value
/// retains every embedded code address and the original bytecode definition.
pub(super) struct TransferCode {
    body: Arc<BytecodeFunction>,
    _body_roots: ActiveBytecodeRoot,
    code: JitBuffer,
    _veneer: JitBuffer,
    _capture: JitBuffer,
    _completion: JitBuffer,
    landing: JitBuffer,
    sites: SysvTransferTable,
    slots: u16,
    cleanup_depths: std::collections::HashMap<u32, usize>,
}

impl TransferCode {
    pub(super) fn compile(body: Arc<BytecodeFunction>) -> Option<Self> {
        if body.variadic || body.has_env {
            return None;
        }
        // These scopes need executing identity, dynamic values or inherited
        // state. Do not invent fresh tokens during recovery for live closures.
        if body.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::PushBlock { register: true, .. } | Instr::NamedTag { .. }
            )
        }) {
            return None;
        }
        let roots = ActiveBytecodeRoot::new(&body);
        let ir = torcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body).ok()?;
        let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8))?;
        let veneer = JitBuffer::new(&emit_helper_veneer(call_or_throw, capture.as_ptr()))?;
        let base_slots = body.n_locals.checked_add(body.max_stack)?;
        let completion = JitBuffer::new(&emit_helper_veneer(complete_cleanup, capture.as_ptr()))?;
        let landing = JitBuffer::new(&emit_native_landing_stub())?;
        let (emitted, sites) = torcl_compiler::t2::emit::emit_framed_native_cleanups(
            &ir,
            veneer.as_ptr() as u64,
            base_slots,
            save_cleanup as *const () as u64,
            completion.as_ptr() as u64,
            c2i_clear_mv as *const () as u64,
        )
        .ok()?;
        // Reconstruct local, non-escaping BLOCK/TAGBODY records and pending
        // UNWIND-PROTECT cleanups from exact scope maps. Other records require the
        // forthcoming native scope state; rejecting them is part of admission.
        for site in sites.sites() {
            if site.map().control_scopes.iter().any(|scope| {
                scope.ownership != Ownership::Local
                    || !matches!(
                        scope.kind,
                        ScopeKind::Block {
                            register: false,
                            ..
                        } | ScopeKind::Tagbody { .. }
                            | ScopeKind::Unwind { .. }
                            | ScopeKind::Cleanup { .. }
                            | ScopeKind::Catch { .. }
                    )
            }) {
                return None;
            }
        }
        let scopes = torcl_compiler::control_scope::ScopeMap::analyze_function(&body).ok()?;
        let mut cleanup_depths = std::collections::HashMap::new();
        for instruction in &body.code {
            if let Instr::PushUnwind { cleanup_bcp, .. } = instruction {
                let depth = scopes
                    .before(*cleanup_bcp)?
                    .iter()
                    .filter(|scope| {
                        matches!(
                            scope.kind,
                            ScopeKind::Block { .. }
                                | ScopeKind::Tagbody { .. }
                                | ScopeKind::Unwind { .. }
                                | ScopeKind::Catch { .. }
                        )
                    })
                    .count();
                cleanup_depths.insert(*cleanup_bcp, depth);
            }
        }
        let slots = base_slots.checked_add(emitted.shadow_root_slots)?;
        let code = JitBuffer::new(&emitted.code)?;
        Some(Self {
            body,
            _body_roots: roots,
            code,
            _veneer: veneer,
            _capture: capture,
            _completion: completion,
            landing,
            sites,
            slots,
            cleanup_depths,
        })
    }

    pub(super) fn run(&self, args: &[TorclVal], env: &mut Env) -> Result<TorclVal, TorclError> {
        torcl_rt::rooted!(args = args.to_vec());
        torcl_rt::rooted_ref!(_env = &mut *env);
        if args.len() != usize::from(self.body.arity) {
            return Err(TorclError::ProgramError(
                "native transfer entry: wrong argument count".into(),
            ));
        }
        validate_declared_args(&self.body, &args)?;
        NATIVE_DEPTH.with(|depth| depth.set(depth.get() + 1));
        let _depth = NativeDepthGuard;
        // Every snapshot is reserved and rooted before machine entry. Cold
        // preparation only copies words; no native frame survives into fallback.
        let mut snapshots = self
            .sites
            .sites()
            .map(|site| site.reserve_snapshot())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid_capture())?;
        torcl_rt::rooted_ref!(_snapshots = &mut snapshots);
        let stack = torcl_rt::current_stack();
        let frame = stack
            .push_frame(NIL, std::ptr::null(), self.slots, FLAG_CALL)
            .ok_or_else(|| {
                TorclError::StackOverflow(
                    torcl_rt::current_fiber_id()
                        .unwrap_or_else(|| torcl_rt::FiberId(torcl_rt::current_thread_id().0)),
                )
            })?;
        let mut frame_guard = FrameGuard(Some(frame));
        bind_params(&self.body, frame, &args, None);
        let mut cleanups = Vec::<SavedCleanup>::with_capacity(self.cleanup_depths.len());
        torcl_rt::rooted_ref!(_cleanups = &mut cleanups);
        let mut catches = Vec::<SavedCatch>::with_capacity(
            self.body
                .code
                .iter()
                .filter(|i| matches!(i, Instr::PushCatch { .. }))
                .count(),
        );
        let _catch_guard = CatchScopeGuard {
            env,
            base: env.catch_stack.len(),
        };
        let mut context = CaptureContext {
            frame,
            body: self.body.as_ref(),
            catches: &mut catches,
            completed_cleanup: None,
            landing_stub: self.landing.as_ptr(),
            landing: SysvNativeLanding {
                stack_pointer: std::ptr::null_mut(),
                entry: std::ptr::null(),
            },
            dispatch: DispatchPacket {
                entry: std::ptr::null(),
                request: std::ptr::null_mut(),
            },
            cleanups: &mut cleanups,
            cleanup_depths: &self.cleanup_depths,
            code_base: self.code.as_ptr() as usize,
            sites: &self.sites,
            snapshots: snapshots.as_mut_ptr().cast(),
            activation: unsafe { frame.add(1).cast::<TorclVal>() },
            slots: usize::from(self.slots),
            selected: None,
            failure: None,
        };
        let _scope = BlockScopeGuard {
            env,
            blocks: std::mem::take(&mut env.block_stack),
            tags: std::mem::take(&mut env.tag_stack),
        };
        env.clear_mv();
        // Nested legacy callees and other executions must not overwrite this
        // invocation's capture pointer or an enclosing pending error.
        let mut entry = EntryGuard {
            env: NATIVE_ENV.with(|slot| slot.replace(env)),
            capture: CAPTURE.with(|slot| slot.replace(&mut context)),
            error: NATIVE_ERROR.with(|slot| slot.take()),
        };
        torcl_rt::rooted_ref!(_entry = &mut entry);
        // An enclosing legacy recovery PC describes its own machine frame,
        // never this one or our Rust helpers. This entry has no fault maps yet.
        let _recovery = FaultRecoveryGuard {
            null: torcl_rt::runtime::current_sigsegv_null_guard_recovery_ip(),
            stack: torcl_rt::runtime::current_sigsegv_stack_guard_recovery_ip(),
        };
        torcl_rt::runtime::set_sigsegv_recovery_ips(0, 0);
        let outcome = unsafe {
            native_transfer::invoke_native_segment(
                self.code.as_ptr(),
                context.activation.cast(),
                stack,
            )
        }
        .map_err(|_| TorclError::Internal("native segment is unavailable".into()))?;
        // No Lisp allocation between the machine return and rooting its value
        // or taking ownership of the execution-local error.
        torcl_rt::rooted!(primary = outcome.value);
        torcl_rt::rooted!(error = NATIVE_ERROR.with(|slot| slot.take()));
        if context.failure.is_some() {
            return Err(invalid_capture());
        }
        match outcome.exit {
            NativeExit::Returned
                if error.is_none()
                    && context.selected.is_none()
                    && cleanups.is_empty()
                    && catches.is_empty() =>
            {
                Ok(*primary)
            }
            NativeExit::Transfer => {
                let index = context.selected.ok_or_else(invalid_capture)?;
                if error.is_none() {
                    return Err(invalid_capture());
                }
                // This path currently admits only tagged values. A future
                // unboxed emitter must supply its boxing/emergency contract.
                torcl_rt::rooted!(
                    frames = snapshots[index]
                        .reconstruct(|_| unreachable!("tagged-only entry"))
                        .map_err(|_| invalid_capture())?
                );
                let site = self.sites.sites().nth(index).ok_or_else(invalid_capture)?;
                let saved = frames
                    .first()
                    .filter(|saved| {
                        frames.len() == 1
                            && saved.resume_pc == site.map().origin_bcp
                            && saved.locals.len() == usize::from(self.body.n_locals)
                            && saved.stack.len() <= usize::from(self.body.max_stack)
                            && matches!(
                                self.body.code.get(saved.resume_pc as usize),
                                Some(
                                    Instr::CallNamed { .. }
                                        | Instr::CleanupReturn
                                        | Instr::SetValues(_)
                                        | Instr::Throw
                                        | Instr::PushCatch { .. }
                                        | Instr::PopHandler
                                )
                            )
                    })
                    .ok_or_else(invalid_capture)?;
                for (index, value) in saved.locals.iter().chain(&saved.stack).enumerate() {
                    unsafe { slot_set(frame, index as u16, *value) };
                }
                let running = site.map().control_scopes.iter().filter_map(|scope| {
                    if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
                        (Some(cleanup_bcp) != context.completed_cleanup).then_some(cleanup_bcp)
                    } else {
                        None
                    }
                });
                if !running.eq(cleanups.iter().map(|saved| saved.cleanup_bcp)) {
                    return Err(invalid_capture());
                }
                let live_catches = site.map().control_scopes.iter().filter_map(|scope| {
                    matches!(scope.kind, ScopeKind::Catch { .. }).then_some(scope.push_bcp)
                });
                if !live_catches.eq(catches.iter().map(|saved| saved.push_bcp)) {
                    return Err(invalid_capture());
                }
                let handlers = site
                    .map()
                    .control_scopes
                    .iter()
                    .filter(|scope| !matches!(scope.kind, ScopeKind::Cleanup { .. }))
                    .map(|scope| match scope.kind {
                        ScopeKind::Block {
                            id,
                            resume_bcp,
                            register: false,
                        } => Handler::Block {
                            block_id: id,
                            token: String::new(),
                            resume_bcp,
                            sp_restore: scope.sp_restore,
                        },
                        ScopeKind::Tagbody { id } => Handler::Tag {
                            tagbody_id: id,
                            sp_restore: scope.sp_restore,
                            token: None,
                            tag_bcps: Vec::new(),
                        },
                        ScopeKind::Unwind { cleanup_bcp } => Handler::Unwind {
                            cleanup_bcp,
                            sp_restore: scope.sp_restore,
                        },
                        ScopeKind::Catch { resume_bcp } => Handler::Catch {
                            token: catches
                                .iter()
                                .find(|saved| saved.push_bcp == scope.push_bcp)
                                .expect("checked catch identity")
                                .token
                                .clone(),
                            resume_bcp,
                            sp_restore: scope.sp_restore,
                        },
                        _ => unreachable!("admission checked all scope records"),
                    })
                    .collect();
                let mut acts = vec![Activation {
                    frame,
                    func: self.body.clone(),
                    bcp: saved.resume_pc as usize,
                    sp_top: saved.stack.len() as u16,
                    n_locals: self.body.n_locals,
                    handlers,
                    cleanup_conts: cleanups.drain(..).map(|saved| saved.continuation).collect(),
                    dyn_binds: Vec::new(),
                    env_frame: None,
                    fn_obj: None,
                    sym: u32::MAX,
                }];
                torcl_rt::rooted_ref!(_acts = &mut acts);
                // Transfer ownership only after constructing the activation.
                // Crucially: unwind FIRST, never run the failed CallNamed again.
                frame_guard.0 = None;
                let pending = error_to_pending(error.take().expect("checked pending error"), env);
                let result = initiate_unwind(&mut acts, stack, env, pending)
                    .and_then(|()| run_loop(&mut acts, env));
                while let Some(mut act) = acts.pop() {
                    release_activation_handlers(&mut act, stack, env);
                    stack.pop_frame();
                }
                result
            }
            _ => Err(invalid_capture()),
        }
    }
}

fn invalid_capture() -> TorclError {
    TorclError::Internal("invalid native transfer capture".into())
}

struct FaultRecoveryGuard {
    null: usize,
    stack: usize,
}
impl Drop for FaultRecoveryGuard {
    fn drop(&mut self) {
        torcl_rt::runtime::set_sigsegv_recovery_ips(self.null, self.stack);
    }
}

struct FrameGuard(Option<*mut Frame>);
impl Drop for FrameGuard {
    fn drop(&mut self) {
        if let Some(frame) = self.0 {
            let stack = torcl_rt::current_stack();
            debug_assert_eq!(stack.fp(), frame);
            stack.pop_frame();
        }
    }
}

struct SavedCleanup {
    cleanup_bcp: u32,
    continuation: CleanupCont,
}

struct SavedCatch {
    push_bcp: u32,
    token: String,
}

struct CatchScopeGuard {
    env: *mut Env,
    base: usize,
}
impl Drop for CatchScopeGuard {
    fn drop(&mut self) {
        unsafe {
            (*self.env).catch_stack.truncate(self.base);
        }
    }
}
impl torcl_rt::gc::TraceHostRoots for SavedCleanup {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        self.continuation.trace_host_roots(visit);
    }
}

// These helpers never execute Lisp, collect, yield or signal. Rust allocation
// of the MV copy retains the current runtime's host-allocation policy; emergency
// storage exhaustion is still an installation gate for the entire opt-in ABI.
unsafe extern "C" fn save_cleanup(cleanup_bcp: u32, resume_bcp: u32, value: TorclVal) -> TorclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let env = unsafe { &*NATIVE_ENV.with(Cell::get) };
    let cleanups = unsafe { &mut *context.cleanups };
    let depths = unsafe { &*context.cleanup_depths };
    // Capacity was reserved before native entry; no continuation-stack growth
    // occurs inside the helper. Nested function entries own separate stacks.
    assert!(cleanups.len() < cleanups.capacity());
    cleanups.push(SavedCleanup {
        cleanup_bcp,
        continuation: CleanupCont {
            handler_depth: depths[&cleanup_bcp],
            action: CleanupAction::Normal {
                resume_bcp,
                value,
                values: env.mv_active.then(|| env.mv.clone()),
            },
        },
    });
    NIL
}

unsafe extern "C" fn call_or_throw(
    request: *mut u8,
    out: *mut torcl_rt::native_transfer::NativeOutcome,
) {
    use torcl_compiler::t2::emit::{
        TRANSFER_CATCH_ENTER_REQUEST, TRANSFER_CATCH_LEAVE_REQUEST, TRANSFER_THROW_REQUEST,
        TransferCallRequest,
    };
    use torcl_rt::native_transfer::NativeOutcome;
    let call = unsafe { &*request.cast::<TransferCallRequest>() };
    let request_kind = call.symbol & !u64::from(u32::MAX);
    if matches!(
        request_kind,
        TRANSFER_CATCH_ENTER_REQUEST | TRANSFER_CATCH_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let catches = unsafe { &mut *context.catches };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                assert!(matches!(
                    body.code.get(push_bcp as usize),
                    Some(Instr::PushCatch { .. })
                ));
                if request_kind == TRANSFER_CATCH_ENTER_REQUEST {
                    assert_eq!(call.nargs, 1);
                    assert!(catches.len() < catches.capacity());
                    let tag = unsafe { call.args.read() };
                    let token = super::super::next_control_token("__THROW__");
                    env.catch_stack.push((tag, token.clone()));
                    catches.push(SavedCatch { push_bcp, token });
                } else {
                    assert_eq!(call.nargs, 0);
                    let saved = catches.last().expect("live native catch");
                    assert_eq!(saved.push_bcp, push_bcp);
                    assert!(
                        env.catch_stack
                            .last()
                            .is_some_and(|(_, token)| token == &saved.token)
                    );
                    env.catch_stack.pop();
                    catches.pop();
                }
                Ok(NIL)
            });
            if let Err(error) = result {
                NATIVE_ERROR.with(|slot| slot.set_first(error));
            }
        }
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: if native_error_pending() {
                    NativeExit::Transfer
                } else {
                    NativeExit::Returned
                },
            });
        }
        return;
    }
    if call.symbol != TRANSFER_THROW_REQUEST {
        unsafe {
            c2i_call_legacy_v2(request, out);
        }
        return;
    }
    if !native_error_pending() {
        let result = guard_c2i(|| {
            assert_eq!(call.nargs, 2);
            torcl_rt::rooted!(tag = unsafe { call.args.read() });
            torcl_rt::rooted!(value = unsafe { call.args.add(1).read() });
            let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
            let token = env
                .catch_stack
                .iter()
                .rev()
                .find(|(live_tag, _)| *live_tag == *tag)
                .map(|(_, token)| token.clone());
            match token {
                Some(token) => {
                    super::super::store_control_mv(&token, *value, env);
                    Err(TorclError::Internal(token))
                }
                None => Err(TorclError::ControlError(format!(
                    "attempt to THROW to a tag that is not active: {}",
                    val_as_str(*tag)
                ))),
            }
        });
        if let Err(error) = result {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
        }
    }
    unsafe {
        out.write(NativeOutcome {
            value: NIL,
            exit: NativeExit::Transfer,
        });
    }
}

unsafe extern "C" fn complete_cleanup(
    request: *mut u8,
    out: *mut torcl_rt::native_transfer::NativeOutcome,
) {
    use torcl_compiler::t2::emit::TransferCleanupRequest;
    use torcl_rt::native_transfer::NativeOutcome;
    let request = unsafe { &*request.cast::<TransferCleanupRequest>() };
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    assert_eq!(request.activation, context.activation);
    let cleanups = unsafe { &mut *context.cleanups };
    let saved = cleanups.last().expect("verified cleanup stack");
    assert_eq!(u64::from(saved.cleanup_bcp), request.cleanup_bcp);
    if matches!(saved.continuation.action, CleanupAction::Resume { .. }) {
        // Keep the continuation rooted and the pre-op stack intact until cold
        // capture has consumed its exact source map. Rust returns first.
        context.completed_cleanup = Some(saved.cleanup_bcp);
        unsafe {
            out.write(NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            });
        }
        return;
    }
    let saved = cleanups.pop().expect("verified cleanup stack");
    let CleanupAction::Normal {
        resume_bcp,
        value,
        values,
    } = saved.continuation.action
    else {
        unreachable!("checked normal continuation")
    };
    assert_eq!(u64::from(resume_bcp), request.resume_bcp);
    match values {
        Some(values) => env.set_mv(values),
        None => env.clear_mv(),
    }
    unsafe {
        out.write(NativeOutcome {
            value,
            exit: NativeExit::Returned,
        });
    }
}

#[repr(C)]
struct DispatchPacket {
    entry: *const u8,
    request: *mut u8,
}

const _: () = {
    assert!(std::mem::size_of::<DispatchPacket>() == 16);
    assert!(std::mem::offset_of!(DispatchPacket, entry) == 0);
    assert!(std::mem::offset_of!(DispatchPacket, request) == 8);
};

#[cfg(test)]
static NATIVE_CLEANUP_COUNT: torcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { torcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_cleanup_count() -> usize {
    NATIVE_CLEANUP_COUNT.with(|count| count.replace(0))
}

struct CaptureContext {
    frame: *mut Frame,
    body: *const BytecodeFunction,
    catches: *mut Vec<SavedCatch>,
    completed_cleanup: Option<u32>,
    landing_stub: *const u8,
    landing: SysvNativeLanding,
    dispatch: DispatchPacket,
    cleanups: *mut Vec<SavedCleanup>,
    cleanup_depths: *const std::collections::HashMap<u32, usize>,
    code_base: usize,
    sites: *const SysvTransferTable,
    snapshots: *mut u8,
    activation: *mut TorclVal,
    slots: usize,
    selected: Option<usize>,
    failure: Option<TransferSiteError>,
}
static CAPTURE: torcl_rt::execution_local::ExecutionLocal<Cell<*mut CaptureContext>> =
    unsafe { torcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };
struct EntryGuard {
    env: *mut Env,
    capture: *mut CaptureContext,
    error: Option<TorclError>,
}
impl torcl_rt::gc::TraceHostRoots for EntryGuard {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        self.error.trace_host_roots(visit);
    }
}
impl Drop for EntryGuard {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.env));
        CAPTURE.with(|slot| slot.set(self.capture));
        NATIVE_ERROR.with(|slot| slot.replace(self.error.take()));
    }
}

// No Lisp allocation, GC, yield or Lisp execution here. The owning invocation has
// already rooted/reserved snapshots and activation slots. Rust returns before
// the capture stub dispatches; assembly only discards generated frames.
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    context.dispatch = DispatchPacket {
        entry: native_transfer::leave_native_segment as *const u8,
        request: native_transfer::current_segment().cast(),
    };
    if context.dispatch.request.is_null() || torcl_rt::current_stack().fp() != context.frame {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    } else if let Err(error) = unsafe { prepare_transfer(context, capture) } {
        context.failure = Some(error);
    }
    capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
}

unsafe fn prepare_transfer(
    context: &mut CaptureContext,
    capture: &mut SysvTransferCapture,
) -> Result<(), TransferSiteError> {
    let sites = unsafe { &*context.sites };
    let site = sites
        .lookup(context.code_base, capture.return_pc as usize)
        .ok_or(TransferSiteError::WrongReturnPc)?;
    let index = sites
        .sites()
        .position(|candidate| std::ptr::eq(candidate, site))
        .ok_or(TransferSiteError::WrongReturnPc)?;
    let snapshot = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot<'_>>().add(index) };
    unsafe {
        snapshot.capture_from_activation(
            context.code_base,
            capture,
            std::slice::from_raw_parts(context.activation, context.slots),
        )?;
    }
    context.selected = Some(index);
    let cleanups = unsafe { &mut *context.cleanups };
    if let Some(completed) = context.completed_cleanup {
        let saved = cleanups.pop().expect("completing rooted native cleanup");
        assert_eq!(saved.cleanup_bcp, completed);
        let CleanupAction::Resume { pending, payload } = saved.continuation.action else {
            unreachable!("completion helper checked pending continuation")
        };
        payload.restore();
        NATIVE_ERROR.with(|slot| {
            assert!(!slot.is_some());
            slot.replace(Some(unmatched_error(pending)));
        });
    }
    let Some(landing) = site.native_cleanup_landing(context.code_base, capture)? else {
        return Ok(());
    };
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    // Raw errors must first signal with live handlers/restarts. Until native
    // signaling is wired, only a selected transfer to a live outer CATCH takes
    // this route. Other outcomes retain explicit bytecode unwinding fallback.
    let mut selected_throw = false;
    let mut selected_target = SelectedTarget::OutsideFrame;
    NATIVE_ERROR.with(|slot| {
        slot.visit(|error| {
            selected_throw = matches!(error, TorclError::Internal(token)
            if env.catch_stack.iter().any(|(_, live)| live == token));
            if let TorclError::Internal(token) = error {
                if let Some(saved) = unsafe { &*context.catches }
                    .iter()
                    .find(|saved| &saved.token == token)
                {
                    selected_target = SelectedTarget::Scope {
                        push_bcp: saved.push_bcp,
                    };
                }
            }
        })
    });
    if !selected_throw {
        return Ok(());
    }
    // Local catch identity survives fallback; only intervening cleanups may run
    // natively until the selected catch has its own verified landing.
    let NativeUnwindStep::RunCleanup {
        scope_index,
        handler_depth,
    } = next_unwind_step(&site.map().control_scopes, selected_target)
    else {
        return Ok(());
    };
    let ScopeKind::Unwind { cleanup_bcp } = site.map().control_scopes[scope_index].kind else {
        unreachable!("unwind selector returned a cleanup scope")
    };
    // Restore native homes while canonical snapshots still own every root.
    unsafe {
        snapshot.write_back(context.code_base, capture)?;
    }
    let Some(TorclError::Internal(token)) = NATIVE_ERROR.with(|slot| slot.take()) else {
        unreachable!("selected a rooted throw token")
    };
    while cleanups
        .last()
        .is_some_and(|saved| saved.continuation.handler_depth > handler_depth)
    {
        // Dropping the private payload cannot erase a newer throw's values,
        // even when both transfers name the same catch binding.
        cleanups.pop();
    }
    assert!(cleanups.len() < cleanups.capacity());
    cleanups.push(SavedCleanup {
        cleanup_bcp,
        continuation: CleanupCont {
            handler_depth,
            action: CleanupAction::Resume {
                payload: ControlPayload::take(&token),
                pending: Pending::Token(token),
            },
        },
    });
    // The rooted continuation owns the pending transfer before generated
    // cleanup code can allocate or suspend. Clear any helper's secondary values.
    env.clear_mv();
    context.completed_cleanup = None;
    context.selected = None;
    context.landing = landing;
    context.dispatch = DispatchPacket {
        entry: context.landing_stub,
        request: std::ptr::from_mut(&mut context.landing).cast(),
    };
    #[cfg(test)]
    NATIVE_CLEANUP_COUNT.with(|count| count.set(count.get() + 1));
    Ok(())
}

#[unsafe(naked)]
unsafe extern "C" fn dispatch(_packet: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "mov rax, [rdi]", "mov rdi, [rdi + 8]", "jmp rax");
}
