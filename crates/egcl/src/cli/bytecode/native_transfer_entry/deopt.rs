// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A guard completes its exact bytecode continuation before returning to its
//! native caller. No pre-guard SSA home is read after that continuation runs.

use super::*;
use egcl_compiler::t2::emit::TransferDeoptRequest;

/// T0 owns and pops the current frame while running the continuation. Restore
/// the exact native-owned extent, with empty slots, before its caller retires
/// it. This also bounds any frames left behind by an intercepted Rust panic.
struct RestoreNativeFrame {
    frame: *mut Frame,
    header: Frame,
    sp: *const u8,
}

impl egcl_rt::gc::TraceHostRoots for RestoreNativeFrame {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        visit(&mut self.header.function);
    }
}

impl Drop for RestoreNativeFrame {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        while stack.fp() != self.header.prev_fp {
            assert!(
                !stack.fp().is_null(),
                "deopt crossed its owning frame boundary"
            );
            stack.pop_frame();
        }
        // The identical extent was occupied immediately before resumption;
        // this cannot exhaust the stack and does not allocate or safepoint.
        let frame = stack
            .push_frame(
                self.header.function,
                self.header.code_info,
                self.header.num_locals,
                self.header.flags,
            )
            .expect("restore occupied native extent");
        assert_eq!(frame, self.frame);
        assert_eq!(stack.sp(), self.sp);
        unsafe {
            (*frame).return_pc = self.header.return_pc;
        }
    }
}

/// Make the loop a guard failure resumed in T0 re-warm before it may enter
/// native code again by OSR (bliss-s0v50), as the checked OSR backs off. The
/// resumed activation runs nested inside this machine activation, so entering
/// again at once would nest one more machine activation per failed guard.
fn back_off_interpreter_osr(function: EgclVal) {
    if egcl_rt::function::is_interpreted_function(function) {
        egcl_rt::function::reset_back_edge_count(function);
    }
}

// FX/single failures require an exact installed numeric site. Keep the legacy
// raw-double policy for entry unboxing and its existing per-function backoff.
fn numeric_deopt_uses_function_policy(sites: &[(u32, SpecType)], guard_bcp: u32) -> bool {
    sites
        .iter()
        .any(|&(site, kind)| site == guard_bcp || kind == SpecType::DoubleFloat)
}

fn note_segment_deopt(code: &TransferCode, guard_bcp: u32) {
    let count = code.deopts.get().saturating_add(1);
    code.deopts.set(count);
    let installed = code.installed_symbol.and_then(|symbol| {
        NATIVE_REGISTRY.with(|registry| {
            registry
                .borrow()
                .get(&symbol)
                .filter(|native| {
                    matches!(&native._storage, NativeCodeStorage::Mapped(mapped)
                    if std::ptr::eq(mapped.as_ref(), code))
                })
                .cloned()
                .map(|native| (symbol, native))
        })
    });
    // Ordinary T1 keeps its existing per-function backoff policy, including
    // T0 blacklisting for unsupported domains. Retain this exact version and
    // release the registry borrow before policy code retires or replaces it.
    if let Some((symbol, native)) = &installed {
        // Installed numeric guards must reach the persistent family feedback,
        // including FX overflow with valid FX operands. Unrelated nonnumeric
        // T2 guard failures retain the segment policy below.
        let numeric_failure = native.is_t2
            && native.t2_metadata.as_ref().is_some_and(|installed| {
                numeric_deopt_uses_function_policy(&installed.speculations, guard_bcp)
            });
        if !native.is_t2 || numeric_failure {
            note_native_deopt(*symbol, Some(native), Some(guard_bcp));
            // Function policy can retire this publication immediately. Retained
            // callers must also stop entering its failed machine body; only
            // this exact activation's code is marked, never its replacement.
            let still_installed = NATIVE_REGISTRY.with(|registry| {
                registry
                    .borrow()
                    .get(symbol)
                    .is_some_and(|current| Rc::ptr_eq(current, native))
            });
            if !still_installed {
                code.request_recompile();
            }
            return;
        }
    }
    DEOPT_COUNT.fetch_add(1, AtomicOrdering::Relaxed);
    let key = Arc::as_ptr(&code.body) as usize;
    let current = SEGMENT_CACHE.with(|cache| {
        cache
            .borrow()
            .get(&key)
            .and_then(|entry| entry.code.as_ref())
            .is_some_and(|entry| std::ptr::eq(entry.as_ref(), code))
    });
    if !(current || installed.is_some() || call_table::owns_mapped(code)) {
        return;
    }
    if count == 1 {
        decay_failed_speculation(&code.body, None, code.deopt_metadata.as_deref());
    }
    // Recompile only this cached version. Resumed T0 samples the retained
    // original body; unsupported numeric phases then leave their calls generic.
    // An older suspended activation must not retire its replacement.
    if count >= deopt_blacklist_threshold() {
        code.request_recompile();
    }
}

/// Preserve the executing definition in materialized native self frames. The
/// native stream validator has already proved every suspended self-call link;
/// the general cross-function materializer intentionally only records symbols.
unsafe fn retain_self_scope_callables(
    code: &TransferCode,
    scopes: &[InlinedResumeScope],
) -> Result<(), EgclError> {
    if scopes.len() <= 1 {
        return Ok(());
    }
    if code.ordinary_entry.is_none()
        || scopes
            .iter()
            .any(|scope| !Arc::ptr_eq(&scope.body, &code.body) || scope.frame.is_null())
    {
        return Err(invalid_capture());
    }
    let callable = unsafe { (*scopes[0].frame).function };
    if !egcl_rt::function::is_interpreted_function(callable) {
        return Err(invalid_capture());
    }
    // All checks precede writes. No safepoint separates the read from these
    // stores into scanned frame headers; never resolve the mutable public name.
    for scope in &scopes[1..] {
        unsafe {
            (*scope.frame).function = callable;
        }
    }
    Ok(())
}

/// Move the outer retained activation's local handler order without creating
/// registrations. Scope-free inlined self frames rebuild their own local blocks
/// during interpreter resumption; only the outer frame owns the native ledger.
unsafe fn take_guard_resume_state(
    context: *mut CaptureContext,
    code: &TransferCode,
    scopes: &[InlinedResumeScope],
    env: &Env,
) -> Result<OuterResumeState, EgclError> {
    let Some(scope) = scopes.first() else {
        return Err(invalid_capture());
    };
    if !Arc::ptr_eq(&scope.body, &code.body) {
        return Err(invalid_capture());
    }
    if scopes.len() > 1 {
        if code.ordinary_entry.is_none()
            || code.body.has_env
            || !code.body.params_form.is_nil()
            || !scope_free_native_body(&code.body, false)
        {
            return Err(invalid_capture());
        }
        for scope in scopes {
            if !Arc::ptr_eq(&scope.body, &code.body) {
                return Err(invalid_capture());
            }
            let active = code
                .control_scopes
                .before(scope.bcp)
                .ok_or_else(invalid_capture)?;
            if active.iter().any(|scope| {
                scope.ownership != Ownership::Local
                    || !matches!(
                        scope.kind,
                        ScopeKind::Block {
                            register: false,
                            ..
                        }
                    )
            }) {
                return Err(invalid_capture());
            }
        }
    }
    // Prepared transfers cannot be replaced by lexical state. Running
    // cleanup continuations are validated below and moved with their payloads.
    let context = unsafe { &*context };
    if unsafe {
        context.completed_cleanup.is_some()
            || (*context.prepared_handler).is_some()
            || (*context.prepared_catch).is_some()
    } {
        return Err(invalid_capture());
    }
    let active = code
        .control_scopes
        .before(scope.bcp)
        .ok_or_else(invalid_capture)?;
    if active.iter().any(|scope| {
        scope.ownership != Ownership::Local
            || !matches!(
                scope.kind,
                ScopeKind::Block { register: false, .. }
                    | ScopeKind::Tagbody { .. }
                    | ScopeKind::Catch { .. }
                    | ScopeKind::Unwind { .. }
                    | ScopeKind::Cleanup { .. }
                    | ScopeKind::HandlerCase { .. }
                    | ScopeKind::HandlerBind { .. }
                    | ScopeKind::RestartCase { .. } | ScopeKind::SpecialBinding { .. }
                    // The resumed activation takes the state's EnvFrame.
                    | ScopeKind::LexicalEnvironment
            )
    }) {
        return Err(invalid_capture());
    }
    // PendingCleanup is deliberately excluded: its unwind handler has been
    // popped, but EnterCleanupNormal has not yet saved a continuation. Neither
    // reconstructing an Unwind nor inventing a CleanupCont represents it.
    let cleanups = unsafe { &mut *context.cleanups };
    let mut saved_cleanups = cleanups.iter();
    let mut handler_depth = 0;
    for scope in active {
        if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
            let saved = saved_cleanups.next().ok_or_else(invalid_capture)?;
            if saved.cleanup_bcp != cleanup_bcp || saved.continuation.handler_depth != handler_depth
            {
                return Err(invalid_capture());
            }
        } else if !matches!(
            scope.kind,
            ScopeKind::SpecialBinding { .. } | ScopeKind::LexicalEnvironment
        ) {
            handler_depth += 1;
        }
    }
    if saved_cleanups.next().is_some() {
        return Err(invalid_capture());
    }
    if !unsafe { validate_special_bindings(context, active) } {
        return Err(invalid_capture());
    }
    let specials = unsafe { &mut *context.specials };
    let catches = unsafe { &mut *context.catches };
    let expected_catches = active.iter().filter_map(|scope| {
        matches!(scope.kind, ScopeKind::Catch { .. }).then_some(scope.push_bcp)
    });
    let tail = env
        .catch_stack
        .get(context.catch_base..)
        .ok_or_else(invalid_capture)?;
    if !expected_catches.eq(catches.iter().map(|saved| saved.push_bcp))
        || tail.len() != catches.len()
        || !tail
            .iter()
            .zip(catches.iter())
            .all(|((_, token), saved)| *token == saved.token)
    {
        return Err(invalid_capture());
    }
    // Complete every fallible step before taking the native ledger. The
    // remaining moves neither allocate Lisp objects nor change Env's tags.
    let mut state = if context.recursive.is_null() {
        let reserved = unsafe { &mut *context.guard_resume };
        let state = reserved.as_ref().ok_or_else(invalid_capture)?;
        if state.debug_call.is_some()
            || !state.handlers.is_empty()
            || !state.cleanup_conts.is_empty()
            || !state.dyn_binds.is_empty()
            || state.handlers.capacity() < handler_depth
            || state.cleanup_conts.capacity() < cleanups.len()
            || state.dyn_binds.capacity() < specials.len()
        {
            return Err(invalid_capture());
        }
        reserved
            .take()
            .expect("validated guard recovery reservation")
    } else {
        // Direct recursive frames share this context but remain scope-free.
        // Do not consume the outer activation's one-shot cleanup reservation.
        if !cleanups.is_empty()
            || !catches.is_empty()
            || !specials.is_empty()
            || unsafe {
                !(*context.handlers).is_empty()
                    || !(*context.handler_binds).is_empty()
                    || !(*context.restart_cases).is_empty()
                    || !(*context.dynamic_scopes).is_empty()
            }
            || active.iter().any(|scope| {
                matches!(
                    scope.kind,
                    ScopeKind::Unwind { .. }
                        | ScopeKind::Cleanup { .. }
                        | ScopeKind::Catch { .. }
                        | ScopeKind::HandlerCase { .. }
                        | ScopeKind::HandlerBind { .. }
                        | ScopeKind::RestartCase { .. }
                        | ScopeKind::SpecialBinding { .. }
                )
            })
        {
            return Err(invalid_capture());
        }
        let mut handlers = Vec::new();
        handlers
            .try_reserve(handler_depth)
            .map_err(|_| EgclError::Oom)?;
        OuterResumeState {
            debug_call: None,
            handlers,
            cleanup_conts: Vec::new(),
            dyn_binds: Vec::new(),
        }
    };
    egcl_rt::rooted_ref!(_state_root = &mut state);
    state
        .cleanup_conts
        .extend(cleanups.drain(..).map(|saved| saved.continuation));
    state
        .dyn_binds
        .extend(specials.drain(..).map(|saved| saved.binding));
    let mut saved_catches = catches.drain(..);
    // The shared validator checked each ledger against this lexical order
    // before any native metadata changed. Move registrations, never rebuild
    // them: tokens, restart binding IDs and captured handler functions stay live.
    let mut saved_handlers = unsafe { &mut *context.handlers }.drain(..);
    let mut saved_binds = unsafe { &mut *context.handler_binds }.drain(..);
    let mut saved_restarts = unsafe { &mut *context.restart_cases }.drain(..);
    // The resumed activation takes the guard site's frame; each handler keeps
    // the frame it was established in (bliss-uhsp5).
    let site_env_frame = unsafe { (*context.env_frame).clone() };
    for (scope, scope_env) in active.iter().zip(scope_env_frames(active, &site_env_frame)) {
        let handler = match scope.kind {
            ScopeKind::Block {
                id,
                resume_bcp,
                register: false,
            } => Handler::Block {
                block_id: id,
                token: String::new(),
                resume_bcp,
                sp_restore: scope.sp_restore,
                env: scope_env,
            },
            ScopeKind::Tagbody { id } => Handler::Tag {
                tagbody_id: id,
                sp_restore: scope.sp_restore,
                token: None,
                tag_bcps: Vec::new(),
                env: scope_env,
            },
            ScopeKind::Unwind { cleanup_bcp } => Handler::Unwind {
                cleanup_bcp,
                sp_restore: scope.sp_restore,
                env: scope_env,
            },
            ScopeKind::Cleanup { .. }
            | ScopeKind::SpecialBinding { .. }
            | ScopeKind::LexicalEnvironment => continue,
            ScopeKind::Catch { resume_bcp } => Handler::Catch {
                token: saved_catches.next().expect("validated catch order").token,
                resume_bcp,
                sp_restore: scope.sp_restore,
                env: scope_env,
            },
            ScopeKind::HandlerCase { .. } => {
                let saved = saved_handlers.next().expect("validated handler-case order");
                Handler::HandlerCase {
                    clauses: saved.clauses,
                    sp_restore: scope.sp_restore,
                    cluster_base: saved.cluster_base,
                    cluster_frame: saved.cluster_frame,
                    env: scope_env,
                }
            }
            ScopeKind::HandlerBind { .. } => {
                let saved = saved_binds.next().expect("validated handler-bind order");
                Handler::HandlerBind {
                    cluster_base: saved.cluster_base,
                    cluster_frame: saved.cluster_frame,
                }
            }
            ScopeKind::RestartCase { resume_bcp, .. } => {
                let saved = saved_restarts.next().expect("validated restart-case order");
                Handler::RestartCase {
                    restart_base: saved.restart_base,
                    resume_bcp,
                    sp_restore: scope.sp_restore,
                    cluster_frame: saved.cluster_frame,
                    env: scope_env,
                }
            }
            _ => unreachable!("validated local guard scopes"),
        };
        state.handlers.push(handler);
    }
    // T0 now owns the cluster frames and Env registrations. Source native
    // scope guards must not retire them again when this continuation returns.
    unsafe { &mut *context.dynamic_scopes }.clear();
    Ok(state)
}

/// Validate the complete stream before changing native metadata or activation
/// slots. Ordinary guards may reconstruct retained self frames; legacy mapped
/// guards still own exactly one logical frame.
unsafe fn validate_guard_stream(
    request: &TransferDeoptRequest,
    code: &TransferCode,
    header: &Frame,
    metadata: &T2InstalledMetadata,
) -> Result<u32, EgclError> {
    unsafe { validate_guard_stream_slots(request, code, header.num_locals, metadata) }
}

unsafe fn validate_guard_stream_slots(
    request: &TransferDeoptRequest,
    code: &TransferCode,
    slots: u16,
    metadata: &T2InstalledMetadata,
) -> Result<u32, EgclError> {
    let max_words = request
        .n_scopes
        .checked_mul(4 + u64::from(code.body.n_locals) + u64::from(code.body.max_stack))
        .ok_or_else(invalid_capture)?;
    let word_count = usize::try_from(request.n_words).map_err(|_| invalid_capture())?;
    if request.n_scopes == 0
        || (code.ordinary_entry.is_none() && request.n_scopes != 1)
        || request.words.is_null()
        || request.n_words < 4
        || request.n_scopes > request.n_words / 4
        || request.n_words > max_words
        || word_count > isize::MAX as usize / std::mem::size_of::<u64>()
    {
        return Err(invalid_capture());
    }
    // Ordinary ingress additionally bounds this entire buffer within the
    // published native temporary extent before adapting the request.
    let words = unsafe { std::slice::from_raw_parts(request.words, word_count) };
    validate_guard_scope_chain(words, request.n_scopes, code, slots, metadata)
}

/// Validate retained logical self frames without changing slots or native state.
/// Only ordinary scope-free self expansion may own more than one logical frame.
fn validate_guard_scope_chain(
    words: &[u64],
    count: u64,
    code: &TransferCode,
    slots: u16,
    metadata: &T2InstalledMetadata,
) -> Result<u32, EgclError> {
    let ranges = deopt_payload_ranges(words, count)?;
    let nested = ranges.len() > 1;
    if slots < code.body.num_slots()
        || (nested
            && (code.ordinary_entry.is_none()
                || code.body.has_env
                || !code.body.params_form.is_nil()
                || !scope_free_native_body(&code.body, false)))
    {
        return Err(invalid_capture());
    }
    let depths = verified_operand_stack_depths(&code.body).map_err(|_| invalid_capture())?;
    for (index, range) in ranges.iter().enumerate() {
        let header = &words[range.start - 4..range.start];
        let function = u32::try_from(header[0]).map_err(|_| invalid_capture())?;
        let bcp = u32::try_from(header[1]).map_err(|_| invalid_capture())?;
        let body = metadata
            .deopt_bodies
            .get(&function)
            .ok_or_else(invalid_capture)?;
        if !Arc::ptr_eq(body, &code.body)
            || header[2] != u64::from(body.n_locals)
            || header[3] > u64::from(body.max_stack)
        {
            return Err(invalid_capture());
        }
        let active = code
            .control_scopes
            .before(bcp)
            .ok_or_else(invalid_capture)?;
        if nested
            && active.iter().any(|scope| {
                scope.ownership != Ownership::Local
                    || !matches!(
                        scope.kind,
                        ScopeKind::Block {
                            register: false,
                            ..
                        }
                    )
            })
        {
            return Err(invalid_capture());
        }
        let suspended = index + 1 < ranges.len();
        let saved = header[3] as u32;
        if depths.get(bcp as usize).copied().flatten() != Some(saved + u32::from(suspended)) {
            return Err(invalid_capture());
        }
        if suspended {
            let call_pc = bcp.checked_sub(1).ok_or_else(invalid_capture)?;
            let Some(Instr::CallNamed { sym, nargs }) = body.code.get(call_pc as usize) else {
                return Err(invalid_capture());
            };
            if *sym != function
                || *nargs != body.arity
                || !body.self_call_sites.contains(&call_pc)
                || depths
                    .get(call_pc as usize)
                    .copied()
                    .flatten()
                    .and_then(|depth| depth.checked_sub(u32::from(*nargs)))
                    != Some(saved)
                || words[ranges[index + 1].start - 4] != u64::from(*sym)
            {
                return Err(invalid_capture());
            }
        }
    }
    u32::try_from(words[1]).map_err(|_| invalid_capture())
}

/// The top deoptimizing native map deliberately omits its old SSA homes.
/// Keep the serialized tagged payload and immutable argument copies rooted in
/// place until the resumed interpreter owns its values. Header words are raw.
struct NativeDeoptRoots {
    payload: *mut EgclVal,
    payload_ranges: Vec<std::ops::Range<usize>>,
    arguments: *mut EgclVal,
    argument_count: usize,
}

impl egcl_rt::gc::TraceHostRoots for NativeDeoptRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for range in &self.payload_ranges {
            for index in range.clone() {
                visit(unsafe { self.payload.add(index) });
            }
        }
        for index in 0..self.argument_count {
            visit(unsafe { self.arguments.add(index) });
        }
    }
}

/// Structural validation only: callers must separately validate retained bodies,
/// bytecode PCs, slot capacities and representations before scanning these roots.
fn deopt_payload_ranges(
    words: &[u64],
    n_scopes: u64,
) -> Result<Vec<std::ops::Range<usize>>, EgclError> {
    let count = usize::try_from(n_scopes).map_err(|_| invalid_capture())?;
    if count == 0 || count > words.len() / 4 {
        return Err(invalid_capture());
    }
    let mut ranges = Vec::new();
    ranges.try_reserve(count).map_err(|_| EgclError::Oom)?;
    let mut offset = 0usize;
    for _ in 0..count {
        let start = offset.checked_add(4).ok_or_else(invalid_capture)?;
        let header = words.get(offset..start).ok_or_else(invalid_capture)?;
        let locals = usize::try_from(header[2]).map_err(|_| invalid_capture())?;
        let stack = usize::try_from(header[3]).map_err(|_| invalid_capture())?;
        let end = start
            .checked_add(locals)
            .and_then(|end| end.checked_add(stack))
            .filter(|&end| end <= words.len())
            .ok_or_else(invalid_capture)?;
        ranges.push(start..end);
        offset = end;
    }
    if offset != words.len() {
        return Err(invalid_capture());
    }
    Ok(ranges)
}

#[cfg(test)]
mod retained_guard_chain_tests {
    use super::*;
    use egcl_rt::Collector;

    thread_local! {
        static OBSERVED_SCOPES: Cell<u64> = const { Cell::new(0) };
        static OBSERVED_GUARDS: Cell<u64> = const { Cell::new(0) };
        static COLLECT_AT_NESTED_GUARD: Cell<bool> = const { Cell::new(false) };
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn ordinary_nested_self_guard_resumes_three_real_scopes() {
        use egcl_compiler::t2::ir::{AuxData, Opcode};
        use egcl_compiler::t2::{build, inlining::InlineOptions, speculate, verify};
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        crate::cli::read_eval_all_env(
            "(defun ordinary-nested-self (n x) (declare (type fixnum n))
               (if (< n 1) (progn (+ (car x) 1) (+ (car x) 1))
                   (+ 1 (ordinary-nested-self (- n 1) x))))",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
        egcl_rt::rooted!(
            forms = reader::read_from_string(
                "((declare (type fixnum n))
              (if (< n 1) (progn (+ (car x) 1) (+ (car x) 1))
                  (+ 1 (ordinary-nested-self (- n 1) x))))"
            )
            .unwrap()
            .0
        );
        let symbol = egcl_rt::symbols::intern("ORDINARY-NESTED-SELF");
        let body = Arc::new(
            compile_function("ORDINARY-NESTED-SELF", *params, *forms, &env, false, false).unwrap(),
        );
        assert_eq!(body.self_call_sites.len(), 1);
        registry_put(symbol, Arc::clone(&body));
        let input = egcl_rt::CrossThreadRoot::new(snapshot_t2_input(symbol, 0).unwrap());
        let call_cells = input.with_gc_stable_mutator(|input| input.call_cells.clone());
        let mut options = InlineOptions::default()
            .with_root_symbol(symbol)
            .with_body(symbol, Arc::clone(&body));
        options.config.self_inline_depth = 2;
        options.config.small_threshold = 100;
        let mut ordinary = build::build_from_bytecode_with_inline_options(&body, options).unwrap();
        speculate::speculate(&mut ordinary, &|_| Some(speculate::SpecType::Fixnum));
        verify::verify(&ordinary).unwrap();
        assert!(
            ordinary
                .frame_states
                .iter()
                .any(|(_, state)| state.scopes.len() == 3),
            "fixture must actually expand two recursive levels"
        );
        assert!(ordinary.block_order().iter().flat_map(|&block| &ordinary.block(block).insts)
            .any(|&inst| ordinary.inst(inst).opcode == Opcode::Call
                && matches!(ordinary.inst(inst).aux, AuxData::CallTarget(target) if target == symbol)),
            "bounded expansion must retain a recursive leaf");
        assert_retained_leaf_identity(&ordinary, &body, symbol);
        assert_ordinary_scope_shape_rejections(&ordinary, &call_cells);
        let mut baseline = build::build_for_transfer_optimization(&body, symbol).unwrap();
        let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).unwrap();
        build::legalize_transfer_calls(&mut baseline, &scopes).unwrap();
        let metadata = Arc::new(T2InstalledMetadata {
            speculations: Vec::new(),
            cold_guesses: Vec::new(),
            _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
                bodies: vec![Arc::clone(&body)],
            }),
            deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
        });
        let mut code = TransferCode::emit_prepared_with_ordinary(
            Arc::clone(&body),
            retain_native_body(&body),
            &baseline,
            Some(&ordinary),
            Some(metadata),
            &call_cells,
            false,
            CompilationTier::T2,
        )
        .expect("emit retained self expansion");
        code.set_installed_symbol(symbol);
        assert!(
            code.ordinary_entry.is_some(),
            "nested guard fixture must use the ordinary ABI"
        );
        assert!(code.has_deopt);
        assert_guard_request_bound_rejections(&mut code);
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        let environment_depths = (
            env.handlers.len(),
            env.restarts.len(),
            env.catch_stack.len(),
        );
        egcl_rt::rooted!(callable = egcl_rt::symbols::symbol_function(symbol).unwrap());
        // A direct float parameter would fail an entry preguard before reaching
        // the nested branch. Its first addition fails a guard; the second must
        // reload X from a relocated logical local after recovery.
        for (mode, n) in [
            ("normal", 2),
            ("gc", 2),
            ("error", 2),
            ("deep", 5),
            ("redefined", 5),
        ] {
            if mode == "redefined" {
                crate::cli::read_eval_all_env(
                    "(defun ordinary-nested-self (n x) (declare (ignore n x)) 100)",
                    &mut env,
                )
                .unwrap();
            }
            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
            egcl_rt::rooted!(
                argument = crate::cli::arena_cons(
                    if mode == "error" {
                        NIL
                    } else {
                        EgclVal::from_single_float(2.5)
                    },
                    NIL
                )
            );
            let original = argument.to_raw();
            OBSERVED_SCOPES.with(|seen| seen.set(0));
            OBSERVED_GUARDS.with(|seen| seen.set(0));
            COLLECT_AT_NESTED_GUARD.with(|collect| collect.set(mode == "gc"));
            egcl_rt::rooted!(
                result = with_ordinary_guard_resume_observer_for_test(
                    |scopes| {
                        assert_eq!(scopes, 3);
                        OBSERVED_SCOPES.with(|seen| seen.set(scopes));
                        OBSERVED_GUARDS.with(|seen| seen.set(seen.get() + 1));
                        if COLLECT_AT_NESTED_GUARD.with(Cell::get) {
                            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
                        }
                    },
                    || code.run_callable(
                        *callable,
                        &[EgclVal::from_fixnum(n), *argument],
                        &mut env
                    ),
                )
            );
            assert_eq!(
                OBSERVED_SCOPES.with(Cell::get),
                3,
                "observe actual native guard request"
            );
            assert_eq!(
                OBSERVED_GUARDS.with(Cell::get),
                if n == 2 { 1 } else { 2 },
                "the deep child and its native parent's result guard each resume once: {mode}"
            );
            if mode == "error" {
                assert!(
                    matches!(&*result, Err(EgclError::Signalled(details))
                    if condition_matches_handler(&env, details.condition, "TYPE-ERROR")),
                    "{:?}",
                    &*result
                );
            } else {
                assert_eq!(
                    *result.as_ref().unwrap(),
                    EgclVal::from_single_float(n as f32 + 3.5),
                    "{mode}"
                );
            }
            if mode == "gc" {
                assert_ne!(
                    argument.to_raw(),
                    original,
                    "guard collection must really relocate X"
                );
            }
            assert_eq!((stack.fp(), stack.sp()), before, "{mode}");
            assert_eq!(
                (
                    env.handlers.len(),
                    env.restarts.len(),
                    env.catch_stack.len()
                ),
                environment_depths
            );
        }
        exercise_retained_cold_leaf(&code, *callable, &mut env);
    }

    fn exercise_retained_cold_leaf(code: &TransferCode, callable: EgclVal, env: &mut Env) {
        use super::super::ordinary_calls::SELF_COLD_OBSERVER;
        use egcl_compiler::t2::emit::NativeRecursiveControl;
        thread_local! {
            static SAVED_LIMIT: Cell<u64> = const { Cell::new(0) };
            static COLD_OBSERVED: Cell<u64> = const { Cell::new(0) };
        }
        fn control() -> *mut NativeRecursiveControl {
            let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
            let control = unsafe { &*execution }.active_native_control();
            assert_ne!(control, 0);
            control as *mut NativeRecursiveControl
        }
        fn deny_child() {
            let control = control();
            SAVED_LIMIT.with(|saved| saved.set(unsafe { (*control).stack_limit }));
            unsafe {
                (*control).stack_limit = u64::MAX;
            }
        }
        fn collect_in_cold_call() {
            unsafe {
                (*control()).stack_limit = SAVED_LIMIT.with(Cell::get);
            }
            COLD_OBSERVED.with(|seen| seen.set(seen.get() + 1));
            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
        }
        struct RestoreObserver(Option<fn()>);
        impl Drop for RestoreObserver {
            fn drop(&mut self) {
                SELF_COLD_OBSERVER.with(|hook| hook.set(self.0));
            }
        }
        egcl_rt::rooted!(callable = callable);
        let _restore = RestoreObserver(
            SELF_COLD_OBSERVER.with(|hook| hook.replace(Some(collect_in_cold_call))),
        );
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        let environment_depths = (
            env.handlers.len(),
            env.restarts.len(),
            env.catch_stack.len(),
        );
        // The public name already returns 100. Both cases must dispatch the
        // retained definition with the leaf's n=2, not the root's n=5.
        for error in [false, true] {
            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
            egcl_rt::rooted!(
                argument = crate::cli::arena_cons(
                    if error {
                        NIL
                    } else {
                        EgclVal::from_single_float(2.5)
                    },
                    NIL
                )
            );
            let original = argument.to_raw();
            COLD_OBSERVED.with(|seen| seen.set(0));
            egcl_rt::rooted!(
                result = with_ordinary_entry_hook_for_test(deny_child, || code.run_callable(
                    *callable,
                    &[EgclVal::from_fixnum(5), *argument],
                    env
                ))
            );
            assert_eq!(
                COLD_OBSERVED.with(Cell::get),
                1,
                "force exactly one retained cold leaf"
            );
            assert_ne!(
                argument.to_raw(),
                original,
                "cold collection must relocate the argument"
            );
            if error {
                assert!(
                    matches!(&*result, Err(EgclError::Signalled(details))
                    if condition_matches_handler(env, details.condition, "TYPE-ERROR")),
                    "{:?}",
                    &*result
                );
            } else {
                assert_eq!(*result.as_ref().unwrap(), EgclVal::from_single_float(8.5));
            }
            assert_eq!((stack.fp(), stack.sp()), before);
            assert_eq!(
                (
                    env.handlers.len(),
                    env.restarts.len(),
                    env.catch_stack.len()
                ),
                environment_depths
            );
        }
    }

    fn assert_retained_leaf_identity(
        ir: &egcl_compiler::t2::ir::Function,
        body: &BytecodeFunction,
        symbol: u32,
    ) {
        use egcl_compiler::t2::ir::{AuxData, Opcode};
        let leaves: Vec<_> = ir
            .block_order()
            .iter()
            .flat_map(|&block| ir.block(block).insts.iter().copied())
            .filter(|&inst| {
                ir.inst(inst).opcode == Opcode::Call
                    && matches!(ir.inst(inst).aux, AuxData::CallTarget(target) if target == symbol)
            })
            .collect();
        assert!(!leaves.is_empty());
        assert_eq!(
            ordinary_t2::proven_self_calls(ir, body, symbol),
            leaves,
            "residual calls inside retained self frames must keep direct self identity"
        );
        let mut unproven = body.clone();
        unproven.self_call_sites.clear();
        assert!(
            ordinary_t2::proven_self_calls(ir, &unproven, symbol).is_empty(),
            "matching names alone do not prove retained identity"
        );
        for fault in [
            "mixed", "parent", "leaf", "empty", "locals", "stack", "arity",
        ] {
            let mut invalid = ir.clone();
            for &leaf in &leaves {
                let state = invalid.inst(leaf).frame_state.unwrap();
                if fault == "arity" {
                    invalid.inst_mut(leaf).args.pop();
                }
                let scopes = &mut invalid.frame_states.get_mut(state).scopes;
                assert_eq!(scopes.len(), 3);
                match fault {
                    "mixed" => scopes[1].function = symbol.wrapping_add(1),
                    "parent" => scopes[0].bcp = 0,
                    "leaf" => scopes.last_mut().unwrap().bcp = u32::MAX,
                    "empty" => scopes.clear(),
                    "locals" => scopes[1].locals.clear(),
                    "stack" => scopes[0].stack.clear(),
                    _ => {}
                }
            }
            assert!(
                ordinary_t2::proven_self_calls(&invalid, body, symbol).is_empty(),
                "{fault}"
            );
        }
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn nested_argument_guard_keeps_retained_contract_after_redefinition() {
        use egcl_compiler::t2::{build, inlining::InlineOptions, speculate};
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "RETAINED-NESTED-CONTRACT";
        let forms = "((declare (type (integer 1 10) n))
            (if (< n 1) 0 (+ 1 (retained-nested-contract (- n 1)))))";
        crate::cli::read_eval_all_env(
            "(defun retained-nested-contract (n) (declare (type (integer 1 10) n))
                (if (< n 1) 0 (+ 1 (retained-nested-contract (- n 1)))))",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
        let symbol = egcl_rt::symbols::intern(name);
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        registry_put(symbol, Arc::clone(&body));
        egcl_rt::rooted!(callable = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let input = egcl_rt::CrossThreadRoot::new(snapshot_t2_input(symbol, 0).unwrap());
        let cells = input.with_gc_stable_mutator(|input| input.call_cells.clone());
        let mut options = InlineOptions::default()
            .with_root_symbol(symbol)
            .with_body(symbol, Arc::clone(&body));
        options.config.self_inline_depth = 2;
        options.config.small_threshold = 100;
        let mut ordinary = build::build_from_bytecode_with_inline_options(&body, options).unwrap();
        speculate::speculate(&mut ordinary, &|_| Some(speculate::SpecType::Fixnum));
        let mut baseline = build::build_for_transfer_optimization(&body, symbol).unwrap();
        let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).unwrap();
        build::legalize_transfer_calls(&mut baseline, &scopes).unwrap();
        let metadata = Arc::new(T2InstalledMetadata {
            speculations: Vec::new(),
            cold_guesses: Vec::new(),
            _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
                bodies: vec![Arc::clone(&body)],
            }),
            deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
        });
        let mut code = TransferCode::emit_prepared_with_ordinary(
            Arc::clone(&body),
            retain_native_body(&body),
            &baseline,
            Some(&ordinary),
            Some(metadata),
            &cells,
            false,
            CompilationTier::T2,
        )
        .unwrap();
        code.set_installed_symbol(symbol);
        assert!(code.ordinary_entry.is_some());
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        for (redefined, n) in [(false, 2), (true, 2), (true, 5)] {
            if redefined {
                crate::cli::read_eval_all_env(
                    "(defun retained-nested-contract (n) (declare (ignore n)) 100)",
                    &mut env,
                )
                .unwrap();
            }
            OBSERVED_SCOPES.with(|seen| seen.set(0));
            egcl_rt::rooted!(
                result = with_ordinary_guard_resume_observer_for_test(
                    |scopes| {
                        OBSERVED_SCOPES.with(|seen| seen.set(scopes));
                    },
                    || code.run_callable(*callable, &[EgclVal::from_fixnum(n)], &mut env),
                )
            );
            assert_eq!(
                OBSERVED_SCOPES.with(Cell::get),
                2,
                "the second erased call must fail its argument contract inside an inlined frame"
            );
            assert!(
                matches!(&*result, Err(EgclError::Signalled(details))
                if condition_matches_handler(&env, details.condition, "TYPE-ERROR")),
                "retained argument zero must fail, redefined={redefined}, n={n}: {:?}",
                &*result
            );
            assert_eq!((stack.fp(), stack.sp()), before);
        }
    }

    fn assert_guard_request_bound_rejections(code: &mut TransferCode) {
        let words = [0u64; 8];
        for (n_scopes, n_words) in [(0, 4), (u64::MAX, 8), (1, u64::MAX), (3, 8)] {
            let request = TransferDeoptRequest {
                n_scopes,
                n_words,
                words: words.as_ptr(),
                activation: std::ptr::null_mut(),
            };
            assert!(
                unsafe {
                    validate_guard_stream_slots(
                        &request,
                        code,
                        code.body.num_slots(),
                        code.deopt_metadata.as_ref().unwrap(),
                    )
                }
                .is_err(),
                "invalid bounds must reject before reading beyond the buffer"
            );
        }
        let entry = code.ordinary_entry.take();
        let request = TransferDeoptRequest {
            n_scopes: 2,
            n_words: 8,
            words: words.as_ptr(),
            activation: std::ptr::null_mut(),
        };
        let rejected = unsafe {
            validate_guard_stream_slots(
                &request,
                code,
                code.body.num_slots(),
                code.deopt_metadata.as_ref().unwrap(),
            )
        }
        .is_err();
        code.ordinary_entry = entry;
        assert!(rejected, "legacy mapped recovery remains single-scope");
    }

    fn assert_ordinary_scope_shape_rejections(
        ordinary: &egcl_compiler::t2::ir::Function,
        cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
    ) {
        use egcl_compiler::t2::{
            emit,
            ir::{AuxData, Opcode},
        };
        let mut ordinary = ordinary.clone();
        let mut cells = cells.to_vec();
        ordinary_primitives::lower(&mut ordinary, &mut cells).unwrap();
        egcl_compiler::t2::opt_clear_mv::eliminate_redundant(&mut ordinary, true);
        let targets: Vec<_> = ordinary
            .block_order()
            .iter()
            .flat_map(|&block| ordinary.block(block).insts.iter().copied())
            .filter(|&inst| ordinary.inst(inst).opcode == Opcode::Call)
            .map(|inst| {
                let AuxData::CallTarget(symbol) = ordinary.inst(inst).aux else {
                    panic!("named call")
                };
                (
                    inst,
                    emit::OrdinaryCallTarget {
                        symbol,
                        descriptor_slot: 1,
                        heat_slot: 1,
                        revision_address: 1,
                    },
                )
            })
            .collect();
        // These nonzero addresses are emitted only, never executed.
        let emit = |function: &egcl_compiler::t2::ir::Function| {
            emit::emit_framed_ordinary_function_with_poll(
                function,
                emit::OrdinaryLispEntryOptions {
                    require_callable_home: false,
                    arity: 2,
                    arity_error_veneer: 1,
                    binding: None,
                },
                &targets,
                emit::OrdinaryCallFlags {
                    gc_requested: 1,
                    sigusr1_pending: 1,
                    posted_signal: 1,
                    native_errors: 1,
                    signal_epoch: 1,
                },
                1,
                emit::OrdinaryGuardOptions {
                    veneer: 1,
                    multiple_values: 1,
                },
                1,
            )
        };
        assert!(emit(&ordinary).is_ok(), "valid same-function chain control");
        let nested = ordinary
            .frame_states
            .iter()
            .find(|(_, state)| state.scopes.len() == 3)
            .unwrap()
            .0;
        for mixed in [false, true] {
            let mut invalid = ordinary.clone();
            let scopes = &mut invalid.frame_states.get_mut(nested).scopes;
            if mixed {
                scopes[1].function = scopes[0].function.wrapping_add(1);
            } else {
                scopes.clear();
            }
            assert!(
                emit(&invalid).is_err(),
                "empty or cross-function chains must not emit"
            );
        }
    }

    fn fixture(env: &mut Env) -> (u32, Rc<NativeCode>) {
        crate::cli::read_eval_all_env(
            "(defun retained-guard-chain (n)
               (declare (type fixnum n))
               (if (< n 1) n (+ 1 (retained-guard-chain (- n 1)))))",
            env,
        )
        .expect("install the source identity before proving recursive calls");
        crate::cli::bytecode::native_transfer_tests::ordinary_t2::install(
            "RETAINED-GUARD-CHAIN",
            "(n)",
            "((declare (type fixnum n))
              (if (< n 1) n (+ 1 (retained-guard-chain (- n 1)))))",
            env,
        )
    }

    fn mapped(code: &NativeCode) -> &TransferCode {
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            panic!("fixture must install ordinary mapped T2");
        };
        assert!(code.is_t2);
        assert!(mapped.ordinary_entry.is_some());
        mapped
    }

    fn frame_words(symbol: u32, body: &BytecodeFunction, bcp: u32, depth: u32) -> Vec<u64> {
        let mut words = vec![
            u64::from(symbol),
            u64::from(bcp),
            u64::from(body.n_locals),
            u64::from(depth),
        ];
        words.resize(
            4 + usize::from(body.n_locals) + depth as usize,
            EgclVal::from_fixnum(1).to_raw(),
        );
        words
    }

    fn validate(words: &[u64], count: u64, code: &TransferCode) -> Result<u32, EgclError> {
        validate_guard_scope_chain(
            words,
            count,
            code,
            code.body.num_slots(),
            code.deopt_metadata
                .as_ref()
                .expect("retained deopt metadata"),
        )
    }

    fn validate_native(words: &[u64], count: u64, code: &TransferCode) -> Result<u32, EgclError> {
        let request = TransferDeoptRequest {
            n_scopes: count,
            n_words: words.len() as u64,
            words: words.as_ptr(),
            activation: std::ptr::null_mut(),
        };
        unsafe {
            validate_guard_stream_slots(
                &request,
                code,
                code.body.num_slots(),
                code.deopt_metadata
                    .as_ref()
                    .expect("retained deopt metadata"),
            )
        }
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn retained_self_guard_chain_validates_every_logical_frame() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, owner) = fixture(&mut env);
        let code = mapped(&owner);
        let body = &code.body;
        let depths = verified_operand_stack_depths(body).expect("verified fixture bytecode");
        let &[call_pc] = body.self_call_sites.as_slice() else {
            panic!("fixture must contain exactly one source-proven self call");
        };
        let Instr::CallNamed { sym, nargs } = body.code[call_pc as usize] else {
            panic!("retained source site must be a named call");
        };
        assert_eq!(sym, symbol);
        assert_eq!(nargs, body.arity);
        let parent_pc = call_pc + 1;
        let pending_depth = depths[call_pc as usize].unwrap() - u32::from(nargs);
        assert_eq!(depths[parent_pc as usize], Some(pending_depth + 1));
        assert!(
            pending_depth > 0,
            "fixture retains a live left operand across recursion"
        );
        let child = frame_words(symbol, body, 0, depths[0].unwrap());
        assert_eq!(
            validate(&child, 1, code).unwrap(),
            0,
            "single-frame control"
        );
        let parent = frame_words(symbol, body, parent_pc, pending_depth);
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        for count in [2usize, 3] {
            let mut words = parent.repeat(count - 1);
            words.extend_from_slice(&child);
            let original = words.clone();
            let result = validate(&words, count as u64, code);
            assert_eq!(words, original, "validation must not rewrite payloads");
            assert_eq!(
                (stack.fp(), stack.sp()),
                before,
                "validation must not materialize frames"
            );
            assert_eq!(
                result.unwrap_or_else(|error| panic!(
                    "valid {count}-frame retained self chain rejected: {error:?}"
                )),
                parent_pc
            );
            assert_eq!(
                validate_native(&words, count as u64, code).unwrap(),
                parent_pc,
                "ordinary recovery accepts the same validated retained self chain"
            );
        }
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn retained_self_guard_chain_resumes_with_outer_state() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for mode in ["normal", "error", "redefined"] {
            exercise_retained_chain_handoff(mode);
        }
    }

    fn exercise_retained_chain_handoff(mode: &str) {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, owner) = fixture(&mut env);
        egcl_rt::rooted!(callable = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let code = mapped(&owner);
        let body = &code.body;
        let depths = verified_operand_stack_depths(body).unwrap();
        let call_pc = body.self_call_sites[0];
        let pending = depths[call_pc as usize].unwrap() - u32::from(body.arity);
        let parent = frame_words(symbol, body, call_pc + 1, pending);
        let mut words = parent.repeat(2);
        let child_header = words.len();
        words.extend(frame_words(symbol, body, 0, 0));
        let VarLoc::Slot(argument_slot) = body.param_layout[0].1 else {
            panic!("scope-free fixture parameter must occupy a frame slot");
        };
        let child_arg = child_header + 4 + usize::from(argument_slot);
        if mode == "error" {
            words[child_arg] = NIL.to_raw();
        }
        if mode == "redefined" {
            words[child_arg] = EgclVal::from_fixnum(0).to_raw();
            crate::cli::read_eval_all_env(
                "(defun retained-guard-chain (n) (declare (ignore n)) 100)",
                &mut env,
            )
            .unwrap();
        }
        assert!(validate(&words, 3, code).is_ok());
        let mut activation = ActivationState::prepare(code, &mut env).unwrap();
        egcl_rt::rooted_ref!(_activation = &mut activation);
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        let environment_depths = (
            env.handlers.len(),
            env.restarts.len(),
            env.catch_stack.len(),
        );
        let _retire = RetireNativeDeoptFrame {
            fp: before.0,
            sp: before.1,
        };
        let frame = stack
            .push_frame(
                EgclVal::from_symbol_index(symbol),
                std::ptr::null(),
                body.num_slots(),
                FLAG_CALL,
            )
            .unwrap();
        let mut context =
            activation.context(code, frame, std::ptr::null_mut(), std::ptr::null_mut());
        let (mut scopes, metadata) = materialize_t2_deopt_scopes(
            3,
            words.len() as u64,
            words.as_ptr(),
            Some(frame),
            code.deopt_metadata.clone(),
        )
        .expect("materialize validated retained self frames");
        assert!(
            unsafe { retain_self_scope_callables(code, &scopes) }.is_err(),
            "a public symbol cannot supply retained callable identity"
        );
        assert!(
            scopes
                .iter()
                .all(|scope| unsafe { (*scope.frame).function.is_symbol() })
        );
        unsafe {
            (*frame).function = *callable;
        }
        let inner_pc = scopes[1].bcp;
        scopes[1].bcp = u32::MAX;
        assert!(unsafe { take_guard_resume_state(&mut context, code, &scopes, &env) }.is_err());
        assert!(
            unsafe { (*context.guard_resume).is_some() },
            "invalid inner PC must not consume the outer reservation"
        );
        scopes[1].bcp = inner_pc;
        scopes[1].body = Arc::new(body.as_ref().clone());
        assert!(unsafe { retain_self_scope_callables(code, &scopes) }.is_err());
        assert!(
            scopes[1..]
                .iter()
                .all(|scope| unsafe { (*scope.frame).function.is_symbol() }),
            "invalid identity proof must not change inner frame headers"
        );
        assert!(unsafe { take_guard_resume_state(&mut context, code, &scopes, &env) }.is_err());
        assert!(
            unsafe { (*context.guard_resume).is_some() },
            "equal but nonidentical retained body must not consume the reservation"
        );
        scopes[1].body = Arc::clone(body);
        unsafe { retain_self_scope_callables(code, &scopes) }.unwrap();
        assert!(
            scopes
                .iter()
                .all(|scope| unsafe { (*scope.frame).function == *callable })
        );
        let state = unsafe { take_guard_resume_state(&mut context, code, &scopes, &env) }
            .expect("transfer outer state without rejecting retained inner frames");
        assert!(
            unsafe { (*context.guard_resume).is_none() },
            "outer reservation transfers exactly once"
        );
        assert!(
            unsafe { take_guard_resume_state(&mut context, code, &scopes, &env) }.is_err(),
            "a second handoff must not duplicate outer state"
        );
        egcl_rt::rooted!(
            result = resume_inlined_in_t0_with_state(scopes, metadata, None, Some(state), &mut env)
        );
        if mode == "error" {
            assert!(
                matches!(&*result, Err(EgclError::Signalled(details))
                if condition_matches_handler(&env, details.condition, "TYPE-ERROR")),
                "{:?}",
                &*result
            );
        } else {
            assert_eq!(
                *result.as_ref().unwrap(),
                EgclVal::from_fixnum(if mode == "redefined" { 2 } else { 3 })
            );
        }
        assert_eq!(
            (stack.fp(), stack.sp()),
            before,
            "resumption must retire all materialized frames"
        );
        assert_eq!(
            (
                env.handlers.len(),
                env.restarts.len(),
                env.catch_stack.len()
            ),
            environment_depths
        );
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn retained_guard_rejects_wrong_reachable_operand_depth() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, owner) = fixture(&mut env);
        let code = mapped(&owner);
        let depths = verified_operand_stack_depths(&code.body).unwrap();
        assert_eq!(depths[0], Some(0));
        let valid = frame_words(symbol, &code.body, 0, 0);
        assert_eq!(validate(&valid, 1, code).unwrap(), 0);
        let wrong = frame_words(symbol, &code.body, 0, 1);
        assert!(
            code.body.max_stack > 1,
            "wrong depth must still fit declared capacity"
        );
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        let result = validate(&wrong, 1, code);
        assert_eq!((stack.fp(), stack.sp()), before);
        assert!(
            result.is_err(),
            "capacity alone does not establish the bytecode continuation depth"
        );
        assert!(validate_native(&wrong, 1, code).is_err());
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn retained_guard_chain_rejects_malformed_headers_and_capacity() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let (symbol, owner) = fixture(&mut env);
        let code = mapped(&owner);
        let body = &code.body;
        let depths = verified_operand_stack_depths(body).unwrap();
        let call_pc = body.self_call_sites[0];
        let pending = depths[call_pc as usize].unwrap() - u32::from(body.arity);
        let mut words = frame_words(symbol, body, call_pc + 1, pending);
        let child_header = words.len();
        words.extend(frame_words(symbol, body, 0, 0));
        assert!(validate(&words, 2, code).is_ok());
        let stack = egcl_rt::current_stack();
        let before = (stack.fp(), stack.sp());
        for (offset, value) in [
            (0, u64::MAX),
            (1, 0),
            (2, u64::MAX),
            (3, u64::MAX),
            (child_header, u64::from(u32::MAX)),
            (child_header + 1, u64::MAX),
            (child_header + 2, u64::MAX),
        ] {
            let mut invalid = words.clone();
            invalid[offset] = value;
            assert!(validate(&invalid, 2, code).is_err(), "header word {offset}");
        }
        for count in [0, 1, 3, u64::MAX] {
            assert!(
                validate(&words, count, code).is_err(),
                "scope count {count}"
            );
        }
        for end in 0..words.len() {
            assert!(
                validate(&words[..end], 2, code).is_err(),
                "truncated at {end}"
            );
        }
        let mut trailing = words.clone();
        trailing.push(NIL.to_raw());
        assert!(validate(&trailing, 2, code).is_err());
        let metadata = code.deopt_metadata.as_ref().unwrap();
        let single = frame_words(symbol, body, 0, 0);
        assert!(
            validate_guard_scope_chain(&single, 1, code, body.num_slots() - 1, metadata).is_err(),
            "saved values fitting is insufficient for future interpreter execution"
        );
        assert_eq!((stack.fp(), stack.sp()), before);
    }
}

#[cfg(test)]
mod payload_root_tests {
    use super::*;
    use egcl_rt::gc::TraceHostRoots;

    #[test]
    fn nested_guard_roots_exclude_every_virtual_frame_header() {
        let mut words = [11, 3, 1, 1, 80, 88, 11, 5, 1, 0, 96];
        let base = words.as_mut_ptr().cast::<EgclVal>();
        let mut roots = NativeDeoptRoots {
            payload: base,
            payload_ranges: deopt_payload_ranges(&words, 2).unwrap(),
            arguments: std::ptr::null_mut(),
            argument_count: 0,
        };
        let mut visited = Vec::new();
        roots.trace_host_roots(&mut |slot| {
            visited.push(unsafe { slot.offset_from(base) });
            unsafe {
                *slot = EgclVal::from_fixnum(99);
            }
        });
        assert_eq!(
            visited,
            vec![4, 5, 10],
            "only locals and operand-stack slots are roots"
        );
        assert_eq!(&words[..4], &[11, 3, 1, 1]);
        assert_eq!(&words[6..10], &[11, 5, 1, 0]);
    }

    #[test]
    fn guard_payload_ranges_require_exact_bounded_streams() {
        assert_eq!(deopt_payload_ranges(&[11, 3, 0, 0], 1).unwrap(), vec![4..4]);
        assert_eq!(
            deopt_payload_ranges(&[11, 3, 1, 1, 80, 88], 1).unwrap(),
            vec![4..6]
        );
        assert_eq!(
            deopt_payload_ranges(&[11, 3, 0, 0, 11, 4, 0, 1, 80], 2).unwrap(),
            vec![4..4, 8..9]
        );
        for (words, count) in [
            (vec![], 0),
            (vec![11, 3, 0, 0], 0),
            (vec![11, 3, 0], 1),
            (vec![11, 3, 1, 0], 1),
            (vec![11, 3, 0, 0, 80], 1),
            (vec![11, 3, 0, 0], 2),
            (vec![11, 3, 0, 0, 11, 4, 0], 2),
            (vec![11, 3, 0, 0, 11, 4, 1, 0], 2),
            (vec![11, 3, u64::MAX, 1], 1),
            (vec![11, 3, 0, u64::MAX], 1),
            (vec![11, 3, 0, 0], u64::MAX),
        ] {
            assert!(
                deopt_payload_ranges(&words, count).is_err(),
                "{words:?}, scopes={count}"
            );
        }
    }

    #[test]
    fn nested_guard_payload_and_argument_roots_follow_moving_gc() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (mut words, mut arguments) = {
            egcl_rt::rooted!(first = crate::cli::arena_cons(EgclVal::from_fixnum(41), NIL));
            egcl_rt::rooted!(second = crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL));
            egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(43), NIL));
            (
                [11, 3, 1, 0, first.to_raw(), 11, 5, 0, 1, second.to_raw()],
                [*argument],
            )
        };
        // No Lisp allocation occurs between dropping construction roots and
        // registering the native stream/argument descriptor below.
        let before = [words[4], words[9], arguments[0].to_raw()];
        let mut roots = NativeDeoptRoots {
            payload: words.as_mut_ptr().cast(),
            payload_ranges: deopt_payload_ranges(&words, 2).unwrap(),
            arguments: arguments.as_mut_ptr(),
            argument_count: arguments.len(),
        };
        egcl_rt::rooted_ref!(_roots = &mut roots);
        egcl_rt::gc::full_gc().unwrap();
        for (index, (value, expected)) in [
            (EgclVal(words[4]), 41),
            (EgclVal(words[9]), 42),
            (arguments[0], 43),
        ]
        .into_iter()
        .enumerate()
        {
            assert_ne!(
                value.to_raw(),
                before[index],
                "prove relocation for root {index}"
            );
            assert_eq!(crate::cli::cp(value).0, EgclVal::from_fixnum(expected));
        }
        assert_eq!(&words[..4], &[11, 3, 1, 0]);
        assert_eq!(&words[5..9], &[11, 5, 0, 1]);
    }
}

/// A native child acquires a managed frame only on this cold continuation.
/// T0 normally pops it; errors and intercepted Rust panics also retire it.
/// Unlike RestoreNativeFrame, never recreate a child which has no native owner.
struct RetireNativeDeoptFrame {
    fp: *const Frame,
    sp: *const u8,
}

impl Drop for RetireNativeDeoptFrame {
    fn drop(&mut self) {
        let stack = egcl_rt::current_stack();
        while stack.fp() != self.fp {
            assert!(
                !stack.fp().is_null(),
                "native child deopt crossed its anchor"
            );
            stack.pop_frame();
        }
        assert_eq!(
            stack.sp(),
            self.sp,
            "native child deopt changed its anchor extent"
        );
    }
}

pub(super) unsafe extern "C" fn resume_native_guard(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut SysvTransferCapture,
) {
    unsafe {
        root_publication::published(image, || resume_native_guard_unpublished(request, out));
    }
}

unsafe fn resume_native_guard_unpublished(request: *mut u8, out: *mut NativeOutcome) {
    use egcl_compiler::t2::emit::NativeFrameDeoptRequest;
    use egcl_compiler::t2::x64_calls::NativeCallValues;
    const _: () = assert!(std::mem::size_of::<NativeFrameDeoptRequest>() == 32);

    let context = CAPTURE.with(Cell::get);
    let mut managed_outcome_written = false;
    let result = guard_c2i(|| {
        let boundary = root_publication::ACTIVE.with(Cell::get);
        if context.is_null() || boundary.is_null() {
            return Err(invalid_capture());
        }
        // Copy geometry, never retain a mutable GC-scanned context borrow.
        let (owner, cursor, bounds) = unsafe {
            (
                (*boundary).owner,
                (*boundary).cursor,
                (*boundary).bounds.clone(),
            )
        };
        if owner.is_null()
            || owner != unsafe { (*context).owner }
            || unsafe { !(*context).recursive.is_null() }
        {
            return Err(invalid_capture());
        }
        let code = unsafe { &*owner };
        let base = code.code.as_ptr() as usize;
        let offset = cursor.pc.checked_sub(base).ok_or_else(invalid_capture)?;
        if !code.sites.is_deopt_return(base, cursor.pc) {
            return Err(invalid_capture());
        }
        let (native_slots, f64_slots) = match code._native_calls.value_map(offset) {
            Some(NativeCallValues::Deoptimizing {
                native_slots,
                f64_slots,
            }) => (*native_slots, f64_slots.as_ref()),
            _ => return Err(invalid_capture()),
        };
        let read_word = |address: usize| -> Option<usize> {
            if address % 8 != 0 || address < bounds.start || address.checked_add(8)? > bounds.end {
                return None;
            }
            Some(unsafe { (address as *const usize).read() })
        };
        let body_sp = code
            ._native_calls
            .unwind(base, &cursor, bounds.clone(), read_word)
            .and_then(|step| step.body_sp)
            .ok_or_else(invalid_capture)?;
        let native_word = |slot: u32| -> Option<usize> {
            if slot >= native_slots {
                return None;
            }
            body_sp.checked_add(usize::try_from(slot).ok()?.checked_mul(8)?)
        };
        let kind_address = native_word(code.native_entry_kind_slot.ok_or_else(invalid_capture)?)
            .ok_or_else(invalid_capture)?;
        let control_address = native_word(code.native_control_slot.ok_or_else(invalid_capture)?)
            .ok_or_else(invalid_capture)?;
        let control = unsafe { (*context).native_recursive.as_ref() }
            .map(|control| control as *const _ as usize)
            .ok_or_else(invalid_capture)?;
        if kind_address == control_address || read_word(control_address) != Some(control) {
            return Err(invalid_capture());
        }
        let in_temporary = |start: usize, bytes: usize| {
            start % 8 == 0
                && start >= cursor.call_sp
                && start.checked_add(bytes).is_some_and(|end| end <= body_sp)
        };
        let request_address = request as usize;
        if !in_temporary(
            request_address,
            std::mem::size_of::<NativeFrameDeoptRequest>(),
        ) {
            return Err(invalid_capture());
        }
        // No request field is read until its whole object is proven owned.
        let request = unsafe { request.cast::<NativeFrameDeoptRequest>().read() };
        if request.entry_kind > 1
            || read_word(kind_address) != usize::try_from(request.entry_kind).ok()
            || request.n_scopes != 1
            || request.n_words < 4
            || request.n_words > 4 + u64::from(code.body.n_locals) + u64::from(code.body.max_stack)
        {
            return Err(invalid_capture());
        }
        let words_len = usize::try_from(request.n_words).map_err(|_| invalid_capture())?;
        let words_bytes = words_len.checked_mul(8).ok_or_else(invalid_capture)?;
        let words_address = request.words as usize;
        if !in_temporary(words_address, words_bytes)
            || words_address
                .checked_add(words_bytes)
                .is_none_or(|end| end > request_address)
        {
            return Err(invalid_capture());
        }
        let count = usize::from(code.native_argument_count);
        let argument_slot = code.native_argument_base.ok_or_else(invalid_capture)?;
        if count != usize::from(code.body.arity)
            || argument_slot
                .checked_add(u32::from(code.native_argument_count))
                .is_none_or(|end| end > native_slots)
            || !fixed_tagged_parameters(&code.body)
            || !scope_free_native_body(&code.body, true)
        {
            return Err(invalid_capture());
        }
        let arguments_address = body_sp
            .checked_add(argument_slot as usize * 8)
            .ok_or_else(invalid_capture)?;
        for index in 0..count {
            let address = arguments_address
                .checked_add(index * 8)
                .ok_or_else(invalid_capture)?;
            if address == kind_address || address == control_address || read_word(address).is_none()
            {
                return Err(invalid_capture());
            }
        }
        let frame = unsafe { (*context).frame };
        let stack = egcl_rt::current_stack();
        if frame.is_null()
            || stack.fp() != frame
            || !std::ptr::eq(unsafe { (*boundary).stack }, stack)
            || unsafe {
                !std::ptr::eq((*frame).code_info, code.code_info) || !(*frame).return_pc.is_null()
            }
        {
            return Err(invalid_capture());
        }
        let metadata = code.deopt_metadata.clone().ok_or_else(invalid_capture)?;
        let adapted = TransferDeoptRequest {
            n_scopes: request.n_scopes,
            n_words: request.n_words,
            words: request.words,
            activation: unsafe { frame.add(1).cast() },
        };
        let bcp = unsafe { validate_guard_stream(&adapted, code, &*frame, &metadata) }?;
        let active = code
            .control_scopes
            .before(bcp)
            .ok_or_else(invalid_capture)?;
        if active.iter().any(|scope| {
            scope.ownership != Ownership::Local
                || !matches!(
                    scope.kind,
                    ScopeKind::Block {
                        register: false,
                        ..
                    }
                )
        }) {
            return Err(invalid_capture());
        }
        let mut roots = NativeDeoptRoots {
            payload: request.words.cast_mut().cast(),
            // The reboxer roots tagged words while this owner roots arguments.
            // Do not scan raw F64 words with the completed-payload descriptor.
            payload_ranges: Vec::new(),
            arguments: arguments_address as *mut EgclVal,
            argument_count: count,
        };
        egcl_rt::rooted_ref!(_native_payload = &mut roots);
        // SAFETY: the request is proven inside this live native frame, and its
        // exact return-PC metadata describes the emitted word representations.
        unsafe {
            egcl_compiler::t2::deopt::rebox_f64_payload(
                std::slice::from_raw_parts_mut(request.words.add(4).cast_mut(), words_len - 4),
                f64_slots,
                egcl_rt::gc::alloc_double_float,
            )
        }
        .map_err(|_| invalid_capture())?;
        roots.payload_ranges = deopt_payload_ranges(
            unsafe { std::slice::from_raw_parts(request.words, words_len) },
            request.n_scopes,
        )?;
        // Preserve the existing all-or-unavailable debugger policy for mutated
        // parameter bindings; immutable native copies are still rooted above.
        let recipe = native_debug::arguments(&code.body, false);
        egcl_rt::rooted!(
            arguments = if recipe
                .as_ref()
                .is_some_and(|recipe| recipe.parameters.iter().all(|(_, slot)| slot.is_some()))
            {
                Some(unsafe { std::slice::from_raw_parts(roots.arguments, count) }.to_vec())
            } else {
                None
            }
        );
        if request.entry_kind == 0 {
            unsafe {
                resume_guard_with_arguments(
                    (&adapted as *const TransferDeoptRequest).cast_mut().cast(),
                    out,
                    Some(arguments.take()),
                );
            }
            managed_outcome_written = true;
            return Ok(NIL);
        }
        unsafe {
            resume_native_child_guard(
                code,
                &adapted,
                metadata,
                (*frame).function,
                arguments.take(),
                bcp,
            )
        }
    });
    if managed_outcome_written && result.is_ok() {
        return;
    }
    let outcome = match result {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            stash_native_error(error);
            if !context.is_null() {
                unsafe {
                    (*context).completed_deopt = true;
                }
            }
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe {
        out.write(outcome);
    }
}

// Both native self-recursion and ordinary foreign owners use the same cold
// materialization/retirement path. Only the failing activation becomes T0.
unsafe fn resume_native_child_guard(
    code: &TransferCode,
    request: &TransferDeoptRequest,
    metadata: Arc<T2InstalledMetadata>,
    callable: EgclVal,
    arguments: Option<Vec<EgclVal>>,
    bcp: u32,
) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(
        _active_code_owner =
            super::super::ActiveCodeOwnerRoot(code.native_callable_slot.and(code.installed_symbol))
    );
    let env = NATIVE_ENV.with(Cell::get);
    if env.is_null() {
        return Err(invalid_capture());
    }
    egcl_rt::rooted!(callable = callable);
    egcl_rt::rooted!(arguments = arguments);
    let stack = egcl_rt::current_stack();
    let active = code
        .control_scopes
        .before(bcp)
        .ok_or_else(invalid_capture)?;
    if !egcl_rt::function::is_interpreted_function(*callable) {
        return Err(invalid_capture());
    }
    if code.native_callable_slot.is_some() && !code.accepts_explicit_callable(*callable) {
        return Err(invalid_capture());
    }
    let captured_env = if code.native_callable_slot.is_some() && code.body.has_env {
        Some(closure_captured_env(*callable).ok_or_else(invalid_capture)?)
    } else {
        None
    };
    let mut state = OuterResumeState {
        debug_call: None,
        handlers: Vec::new(),
        cleanup_conts: Vec::new(),
        dyn_binds: Vec::new(),
    };
    state
        .handlers
        .try_reserve(active.len())
        .map_err(|_| EgclError::Oom)?;
    for scope in active {
        // A native child resumes with no EnvFrame of its own. Its local
        // BLOCK and TAGBODY records carry no token: nothing outside this
        // activation can name them (see `ordinary_native_body`).
        let handler = match scope.kind {
            ScopeKind::Block {
                id,
                resume_bcp,
                register: false,
            } => Handler::Block {
                block_id: id,
                token: String::new(),
                resume_bcp,
                sp_restore: scope.sp_restore,
                env: captured_env.clone(),
            },
            ScopeKind::Tagbody { id } if scope.ownership == Ownership::Local => Handler::Tag {
                tagbody_id: id,
                sp_restore: scope.sp_restore,
                token: None,
                tag_bcps: Vec::new(),
                env: captured_env.clone(),
            },
            _ => return Err(invalid_capture()),
        };
        state.handlers.push(handler);
    }
    egcl_rt::rooted!(state = Some(state));
    let _capture_pause = CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
    let _retire = RetireNativeDeoptFrame {
        fp: stack.fp(),
        sp: stack.sp(),
    };
    let child = stack
        .push_frame(
            *callable,
            std::ptr::null(),
            code.body.num_slots(),
            FLAG_CALL,
        )
        .ok_or_else(|| {
            EgclError::StackOverflow(
                egcl_rt::current_fiber_id()
                    .unwrap_or_else(|| egcl_rt::FiberId(egcl_rt::current_thread_id().0)),
            )
        })?;
    let (scopes, metadata) = materialize_t2_deopt_scopes(
        request.n_scopes,
        request.n_words,
        request.words,
        Some(child),
        Some(metadata),
    )
    .map_err(|message| EgclError::Internal(message.into()))?;
    unsafe {
        retain_self_scope_callables(code, &scopes)?;
    }
    note_segment_deopt(code, bcp);
    back_off_interpreter_osr(*callable);
    if let Some(arguments) = arguments.as_ref() {
        state
            .as_mut()
            .expect("native child resume state")
            .debug_call = Some(unsafe {
            egcl_rt::debug_stack::CallFrame::enter_managed_at(&code.body.name, arguments, child)
        });
    }
    resume_inlined_in_t0_with_state(scopes, metadata, captured_env, state.take(), unsafe {
        &mut *env
    })
}

/// Complete one guarded ordinary activation while its native callers remain
/// published. The wrapper owns the ordinary tuple/error landing protocol.
#[cfg(test)]
type OrdinaryGuardObserver = Cell<Option<fn(u64)>>;

#[cfg(test)]
static ORDINARY_GUARD_OBSERVER: egcl_rt::execution_local::ExecutionLocal<OrdinaryGuardObserver> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

#[cfg(test)]
pub(in crate::cli::bytecode) fn with_ordinary_guard_resume_observer_for_test<R>(
    observe: fn(u64),
    body: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<fn(u64)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ORDINARY_GUARD_OBSERVER.with(|slot| slot.set(self.0));
        }
    }
    let _restore = Restore(ORDINARY_GUARD_OBSERVER.with(|slot| slot.replace(Some(observe))));
    body()
}

// The observer sees a validated, rooted, fully tagged stream. Its pointer is
// valid only for the duration of the callback; collecting callbacks must read
// it after collection rather than retain a borrowed slice across collection.
#[cfg(test)]
type OrdinaryGuardPayloadObserver = Cell<Option<fn(*const u64, usize, bool)>>;

#[cfg(test)]
static ORDINARY_GUARD_PAYLOAD_OBSERVER: egcl_rt::execution_local::ExecutionLocal<
    OrdinaryGuardPayloadObserver,
> = unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(None)) };

#[cfg(test)]
pub(in crate::cli::bytecode) fn with_ordinary_guard_payload_observer_for_test<R>(
    observe: fn(*const u64, usize, bool),
    body: impl FnOnce() -> R,
) -> R {
    struct Restore(Option<fn(*const u64, usize, bool)>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ORDINARY_GUARD_PAYLOAD_OBSERVER.with(|slot| slot.set(self.0));
        }
    }
    let _restore =
        Restore(ORDINARY_GUARD_PAYLOAD_OBSERVER.with(|slot| slot.replace(Some(observe))));
    body()
}

pub(super) unsafe fn resume_ordinary_guard(request: *mut u8) -> Result<EgclVal, EgclError> {
    use egcl_compiler::t2::emit::OrdinaryGuardRequest;
    use egcl_compiler::t2::x64_calls::{NativeCallOrigin, NativeCallValues};
    let context = CAPTURE.with(Cell::get);
    let boundary = root_publication::ACTIVE.with(Cell::get);
    if context.is_null() || boundary.is_null() {
        return Err(invalid_capture());
    }
    let boundary = unsafe { &*boundary };
    if boundary.owner.is_null() || boundary.context != context {
        return Err(invalid_capture());
    }
    let code = unsafe { &*boundary.owner };
    egcl_rt::rooted!(
        _active_code_owner =
            super::super::ActiveCodeOwnerRoot(code.native_callable_slot.and(code.installed_symbol))
    );
    if code.ordinary_entry.is_none() || !code.has_deopt || !ordinary_native_body(&code.body) {
        return Err(invalid_capture());
    }
    let base = code.code.as_ptr() as usize;
    let offset = boundary
        .cursor
        .pc
        .checked_sub(base)
        .ok_or_else(invalid_capture)?;
    if !code
        ._native_calls
        .get(offset)
        .is_some_and(|site| matches!(site.origin, NativeCallOrigin::Deopt(_)))
    {
        return Err(invalid_capture());
    }
    let (native_slots, f64_slots) = match code._native_calls.value_map(offset) {
        Some(NativeCallValues::Deoptimizing {
            native_slots,
            f64_slots,
        }) => (*native_slots, f64_slots),
        _ => return Err(invalid_capture()),
    };
    let read_word = |address: usize| {
        (address % 8 == 0
            && address >= boundary.bounds.start
            && address
                .checked_add(8)
                .is_some_and(|end| end <= boundary.bounds.end))
        .then(|| unsafe { (address as *const usize).read() })
    };
    let step = code
        ._native_calls
        .unwind(base, &boundary.cursor, boundary.bounds.clone(), read_word)
        .ok_or_else(invalid_capture)?;
    let body_sp = step.body_sp.ok_or_else(invalid_capture)?;
    let temporary = |address: usize, bytes: usize| {
        address % 8 == 0
            && address >= boundary.cursor.call_sp
            && address.checked_add(bytes).is_some_and(|end| end <= body_sp)
    };
    if !temporary(
        request as usize,
        std::mem::size_of::<OrdinaryGuardRequest>(),
    ) {
        return Err(invalid_capture());
    }
    let request = unsafe { &*request.cast::<OrdinaryGuardRequest>() };
    let words = usize::try_from(request.n_words)
        .ok()
        .and_then(|words| words.checked_mul(8))
        .ok_or_else(invalid_capture)?;
    if !temporary(request.words as usize, words)
        || (request.words as usize)
            .checked_add(words)
            .is_none_or(|end| end > request as *const _ as usize)
    {
        return Err(invalid_capture());
    }
    let native_word = |slot: u32| {
        (slot < native_slots)
            .then_some(slot)
            .and_then(|slot| body_sp.checked_add(slot as usize * 8))
    };
    let control = unsafe { (*context).native_recursive.as_ref() }
        .map(|control| control as *const _ as usize)
        .ok_or_else(invalid_capture)?;
    let control_home = native_word(code.native_control_slot.ok_or_else(invalid_capture)?)
        .ok_or_else(invalid_capture)?;
    if request.control as usize != control || read_word(control_home) != Some(control) {
        return Err(invalid_capture());
    }
    let count = usize::from(code.native_argument_count);
    let argument_slot = code.native_argument_base.ok_or_else(invalid_capture)?;
    if count != code.ordinary_parameter_count()
        || argument_slot
            .checked_add(count as u32)
            .is_none_or(|end| end > native_slots)
    {
        return Err(invalid_capture());
    }
    // An empty argument range may begin exactly at the end of the native
    // frame. It still supplies a nonnull aligned zero-length slice pointer.
    let arguments_address = body_sp
        .checked_add(argument_slot as usize * 8)
        .ok_or_else(invalid_capture)?;
    for index in 0..count {
        let address = arguments_address
            .checked_add(index * 8)
            .ok_or_else(invalid_capture)?;
        if address == control_home || read_word(address).is_none() {
            return Err(invalid_capture());
        }
    }
    let arguments = arguments_address as *mut EgclVal;
    let metadata = code.deopt_metadata.clone().ok_or_else(invalid_capture)?;
    let mut adapted = TransferDeoptRequest {
        n_scopes: request.n_scopes,
        n_words: request.n_words,
        words: request.words,
        activation: std::ptr::null_mut(),
    };
    let bcp =
        unsafe { validate_guard_stream_slots(&adapted, code, code.body.num_slots(), &metadata) }?;
    // Raw doubles are reboxed in place below; their indexes describe one
    // logical frame's payload (bliss-k4psf).
    if !f64_slots.is_empty() && request.n_scopes != 1 {
        return Err(invalid_capture());
    }
    let payload_ranges = || {
        deopt_payload_ranges(
            unsafe { std::slice::from_raw_parts(request.words, request.n_words as usize) },
            request.n_scopes,
        )
    };
    let mut roots = NativeDeoptRoots {
        payload: request.words.cast_mut().cast(),
        // Never scan raw F64 words as tagged: the reboxer roots the tagged
        // words while it allocates, and this owner the arguments.
        payload_ranges: if f64_slots.is_empty() {
            payload_ranges()?
        } else {
            Vec::new()
        },
        arguments,
        argument_count: count,
    };
    egcl_rt::rooted_ref!(_roots = &mut roots);
    if !f64_slots.is_empty() {
        // SAFETY: the request is proven inside this live native frame, and its
        // exact return-PC metadata describes the emitted word representations.
        unsafe {
            egcl_compiler::t2::deopt::rebox_f64_payload(
                std::slice::from_raw_parts_mut(
                    request.words.add(4).cast_mut(),
                    request.n_words as usize - 4,
                ),
                f64_slots,
                egcl_rt::gc::alloc_double_float,
            )
        }
        .map_err(|_| invalid_capture())?;
        roots.payload_ranges = payload_ranges()?;
    }
    #[cfg(test)]
    if let Some(observe) = ORDINARY_GUARD_PAYLOAD_OBSERVER.with(Cell::get) {
        observe(
            request.words,
            request.n_words as usize,
            !f64_slots.is_empty(),
        );
    }
    // A `&key` body's native arguments are its bound slots, not the call's
    // actuals; like its managed frame, it records no original arguments.
    egcl_rt::rooted!(
        original_arguments = code
            .ordinary_bound_entry
            .is_none()
            .then(|| unsafe { std::slice::from_raw_parts(arguments, count) }.to_vec())
    );
    let segment = boundary.segment;
    if segment.is_null() {
        return Err(invalid_capture());
    }
    // Equal code owners can occur in different physical activations. Only the
    // actual root frame is allowed to reuse the managed anchor's callable/state.
    let is_root = boundary.owner == unsafe { (*context).owner }
        && (step.caller.call_sp, step.caller.pc)
            == unsafe { ((*segment).saved_sp, (*segment).return_pc()) };
    if is_root {
        let frame = unsafe { (*context).frame };
        if frame.is_null() || egcl_rt::current_stack().fp() != frame {
            return Err(invalid_capture());
        }
        adapted.activation = unsafe { frame.add(1).cast() };
        let mut out = NativeOutcome {
            value: NIL,
            exit: NativeExit::Returned,
        };
        #[cfg(test)]
        if let Some(observe) = ORDINARY_GUARD_OBSERVER.with(Cell::get) {
            observe(request.n_scopes);
        }
        unsafe {
            resume_guard_with_arguments(
                (&adapted as *const TransferDeoptRequest).cast_mut().cast(),
                &mut out,
                Some(original_arguments.take()),
            );
        }
        return Ok(out.value); // the ordinary wrapper consumes any stashed error
    }
    egcl_rt::rooted!(callable = NIL);
    if code.native_callable_slot.is_some() {
        *callable = root_publication::frame_callable(boundary).ok_or_else(invalid_capture)?;
    } else {
        let retained = boundary
            .directory
            .callable_for_pc(boundary.cursor.pc)
            .ok_or_else(invalid_capture)?;
        retained.with_gc_stable_mutator(|value| *callable = *value);
    }
    #[cfg(test)]
    if let Some(observe) = ORDINARY_GUARD_OBSERVER.with(Cell::get) {
        observe(request.n_scopes);
    }
    unsafe {
        resume_native_child_guard(
            code,
            &adapted,
            metadata,
            *callable,
            original_arguments.take(),
            bcp,
        )
    }
}

pub(super) unsafe extern "C" fn resume_guard(
    request: *mut u8,
    out: *mut NativeOutcome,
    image: *mut egcl_compiler::t2::native_transfer::SysvTransferCapture,
) {
    unsafe { super::root_publication::published(image, || resume_guard_unpublished(request, out)) }
}

unsafe fn resume_guard_unpublished(request: *mut u8, out: *mut NativeOutcome) {
    unsafe {
        resume_guard_with_arguments(request, out, None);
    }
}

// The outer option distinguishes legacy managed recipes from an explicit
// native-frame snapshot, whose inner None means arguments are unavailable.
unsafe fn resume_guard_with_arguments(
    request: *mut u8,
    out: *mut NativeOutcome,
    mut native_arguments: Option<Option<Vec<EgclVal>>>,
) {
    // Native-frame entry already reboxed and rooted this exact payload. Avoid
    // reboxing twice or registering the same physical words twice as roots.
    let payload_already_boxed = native_arguments.is_some();
    egcl_rt::rooted_ref!(_native_arguments = &mut native_arguments);
    let request = unsafe { &*request.cast::<TransferDeoptRequest>() };
    let context = CAPTURE.with(Cell::get);
    let owner = unsafe { (*context).owner };
    let code = unsafe { &*owner };
    // Live condition clusters are younger Special frames. The retained
    // callable, not stack.fp(), owns the guard's activation slots.
    let frame = unsafe {
        if (*context).recursive.is_null() {
            (*context).frame
        } else {
            (*(*context).recursive).frame
        }
    };
    let result = guard_c2i(|| {
        if frame.is_null()
            || (unsafe { !(*context).recursive.is_null() }
                && egcl_rt::current_stack().fp() != frame)
            || unsafe { frame.add(1).cast::<EgclVal>() } != request.activation
        {
            return Err(invalid_capture());
        }
        let header = unsafe { std::ptr::read(frame) };
        // Accept only this retained code's native metadata. A child header
        // also retains the exact heap callable while T0 owns/pops its frame.
        if !std::ptr::eq(header.code_info, code.code_info) || !header.return_pc.is_null() {
            return Err(invalid_capture());
        }
        let metadata = code.deopt_metadata.clone().ok_or_else(invalid_capture)?;
        let bcp = unsafe { validate_guard_stream(request, code, &header, &metadata) }?;
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        if unsafe { (*context).recursive.is_null() } {
            let active = code
                .control_scopes
                .before(bcp)
                .expect("validated guard bcp");
            if !unsafe {
                validate_condition_clusters(&*context, active, &*env)
                    && validate_special_bindings(&*context, active)
            } {
                return Err(invalid_capture());
            }
        }
        // Native recipes describe proven original arguments before the deopt
        // stream replaces dead locals. Keep a traced copy until a managed
        // debug-call record owns it; unavailable native arguments stay unknown.
        egcl_rt::rooted!(
            original_arguments = match native_arguments.take() {
                Some(arguments) => arguments,
                None => unsafe { (&*header.code_info).original_arguments((*frame).locals()) },
            }
        );
        let mut payload_roots = if payload_already_boxed {
            None
        } else {
            let boundary = root_publication::ACTIVE.with(Cell::get);
            if boundary.is_null() || unsafe { (*boundary).owner != owner } {
                return Err(invalid_capture());
            }
            let base = code.code.as_ptr() as usize;
            let pc = unsafe { (*boundary).cursor.pc };
            let offset = pc.checked_sub(base).ok_or_else(invalid_capture)?;
            if !code.sites.is_deopt_return(base, pc) {
                return Err(invalid_capture());
            }
            let Some(egcl_compiler::t2::x64_calls::NativeCallValues::Deoptimizing {
                f64_slots,
                ..
            }) = code._native_calls.value_map(offset)
            else {
                return Err(invalid_capture());
            };
            let payload_len = request.n_words as usize - 4;
            // SAFETY: validate_guard_stream accepted the retained single-scope
            // request. Its exact native return-PC descriptor supplies the raw
            // indexes; immutable original arguments are independently rooted.
            unsafe {
                egcl_compiler::t2::deopt::rebox_f64_payload(
                    std::slice::from_raw_parts_mut(request.words.add(4).cast_mut(), payload_len),
                    f64_slots,
                    egcl_rt::gc::alloc_double_float,
                )
            }
            .map_err(|_| invalid_capture())?;
            Some(NativeDeoptRoots {
                payload: request.words.cast_mut().cast(),
                payload_ranges: deopt_payload_ranges(
                    unsafe { std::slice::from_raw_parts(request.words, request.n_words as usize) },
                    request.n_scopes,
                )?,
                arguments: std::ptr::null_mut(),
                argument_count: 0,
            })
        };
        egcl_rt::rooted_ref!(_payload_roots = &mut payload_roots);
        // Resumed Lisp owns the frame; it must not borrow the abandoned
        // native capture. Restore that capture only after restoring the frame.
        let _capture_pause = CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
        // T0 consumes the condition-cluster suffix together with its exact
        // handlers. Restore only the native callable shell after it returns;
        // rebuilding those retired clusters would duplicate dynamic scopes.
        let callable_end = unsafe {
            frame
                .add(1)
                .cast::<EgclVal>()
                .add(usize::from(header.num_locals))
                .cast::<u8>()
        };
        let mut restore = RestoreNativeFrame {
            frame,
            header,
            sp: callable_end,
        };
        egcl_rt::rooted_ref!(_restore_root = &mut restore);
        // Materialized T0 locals no longer follow native debugger recipes.
        // T0 scans every tagged slot; the guard restores native metadata when
        // its exact frame extent is recreated after the continuation returns.
        unsafe {
            (*frame).code_info = std::ptr::null();
        }
        let (scopes, metadata) = materialize_t2_deopt_scopes(
            request.n_scopes,
            request.n_words,
            request.words,
            Some(frame),
            Some(metadata),
        )
        .map_err(|message| EgclError::Internal(message.into()))?;
        unsafe {
            retain_self_scope_callables(code, &scopes)?;
        }
        note_segment_deopt(code, bcp);
        back_off_interpreter_osr(unsafe { (*frame).function });
        // Every stream word is now in scanned slots. There is no mutable
        // CaptureContext reference or RefCell borrow across resumed Lisp.
        egcl_rt::rooted!(
            state = Some(unsafe { take_guard_resume_state(context, code, &scopes, &*env) }?)
        );
        if let Some(arguments) = original_arguments.as_ref() {
            state.as_mut().expect("guard resume state").debug_call = Some(unsafe {
                egcl_rt::debug_stack::CallFrame::enter_managed_at(&code.body.name, arguments, frame)
            });
        }
        // The guarded activation's exact EnvFrame, including child frames it
        // entered and bindings it wrote before the guard failed.
        let env_frame = unsafe { (*(*context).env_frame).clone() };
        resume_inlined_in_t0_with_state(scopes, metadata, env_frame, state.take(), unsafe {
            &mut *env
        })
    });
    // Fiber mounting establishes carrier compatibility before resumed Lisp or
    // Rust executes. Publish the value/transfer without another capability test.
    let outcome = match result {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            stash_native_error(error);
            unsafe {
                (*context).completed_deopt = true;
            }
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe {
        out.write(outcome);
    }
}

/// Called only after the deopt Rust adapter has returned. Its precise state
/// was already consumed; ordinary Invoke capture would replay stale state.
pub(super) unsafe extern "C" fn prepare_completed_deopt(capture: *mut SysvTransferCapture) {
    // Publish the same suspended caller the capture stub imaged. The deopt
    // return PC maps to Deoptimizing, so the walk skips this retired top frame
    // — whose roots T0 already owns — and continues through older ones.
    unsafe {
        super::root_publication::published(capture, || prepare_completed_deopt_unpublished(capture))
    }
}

unsafe fn prepare_completed_deopt_unpublished(capture: *mut SysvTransferCapture) {
    let context = unsafe { &mut *CAPTURE.with(Cell::get) };
    let capture = unsafe { &mut *capture };
    let expected = if context.recursive.is_null() {
        context.frame
    } else {
        unsafe { (*context.recursive).frame }
    };
    let sites = unsafe { &*context.sites };
    if !context.completed_deopt
        || egcl_rt::current_stack().fp() != expected
        || !sites.is_deopt_return(context.code_base, capture.return_pc as usize)
        || capture.exit != NativeExit::Transfer
    {
        context.failure = Some(TransferSiteError::InvalidLandingCapture);
    }
    while !context.recursive.is_null() {
        unsafe {
            retire_recursive(context, context.recursive);
        }
    }
    context.completed_deopt = false;
    native_frames::retire_children(context);
    context.recursive_escape = true;
    prepare_escape(context);
    capture.request = std::ptr::from_mut(&mut context.dispatch).cast();
}

#[cfg(test)]
mod tests {
    #[test]
    fn installed_numeric_deopt_dispatches_fixnum_and_single_exact_sites() {
        use super::{SpecType, numeric_deopt_uses_function_policy};
        for kind in [SpecType::Fixnum, SpecType::SingleFloat] {
            assert!(numeric_deopt_uses_function_policy(&[(11, kind)], 11));
            assert!(!numeric_deopt_uses_function_policy(&[(11, kind)], 9));
            assert!(!numeric_deopt_uses_function_policy(&[(11, kind)], 0));
        }
        assert!(!numeric_deopt_uses_function_policy(&[], 11));
        // Preserve existing raw-double entry/guard routing.
        assert!(numeric_deopt_uses_function_policy(
            &[(11, SpecType::DoubleFloat)],
            0
        ));
        assert!(numeric_deopt_uses_function_policy(
            &[(11, SpecType::DoubleFloat)],
            9
        ));
    }

    #[test]
    fn native_v2_raw_f64_deopt_reboxes_before_t0_resume() {
        use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
        use egcl_compiler::t2::ir::{
            AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
        };
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::super::read_eval_all_env(
            "(defun raw-f64-pass-through (x) x) (defun raw-f64-resume (x held) (if held (raw-f64-pass-through (raw-f64-resume x nil)) x))",
            &mut env,
        )
        .unwrap();
        let pass_through = super::super::super::super::read_eval_all_env(
            "(raw-f64-pass-through -0.0d0)",
            &mut env,
        )
        .unwrap();
        assert!(pass_through.is_double_float());
        assert_eq!(
            pass_through.as_double_float().to_bits(),
            (-0.0_f64).to_bits()
        );
        let symbol = super::super::super::super::resolve_sym("RAW-F64-RESUME")
            .unwrap()
            .as_symbol_index();
        egcl_rt::rooted!(params = reader::read_from_string("(x held)").unwrap().0);
        egcl_rt::rooted!(
            forms = reader::read_from_string(
                "((if held (raw-f64-pass-through (raw-f64-resume x nil)) x))"
            )
            .unwrap()
            .0
        );
        let body = Arc::new(
            compile_function("RAW-F64-RESUME", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        assert!(recursive_definition_matches(symbol, Arc::as_ptr(&body)));
        assert!(scope_free_native_body(&body, false), "{:?}", body.code);
        assert!(
            native_frames::flags().is_some(),
            "native carrier flags must be initialized"
        );
        assert!(
            body.code
                .iter()
                .any(|op| matches!(op, Instr::CallNamed { sym, .. } if *sym == symbol)),
            "must retain a non-tail self call: {:?}",
            body.code
        );
        assert_eq!(body.n_locals, 2);
        let mut ir = Function::new("raw-f64-resume");
        let entry = ir.entry();
        let boxed = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let held = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let tagged = |value| ValueSource::Value {
            value,
            repr: ValueRepresentation::Tagged,
        };
        let before = ir.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: symbol,
                bcp: 0,
                locals: vec![tagged(boxed), tagged(held)],
                stack: vec![],
            }],
            remat: vec![],
        });
        let mut unbox = InstData {
            opcode: Opcode::UnboxFloat,
            args: vec![boxed],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: Some(before),
            source_pos: 0,
        };
        unbox.flags.guard = true;
        unbox.flags.effectful = true;
        let (_, raw) = ir.push_inst(
            entry,
            unbox,
            &[(
                IRType::of(TypeBits::DOUBLE_FLOAT),
                ValueRepresentation::UnboxedF64,
            )],
        );
        let after = ir.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: symbol,
                bcp: 0,
                locals: vec![
                    ValueSource::Value {
                        value: raw[0],
                        repr: ValueRepresentation::UnboxedF64,
                    },
                    tagged(held),
                ],
                stack: vec![],
            }],
            remat: vec![],
        });
        let binding = Box::new(1_u64);
        let mut guard = InstData {
            opcode: Opcode::Guard,
            args: vec![],
            results: vec![],
            aux: AuxData::BuiltinBinding {
                address: (&*binding as *const u64) as usize,
                expected: 0,
            },
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: Some(after),
            source_pos: 0,
        };
        guard.flags.guard = true;
        guard.flags.effectful = true;
        ir.push_inst(entry, guard, &[]);
        ir.set_terminator(
            entry,
            InstData {
                opcode: Opcode::Return,
                args: vec![boxed],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        egcl_compiler::t2::verify::verify(&ir).unwrap();
        let metadata = Arc::new(T2InstalledMetadata {
            speculations: vec![],
            cold_guesses: Vec::new(),
            _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
                bodies: vec![Arc::clone(&body)],
            }),
            deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
        });
        let code = TransferCode::emit_prepared(
            Arc::clone(&body),
            retain_native_body(&body),
            &ir,
            Some(metadata),
            &[],
            true,
            CompilationTier::T2,
        )
        .expect("raw F64 native body");
        assert!(code.is_t2 && code.has_deopt);
        assert!(
            code.native_entry_kind_slot.is_some(),
            "must exercise native-frame reboxing before managed T0 resume"
        );
        for number in [1.25_f64, -0.0] {
            egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(number));
            egcl_rt::rooted!(held = egcl_rt::gc::alloc_double_float(73.5));
            let prior = code.deopt_count();
            egcl_rt::rooted!(result = code.run(&[*input, *held], &mut env).unwrap());
            assert_eq!(
                code.deopt_count(),
                prior + 1,
                "must execute the emitted guard and production deopt handler"
            );
            assert!(result.is_double_float());
            assert_eq!(result.as_double_float().to_bits(), number.to_bits());
            assert_ne!(
                result.to_raw(),
                input.to_raw(),
                "raw payload must be reboxed"
            );
        }
    }

    use super::*;
    use egcl_rt::Collector;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static CACHE_MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
    static CACHE_ROOTS: std::sync::Mutex<Vec<std::sync::Weak<ActiveBytecodeRoot>>> =
        std::sync::Mutex::new(Vec::new());

    fn cache_fiber() -> EgclVal {
        // Keep an assertion failure inside Rust rather than crossing the
        // scheduler's non-unwinding machine trampoline.
        std::panic::catch_unwind(cache_fiber_checked).unwrap_or(EgclVal::from_fixnum(0))
    }

    fn cache_fiber_checked() -> EgclVal {
        assert!(
            SEGMENT_CACHE.with(|cache| cache.borrow().is_empty()),
            "a new fiber must not inherit another execution's code cache"
        );
        let mut env = Env::new_impl(false, false, false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
        let body = Arc::new(
            compile_function(
                "MIGRATING-SEGMENT-CACHE",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let old = cached_code(&body).expect("optimized segment");
        assert!(old.has_deopt);
        for _ in 0..200 {
            let carrier = egcl_rt::current_thread_id();
            egcl_rt::thread::fiber_yield().unwrap();
            if carrier != egcl_rt::current_thread_id() {
                CACHE_MIGRATIONS.fetch_add(1, Ordering::Relaxed);
            }
            let selected = cached_code(&body).unwrap();
            assert!(
                Rc::ptr_eq(&old, &selected),
                "migration changed the retained code version"
            );
            assert_eq!(
                SEGMENT_CACHE.with(|cache| cache.borrow().len()),
                1,
                "other fibers must own separate caches"
            );
        }
        let argument = EgclVal::from_single_float(1.5);
        for _ in 0..deopt_blacklist_threshold() {
            assert_eq!(
                old.run(&[argument], &mut env).unwrap(),
                EgclVal::from_single_float(2.5)
            );
        }
        assert!(
            old.recompile.get(),
            "migrated deopts must update the owning cache version"
        );
        let replacement = cached_code(&body).unwrap();
        assert!(!Rc::ptr_eq(&old, &replacement));
        assert_eq!(
            replacement.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
        assert_eq!(replacement.deopt_count(), 0);
        assert_eq!(
            old.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
        assert!(
            !replacement.recompile.get(),
            "old code must not retire its replacement"
        );
        // Only atomic weak root tokens cross executions, never the code's Rc.
        CACHE_ROOTS.lock().unwrap().extend([
            Arc::downgrade(&old._body_roots),
            Arc::downgrade(&replacement._body_roots),
        ]);
        EgclVal::from_fixnum(1)
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn native_v2_segment_cache_follows_fiber_and_retires_exact_version() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        assert!(egcl_rt::native_transfer::is_supported());
        let mut startup = Env::new(false);
        egcl_rt::rooted_ref!(_startup = &mut startup);
        for num_workers in [1, 4] {
            CACHE_MIGRATIONS.store(0, Ordering::Relaxed);
            let group =
                egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers }).unwrap();
            for _ in 0..12 {
                let entry =
                    unsafe { EgclVal::from_function_ptr(cache_fiber as *const () as *mut u8) };
                group
                    .submit(egcl_rt::thread::make_fiber(entry).unwrap())
                    .unwrap();
            }
            assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1); 12]);
            let roots = std::mem::take(&mut *CACHE_ROOTS.lock().unwrap());
            assert_eq!(roots.len(), 24);
            assert!(
                roots.iter().all(|root| root.upgrade().is_none()),
                "stopped fibers must release cached code and its root ownership"
            );
            if num_workers == 4 {
                assert!(
                    CACHE_MIGRATIONS.load(Ordering::Relaxed) > 0,
                    "probe must actually migrate across workers"
                );
            }
        }
    }

    #[test]
    fn native_v2_saved_frame_header_is_a_moving_root() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        // Interpreted callable objects are pinned today. A movable sentinel
        // exercises the saved header's root contract without claiming that a
        // stress run over an immovable callable proves relocation safety.
        let stack = egcl_rt::current_stack();
        let parent = stack.fp();
        let (old_address, frame) = {
            egcl_rt::rooted!(sentinel = super::super::super::super::arena_cons(T, NIL));
            let frame = stack
                .push_frame(*sentinel, std::ptr::null(), 3, FLAG_CALL)
                .unwrap();
            (sentinel.to_raw(), frame)
        };
        {
            let mut restore = RestoreNativeFrame {
                frame,
                header: unsafe { std::ptr::read(frame) },
                sp: stack.sp(),
            };
            egcl_rt::rooted_ref!(_restore_root = &mut restore);
            stack.pop_frame();
            egcl_rt::gc::HeapCollector::new().minor_gc().unwrap();
            assert_ne!(restore.header.function.to_raw(), old_address);
            assert_eq!(
                super::super::super::super::cp(restore.header.function),
                (T, NIL)
            );
        }
        assert_eq!(stack.fp(), frame);
        assert_ne!(unsafe { (*frame).function.to_raw() }, old_address);
        assert_eq!(
            super::super::super::super::cp(unsafe { (*frame).function }),
            (T, NIL)
        );
        stack.pop_frame();
        assert_eq!(stack.fp(), parent);
    }

    #[test]
    fn native_v2_deopt_restores_frame_extent_after_caught_panic() {
        let stack = egcl_rt::current_stack();
        let parent = stack.fp();
        let parent_sp = stack.sp();
        let frame = stack
            .push_frame(NIL, std::ptr::null(), 12, FLAG_CALL)
            .unwrap();
        let sp = stack.sp();
        let result = guard_c2i(|| {
            // Resumed Lisp owns the frame; it must not borrow the abandoned
            // native capture. Restore that capture only after restoring the frame.
            let _capture_pause =
                CapturePause(CAPTURE.with(|slot| slot.replace(std::ptr::null_mut())));
            let mut restore = RestoreNativeFrame {
                frame,
                header: unsafe { std::ptr::read(frame) },
                sp,
            };
            egcl_rt::rooted_ref!(_restore_root = &mut restore);
            // T0 may pop the owned frame, and an OSR/resume path can use a
            // larger extent. Retirement must still see the original extent.
            stack.pop_frame();
            stack
                .push_frame(NIL, std::ptr::null(), 48, FLAG_CALL)
                .unwrap();
            panic!("injected continuation panic");
        });
        assert!(matches!(result, Err(EgclError::Internal(_))));
        assert_eq!(stack.fp(), frame);
        assert_eq!(stack.sp(), sp);
        assert_eq!(unsafe { (*frame).num_locals }, 12);
        assert!(
            unsafe { egcl_rt::EgclStack::frame_slots_mut(frame) }
                .iter()
                .all(|v| v.is_nil())
        );
        stack.pop_frame();
        assert_eq!(stack.fp(), parent);
        assert_eq!(stack.sp(), parent_sp);
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn native_v2_optimized_cache_recompiles_numeric_phase_without_retiring_replacement() {
        let _lock = super::super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        // Keep a long independent integer chain after the changing float site.
        // It retains optimized straight-line polls after baseline refresh; no
        // loop-header heat poll can mask an incorrectly selected T2 helper.
        let chain = "(setq y (+ y 1)) ".repeat(80);
        let source = format!(
            "((segment-phase-touch x)
            (let ((answer (+ x 1))) {chain} (+ answer y)))"
        );
        super::super::super::super::read_eval_all_env(
            &format!(
                "(defun segment-phase-touch (x) nil)
                (defun segment-phase (x y) (progn . {source}))"
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x y)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
        let symbol = super::super::super::super::resolve_sym("SEGMENT-PHASE")
            .unwrap()
            .as_symbol_index();
        let body = Arc::new(
            compile_function("SEGMENT-PHASE", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let key = Arc::as_ptr(&body) as usize;
        let old = Rc::new(TransferCode::compile(Arc::clone(&body)).unwrap());
        assert!(old.has_deopt);
        SEGMENT_CACHE.with(|cache| {
            cache
                .borrow_mut()
                .insert(key, SegmentCacheEntry::new(&body, Some(Rc::clone(&old))))
        });
        let argument = EgclVal::from_single_float(1.5);
        for _ in 0..deopt_blacklist_threshold() {
            assert_eq!(
                old.run(&[argument, EgclVal::from_fixnum(1)], &mut env)
                    .unwrap(),
                EgclVal::from_single_float(83.5)
            );
        }
        assert!(old.recompile.get());
        // Use the actual dispatch-cache replacement path without changing
        // the process-wide opt-in environment in this test.
        let replacement = cached_code(&body).unwrap();
        assert!(!Rc::ptr_eq(&old, &replacement));
        assert!(
            !replacement.is_t2,
            "cache refresh must remain baseline code"
        );
        assert!(
            replacement.has_deopt,
            "the unchanged integer chain must retain speculative optimization"
        );
        assert!(
            root_publication::OBSERVE.with(|hook| hook.get().is_none()),
            "observer-forced publication cannot prove baseline poll selection"
        );
        reset_poll_paths_for_test();
        for _ in 0..12 {
            assert_eq!(
                replacement
                    .run(&[argument, EgclVal::from_fixnum(1)], &mut env)
                    .unwrap(),
                EgclVal::from_single_float(83.5)
            );
        }
        let (idle, published) = take_poll_paths_for_test();
        assert_eq!(idle, 0, "refreshed optimized baseline polls must publish");
        assert!(
            published >= 12,
            "each refreshed call must execute a real straight-line poll: {published}"
        );
        assert_eq!(
            replacement.deopt_count(),
            0,
            "new numeric phase must stop failing old guards"
        );
        assert_eq!(
            old.run(&[argument, EgclVal::from_fixnum(1)], &mut env)
                .unwrap(),
            EgclVal::from_single_float(83.5)
        );
        assert!(
            !replacement.recompile.get(),
            "old suspended code cannot retire the replacement"
        );
        SEGMENT_CACHE.with(|cache| cache.borrow_mut().remove(&key));
    }
}
