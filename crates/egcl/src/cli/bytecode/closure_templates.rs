// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closure templates and instance state (Backpack stage 2, bliss-gg3i1.7.15.3).
//!
//! A nested body that `MakeClosure` instantiates is a *template*. The first
//! instantiation mints its code identity once: an uninterned template symbol,
//! the body's registration under that symbol, and a pinned *prototype*
//! function object installed in the symbol's function cell. The body caches
//! the prototype in its [`ClosureTemplateCell`].
//!
//! Every instance is then one pinned function object that names the template
//! symbol (`name` and `definition_index`), points at the prototype through its
//! `env` cell, and owns its captured frame and lexical exit scope through a
//! raw [`ClosureState`] word. Creating one takes no symbol, registry, code or
//! profile-table work. The bytecode registry, native code, tiering counters,
//! DISASSEMBLE and the profilers all key on the template symbol, so every
//! instance shares one compiled body and one tiering record; the instance's
//! FnMeta counters and tier delegate to the prototype.
//!
//! An instance that captures nothing is the prototype itself.
//!
//! Collection: an instance traces only its own frame. The prototype traces the
//! template body, once per collection however many instances reach it (they
//! reach it through the template symbol's function cell and their `env`
//! cell). A dead instance's state is released by the major collection that
//! frees its storage; a dead prototype takes the template's registrations
//! with it through the ordinary function-capture sweep.
use super::*;
#[cfg(test)]
use std::cell::Cell;

/// Host-side state owned by one closure object: the heap frame its body
/// reaches the enclosing lexicals through, and the BLOCK/TAGBODY scope its
/// free exits resolve in. Immutable once attached.
pub(in crate::cli) struct ClosureState {
    pub(in crate::cli) frame: Option<Arc<FrameCell>>,
    pub(in crate::cli) control: Option<super::super::LexicalExitNames>,
}

fn state_word(state: ClosureState) -> usize {
    Box::into_raw(Box::new(state)) as usize
}

/// The collector's release hook for a dead object's state word.
pub(super) fn release_state(word: usize) {
    // SAFETY: every nonzero state word was made by `state_word`, and the
    // collector swaps it out of the dead object before releasing it, once.
    drop(unsafe { Box::from_raw(word as *mut ClosureState) });
}

/// Read the state of a function object the caller keeps alive. `read` must
/// not allocate Lisp objects.
#[inline]
pub(in crate::cli) fn with_state<R>(
    function: EgclVal,
    read: impl FnOnce(&ClosureState) -> R,
) -> Option<R> {
    if !egcl_rt::function::is_interpreted_function(function) {
        return None;
    }
    // SAFETY: an interpreted function object; its word is 0 or a live state
    // owned by the object, released only after the object is found dead.
    let word = unsafe { egcl_rt::function::closure_state(function) };
    (word != 0).then(|| read(unsafe { &*(word as *const ClosureState) }))
}

/// Attach a captured frame and/or exit scope to a function object that no
/// other thread can call yet (a fresh object, or a restored image's).
/// Fields given replace the existing ones; the others are kept.
pub(in crate::cli) fn attach_state(
    function: EgclVal,
    frame: Option<Arc<FrameCell>>,
    control: Option<super::super::LexicalExitNames>,
) {
    debug_assert!(egcl_rt::function::is_interpreted_function(function));
    install_bytecode_root_scanner();
    // SAFETY: an interpreted function object. Nothing below allocates or
    // reaches a safepoint, so no collection observes the transient 0.
    let old = unsafe { egcl_rt::function::set_closure_state(function, 0) };
    let mut state = if old == 0 {
        ClosureState {
            frame: None,
            control: None,
        }
    } else {
        *unsafe { Box::from_raw(old as *mut ClosureState) }
    };
    if frame.is_some() {
        state.frame = frame;
    }
    if control.is_some() {
        state.control = control;
    }
    if state.frame.is_some() || state.control.is_some() {
        unsafe { egcl_rt::function::set_closure_state(function, state_word(state)) };
    }
}

/// Drop a function's state when delivery prunes it.
pub(in crate::cli) fn clear_state(function: EgclVal) {
    if egcl_rt::function::is_interpreted_function(function) {
        let old = unsafe { egcl_rt::function::set_closure_state(function, 0) };
        if old != 0 {
            release_state(old);
        }
    }
}

/// Every live function object's state, for the image host registry. Call only
/// with mutators stopped after a collection; allocates no Lisp objects.
#[allow(clippy::type_complexity)]
pub(in crate::cli) fn state_entries() -> Vec<(
    EgclVal,
    Option<Arc<FrameCell>>,
    Option<super::super::LexicalExitNames>,
)> {
    let mut entries = Vec::new();
    let _ = egcl_rt::gc::walk_heap(|body, kind, _| {
        if kind == egcl_rt::object::type_id::FUNCTION_INTERPRETED {
            // SAFETY: a function object is never large; its header
            // immediately precedes the body the walk reports.
            let function = unsafe {
                EgclVal::from_heap_ptr(
                    body.sub(std::mem::size_of::<egcl_rt::object::ObjectHeader>())
                        .cast_mut(),
                )
            };
            with_state(function, |state| {
                entries.push((function, state.frame.clone(), state.control.clone()));
            });
        }
        true
    });
    entries
}

/// `EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE` makes every creation a fresh object,
/// even when it could be the shared prototype (for identity-churn tests).
fn sharing_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE").is_none())
}

/// Serializes template publication. Held only across the atomic publish:
/// never across an allocation.
static PUBLISH: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Whether a template body has a free control exit, analyzed once per body.
pub(super) fn control_free(nested: &BytecodeFunction) -> bool {
    nested.closure_template.control_free().unwrap_or_else(|| {
        let free = !bf_has_free_control_exit(nested);
        nested.closure_template.set_control_free(free);
        free
    })
}

/// The prototype and template symbol of `nested`, minting them on first use.
/// The prototype is unrooted: the caller roots it before allocating.
#[inline]
pub(super) fn template_for(nested: &Arc<BytecodeFunction>) -> (EgclVal, u32) {
    if let Some(found) = nested.closure_template.current() {
        return found;
    }
    template_slow(nested)
}

#[cold]
fn template_slow(nested: &Arc<BytecodeFunction>) -> (EgclVal, u32) {
    // A collection freed some pinned function since the cell was validated.
    // Uninterned indices are never reused, so a live template symbol whose
    // cell still holds the recorded prototype proves that prototype live.
    if let Some((prototype, symbol)) = nested.closure_template.candidate()
        && egcl_rt::symbols::symbol_function(symbol) == Some(prototype)
    {
        let _publish = PUBLISH.lock().unwrap_or_else(|error| error.into_inner());
        nested.closure_template.publish(prototype, symbol);
        return (prototype, symbol);
    }
    egcl_rt::rooted!(_nested_root = ActiveBytecodeRoot::new(nested));
    let (prototype, symbol) = mint_template(nested);
    egcl_rt::rooted!(prototype = prototype);
    let _publish = PUBLISH.lock().unwrap_or_else(|error| error.into_inner());
    // A racing thread's template stays valid for its own instances.
    if let Some(winner) = nested.closure_template.current() {
        return winner;
    }
    nested.closure_template.publish(*prototype, symbol);
    (*prototype, symbol)
}

/// Mint the code identity of a template: the only symbol, registry and
/// body-table work its instances ever cause.
fn mint_template(nested: &Arc<BytecodeFunction>) -> (EgclVal, u32) {
    egcl_rt::rooted!(lambda_list = nested.params_form);
    if !nested.variadic {
        *lambda_list = installed_lambda_list(nested.arity);
    }
    egcl_rt::rooted!(name = arena_str("CLOSURE"));
    egcl_rt::rooted!(sym = egcl_rt::symbols::make_uninterned_with_name(*name));
    let index = sym.as_symbol_index();
    // Until the prototype owns it, an active-callable root keeps the body's
    // registration from looking dead to a major sweep.
    egcl_rt::rooted!(_callable_root = ActiveCallableRoot(index));
    closure_bodies().insert(index, Arc::clone(nested));
    publish_fresh_private_bytecode(index, Arc::clone(nested));
    egcl_rt::rooted!(
        prototype = egcl_rt::function::alloc_interpreted(*lambda_list, NIL, NIL, *sym)
    );
    // SAFETY: fresh interpreted function, not yet visible to any thread.
    unsafe { egcl_rt::function::mark_closure_template(*prototype) };
    egcl_rt::symbols::set_symbol_function(index, *prototype);
    #[cfg(test)]
    MINTED.with(|minted| minted.set(minted.get() + 1));
    (*prototype, index)
}

/// Make a closure of template `nested` that owns `captured_env` and
/// `control`. With neither, and `share`, the result is the shared prototype;
/// otherwise a fresh instance object.
pub(super) fn make_instance(
    nested: &Arc<BytecodeFunction>,
    captured_env: Option<Arc<FrameCell>>,
    control: Option<super::super::LexicalExitNames>,
    share: bool,
) -> EgclVal {
    // The caller need not expose this frame through an active activation yet.
    // Own a precise root before pending-major or allocating work.
    egcl_rt::rooted!(
        _capture_root = captured_env
            .as_ref()
            .map(|frame| super::super::SuspendedFrameRoot(Arc::clone(frame)))
    );
    #[cfg(test)]
    collect_during_preparation_for_test();
    // MakeClosure remains a cooperation opportunity even when it allocates
    // nothing: native loop polling may count this helper as its safepoint.
    egcl_rt::safepoint::poll_safepoint();
    collect_requested_major_before_closure();
    let (prototype, _) = template_for(nested);
    #[cfg(test)]
    method_context_use::helper_values::record_creation(nested);
    if captured_env.is_none() && control.is_none() {
        if share && sharing_enabled() {
            return prototype;
        }
        return egcl_rt::function::alloc_closure_instance(prototype, 0);
    }
    note_closure_instance_state();
    let state = state_word(ClosureState {
        frame: captured_env,
        control,
    });
    // The capture root above still traces the frame the state now owns.
    egcl_rt::function::alloc_closure_instance(prototype, state)
}

#[cfg(test)]
thread_local! {
    /// Templates minted on this thread.
    static MINTED: Cell<usize> = const { Cell::new(0) };
}

#[cfg(test)]
static COLLECT_DURING_PREPARATION: egcl_rt::execution_local::ExecutionLocal<Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(false)) };

#[cfg(test)]
pub(super) fn collect_during_preparation_for_test() {
    if COLLECT_DURING_PREPARATION.with(|armed| armed.replace(false)) {
        egcl_rt::gc::collect_t0_minor().unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template_body(label: &str, env: &Env) -> Arc<BytecodeFunction> {
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        Arc::new(compile_function(label, NIL, *form, env, false, false).unwrap())
    }

    #[test]
    fn closure_template_reuses_only_safe_producer_objects_and_keeps_fresh_api() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-CACHE", &env);
        egcl_rt::rooted!(first = make_template_closure(&body, None, &env));
        // A closure that captures nothing is the template's prototype.
        let reused = make_template_closure(&body, None, &env);
        assert_eq!(*first, reused);
        assert!(egcl_rt::function::is_closure_template(*first));
        egcl_rt::gc::full_gc().unwrap();
        assert_eq!(*first, make_template_closure(&body, None, &env));
        egcl_rt::rooted!(fresh = make_bytecode_closure(&body, None));
        assert_ne!(*first, *fresh);
        assert!(Arc::ptr_eq(
            &body,
            &registry_get(egcl_rt::function::definition_index(*fresh)).unwrap()
        ));
        assert_eq!(
            egcl_rt::function::definition_index(*fresh),
            egcl_rt::function::definition_index(*first)
        );
    }

    #[test]
    fn closure_template_cache_does_not_reuse_capturing_objects_or_control_tokens() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        egcl_rt::rooted!(form = crate::cli::vec_to_list(&[NIL]));
        let mut code =
            compile_function("TEMPLATE-CAPTURE", NIL, *form, &env, false, false).unwrap();
        // Free control is rejected independently of the capture_env bit.
        code.names.push("TEMPLATE-BLOCK".into());
        code.constants.push(NIL);
        code.code = vec![
            Instr::Const((code.constants.len() - 1) as u16),
            Instr::ReturnFromNamed { name_idx: 0 },
        ];
        let body = Arc::new(code);
        assert!(bf_has_free_control_exit(&body));
        egcl_rt::rooted!(first = make_template_closure(&body, None, &env));
        egcl_rt::rooted!(second = make_template_closure(&body, None, &env));
        assert_ne!(*first, *second);
        assert_eq!(body.closure_template.control_free(), Some(false));
        let frame = FrameCell::new_arc(EnvFrame {
            vars: Default::default(),
            symbol_vars: Default::default(),
            function_vars: Default::default(),
            parent: None,
        });
        egcl_rt::rooted!(captured_a = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        egcl_rt::rooted!(captured_b = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        assert_ne!(*captured_a, *captured_b);
        for callable in [*captured_a, *captured_b] {
            assert!(Arc::ptr_eq(
                &frame,
                &closure_captured_env(callable).unwrap()
            ));
        }
    }

    #[test]
    fn closure_preparation_roots_sole_captured_cell_before_any_collection() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-SOLE-CAPTURE", &env);
        egcl_rt::gc::collect_t0_minor().unwrap();
        let frame = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL));
        frame.borrow_mut().vars.insert("HELD".into(), *payload);
        drop(payload);
        let before = frame.borrow().vars["HELD"].0;
        // No Env/activation/host root exposes frame here. The builder's own
        // precise root must be installed before this forced collection fires.
        COLLECT_DURING_PREPARATION.with(|armed| armed.set(true));
        egcl_rt::rooted!(function = make_bytecode_closure(&body, Some(Arc::clone(&frame))));
        let held = frame.borrow().vars["HELD"];
        assert_ne!(
            held.0, before,
            "the sole captured payload must actually move inside preparation"
        );
        assert_eq!(crate::cli::cp(held), (EgclVal::from_fixnum(73), NIL));
        assert!(Arc::ptr_eq(
            &frame,
            &closure_captured_env(*function).unwrap()
        ));
    }

    /// Many instances of one template share one code identity: one minted
    /// symbol, one registry entry, one compiled-code key and one tiering
    /// record, while each keeps its own captured frame.
    #[test]
    fn instances_share_one_template_identity_and_tiering_record() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-SHARED", &env);
        let minted = MINTED.with(Cell::get);
        egcl_rt::rooted!(instances = Vec::<EgclVal>::new());
        let mut frames = Vec::new();
        for _ in 0..64 {
            let frame = FrameCell::new_arc(EnvFrame::default());
            frames.push(Arc::clone(&frame));
            let instance = make_template_closure(&body, Some(frame), &env);
            instances.push(instance);
        }
        assert_eq!(MINTED.with(Cell::get), minted + 1, "one template per body");
        let symbol = egcl_rt::function::definition_index(instances[0]);
        let prototype = egcl_rt::symbols::symbol_function(symbol).unwrap();
        assert!(egcl_rt::function::is_closure_template(prototype));
        for (instance, frame) in instances.iter().zip(&frames) {
            assert!(egcl_rt::function::is_closure_instance(*instance));
            assert_eq!(egcl_rt::function::definition_index(*instance), symbol);
            assert_eq!(
                egcl_rt::function::name(*instance).symbol_index(),
                Some(symbol)
            );
            assert_eq!(
                egcl_rt::function::closure_prototype(*instance),
                Some(prototype)
            );
            assert!(Arc::ptr_eq(
                frame,
                &closure_captured_env(*instance).unwrap()
            ));
            let (owner, code) = bytecode_for_callable(symbol, *instance).unwrap();
            assert_eq!(owner, symbol);
            assert!(Arc::ptr_eq(&code, &body));
        }
        // One distinct object per instance.
        let distinct: std::collections::HashSet<_> = instances.iter().map(|f| f.0).collect();
        assert_eq!(distinct.len(), instances.len());
        // The counters and tier of every instance are the prototype's.
        let before = egcl_rt::function::invoke_count(prototype);
        for instance in instances.iter() {
            egcl_rt::function::record_invocation(*instance);
        }
        assert_eq!(
            egcl_rt::function::invoke_count(prototype),
            before + instances.len() as u32
        );
        assert_eq!(
            egcl_rt::function::invoke_count(instances[3]),
            egcl_rt::function::invoke_count(prototype)
        );
        egcl_rt::function::set_tier(instances[7], 1);
        assert_eq!(egcl_rt::function::tier(prototype), 1);
        assert_eq!(egcl_rt::function::tier(instances[9]), 1);
        egcl_rt::function::set_tier(prototype, 0);
    }

    /// Creating an instance publishes nothing: no template, registry,
    /// body-table or symbol work once the template exists.
    #[test]
    fn instance_creation_takes_no_registry_insert() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-NO-INSERT", &env);
        let frame = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::rooted!(first = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        let minted = MINTED.with(Cell::get);
        let registry = REGISTRY.with(|registry| registry.borrow().len());
        let bodies = closure_bodies().borrow().len();
        let published = PRIVATE_CLOSURES_SINCE_MAJOR.load(std::sync::atomic::Ordering::Relaxed);
        for _ in 0..256 {
            let instance = make_template_closure(&body, Some(Arc::clone(&frame)), &env);
            assert_eq!(
                egcl_rt::function::definition_index(instance),
                egcl_rt::function::definition_index(*first)
            );
        }
        assert_eq!(MINTED.with(Cell::get), minted);
        assert_eq!(REGISTRY.with(|registry| registry.borrow().len()), registry);
        assert_eq!(closure_bodies().borrow().len(), bodies);
        assert_eq!(
            PRIVATE_CLOSURES_SINCE_MAJOR.load(std::sync::atomic::Ordering::Relaxed),
            published
        );
    }

    /// A dead instance's state, and with it the frame it alone retained, is
    /// released by the collection that frees the instance.
    #[test]
    fn dead_instances_release_their_captured_frames() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-RECLAIM", &env);
        egcl_rt::rooted!(
            kept =
                make_template_closure(&body, Some(FrameCell::new_arc(EnvFrame::default())), &env)
        );
        let kept_frame = Arc::downgrade(&closure_captured_env(*kept).unwrap());
        let mut dead = Vec::new();
        for _ in 0..32 {
            let frame = FrameCell::new_arc(EnvFrame::default());
            dead.push(Arc::downgrade(&frame));
            make_template_closure(&body, Some(frame), &env);
        }
        assert!(dead.iter().all(|frame| frame.strong_count() == 1));
        egcl_rt::gc::full_gc().unwrap();
        assert!(
            dead.iter().all(|frame| frame.strong_count() == 0),
            "dead instances must release their frames"
        );
        assert_eq!(
            kept_frame.strong_count(),
            1,
            "a live instance keeps its frame"
        );
        // The template survives with its live instance.
        let symbol = egcl_rt::function::definition_index(*kept);
        assert!(registry_get(symbol).is_some_and(|code| Arc::ptr_eq(&code, &body)));
    }

    /// A captured payload reachable only through an old instance's state is
    /// traced and relocated by a minor collection.
    #[test]
    fn instance_frame_is_traced_and_relocated() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env_root = &mut env);
        let body = template_body("TEMPLATE-RELOCATE", &env);
        let frame = FrameCell::new_arc(EnvFrame::default());
        egcl_rt::rooted!(instance = make_template_closure(&body, Some(Arc::clone(&frame)), &env));
        let weak = Arc::downgrade(&frame);
        drop(frame);
        egcl_rt::gc::full_gc().unwrap();
        egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(91), NIL));
        let frame = weak.upgrade().unwrap();
        frame.borrow_mut().vars.insert("HELD".into(), *payload);
        drop(frame);
        let before = payload.0;
        drop(payload);
        egcl_rt::gc::collect_t0_minor().unwrap();
        let frame = closure_captured_env(*instance).unwrap();
        let held = frame.borrow().vars["HELD"];
        assert_ne!(held.0, before, "the captured payload must actually move");
        assert_eq!(crate::cli::cp(held), (EgclVal::from_fixnum(91), NIL));
        drop(frame);
        // And a full collection that finds only the instance keeps it.
        egcl_rt::gc::full_gc().unwrap();
        let frame = closure_captured_env(*instance).unwrap();
        assert_eq!(
            crate::cli::cp(frame.borrow().vars["HELD"]).0,
            EgclVal::from_fixnum(91)
        );
    }
}
