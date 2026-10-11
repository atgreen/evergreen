// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

/// Reserved roots and scope ledgers shared by outer and child activations.
/// Root this owner before publishing pointers into it or running Lisp.
pub(super) struct ActivationState {
    // Prepared while the source machine frame is still published. Trace it
    // through this existing owner until the handoff moves it to the next entry.
    pub(super) ready_osr: Option<Box<ActivationState>>,
    // A nonrecursive guard consumes its activation exactly once. Reserve its
    // handler and cleanup buffers before executing any protected form.
    guard_resume: Option<OuterResumeState>,
    pub(super) snapshots: Vec<SysvSiteSnapshot>,
    pub(super) cleanups: Vec<SavedCleanup>,
    pub(super) specials: Vec<SavedSpecial>,
    pub(super) catches: Vec<SavedCatch>,
    pub(super) handlers: Vec<SavedHandler>,
    pub(super) handler_binds: Vec<SavedHandlerBind>,
    pub(super) restart_cases: Vec<SavedRestartCase>,
    pub(super) restart_templates: RestartFunctionTemplates,
    pub(super) dynamic_scopes: Vec<DynamicScope>,
    pub(super) cluster_frames: Vec<(u32, *mut Frame)>,
    pub(super) prepared_handler: Option<PreparedHandler>,
    pub(super) prepared_catch: Option<PreparedCatch>,
    pub(super) catch_base: usize,
}

impl egcl_rt::gc::TraceHostRoots for ActivationState {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Some(ready) = &mut self.ready_osr {
            ready.trace_host_roots(visit);
        }
        self.guard_resume.trace_host_roots(visit);
        self.snapshots.trace_host_roots(visit);
        self.cleanups.trace_host_roots(visit);
        self.specials.trace_host_roots(visit);
        self.prepared_handler.trace_host_roots(visit);
        self.prepared_catch.trace_host_roots(visit);
        self.restart_templates.trace_host_roots(visit);
    }
}

impl ActivationState {
    pub(super) fn prepare(code: &TransferCode, env: &mut Env) -> Result<Self, EgclError> {
        let special_capacity = code.body.code.iter()
            .filter(|op| matches!(op, Instr::BindSpecial(_))).count();
        let mut specials = Vec::new();
        specials.try_reserve(special_capacity).map_err(|_| EgclError::Oom)?;
        let guard_resume = if code.has_deopt {
            #[cfg(test)]
            if FAIL_GUARD_RESERVATION.with(|fail| fail.replace(false)) {
                return Err(EgclError::Oom);
            }
            let mut state = OuterResumeState {
                debug_call: None,
                handlers: Vec::new(),
                cleanup_conts: Vec::new(),
                dyn_binds: Vec::new(),
            };
            state.handlers.try_reserve(code.guard_handler_capacity).map_err(|_| EgclError::Oom)?;
            state.cleanup_conts.try_reserve(code.cleanup_depths.len()).map_err(|_| EgclError::Oom)?;
            state.dyn_binds.try_reserve(special_capacity).map_err(|_| EgclError::Oom)?;
            Some(state)
        } else {
            None
        };
        // Reserve the control-value map while the caller can still report an
        // ordinary storage condition. Once generated code is running, payload
        // retirement and restoration must not discover a rehash allocation in
        // the middle of an unwind. The estimate covers primary, multiple-value
        // and restart-argument entries for every statically mapped scope.
        let reserve = code
            .cleanup_depths
            .len()
            .saturating_add(code.body.handler_cases.len())
            .saturating_add(
                code.body
                    .code
                    .iter()
                    .filter(|instruction| matches!(instruction, Instr::PushCatch { .. }))
                    .count(),
            )
            .saturating_mul(4)
            .saturating_add(4);
        reserve_control_values(reserve).map_err(|_| EgclError::Oom)?;
        // Registration helpers push into the live Env as well as the private
        // activation records. Reserve those tails while ordinary Rust error
        // reporting is still available; no helper entered from generated code
        // may discover a Vec growth allocation halfway through a transfer.
        env.handlers
            .try_reserve(
                code.body
                    .handler_cases
                    .len()
                    .saturating_add(code.body.handler_binds.len()),
            )
            .map_err(|_| EgclError::Oom)?;
        env.restarts
            .try_reserve(code.body.restart_cases.iter().fold(0usize, |total, info| {
                total.saturating_add(info.restarts.len())
            }))
            .map_err(|_| EgclError::Oom)?;
        env.catch_stack
            .try_reserve(
                code.body
                    .code
                    .iter()
                    .filter(|instruction| matches!(instruction, Instr::PushCatch { .. }))
                    .count(),
            )
            .map_err(|_| EgclError::Oom)?;
        let snapshots = code
            .sites
            .sites()
            .map(|site| site.reserve_snapshot())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid_capture())?;
        let cleanups = Vec::<SavedCleanup>::with_capacity(code.cleanup_depths.len());
        let catches = Vec::<SavedCatch>::with_capacity(
            code.body
                .code
                .iter()
                .filter(|i| matches!(i, Instr::PushCatch { .. }))
                .count(),
        );
        let handlers = Vec::<SavedHandler>::with_capacity(code.body.handler_cases.len());
        let handler_binds = Vec::<SavedHandlerBind>::with_capacity(code.body.handler_binds.len());
        let restart_cases = Vec::<SavedRestartCase>::with_capacity(code.body.restart_cases.len());
        // Restart clause bytecode is immutable apart from GC relocation of its
        // embedded values. Build one rooted template per static clause while
        // ordinary Rust allocation is still allowed; dynamic scope entry then
        // only clones an Rc handle instead of cloning the whole function.
        let mut restart_templates = RestartFunctionTemplates(Vec::new());
        restart_templates
            .0
            .try_reserve(code.body.restart_cases.len())
            .map_err(|_| EgclError::Oom)?;
        for info in &code.body.restart_cases {
            let mut templates = Vec::new();
            templates
                .try_reserve(info.restarts.len())
                .map_err(|_| EgclError::Oom)?;
            for restart in &info.restarts {
                templates.push(Rc::new(RefCell::new((*restart.function).clone())));
            }
            restart_templates.0.push(templates);
        }
        let dynamic_scopes = Vec::<DynamicScope>::with_capacity(
            code.body.handler_cases.len()
                + code.body.handler_binds.len()
                + code.body.restart_cases.len(),
        );
        // `prepare` runs after native code has already crossed the transfer
        // boundary. Keep its frame-chain validation allocation-free: an
        // ordinary Vec growth there would turn an otherwise reserved unwind
        // into an allocator failure before the emergency error path can run.
        let cluster_frames = Vec::<(u32, *mut Frame)>::with_capacity(
            code.body.handler_cases.len()
                + code.body.handler_binds.len()
                + code.body.restart_cases.len(),
        );
        let prepared_handler = None::<PreparedHandler>;
        let prepared_catch = None::<PreparedCatch>;

        Ok(Self {
            ready_osr: None,
            guard_resume,
            snapshots,
            cleanups,
            specials,
            catches,
            handlers,
            handler_binds,
            restart_cases,
            restart_templates,
            dynamic_scopes,
            cluster_frames,
            prepared_handler,
            prepared_catch,
            catch_base: env.catch_stack.len(),
        })
    }

    pub(super) fn is_quiescent(&self) -> bool {
        self.catches.is_empty() && self.cleanups.is_empty() && self.specials.is_empty()
            && self.handlers.is_empty() && self.handler_binds.is_empty()
            && self.restart_cases.is_empty() && self.dynamic_scopes.is_empty()
            && self.can_osr()
    }

    /// Exact registrations and saved cleanup continuations move unchanged.
    /// The OSR selector validates their order against the destination scope map.
    pub(super) fn can_osr(&self) -> bool {
        self.prepared_handler.is_none() && self.prepared_catch.is_none()
    }

    /// Commit scope ownership while the prepared destination is still traced
    /// through this source owner. No allocation separates the take and entry.
    pub(super) fn take_osr_state(&mut self) -> Box<Self> {
        let ready = self.ready_osr.as_mut().expect("OSR was prepared before commit");
        ready.catches = std::mem::take(&mut self.catches);
        ready.cleanups = std::mem::take(&mut self.cleanups);
        ready.specials = std::mem::take(&mut self.specials);
        ready.handlers = std::mem::take(&mut self.handlers);
        ready.handler_binds = std::mem::take(&mut self.handler_binds);
        ready.restart_cases = std::mem::take(&mut self.restart_cases);
        ready.dynamic_scopes = std::mem::take(&mut self.dynamic_scopes);
        ready.catch_base = self.catch_base;
        self.ready_osr.take().unwrap()
    }

    pub(super) fn top_frame(&self, frame: *mut Frame) -> *mut Frame {
        match self.dynamic_scopes.last() {
            Some(DynamicScope::HandlerCase { cluster_frame, .. }
                | DynamicScope::HandlerBind { cluster_frame, .. }
                | DynamicScope::RestartCase { cluster_frame, .. }) => *cluster_frame,
            None => frame,
        }
    }

    pub(super) fn context(
        &mut self,
        code: &TransferCode,
        frame: *mut Frame,
        children: *mut SegmentActivations,
        nested: *mut egcl_compiler::t2::native_transfer::MappedCallRecord,
    ) -> CaptureContext {
        CaptureContext {
            osr: None,
            ready_osr: &mut self.ready_osr,
            guard_resume: &mut self.guard_resume,
            children,
            nested,
            owner: code,
            completed_deopt: false,
            recursive_enabled: code.recursive_symbol.is_some(),
            recursive_profile: !profiling_disabled()
                && egcl_rt::function::is_interpreted_function(unsafe { (*frame).function }),
            recursive_generation: code.recursive_generation,
            recursive: std::ptr::null_mut(),
            recursive_escape: false,
            #[cfg(test)]
            unavailable_catch: code.unavailable_catch,
            #[cfg(test)]
            unavailable_handler: code.unavailable_handler,
            prepared_catch: &mut self.prepared_catch,
            handlers: &mut self.handlers,
            handler_binds: &mut self.handler_binds,
            restart_cases: &mut self.restart_cases,
            restart_templates: &self.restart_templates,
            dynamic_scopes: &mut self.dynamic_scopes,
            cluster_frames: &mut self.cluster_frames,
            prepared_handler: &mut self.prepared_handler,
            frame,
            body: code.body.as_ref(),
            catches: &mut self.catches,
            catch_base: self.catch_base,
            completed_cleanup: None,
            landing_stub: code.landing.as_ptr(),
            landing: SysvNativeLanding {
                stack_pointer: std::ptr::null_mut(),
                entry: std::ptr::null(),
            },
            dispatch: DispatchPacket {
                entry: std::ptr::null(),
                request: std::ptr::null_mut(),
            },
            cleanups: &mut self.cleanups,
            specials: &mut self.specials,
            cleanup_depths: &code.cleanup_depths,
            code_base: code.code.as_ptr() as usize,
            sites: &code.sites,
            snapshots: self.snapshots.as_mut_ptr().cast(),
            activation: unsafe { frame.add(1).cast::<EgclVal>() },
            slots: usize::from(code.slots),
            selected: None,
            failure: None,
        }
    }
}

#[cfg(test)]
thread_local! {
    static FAIL_GUARD_RESERVATION: Cell<bool> = const { Cell::new(false) };
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn fail_next_guard_reservation_for_test() {
    FAIL_GUARD_RESERVATION.with(|fail| fail.set(true));
}

impl Drop for ActivationState {
    fn drop(&mut self) {
        // Repeated bindings of one symbol must restore newest first.
        while self.specials.pop().is_some() {}
    }
}
