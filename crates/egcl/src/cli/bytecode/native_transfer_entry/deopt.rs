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

fn note_segment_deopt(code: &TransferCode) {
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
        if !native.is_t2 {
            note_native_deopt(*symbol, Some(native));
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
        decay_failed_speculation(
            &code.body,
            None,
            code.deopt_metadata
                .as_ref()
                .map(|m| m.speculations.as_slice()),
        );
    }
    // Recompile only this cached version. Resumed T0 samples the retained
    // original body; unsupported numeric phases then leave their calls generic.
    // An older suspended activation must not retire its replacement.
    if count >= deopt_blacklist_threshold() {
        code.recompile.set(true);
    }
}

/// Rebuild one retained activation's local handler order without creating new
/// registrations. Catch tags remain rooted in Env throughout this handoff.
unsafe fn take_guard_resume_state(
    context: *mut CaptureContext,
    code: &TransferCode,
    scopes: &[InlinedResumeScope],
    env: &Env,
) -> Result<OuterResumeState, EgclError> {
    let [scope] = scopes else {
        return Err(invalid_capture());
    };
    if !Arc::ptr_eq(&scope.body, &code.body) {
        return Err(invalid_capture());
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
    let active = code.control_scopes.before(scope.bcp).ok_or_else(invalid_capture)?;
    if active.iter().any(|scope| {
        scope.ownership != Ownership::Local
            || !matches!(scope.kind,
                ScopeKind::Block { register: false, .. }
                    | ScopeKind::Tagbody { .. }
                    | ScopeKind::Catch { .. }
                    | ScopeKind::Unwind { .. }
                    | ScopeKind::Cleanup { .. }
                    | ScopeKind::HandlerCase { .. }
                    | ScopeKind::HandlerBind { .. }
                    | ScopeKind::RestartCase { .. } | ScopeKind::SpecialBinding { .. })
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
            if saved.cleanup_bcp != cleanup_bcp
                || saved.continuation.handler_depth != handler_depth
            {
                return Err(invalid_capture());
            }
        } else if !matches!(scope.kind, ScopeKind::SpecialBinding { .. }) {
            handler_depth += 1;
        }
    }
    if saved_cleanups.next().is_some() {
        return Err(invalid_capture());
    }
    if !unsafe { validate_special_bindings(context, active) } { return Err(invalid_capture()); }
    let specials = unsafe { &mut *context.specials };
    let catches = unsafe { &mut *context.catches };
    let expected_catches = active.iter().filter_map(|scope| {
        matches!(scope.kind, ScopeKind::Catch { .. }).then_some(scope.push_bcp)
    });
    let tail = env.catch_stack.get(context.catch_base..).ok_or_else(invalid_capture)?;
    if !expected_catches.eq(catches.iter().map(|saved| saved.push_bcp))
        || tail.len() != catches.len()
        || !tail.iter().zip(catches.iter()).all(|((_, token), saved)| *token == saved.token)
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
        reserved.take().expect("validated guard recovery reservation")
    } else {
        // Direct recursive frames share this context but remain scope-free.
        // Do not consume the outer activation's one-shot cleanup reservation.
        if !cleanups.is_empty() || !catches.is_empty() || !specials.is_empty()
            || unsafe { !(*context.handlers).is_empty()
                || !(*context.handler_binds).is_empty()
                || !(*context.restart_cases).is_empty()
                || !(*context.dynamic_scopes).is_empty() }
            || active.iter().any(|scope| matches!(scope.kind,
                ScopeKind::Unwind { .. } | ScopeKind::Cleanup { .. } | ScopeKind::Catch { .. }
                    | ScopeKind::HandlerCase { .. } | ScopeKind::HandlerBind { .. }
                    | ScopeKind::RestartCase { .. } | ScopeKind::SpecialBinding { .. }))
        {
            return Err(invalid_capture());
        }
        let mut handlers = Vec::new();
        handlers.try_reserve(handler_depth).map_err(|_| EgclError::Oom)?;
        OuterResumeState { debug_call: None, handlers, cleanup_conts: Vec::new(), dyn_binds: Vec::new() }
    };
    egcl_rt::rooted_ref!(_state_root = &mut state);
    state.cleanup_conts.extend(cleanups.drain(..).map(|saved| saved.continuation));
    state.dyn_binds.extend(specials.drain(..).map(|saved| saved.binding));
    let mut saved_catches = catches.drain(..);
    // The shared validator checked each ledger against this lexical order
    // before any native metadata changed. Move registrations, never rebuild
    // them: tokens, restart binding IDs and captured handler functions stay live.
    let mut saved_handlers = unsafe { &mut *context.handlers }.drain(..);
    let mut saved_binds = unsafe { &mut *context.handler_binds }.drain(..);
    let mut saved_restarts = unsafe { &mut *context.restart_cases }.drain(..);
    for scope in active {
        let handler = match scope.kind {
            ScopeKind::Block { id, resume_bcp, register: false } => Handler::Block {
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
            ScopeKind::Cleanup { .. } | ScopeKind::SpecialBinding { .. } => continue,
            ScopeKind::Catch { resume_bcp } => Handler::Catch {
                token: saved_catches.next().expect("validated catch order").token,
                resume_bcp,
                sp_restore: scope.sp_restore,
            },
            ScopeKind::HandlerCase { .. } => {
                let saved = saved_handlers.next().expect("validated handler-case order");
                Handler::HandlerCase {
                    clauses: saved.clauses,
                    sp_restore: scope.sp_restore,
                    cluster_base: saved.cluster_base,
                    cluster_frame: saved.cluster_frame,
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

/// Mapped guards materialize exactly one retained frame. Validate its complete
/// stream before changing native metadata or any activation slot; unlike the
/// general inline materializer, this path never needs to grow EgclStack.
unsafe fn validate_guard_stream(
    request: &TransferDeoptRequest,
    code: &TransferCode,
    header: &Frame,
    metadata: &T2InstalledMetadata,
) -> Result<u32, EgclError> {
    if request.n_scopes != 1 || request.words.is_null() || request.n_words < 4
        || request.n_words > 4 + u64::from(code.body.n_locals) + u64::from(code.body.max_stack)
    {
        return Err(invalid_capture());
    }
    let words = unsafe { std::slice::from_raw_parts(request.words, request.n_words as usize) };
    let function = u32::try_from(words[0]).map_err(|_| invalid_capture())?;
    let bcp = u32::try_from(words[1]).map_err(|_| invalid_capture())?;
    let Some(body) = metadata.deopt_bodies.get(&function) else {
        return Err(invalid_capture());
    };
    if !Arc::ptr_eq(body, &code.body)
        || words[2] != u64::from(code.body.n_locals)
        || words[3] > u64::from(code.body.max_stack)
        || words[2] + words[3] + 4 != request.n_words
        || words[2] + words[3] > u64::from(header.num_locals)
        || code.control_scopes.before(bcp).is_none()
    {
        return Err(invalid_capture());
    }
    Ok(bcp)
}

/// The top deoptimizing native map deliberately omits its old SSA homes.
/// Keep the serialized tagged payload and immutable argument copies rooted in
/// place until the resumed interpreter owns its values. Header words are raw.
struct NativeDeoptRoots {
    payload: *mut EgclVal,
    payload_len: usize,
    arguments: *mut EgclVal,
    argument_count: usize,
}

impl egcl_rt::gc::TraceHostRoots for NativeDeoptRoots {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        for index in 0..self.payload_len {
            visit(unsafe { self.payload.add(index) });
        }
        for index in 0..self.argument_count {
            visit(unsafe { self.arguments.add(index) });
        }
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
            Some(NativeCallValues::Deoptimizing { native_slots, f64_slots }) =>
                (*native_slots, f64_slots.as_ref()),
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
            payload: unsafe { request.words.add(4).cast_mut().cast() },
            // The reboxer roots tagged words while this owner roots arguments.
            // Do not scan raw F64 words with the completed-payload descriptor.
            payload_len: 0,
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
        }.map_err(|_| invalid_capture())?;
        roots.payload_len = words_len - 4;
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
        let env = NATIVE_ENV.with(Cell::get);
        if env.is_null() {
            return Err(invalid_capture());
        }
        egcl_rt::rooted!(callable = unsafe { (*frame).function });
        if !egcl_rt::function::is_interpreted_function(*callable) {
            return Err(invalid_capture());
        }
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
            let ScopeKind::Block {
                id,
                resume_bcp,
                register: false,
            } = scope.kind
            else {
                unreachable!()
            };
            state.handlers.push(Handler::Block {
                block_id: id,
                token: String::new(),
                resume_bcp,
                sp_restore: scope.sp_restore,
            });
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
        note_segment_deopt(code);
        if let Some(arguments) = arguments.as_ref() {
            state
                .as_mut()
                .expect("native child resume state")
                .debug_call = Some(unsafe {
                egcl_rt::debug_stack::CallFrame::enter_managed_at(&code.body.name, arguments, child)
            });
        }
        resume_inlined_in_t0_with_state(scopes, metadata, None, state.take(), unsafe { &mut *env })
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
            let active = code.control_scopes.before(bcp).expect("validated guard bcp");
            if !unsafe { validate_condition_clusters(&*context, active, &*env)
                && validate_special_bindings(&*context, active) } {
                return Err(invalid_capture());
            }
        }
        // Native recipes describe proven original arguments before the deopt
        // stream replaces dead locals. Keep a traced copy until a managed
        // debug-call record owns it; unavailable native arguments stay unknown.
        egcl_rt::rooted!(original_arguments = match native_arguments.take() {
            Some(arguments) => arguments,
            None => unsafe { (&*header.code_info).original_arguments((*frame).locals()) },
        });
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
                f64_slots, ..
            }) = code._native_calls.value_map(offset) else {
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
            }.map_err(|_| invalid_capture())?;
            Some(NativeDeoptRoots {
                payload: unsafe { request.words.add(4).cast_mut().cast() },
                payload_len,
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
            frame.add(1).cast::<EgclVal>().add(usize::from(header.num_locals)).cast::<u8>()
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
        note_segment_deopt(code);
        // Every stream word is now in scanned slots. There is no mutable
        // CaptureContext reference or RefCell borrow across resumed Lisp.
        egcl_rt::rooted!(state = Some(unsafe {
            take_guard_resume_state(context, code, &scopes, &*env)
        }?));
        if let Some(arguments) = original_arguments.as_ref() {
            state.as_mut().expect("guard resume state").debug_call = Some(unsafe {
                egcl_rt::debug_stack::CallFrame::enter_managed_at(&code.body.name, arguments, frame)
            });
        }
        resume_inlined_in_t0_with_state(scopes, metadata, None, state.take(), unsafe { &mut *env })
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
        let pass_through =
            super::super::super::super::read_eval_all_env("(raw-f64-pass-through -0.0d0)", &mut env)
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
            forms =
                reader::read_from_string("((if held (raw-f64-pass-through (raw-f64-resume x nil)) x))")
                    .unwrap()
                    .0
        );
        let body =
            Arc::new(compile_function("RAW-F64-RESUME", *params, *forms, &env, false, false).unwrap());
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
