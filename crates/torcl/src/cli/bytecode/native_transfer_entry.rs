//! Opt-in runtime entry for the tagged Invoke emitter. Normal installation is
//! still gated on complete helper/poll/scope coverage. Own code and definitions,
//! capture into rooted snapshots, enter native cleanup for selected throws, and
//! retain bytecode *unwinding* as the fallback.
#![allow(dead_code)]

use super::*;
use super::super::CONTROL_COUNTER;
use std::cell::Cell;
use torcl_compiler::control_scope::{Ownership, ScopeKind};
use torcl_compiler::native_unwind::{next_unwind_step, NativeUnwindStep, SelectedTarget};
use torcl_compiler::t2::native_transfer::{
    emit_capture_stub, emit_helper_veneer, emit_native_landing_stub, SysvNativeLanding,
    SysvTransferCapture,
};
use torcl_compiler::t2::transfer_sites::{SysvSiteSnapshot, SysvTransferTable, TransferSiteError};
use torcl_rt::jit::JitBuffer;
use torcl_rt::native_transfer::{self, NativeExit};

thread_local! {
    /// Opt-in production cache for the segment ABI. Keep the negative result
    /// too: an unsupported body must not be recompiled on every invocation.
    static SEGMENT_CACHE: RefCell<std::collections::HashMap<usize, Option<Rc<TransferCode>>>> =
        RefCell::new(std::collections::HashMap::new());
}

#[cfg(test)]
static SEGMENT_RUNS: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

fn try_clone_string(value: &str) -> Result<String, TorclError> {
    let mut copy = String::new();
    copy.try_reserve(value.len()).map_err(|_| TorclError::Oom)?;
    copy.push_str(value);
    Ok(copy)
}

fn try_control_token(prefix: &str) -> Result<String, TorclError> {
    use std::fmt::Write;
    let id = CONTROL_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut token = String::new();
    token
        .try_reserve(prefix.len().saturating_add(20))
        .map_err(|_| TorclError::Oom)?;
    write!(&mut token, "{prefix}:{id}").expect("writing to a String cannot fail");
    Ok(token)
}

fn try_clone_control_stack(
    stack: &[(String, String)],
) -> Result<Vec<(String, String)>, TorclError> {
    let mut copy = Vec::new();
    copy.try_reserve(stack.len()).map_err(|_| TorclError::Oom)?;
    for (name, token) in stack {
        copy.push((try_clone_string(name)?, try_clone_string(token)?));
    }
    Ok(copy)
}

struct RestartFunctionTemplates(Vec<Vec<Rc<RefCell<torcl_rt::bytecode::BytecodeFunction>>>>);

impl torcl_rt::gc::TraceHostRoots for RestartFunctionTemplates {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        for templates in &self.0 {
            for function in templates {
                super::super::visit_bytecode_function_roots(&mut function.borrow_mut(), visit);
            }
        }
    }
}

/// Only constructible through the host-specific emitter. Retaining this value
/// retains every embedded code address and the original bytecode definition.
pub(super) struct TransferCode {
    body: Arc<BytecodeFunction>,
    _body_roots: ActiveBytecodeRoot,
    code: JitBuffer,
    _veneer: JitBuffer,
    _capture: JitBuffer,
    _poll: JitBuffer,
    _completion: JitBuffer,
    landing: JitBuffer,
    sites: SysvTransferTable,
    slots: u16,
    cleanup_depths: std::collections::HashMap<u32, usize>,
    #[cfg(test)]
    unavailable_catch: Option<u32>,
    #[cfg(test)]
    unavailable_handler: Option<(u32, u32)>,
}

/// Try the new segment ABI for an ordinary native invocation. This remains an
/// explicit rollout switch until the platform gates are complete; callers fall
/// back to the legacy checked ABI when the machine transition or body shape is
/// unavailable. The cache owns each compiled body through `TransferCode::body`.
pub(super) fn try_run(
    body: Arc<BytecodeFunction>,
    args: &[TorclVal],
    env: &mut Env,
) -> Option<Result<TorclVal, TorclError>> {
    if std::env::var_os("TORCL_NATIVE_TRANSFER") != Some(std::ffi::OsString::from("1"))
        || !native_transfer::is_supported()
    {
        return None;
    }
    // A nested segment would need a fresh activation frame and a second host
    // landing boundary. Recursive calls already have a bounded, frame-safe
    // c2i/native bridge, so keep them on that path while the outer segment
    // remains active. This admits the outer native handler/cleanup machinery
    // without manufacturing an unsafe recursive segment chain.
    if !native_transfer::current_segment().is_null() {
        return None;
    }
    let key = Arc::as_ptr(&body) as usize;
    let code = SEGMENT_CACHE.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache
            .entry(key)
            .or_insert_with(|| TransferCode::compile(Arc::clone(&body)).map(Rc::new))
            .clone()
    });
    code.map(|code| {
        #[cfg(test)]
        SEGMENT_RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        code.run(args, env)
    })
}

#[cfg(test)]
pub(super) fn take_segment_run_count() -> u64 {
    SEGMENT_RUNS.swap(0, std::sync::atomic::Ordering::Relaxed)
}

impl TransferCode {
    #[cfg(test)]
    pub(super) fn without_catch_destination(mut self, push_bcp: u32) -> Self {
        assert!(matches!(
            self.body.code.get(push_bcp as usize),
            Some(Instr::PushCatch { .. })
        ));
        self.unavailable_catch = Some(push_bcp);
        self
    }

    #[cfg(test)]
    pub(super) fn without_handler_destination(mut self, push_bcp: u32, clause_index: u32) -> Self {
        let Some(Instr::PushHandlerCase { hc, .. }) = self.body.code.get(push_bcp as usize) else {
            panic!("handler scope");
        };
        assert!((clause_index as usize) < self.body.handler_cases[*hc as usize].clauses.len());
        self.unavailable_handler = Some((push_bcp, clause_index));
        self
    }

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
        let poll = JitBuffer::new(&emit_helper_veneer(poll_or_transfer, capture.as_ptr()))?;
        let base_slots = body.n_locals.checked_add(body.max_stack)?;
        let completion = JitBuffer::new(&emit_helper_veneer(complete_cleanup, capture.as_ptr()))?;
        let landing = JitBuffer::new(&emit_native_landing_stub())?;
        let (emitted, sites) = torcl_compiler::t2::emit::emit_framed_native_handlers_with_poll(
            &ir,
            veneer.as_ptr() as u64,
            base_slots,
            save_cleanup as *const () as u64,
            completion.as_ptr() as u64,
            c2i_clear_mv as *const () as u64,
            deliver_catch as *const () as u64,
            deliver_handler as *const () as u64,
            poll.as_ptr() as u64,
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
                            | ScopeKind::HandlerCase { .. }
                            | ScopeKind::HandlerBind { .. }
                            | ScopeKind::RestartCase { .. }
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
                                | ScopeKind::HandlerCase { .. }
                                | ScopeKind::HandlerBind { .. }
                                | ScopeKind::RestartCase { .. }
                        )
                    })
                    .count();
                cleanup_depths.insert(*cleanup_bcp, depth);
            }
        }
        let slots = base_slots.checked_add(emitted.shadow_root_slots)?;
        let code = JitBuffer::new(&emitted.code)?;
        Some(Self {
            #[cfg(test)]
            unavailable_catch: None,
            #[cfg(test)]
            unavailable_handler: None,
            body,
            _body_roots: roots,
            code,
            _veneer: veneer,
            _capture: capture,
            _poll: poll,
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
        // Reserve the control-value map while the caller can still report an
        // ordinary storage condition. Once generated code is running, payload
        // retirement and restoration must not discover a rehash allocation in
        // the middle of an unwind. The estimate covers primary, multiple-value
        // and restart-argument entries for every statically mapped scope.
        let reserve = self
            .cleanup_depths
            .len()
            .saturating_add(self.body.handler_cases.len())
            .saturating_add(
                self.body
                    .code
                    .iter()
                    .filter(|instruction| matches!(instruction, Instr::PushCatch { .. }))
                    .count(),
            )
            .saturating_mul(4)
            .saturating_add(4);
        reserve_control_values(reserve).map_err(|_| TorclError::Oom)?;
        // Registration helpers push into the live Env as well as the private
        // activation records. Reserve those tails while ordinary Rust error
        // reporting is still available; no helper entered from generated code
        // may discover a Vec growth allocation halfway through a transfer.
        env.handlers
            .try_reserve(
                self.body
                    .handler_cases
                    .len()
                    .saturating_add(self.body.handler_binds.len()),
            )
            .map_err(|_| TorclError::Oom)?;
        env.restarts
            .try_reserve(self.body.restart_cases.iter().fold(0usize, |total, info| {
                total.saturating_add(info.restarts.len())
            }))
            .map_err(|_| TorclError::Oom)?;
        env.catch_stack
            .try_reserve(
                self.body
                    .code
                    .iter()
                    .filter(|instruction| matches!(instruction, Instr::PushCatch { .. }))
                    .count(),
            )
            .map_err(|_| TorclError::Oom)?;
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
        let mut handlers = Vec::<SavedHandler>::with_capacity(self.body.handler_cases.len());
        let mut handler_binds =
            Vec::<SavedHandlerBind>::with_capacity(self.body.handler_binds.len());
        let mut restart_cases =
            Vec::<SavedRestartCase>::with_capacity(self.body.restart_cases.len());
        // Restart clause bytecode is immutable apart from GC relocation of its
        // embedded values. Build one rooted template per static clause while
        // ordinary Rust allocation is still allowed; dynamic scope entry then
        // only clones an Rc handle instead of cloning the whole function.
        let mut restart_templates = RestartFunctionTemplates(Vec::new());
        restart_templates
            .0
            .try_reserve(self.body.restart_cases.len())
            .map_err(|_| TorclError::Oom)?;
        for info in &self.body.restart_cases {
            let mut templates = Vec::new();
            templates
                .try_reserve(info.restarts.len())
                .map_err(|_| TorclError::Oom)?;
            for restart in &info.restarts {
                templates.push(Rc::new(RefCell::new((*restart.function).clone())));
            }
            restart_templates.0.push(templates);
        }
        torcl_rt::rooted_ref!(_restart_templates = &mut restart_templates);
        let mut dynamic_scopes = Vec::<DynamicScope>::with_capacity(
            self.body.handler_cases.len()
                + self.body.handler_binds.len()
                + self.body.restart_cases.len(),
        );
        // `prepare` runs after native code has already crossed the transfer
        // boundary. Keep its frame-chain validation allocation-free: an
        // ordinary Vec growth there would turn an otherwise reserved unwind
        // into an allocator failure before the emergency error path can run.
        let mut cluster_frames = Vec::<(u32, *mut Frame)>::with_capacity(
            self.body.handler_cases.len()
                + self.body.handler_binds.len()
                + self.body.restart_cases.len(),
        );
        let _dynamic_scope_guard = DynamicScopeGuard {
            env,
            scopes: &mut dynamic_scopes,
        };
        let mut prepared_handler = None::<PreparedHandler>;
        torcl_rt::rooted_ref!(_prepared_handler = &mut prepared_handler);
        let mut prepared_catch = None::<PreparedCatch>;
        torcl_rt::rooted_ref!(_prepared_catch = &mut prepared_catch);
        let mut context = CaptureContext {
            #[cfg(test)]
            unavailable_catch: self.unavailable_catch,
            #[cfg(test)]
            unavailable_handler: self.unavailable_handler,
            prepared_catch: &mut prepared_catch,
            handlers: &mut handlers,
            handler_binds: &mut handler_binds,
            restart_cases: &mut restart_cases,
            restart_templates: &restart_templates,
            dynamic_scopes: &mut dynamic_scopes,
            cluster_frames: &mut cluster_frames,
            prepared_handler: &mut prepared_handler,
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
        // The segment owns a rooted TorclStack frame and all host-side
        // execution state at this point, so this is a safe entry poll: a
        // moving GC may park and scan before generated code starts. Native
        // loop/back-edge polling remains a separate emitter gate.
        torcl_rt::safepoint::poll_safepoint();
        if let Some(error) = pending_signal_error_for_current_execution() {
            return Err(error);
        }
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
                    && catches.is_empty()
                    && handler_binds.is_empty()
                    && restart_cases.is_empty()
                    && prepared_catch.is_none()
                    && handlers.is_empty()
                    && prepared_handler.is_none() =>
            {
                Ok(*primary)
            }
            NativeExit::Transfer => {
                #[cfg(test)]
                NATIVE_FALLBACK_COUNT.with(|count| count.set(count.get() + 1));
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
                                        | Instr::PushHandlerCase { .. }
                                        | Instr::PopHandlerCase
                                        | Instr::PushHandlerBind { .. }
                                        | Instr::PopHandlerBind
                                        | Instr::PushRestartCase { .. }
                                        | Instr::PopRestartCase
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
                let live_handlers = site.map().control_scopes.iter().filter_map(|scope| {
                    matches!(scope.kind, ScopeKind::HandlerCase { .. }).then_some(scope.push_bcp)
                });
                if !live_handlers.eq(handlers.iter().map(|saved| saved.push_bcp)) {
                    return Err(invalid_capture());
                }
                let live_handler_binds = site.map().control_scopes.iter().filter_map(|scope| {
                    matches!(scope.kind, ScopeKind::HandlerBind { .. }).then_some(scope.push_bcp)
                });
                if !live_handler_binds.eq(handler_binds.iter().map(|saved| saved.push_bcp)) {
                    return Err(invalid_capture());
                }
                let live_restart_cases = site.map().control_scopes.iter().filter_map(|scope| {
                    matches!(scope.kind, ScopeKind::RestartCase { .. }).then_some(scope.push_bcp)
                });
                if !live_restart_cases.eq(restart_cases.iter().map(|saved| saved.push_bcp)) {
                    return Err(invalid_capture());
                }
                let restored_handlers = site
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
                        ScopeKind::HandlerCase { .. } => {
                            let saved = handlers
                                .iter_mut()
                                .find(|saved| saved.push_bcp == scope.push_bcp)
                                .expect("checked handler identity");
                            Handler::HandlerCase {
                                clauses: std::mem::take(&mut saved.clauses),
                                sp_restore: scope.sp_restore,
                                cluster_base: saved.cluster_base,
                                cluster_frame: saved.cluster_frame,
                            }
                        }
                        ScopeKind::HandlerBind { .. } => {
                            let saved = handler_binds
                                .iter()
                                .find(|saved| saved.push_bcp == scope.push_bcp)
                                .expect("checked handler-bind identity");
                            Handler::HandlerBind {
                                cluster_base: saved.cluster_base,
                                cluster_frame: saved.cluster_frame,
                            }
                        }
                        ScopeKind::RestartCase { resume_bcp, .. } => {
                            let saved = restart_cases
                                .iter()
                                .find(|saved| saved.push_bcp == scope.push_bcp)
                                .expect("checked restart-case identity");
                            Handler::RestartCase {
                                restart_base: saved.restart_base,
                                resume_bcp,
                                sp_restore: scope.sp_restore,
                                cluster_frame: saved.cluster_frame,
                            }
                        }
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
                // Bytecode now owns the live cluster frames and registrations.
                handlers.clear();
                handler_binds.clear();
                restart_cases.clear();
                dynamic_scopes.clear();
                let mut acts = vec![Activation {
                    frame,
                    func: self.body.clone(),
                    bcp: saved.resume_pc as usize,
                    sp_top: saved.stack.len() as u16,
                    n_locals: self.body.n_locals,
                    handlers: restored_handlers,
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

struct SavedHandler {
    push_bcp: u32,
    clauses: Vec<RuntimeClause>,
    cluster_base: usize,
    cluster_frame: *mut Frame,
}

struct SavedHandlerBind {
    push_bcp: u32,
    cluster_base: usize,
    cluster_frame: *mut Frame,
}

struct SavedRestartCase {
    push_bcp: u32,
    restart_base: usize,
    resume_bcp: u32,
    sp_restore: u16,
    cluster_frame: *mut Frame,
}

enum DynamicScope {
    HandlerCase {
        cluster_base: usize,
        cluster_frame: *mut Frame,
    },
    HandlerBind {
        cluster_base: usize,
        cluster_frame: *mut Frame,
    },
    RestartCase {
        restart_base: usize,
        cluster_frame: *mut Frame,
    },
}

struct DynamicScopeGuard {
    env: *mut Env,
    scopes: *mut Vec<DynamicScope>,
}
impl Drop for DynamicScopeGuard {
    fn drop(&mut self) {
        let stack = torcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let scopes = unsafe { &mut *self.scopes };
        while let Some(scope) = scopes.pop() {
            match scope {
                DynamicScope::HandlerCase {
                    cluster_base,
                    cluster_frame,
                }
                | DynamicScope::HandlerBind {
                    cluster_base,
                    cluster_frame,
                } => {
                    env.handlers.truncate(cluster_base);
                    pop_condition_cluster_frame(stack, cluster_frame);
                }
                DynamicScope::RestartCase {
                    restart_base,
                    cluster_frame,
                } => {
                    env.restarts.truncate(restart_base);
                    pop_condition_cluster_frame(stack, cluster_frame);
                }
            }
        }
    }
}

struct HandlerScopeGuard {
    env: *mut Env,
    handlers: *mut Vec<SavedHandler>,
}
impl Drop for HandlerScopeGuard {
    fn drop(&mut self) {
        let stack = torcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let handlers = unsafe { &mut *self.handlers };
        while let Some(saved) = handlers.pop() {
            env.handlers.truncate(saved.cluster_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct HandlerBindScopeGuard {
    env: *mut Env,
    handler_binds: *mut Vec<SavedHandlerBind>,
}
impl Drop for HandlerBindScopeGuard {
    fn drop(&mut self) {
        let stack = torcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let binds = unsafe { &mut *self.handler_binds };
        while let Some(saved) = binds.pop() {
            env.handlers.truncate(saved.cluster_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct RestartCaseScopeGuard {
    env: *mut Env,
    restart_cases: *mut Vec<SavedRestartCase>,
}
impl Drop for RestartCaseScopeGuard {
    fn drop(&mut self) {
        let stack = torcl_rt::current_stack();
        let env = unsafe { &mut *self.env };
        let cases = unsafe { &mut *self.restart_cases };
        while let Some(saved) = cases.pop() {
            env.restarts.truncate(saved.restart_base);
            pop_condition_cluster_frame(stack, saved.cluster_frame);
        }
    }
}

struct PreparedHandler {
    push_bcp: u32,
    clause_index: u32,
    token: String,
    payload: ControlPayload,
}
impl torcl_rt::gc::TraceHostRoots for PreparedHandler {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        self.payload.trace_host_roots(visit);
    }
}

unsafe extern "C" fn deliver_handler(
    push_bcp: u32,
    clause_index: u32,
    _unused: TorclVal,
) -> TorclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let prepared = unsafe { &mut *context.prepared_handler }
        .take()
        .expect("prepared native handler payload");
    assert_eq!(prepared.push_bcp, push_bcp);
    assert_eq!(prepared.clause_index, clause_index);
    prepared.payload.restore();
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    env.clear_mv();
    #[cfg(test)]
    NATIVE_HANDLER_COUNT.with(|count| count.set(count.get() + 1));
    super::super::take_control_value(&prepared.token)
}

struct SavedCatch {
    push_bcp: u32,
    token: String,
}

struct PreparedCatch {
    push_bcp: u32,
    resume_bcp: u32,
    token: String,
    payload: ControlPayload,
}
impl torcl_rt::gc::TraceHostRoots for PreparedCatch {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut TorclVal)) {
        self.payload.trace_host_roots(visit);
    }
}

// Noncollecting: the prepared payload is rooted until this helper consumes it,
// and no Lisp allocation or suspension occurs before the returned primary is
// stored in its generated-code home.
unsafe extern "C" fn deliver_catch(push_bcp: u32, resume_bcp: u32, _unused: TorclVal) -> TorclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let prepared = unsafe { &mut *context.prepared_catch }
        .take()
        .expect("prepared native catch payload");
    assert_eq!(prepared.push_bcp, push_bcp);
    assert_eq!(prepared.resume_bcp, resume_bcp);
    prepared.payload.restore();
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let primary = super::super::take_control_mv(&prepared.token, env);
    #[cfg(test)]
    NATIVE_CATCH_COUNT.with(|count| count.set(count.get() + 1));
    primary
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
    // Do not use `Vec::clone` after native entry. Its infallible allocation
    // could abort before the storage-condition route gets a chance to run.
    // Reserve first, then copy into the exact capacity; a failed reserve is
    // reported through the execution-local error slot and the generated
    // helper chain will leave native code on its next transfer boundary.
    let values = if env.mv_active {
        let mut values = Vec::new();
        if values.try_reserve(env.mv.len()).is_err() {
            // Install a resumable propagation continuation instead of leaving
            // `complete_cleanup` with no saved record. The completion helper
            // will return through the normal transfer machinery, which turns
            // this pending OOM into the preallocated storage-condition path.
            cleanups.push(SavedCleanup {
                cleanup_bcp,
                continuation: CleanupCont {
                    handler_depth: depths[&cleanup_bcp],
                    action: CleanupAction::Resume {
                        pending: Pending::Propagate(TorclError::Oom),
                        payload: ControlPayload::default(),
                    },
                },
            });
            return NIL;
        }
        values.extend_from_slice(&env.mv);
        Some(values)
    } else {
        None
    };
    cleanups.push(SavedCleanup {
        cleanup_bcp,
        continuation: CleanupCont {
            handler_depth: depths[&cleanup_bcp],
            action: CleanupAction::Normal {
                resume_bcp,
                value,
                values,
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
        TransferCallRequest, TRANSFER_CATCH_ENTER_REQUEST, TRANSFER_CATCH_LEAVE_REQUEST,
        TRANSFER_THROW_REQUEST,
    };
    use torcl_rt::native_transfer::NativeOutcome;
    let call = unsafe { &*request.cast::<TransferCallRequest>() };
    let request_kind = call.symbol & !u64::from(u32::MAX);
    use torcl_compiler::t2::emit::{
        TRANSFER_HANDLER_BIND_ENTER_REQUEST, TRANSFER_HANDLER_BIND_LEAVE_REQUEST,
        TRANSFER_HANDLER_ENTER_REQUEST, TRANSFER_HANDLER_LEAVE_REQUEST,
        TRANSFER_RESTART_CASE_ENTER_REQUEST, TRANSFER_RESTART_CASE_LEAVE_REQUEST,
    };
    if matches!(
        request_kind,
        TRANSFER_HANDLER_BIND_ENTER_REQUEST
            | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
            | TRANSFER_RESTART_CASE_ENTER_REQUEST
            | TRANSFER_RESTART_CASE_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 0);
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                if matches!(
                    request_kind,
                    TRANSFER_HANDLER_BIND_ENTER_REQUEST | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
                ) {
                    let binds = unsafe { &mut *context.handler_binds };
                    let Some(instruction) = body.code.get(push_bcp as usize) else {
                        return Err(invalid_capture());
                    };
                    let Instr::PushHandlerBind { hb } = instruction else {
                        return Err(invalid_capture());
                    };
                    if request_kind == TRANSFER_HANDLER_BIND_ENTER_REQUEST {
                        assert!(binds.len() < binds.capacity());
                        let info = &body.handler_binds[*hb as usize];
                        let cluster_base = env.handlers.len();
                        let mut entries = Vec::new();
                        entries
                            .try_reserve(info.bindings.len())
                            .map_err(|_| TorclError::Oom)?;
                        let mut values = Vec::new();
                        values
                            .try_reserve(info.bindings.len().saturating_mul(2))
                            .map_err(|_| TorclError::Oom)?;
                        torcl_rt::rooted_ref!(_entries = &mut entries);
                        torcl_rt::rooted_ref!(_values = &mut values);
                        for (type_name, form) in &info.bindings {
                            let handler = eval_form(*form, env)
                                .map(HandlerImpl::Function)
                                .unwrap_or(HandlerImpl::Function(*form));
                            let handler_value = match &handler {
                                HandlerImpl::Function(value) => *value,
                                HandlerImpl::HandlerCase { .. } => NIL,
                            };
                            entries.push(HandlerEntry {
                                type_name: try_clone_string(type_name)?,
                                handler,
                            });
                            values.push(resolve_sym(type_name).ok_or_else(invalid_capture)?);
                            values.push(handler_value);
                        }
                        let cluster_frame =
                            push_condition_cluster_frame(torcl_rt::current_stack(), &values)?;
                        env.handlers.push(HandlerCluster { entries });
                        binds.push(SavedHandlerBind {
                            push_bcp,
                            cluster_base,
                            cluster_frame,
                        });
                        unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::HandlerBind {
                            cluster_base,
                            cluster_frame,
                        });
                    } else {
                        let saved = binds.pop().expect("live native handler-bind");
                        assert_eq!(saved.push_bcp, push_bcp);
                        let Some(DynamicScope::HandlerBind {
                            cluster_base: live_base,
                            cluster_frame: live_frame,
                        }) = unsafe { &mut *context.dynamic_scopes }.pop()
                        else {
                            return Err(invalid_capture());
                        };
                        assert_eq!(
                            (live_base, live_frame),
                            (saved.cluster_base, saved.cluster_frame)
                        );
                        env.handlers.truncate(saved.cluster_base);
                        pop_condition_cluster_frame(torcl_rt::current_stack(), saved.cluster_frame);
                    }
                } else {
                    let cases = unsafe { &mut *context.restart_cases };
                    let Some(instruction) = body.code.get(push_bcp as usize) else {
                        return Err(invalid_capture());
                    };
                    let Instr::PushRestartCase {
                        rc,
                        resume_bcp,
                        sp_restore,
                    } = instruction
                    else {
                        return Err(invalid_capture());
                    };
                    if request_kind == TRANSFER_RESTART_CASE_ENTER_REQUEST {
                        assert!(cases.len() < cases.capacity());
                        let info = &body.restart_cases[*rc as usize];
                        let restart_base = env.restarts.len();
                        let captured_frame = Arc::clone(&env.frame);
                        let mut values = Vec::new();
                        values
                            .try_reserve(info.restarts.len().saturating_mul(5))
                            .map_err(|_| TorclError::Oom)?;
                        torcl_rt::rooted_ref!(_values = &mut values);
                        // Finish every fallible string/stack copy before
                        // mutating the live restart stack. A later OOM then
                        // cannot leave a partially registered restart-case.
                        let mut prepared = Vec::new();
                        prepared
                            .try_reserve(info.restarts.len())
                            .map_err(|_| TorclError::Oom)?;
                        for restart in &info.restarts {
                            prepared.push((
                                try_clone_string(&restart.name)?,
                                try_clone_control_stack(&env.block_stack)?,
                                try_clone_control_stack(&env.tag_stack)?,
                            ));
                        }
                        let mut restart_symbols = Vec::new();
                        restart_symbols
                            .try_reserve(info.restarts.len())
                            .map_err(|_| TorclError::Oom)?;
                        for restart in &info.restarts {
                            restart_symbols.push(
                                resolve_sym(&restart.name).ok_or_else(invalid_capture)?,
                            );
                        }
                        let templates = unsafe { &*context.restart_templates };
                        let Some(function_templates) = templates.0.get(*rc as usize) else {
                            return Err(invalid_capture());
                        };
                        if function_templates.len() != info.restarts.len() {
                            return Err(invalid_capture());
                        }
                        for (
                            (_index, ((_restart, (name, captured_blocks, captured_tags)), symbol)),
                            function,
                        ) in info
                            .restarts
                            .iter()
                            .zip(prepared)
                            .zip(restart_symbols)
                            .enumerate()
                            .zip(function_templates)
                        {
                            env.restarts.push(RestartEntry {
                                name,
                                captured_blocks,
                                captured_tags,
                                function: RestartFunction::Bytecode {
                                    function: Rc::clone(function),
                                    captured_frame: Arc::clone(&captured_frame),
                                },
                                interactive_function: None,
                                test_function: None,
                                unwind_on_invoke: true,
                                group_base: restart_base,
                                id: super::super::next_restart_id(),
                                restart_obj: NIL,
                                report: NIL,
                            });
                            values.push(symbol);
                            values.extend([NIL, NIL, NIL, NIL]);
                        }
                        let cluster_frame =
                            push_condition_cluster_frame(torcl_rt::current_stack(), &values)?;
                        cases.push(SavedRestartCase {
                            push_bcp,
                            restart_base,
                            resume_bcp: *resume_bcp,
                            sp_restore: *sp_restore,
                            cluster_frame,
                        });
                        unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::RestartCase {
                            restart_base,
                            cluster_frame,
                        });
                    } else {
                        let saved = cases.pop().expect("live native restart-case");
                        assert_eq!(saved.push_bcp, push_bcp);
                        let Some(DynamicScope::RestartCase {
                            restart_base: live_base,
                            cluster_frame: live_frame,
                        }) = unsafe { &mut *context.dynamic_scopes }.pop()
                        else {
                            return Err(invalid_capture());
                        };
                        assert_eq!(
                            (live_base, live_frame),
                            (saved.restart_base, saved.cluster_frame)
                        );
                        env.restarts.truncate(saved.restart_base);
                        pop_condition_cluster_frame(torcl_rt::current_stack(), saved.cluster_frame);
                    }
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
    if matches!(
        request_kind,
        TRANSFER_HANDLER_ENTER_REQUEST
            | TRANSFER_HANDLER_LEAVE_REQUEST
            | TRANSFER_HANDLER_BIND_ENTER_REQUEST
            | TRANSFER_HANDLER_BIND_LEAVE_REQUEST
            | TRANSFER_RESTART_CASE_ENTER_REQUEST
            | TRANSFER_RESTART_CASE_LEAVE_REQUEST
    ) {
        if !native_error_pending() {
            let result = guard_c2i(|| {
                assert_eq!(call.nargs, 0);
                let context = unsafe { &mut *CAPTURE.with(Cell::get) };
                let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
                let handlers = unsafe { &mut *context.handlers };
                let push_bcp = call.symbol as u32;
                let body = unsafe { &*context.body };
                let Some(Instr::PushHandlerCase { hc, .. }) = body.code.get(push_bcp as usize)
                else {
                    return Err(invalid_capture());
                };
                if request_kind == TRANSFER_HANDLER_ENTER_REQUEST {
                    assert!(handlers.len() < handlers.capacity());
                    let info = &body.handler_cases[*hc as usize];
                    let cluster_base = env.handlers.len();
                    let mut clauses = Vec::new();
                    clauses
                        .try_reserve(info.clauses.len())
                        .map_err(|_| TorclError::Oom)?;
                    let mut entries = Vec::new();
                    entries
                        .try_reserve(info.clauses.len())
                        .map_err(|_| TorclError::Oom)?;
                    let mut values = Vec::new();
                    values
                        .try_reserve(info.clauses.len().saturating_mul(2))
                        .map_err(|_| TorclError::Oom)?;
                    torcl_rt::rooted_ref!(_values = &mut values);
                    for clause in &info.clauses {
                        let token = try_control_token("__HANDLER_CASE__")?;
                        entries.push(HandlerEntry {
                            type_name: try_clone_string(&clause.type_name)?,
                            handler: HandlerImpl::HandlerCase {
                                token: try_clone_string(&token)?,
                                var_name: None,
                                body: NIL,
                                captured_frame: Arc::clone(&env.frame),
                            },
                        });
                        clauses.push(RuntimeClause {
                            token,
                            type_name: try_clone_string(&clause.type_name)?,
                            body_bcp: clause.body_bcp,
                            var_slot: clause.var_slot,
                        });
                        values.push(resolve_sym(&clause.type_name).ok_or_else(invalid_capture)?);
                        values.push(NIL);
                    }
                    let cluster_frame =
                        push_condition_cluster_frame(torcl_rt::current_stack(), &values)?;
                    env.handlers.push(HandlerCluster { entries });
                    handlers.push(SavedHandler {
                        push_bcp,
                        clauses,
                        cluster_base,
                        cluster_frame,
                    });
                    unsafe { &mut *context.dynamic_scopes }.push(DynamicScope::HandlerCase {
                        cluster_base,
                        cluster_frame,
                    });
                } else {
                    let saved = handlers.pop().expect("live native handler cluster");
                    assert_eq!(saved.push_bcp, push_bcp);
                    let Some(DynamicScope::HandlerCase {
                        cluster_base: live_base,
                        cluster_frame: live_frame,
                    }) = unsafe { &mut *context.dynamic_scopes }.pop()
                    else {
                        return Err(invalid_capture());
                    };
                    assert_eq!(
                        (live_base, live_frame),
                        (saved.cluster_base, saved.cluster_frame)
                    );
                    env.handlers.truncate(saved.cluster_base);
                    pop_condition_cluster_frame(torcl_rt::current_stack(), saved.cluster_frame);
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
                    let token = try_control_token("__THROW__")?;
                    env.catch_stack
                        .push((tag, try_clone_string(&token)?));
                    catches.push(SavedCatch { push_bcp, token });
                } else {
                    assert_eq!(call.nargs, 0);
                    let saved = catches.last().expect("live native catch");
                    assert_eq!(saved.push_bcp, push_bcp);
                    assert!(env
                        .catch_stack
                        .last()
                        .is_some_and(|(_, token)| token == &saved.token));
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

/// Native loop-header poll used by the segment emitter. The poll may park for
/// a moving-GC rendezvous and may publish a pending signal/error, so it runs
/// behind the same rooted helper veneer as an exceptional call. A normal poll
/// returns a successful outcome; the veneer never performs a status check in
/// generated code. A nonzero result enters the existing capture/landing path.
unsafe extern "C" fn poll_or_transfer(
    _request: *mut u8,
    out: *mut torcl_rt::native_transfer::NativeOutcome,
) {
    use torcl_rt::native_transfer::{NativeExit, NativeOutcome};
    torcl_rt::safepoint::poll_safepoint();
    // A fiber can resume this pinned segment on another carrier. Recheck the
    // hardening contract only on that change; the unchanged-carrier path is a
    // single execution-local comparison. If the new carrier is incompatible,
    // leave through the ordinary capture/fallback path before running more
    // generated code.
    let carrier_ok = torcl_rt::native_transfer::revalidate_current_segment();
    if !carrier_ok {
        NATIVE_ERROR.with(|slot| {
            slot.set_first(TorclError::Internal(
                "native segment capability changed after fiber migration".into(),
            ));
        });
    }
    let exit = if !carrier_ok || super::native_loop_should_exit() != 0 {
        NativeExit::Transfer
    } else {
        NativeExit::Returned
    };
    unsafe {
        out.write(NativeOutcome { value: NIL, exit });
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

#[cfg(test)]
static NATIVE_CATCH_COUNT: torcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { torcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
static NATIVE_FALLBACK_COUNT: torcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { torcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_catch_count() -> usize {
    NATIVE_CATCH_COUNT.with(|count| count.replace(0))
}
#[cfg(test)]
pub(super) fn take_native_fallback_count() -> usize {
    NATIVE_FALLBACK_COUNT.with(|count| count.replace(0))
}

#[cfg(test)]
static NATIVE_HANDLER_COUNT: torcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { torcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(super) fn take_native_handler_count() -> usize {
    NATIVE_HANDLER_COUNT.with(|count| count.replace(0))
}

struct CaptureContext {
    handlers: *mut Vec<SavedHandler>,
    handler_binds: *mut Vec<SavedHandlerBind>,
    restart_cases: *mut Vec<SavedRestartCase>,
    restart_templates: *const RestartFunctionTemplates,
    dynamic_scopes: *mut Vec<DynamicScope>,
    cluster_frames: *mut Vec<(u32, *mut Frame)>,
    prepared_handler: *mut Option<PreparedHandler>,
    #[cfg(test)]
    unavailable_catch: Option<u32>,
    #[cfg(test)]
    unavailable_handler: Option<(u32, u32)>,
    prepared_catch: *mut Option<PreparedCatch>,
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

// Capture into reserved, rooted snapshots before live signaling can allocate
// or reenter Lisp. Rust returns before the capture stub dispatches; assembly
// only discards generated frames, never a Rust signaling or handler frame.
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    context.dispatch = DispatchPacket {
        entry: native_transfer::leave_native_segment as *const u8,
        request: native_transfer::current_segment().cast(),
    };
    let mut expected_frame = context.frame;
    let mut frames_valid = true;
    let cluster_frames = unsafe { &mut *context.cluster_frames };
    cluster_frames.clear();
    cluster_frames.extend(
        unsafe { &*context.handlers }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.extend(
        unsafe { &*context.handler_binds }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.extend(
        unsafe { &*context.restart_cases }
            .iter()
            .map(|saved| (saved.push_bcp, saved.cluster_frame)),
    );
    cluster_frames.sort_unstable_by_key(|(push_bcp, _)| *push_bcp);
    for &(_, cluster_frame) in cluster_frames.iter() {
        frames_valid &= unsafe { (*cluster_frame).prev_fp == expected_frame };
        expected_frame = cluster_frame;
    }
    if context.dispatch.request.is_null()
        || !frames_valid
        || torcl_rt::current_stack().fp() != expected_frame
    {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    } else {
        let result = guard_c2i(|| {
            if let Err(error) = unsafe { prepare_transfer(context, capture) } {
                context.failure = Some(error);
                return Err(invalid_capture());
            }
            Ok(NIL)
        });
        if let Err(error) = result {
            context
                .failure
                .get_or_insert(TransferSiteError::InvalidLandingCapture);
            NATIVE_ERROR.with(|slot| slot.set_first(error));
        }
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
    // End the mutable snapshot borrow before live signaling can allocate or
    // reenter Lisp. The execution-owned snapshot vector remains rooted.
    unsafe {
        (&mut *context.snapshots.cast::<SysvSiteSnapshot<'_>>().add(index))
            .capture_from_activation(
                context.code_base,
                capture,
                std::slice::from_raw_parts(context.activation, context.slots),
            )?;
    }
    context.selected = Some(index);
    if let Some(completed) = context.completed_cleanup {
        let cleanups = unsafe { &mut *context.cleanups };
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
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    // Signal before retiring dynamic state. Returning handlers and restart
    // searches see the original context; only selected transfers start unwind.
    if let Some(error) = NATIVE_ERROR.with(|slot| slot.take()) {
        let selected = if matches!(
            error,
            TorclError::Internal(_) | TorclError::Signalled { .. }
        ) {
            error
        } else {
            signal_raw_error_in_context(env, error)
        };
        NATIVE_ERROR.with(|slot| {
            slot.replace(Some(selected));
        });
    }
    let mut selected_transfer = false;
    let mut selected_target = SelectedTarget::OutsideFrame;
    let mut selected_clause = None;
    let mut handler_token = None;
    NATIVE_ERROR.with(|slot| {
        slot.visit(|error| {
            // A declined condition is already signaled and must leave this
            // activation. A selected enclosing restart likewise unwinds here
            // before its owner consumes the arguments. Both may run checked
            // local cleanup without guessing an outer native destination.
            if matches!(error, TorclError::Signalled { .. }) {
                selected_transfer = true;
            } else if let Some(id) = super::super::restart_invoked_id(error) {
                selected_transfer = env.restarts.iter().any(|restart| restart.id == id);
            } else if let Some(token) = handler_case_token(error) {
                selected_transfer = env.handlers.iter().any(|cluster| cluster.entries.iter().any(|entry|
                    matches!(&entry.handler, HandlerImpl::HandlerCase { token: live, .. } if live == &token)));
                for saved in unsafe { &*context.handlers } {
                    if let Some(index) = saved.clauses.iter().position(|clause| clause.token == token) {
                        selected_target = SelectedTarget::Scope { push_bcp: saved.push_bcp };
                        selected_clause = Some(index as u32);
                        break;
                    }
                }
                handler_token = Some(token);
            } else if let TorclError::Internal(token) = error {
                selected_transfer = env.catch_stack.iter().any(|(_, live)| live == token);
                if let Some(saved) = unsafe { &*context.catches }.iter().find(|saved| &saved.token == token) {
                    selected_target = SelectedTarget::Scope { push_bcp: saved.push_bcp };
                }
            }
        })
    });
    if !selected_transfer {
        return Ok(());
    }
    // Plan against scope prefixes without mutating live catch registrations.
    // If any subsequent step lacks a native destination, fallback still sees
    // the exact state described by this source site's reconstruction map.
    let scopes = &site.map().control_scopes;
    let mut remaining = scopes.as_slice();
    let action = loop {
        match next_unwind_step(remaining, selected_target) {
            NativeUnwindStep::RetireCatch { scope_index }
            | NativeUnwindStep::RetireHandler { scope_index } => {
                remaining = &remaining[..scope_index];
            }
            action => break action,
        }
    };
    let (scope_index, handler_depth, landing) = match action {
        NativeUnwindStep::RunCleanup {
            scope_index,
            handler_depth,
        } => {
            let Some(landing) = site.native_cleanup_landing(context.code_base, capture)? else {
                return Ok(());
            };
            (scope_index, handler_depth, landing)
        }
        NativeUnwindStep::EnterTarget { scope_index } => {
            let landing = match scopes[scope_index].kind {
                ScopeKind::Catch { .. } => {
                    #[cfg(test)]
                    if context.unavailable_catch == Some(scopes[scope_index].push_bcp) {
                        return Ok(());
                    }
                    site.native_catch_landing(
                        context.code_base,
                        capture,
                        scopes[scope_index].push_bcp,
                    )?
                }
                ScopeKind::HandlerCase { .. } => {
                    let Some(clause) = selected_clause else {
                        return Ok(());
                    };
                    #[cfg(test)]
                    if context.unavailable_handler == Some((scopes[scope_index].push_bcp, clause)) {
                        return Ok(());
                    }
                    site.native_handler_landing(
                        context.code_base,
                        capture,
                        scopes[scope_index].push_bcp,
                        clause,
                    )?
                }
                _ => return Ok(()),
            };
            let Some(landing) = landing else {
                return Ok(());
            };
            let handler_depth = scopes[..scope_index]
                .iter()
                .filter(|scope| {
                    matches!(
                        scope.kind,
                        ScopeKind::Block { .. }
                            | ScopeKind::Tagbody { .. }
                            | ScopeKind::Catch { .. }
                            | ScopeKind::Unwind { .. }
                            | ScopeKind::HandlerCase { .. }
                            | ScopeKind::HandlerBind { .. }
                            | ScopeKind::RestartCase { .. }
                    )
                })
                .count();
            (scope_index, handler_depth, landing)
        }
        _ => return Ok(()),
    };
    // Validate every retiring record before changing either stack. This also
    // checks selected-catch identity independently of its resume address.
    let retiring = scopes[scope_index..]
        .iter()
        .rev()
        .filter(|scope| matches!(scope.kind, ScopeKind::Catch { .. }));
    let catches = unsafe { &mut *context.catches };
    let mut saved = catches.iter().rev();
    let mut live = env.catch_stack.iter().rev();
    let mut retire_count = 0;
    for scope in retiring {
        let Some(record) = saved.next() else {
            return Ok(());
        };
        let Some((_, token)) = live.next() else {
            return Ok(());
        };
        if record.push_bcp != scope.push_bcp || &record.token != token {
            return Ok(());
        }
        retire_count += 1;
    }
    let handlers = unsafe { &mut *context.handlers };
    let mut saved_handlers = handlers.iter().rev();
    let mut handler_count = 0;
    let mut cluster_end = env.handlers.len();
    for scope in scopes[scope_index..]
        .iter()
        .rev()
        .filter(|s| matches!(s.kind, ScopeKind::HandlerCase { .. }))
    {
        let Some(saved) = saved_handlers.next() else {
            return Ok(());
        };
        if saved.push_bcp != scope.push_bcp || saved.cluster_base + 1 != cluster_end {
            return Ok(());
        }
        let Some(cluster) = env.handlers.get(saved.cluster_base) else {
            return Ok(());
        };
        if cluster.entries.len() != saved.clauses.len() || !cluster.entries.iter().zip(&saved.clauses).all(|(entry, clause)|
            matches!(&entry.handler, HandlerImpl::HandlerCase { token, .. } if token == &clause.token)) {
            return Ok(());
        }
        handler_count += 1;
        cluster_end = saved.cluster_base;
    }
    // Restore native homes while canonical snapshots still own every root.
    unsafe {
        (&*context.snapshots.cast::<SysvSiteSnapshot<'_>>().add(index))
            .write_back(context.code_base, capture)?;
    }
    let error = NATIVE_ERROR
        .with(|slot| slot.take())
        .expect("selected rooted transfer");
    let cleanups = unsafe { &mut *context.cleanups };
    while cleanups
        .last()
        .is_some_and(|saved| saved.continuation.handler_depth > handler_depth)
    {
        // Dropping the private payload cannot erase a newer throw's values,
        // even when both transfers name the same catch binding.
        cleanups.pop();
    }
    match scopes[scope_index].kind {
        ScopeKind::Unwind { cleanup_bcp } => {
            assert!(cleanups.len() < cleanups.capacity());
            cleanups.push(SavedCleanup {
                cleanup_bcp,
                continuation: CleanupCont {
                    handler_depth,
                    action: CleanupAction::Resume {
                        payload: ControlPayload::for_error(&error),
                        pending: error_to_pending(error, env),
                    },
                },
            });
            #[cfg(test)]
            NATIVE_CLEANUP_COUNT.with(|count| count.set(count.get() + 1));
        }
        ScopeKind::Catch { resume_bcp } => {
            let TorclError::Internal(token) = error else {
                unreachable!("selected catch token");
            };
            let prepared = unsafe { &mut *context.prepared_catch };
            assert!(prepared.is_none());
            *prepared = Some(PreparedCatch {
                push_bcp: scopes[scope_index].push_bcp,
                resume_bcp,
                payload: ControlPayload::take(&token),
                token,
            });
        }
        ScopeKind::HandlerCase { .. } => {
            let prepared = unsafe { &mut *context.prepared_handler };
            assert!(prepared.is_none());
            *prepared = Some(PreparedHandler {
                push_bcp: scopes[scope_index].push_bcp,
                clause_index: selected_clause.expect("selected native clause"),
                token: handler_token.expect("selected handler binding"),
                payload: ControlPayload::for_error(&error),
            });
        }
        _ => unreachable!("validated native destination"),
    }
    for _ in 0..handler_count {
        let saved = handlers.pop().expect("validated crossed handler");
        let Some(DynamicScope::HandlerCase {
            cluster_base: live_base,
            cluster_frame: live_frame,
        }) = unsafe { &mut *context.dynamic_scopes }.pop()
        else {
            return Err(TransferSiteError::InvalidLandingCapture);
        };
        if (live_base, live_frame) != (saved.cluster_base, saved.cluster_frame) {
            return Err(TransferSiteError::InvalidLandingCapture);
        }
        env.handlers.truncate(saved.cluster_base);
        pop_condition_cluster_frame(torcl_rt::current_stack(), saved.cluster_frame);
    }
    catches.truncate(catches.len() - retire_count);
    env.catch_stack
        .truncate(env.catch_stack.len() - retire_count);
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
    Ok(())
}

#[unsafe(naked)]
unsafe extern "C" fn dispatch(_packet: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "mov rax, [rdi]", "mov rdi, [rdi + 8]", "jmp rax");
}
