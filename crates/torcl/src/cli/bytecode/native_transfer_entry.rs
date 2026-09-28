//! Opt-in runtime entry for the tagged Invoke emitter. Normal installation is
//! still gated on complete helper/poll/scope coverage. Own code and definitions,
//! capture before leaving generated frames, then enter bytecode *unwinding*.
#![allow(dead_code)]

use super::*;
use std::cell::Cell;
use torcl_compiler::control_scope::{Ownership, ScopeKind};
use torcl_compiler::t2::native_transfer::{
    SysvTransferCapture, emit_capture_stub, emit_helper_veneer,
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
        let ir = torcl_compiler::t2::build::build_from_bytecode_for_transfers(&body).ok()?;
        let capture = JitBuffer::new(&emit_capture_stub(prepare, dispatch as *const u8))?;
        let veneer = JitBuffer::new(&emit_helper_veneer(c2i_call_legacy_v2, capture.as_ptr()))?;
        let base_slots = body.n_locals.checked_add(body.max_stack)?;
        let (emitted, sites) = torcl_compiler::t2::emit::emit_framed_transfers_with_cleanup(
            &ir,
            veneer.as_ptr() as u64,
            base_slots,
            Some((
                save_cleanup as *const () as u64,
                restore_cleanup as *const () as u64,
            )),
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
                    )
            }) {
                return None;
            }
        }
        let scopes = torcl_compiler::control_scope::ScopeMap::analyze_function(&body).ok()?;
        let mut cleanup_depths = std::collections::HashMap::new();
        for instruction in &body.code {
            if let Instr::EnterCleanupNormal { cleanup_bcp, .. } = instruction {
                let depth = scopes
                    .before(*cleanup_bcp)?
                    .iter()
                    .filter(|scope| {
                        matches!(
                            scope.kind,
                            ScopeKind::Block { .. }
                                | ScopeKind::Tagbody { .. }
                                | ScopeKind::Unwind { .. }
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
        let mut context = CaptureContext {
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
                if error.is_none() && context.selected.is_none() && cleanups.is_empty() =>
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
                                Some(Instr::CallNamed { .. })
                            )
                    })
                    .ok_or_else(invalid_capture)?;
                for (index, value) in saved.locals.iter().chain(&saved.stack).enumerate() {
                    unsafe { slot_set(frame, index as u16, *value) };
                }
                let running = site.map().control_scopes.iter().filter_map(|scope| {
                    if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
                        Some(cleanup_bcp)
                    } else {
                        None
                    }
                });
                if !running.eq(cleanups.iter().map(|saved| saved.cleanup_bcp)) {
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

unsafe extern "C" fn restore_cleanup(
    cleanup_bcp: u32,
    resume_bcp: u32,
    _unused: TorclVal,
) -> TorclVal {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let env = unsafe { &mut *NATIVE_ENV.with(Cell::get) };
    let saved = unsafe { &mut *context.cleanups }
        .pop()
        .expect("verified cleanup stack");
    assert_eq!(saved.cleanup_bcp, cleanup_bcp);
    let CleanupAction::Normal {
        resume_bcp: expected,
        value,
        values,
    } = saved.continuation.action
    else {
        unreachable!("normal native cleanup continuation");
    };
    assert_eq!(expected, resume_bcp);
    match values {
        Some(values) => env.set_mv(values),
        None => env.clear_mv(),
    }
    value
}

struct CaptureContext {
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

// No allocation, GC, yield or Lisp execution here. The owning invocation has
// already rooted/reserved snapshots and activation slots. Rust returns before
// the capture stub dispatches; assembly only discards generated frames.
unsafe extern "C" fn prepare(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    let sites = unsafe { &*context.sites };
    let site = sites.lookup(context.code_base, capture.return_pc as usize);
    if let Some((index, _)) = site.and_then(|site| {
        sites
            .sites()
            .enumerate()
            .find(|(_, candidate)| std::ptr::eq(*candidate, site))
    }) {
        let snapshot = unsafe { &mut *context.snapshots.cast::<SysvSiteSnapshot<'_>>().add(index) };
        match unsafe {
            snapshot.capture_from_activation(
                context.code_base,
                capture,
                std::slice::from_raw_parts(context.activation, context.slots),
            )
        } {
            Ok(()) => context.selected = Some(index),
            Err(error) => context.failure = Some(error),
        }
    } else {
        context.failure = Some(TransferSiteError::WrongReturnPc);
    }
    capture.request = native_transfer::current_segment().cast();
}

#[unsafe(naked)]
unsafe extern "C" fn dispatch(_anchor: *mut u8, _value: u64, _exit: NativeExit) -> ! {
    core::arch::naked_asm!("endbr64", "jmp {leave}", leave = sym native_transfer::leave_native_segment);
}
