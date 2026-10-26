// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

#[derive(Clone, Copy, PartialEq, Eq)]
enum ConditionCase {
    Bind,
    Case,
    Restart,
    Mixed,
}
impl ConditionCase {
    fn name(self) -> &'static str {
        match self {
            Self::Bind => "BIND",
            Self::Case => "CASE",
            Self::Restart => "RESTART",
            Self::Mixed => "MIXED",
        }
    }
    fn binds(self) -> bool {
        matches!(self, Self::Bind | Self::Mixed)
    }
    fn restarts(self) -> bool {
        matches!(self, Self::Restart | Self::Mixed)
    }
    fn clusters(self) -> usize {
        if self == Self::Mixed {
            2
        } else {
            1
        }
    }
    fn matches_scope(self, scope: &egcl_compiler::control_scope::ControlScope) -> bool {
        use egcl_compiler::control_scope::{Ownership, ScopeKind};
        scope.ownership == Ownership::Local
            && match self {
                Self::Bind => matches!(scope.kind, ScopeKind::HandlerBind { .. }),
                Self::Case => matches!(scope.kind, ScopeKind::HandlerCase { .. }),
                Self::Restart | Self::Mixed => matches!(scope.kind, ScopeKind::RestartCase { .. }),
            }
    }
}

#[derive(Default)]
struct ConditionObservation {
    symbol: u32,
    case: Option<ConditionCase>,
    replacement: Option<Rc<NativeCode>>,
    parent_owner: usize,
    parent_frame: usize,
    parent_segment: usize,
    parent_root: Option<*const EgclVal>,
    parent_moved: bool,
    nested_seen: bool,
    first_owner: usize,
    next_owner: usize,
    frame: usize,
    source_slots: u16,
    source_cluster_frames: Vec<usize>,
    cluster_suffix_moved: bool,
    handler_base: usize,
    restart_base: usize,
    tokens: Vec<String>,
    restart_ids: Vec<u64>,
    // These point into the fixture's rooted locals, never unrooted values.
    roots: Option<[*const EgclVal; 4]>, // marker, hits, setup, payload
    handler_root: Option<*const EgclVal>,
    guard: bool,
    entered: bool,
    steps_at_handoff: i64,
    finishes: usize,
    collected: bool,
}
thread_local! {
    static CONDITION_OBSERVATION: RefCell<ConditionObservation> = RefCell::new(ConditionObservation::default());
}

fn handler_tokens(env: &Env, base: usize) -> Vec<String> {
    env.handlers[base..]
        .iter()
        .flat_map(|cluster| cluster.entries.iter())
        .filter_map(|entry| {
            if let super::super::super::HandlerImpl::HandlerCase { token, .. } = &entry.handler {
                Some(token.clone())
            } else {
                None
            }
        })
        .collect()
}

fn assert_condition_registrations(env: &Env) {
    CONDITION_OBSERVATION.with(|state| {
        let state = state.borrow();
        let case = state.case.unwrap();
        assert_eq!(
            env.handlers.len(),
            state.handler_base + usize::from(case.binds() || case == ConditionCase::Case)
        );
        assert_eq!(
            env.restarts.len(),
            state.restart_base + usize::from(case.restarts())
        );
        assert_eq!(
            handler_tokens(env, state.handler_base),
            state.tokens,
            "retain exact handler-case clause tokens"
        );
        assert_eq!(
            env.restarts[state.restart_base..]
                .iter()
                .map(|restart| restart.id)
                .collect::<Vec<_>>(),
            state.restart_ids,
            "retain exact restart identities"
        );
        if case.binds() {
            let entries = &env.handlers[state.handler_base].entries;
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].type_name, "SIMPLE-CONDITION");
            let super::super::super::HandlerImpl::Function(handler) = &entries[0].handler else {
                panic!("original handler-bind function remains installed")
            };
            assert_eq!(*handler, unsafe { *state.handler_root.unwrap() });
        }
    });
}

fn assert_condition_cluster_chain(callable: *mut Frame, count: usize) {
    let mut cursor = egcl_rt::current_stack().fp() as *mut Frame;
    for _ in 0..count {
        assert!(
            !cursor.is_null() && cursor != callable,
            "each live condition scope has a younger cluster frame"
        );
        assert_eq!(
            unsafe { (*cursor).flags },
            egcl_rt::stack::FrameType::Special as u32
        );
        assert!(unsafe { (*cursor).code_info.is_null() });
        cursor = unsafe { (*cursor).prev_fp };
    }
    assert_eq!(
        cursor, callable,
        "cluster chain must terminate at the retained callable"
    );
}

fn condition_cluster_addresses(callable: *mut Frame) -> Vec<usize> {
    let mut frames = Vec::new();
    let mut cursor = egcl_rt::current_stack().fp() as *mut Frame;
    let mut traversed = 0;
    while cursor != callable {
        assert!(!cursor.is_null(), "retained callable must remain on the stack");
        traversed += 1;
        assert!(traversed < 128, "condition frame chain must terminate");
        // A T0 observer can have younger ordinary callback frames. Record only
        // the live condition suffix; those callbacks establish no new scopes.
        if unsafe { (*cursor).frame_type() } == egcl_rt::FrameType::Special {
            frames.push(cursor as usize);
        }
        cursor = unsafe { (*cursor).prev_fp };
    }
    frames
}

fn assert_condition_suffix_moved() {
    CONDITION_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        let callable = state.frame as *mut Frame;
        let current_slots = unsafe { (*callable).num_locals };
        assert!(current_slots >= state.source_slots.checked_add(8).unwrap(),
            "fixture must force actual callable growth beneath condition frames");
        let delta = usize::from(current_slots - state.source_slots) * std::mem::size_of::<EgclVal>();
        let current = condition_cluster_addresses(callable);
        assert_eq!(current.len(), state.case.unwrap().clusters());
        assert_eq!(current.len(), state.source_cluster_frames.len());
        for (moved, original) in current.iter().zip(&state.source_cluster_frames) {
            assert_eq!(*moved, original + delta, "each Special frame must shift by the measured growth");
            assert_ne!(moved, original);
        }
        state.cluster_suffix_moved = true;
    });
}

fn collect_condition_roots(env: &mut Env, native: Option<*mut Frame>) -> Result<(), EgclError> {
    let before = CONDITION_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert!(!state.collected);
        state.roots.unwrap().map(|root| unsafe { (*root).to_raw() })
    });
    let parent_before = CONDITION_OBSERVATION.with(|state| {
        state
            .borrow()
            .parent_root
            .map(|root| unsafe { (*root).to_raw() })
    });
    egcl_rt::collect_t0_minor()?;
    assert_condition_registrations(env);
    CONDITION_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        let roots = state.roots.unwrap();
        for (index, root) in roots.into_iter().enumerate() {
            assert_ne!(
                unsafe { (*root).to_raw() },
                before[index],
                "root {index} must actually relocate at handoff/recovery"
            );
        }
        if let Some(frame) = native {
            for (slot, root) in [(0, roots[0]), (1, roots[1]), (2, roots[2]), (4, roots[3])] {
                assert_eq!(unsafe { slot_get(frame, slot) }, unsafe { *root });
            }
            assert_condition_cluster_chain(frame, state.case.unwrap().clusters());
        }
        if let Some(before) = parent_before {
            let held = unsafe { *state.parent_root.unwrap() };
            assert_ne!(held.to_raw(), before, "suspended parent's root must move");
            assert_eq!(
                unsafe { slot_get(state.parent_frame as *mut Frame, 0) },
                held,
                "suspended parent must receive its relocated root"
            );
            state.parent_moved = true;
        }
        state.collected = true;
    });
    Ok(())
}

fn observe_condition_osr_poll() {
    let Some((owner, nested, frame)) = native_transfer_entry::condition_capture_for_test() else {
        return;
    };
    let env_ptr = NATIVE_ENV.with(Cell::get);
    assert!(!frame.is_null() && !env_ptr.is_null());
    let segment = egcl_rt::native_transfer::current_segment() as usize;
    let parent_poll = CONDITION_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        if owner == state.parent_owner {
            assert!(!nested);
            if state.parent_frame != 0 {
                assert_eq!(frame as usize, state.parent_frame);
                assert_eq!(segment, state.parent_segment);
            }
            state.parent_frame = frame as usize;
            state.parent_segment = segment;
            return true;
        }
        if state.parent_owner != 0 {
            assert_ne!(state.parent_frame, 0, "mapped parent poll must run first");
            assert_eq!(
                unsafe { (*frame).prev_fp } as usize,
                state.parent_frame,
                "retained child's callable must stay linked to its mapped parent"
            );
            if owner == state.first_owner {
                assert!(nested, "T1 child must enter through mapped call record");
                assert_eq!(segment, state.parent_segment);
                state.nested_seen = true;
            }
        } else {
            assert!(!nested);
        }
        false
    });
    if parent_poll {
        return;
    }
    let steps = super::super::super::cp(unsafe { slot_get(frame, 0) })
        .1
        .as_fixnum();
    let (publication, collect, registered) = CONDITION_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        assert!(owner == state.first_owner || owner == state.next_owner);
        if state.frame == 0 {
            state.frame = frame as usize;
        }
        assert_eq!(
            frame as usize, state.frame,
            "same callable frame across OSR"
        );
        if owner == state.first_owner {
            if steps == 4 && state.replacement.is_some() {
                let env = unsafe { &*env_ptr };
                state.tokens = handler_tokens(env, state.handler_base);
                state.restart_ids = env.restarts[state.restart_base..]
                    .iter()
                    .map(|entry| entry.id)
                    .collect();
                state.source_slots = unsafe { (*frame).num_locals };
                state.source_cluster_frames = condition_cluster_addresses(frame);
                assert_eq!(state.source_cluster_frames.len(), state.case.unwrap().clusters());
                native_transfer_entry::force_next_osr_frame_growth_for_test(8);
                state.steps_at_handoff = steps;
                let symbol = state.symbol;
                return (
                    state.replacement.take().map(|code| (symbol, code)),
                    false,
                    true,
                );
            }
            return (None, false, false);
        }
        state.entered = true;
        // Final polls can occur after normal scope retirement. The handoff
        // assertions apply while the loop is active, not after its SIGNAL.
        (None, !state.guard && !state.collected, steps < 40)
    });
    if registered {
        assert_condition_registrations(unsafe { &*env_ptr });
        let count = CONDITION_OBSERVATION.with(|state| state.borrow().case.unwrap().clusters());
        assert_condition_cluster_chain(frame, count);
        if CONDITION_OBSERVATION.with(|state| owner == state.borrow().next_owner) {
            assert_condition_suffix_moved();
        }
    }
    if let Some((symbol, code)) = publication {
        NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&code)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    }
    if collect {
        collect_condition_roots(unsafe { &mut *env_ptr }, Some(frame)).unwrap();
    }
}

pub(in crate::cli) fn ordinary_condition_osr_observe_for_test(
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    if CONDITION_OBSERVATION.with(|state| state.borrow().symbol == 0) {
        return Ok(NIL);
    }
    assert_condition_registrations(env);
    assert_condition_suffix_moved();
    let guard = CONDITION_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        state.finishes += 1;
        assert_eq!(state.finishes, 1, "loop finish must not replay");
        assert_eq!(state.steps_at_handoff, 4);
        if !state.guard {
            assert!(state.entered && state.collected);
        }
        state.guard
    });
    if guard {
        collect_condition_roots(env, None)?;
    }
    Ok(NIL)
}

struct ConditionFixtureScope(u32);
impl Drop for ConditionFixtureScope {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        CONDITION_OBSERVATION.with(|state| *state.borrow_mut() = ConditionObservation::default());
        profile_unpin(self.0);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_handler_bind_signal_and_guard_recovery() {
    for guard in [false, true] {
        exercise_condition_osr(ConditionCase::Bind, guard);
    }
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_handler_case_selection_and_guard_recovery() {
    for guard in [false, true] {
        exercise_condition_osr(ConditionCase::Case, guard);
    }
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_restart_case_selection_and_guard_recovery() {
    for guard in [false, true] {
        exercise_condition_osr(ConditionCase::Restart, guard);
    }
}
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_mixed_condition_scopes_and_guard_recovery() {
    for guard in [false, true] {
        exercise_condition_osr(ConditionCase::Mixed, guard);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_child_osr_mixed_condition_scopes_and_guard_recovery() {
    for guard in [false, true] {
        exercise_condition_osr_entry(ConditionCase::Mixed, guard, true);
    }
}

fn exercise_condition_osr(case: ConditionCase, guard: bool) {
    exercise_condition_osr_entry(case, guard, false);
}

fn exercise_condition_osr_entry(case: ConditionCase, guard: bool, nested_child: bool) {
    use egcl_compiler::control_scope::ScopeMap;
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = format!(
        "CONDITION-OSR-{}-{}{}",
        case.name(),
        if guard { "GUARD" } else { "NORMAL" },
        if nested_child { "-CHILD" } else { "" }
    );
    let initial = if guard { "sample" } else { "0" };
    let work = "(dotimes (i n) (setq sum (+ sum i)) (setf (cdr marker) (1+ (cdr marker))))
                (ordinary-condition-osr-observe)";
    let finish = match case {
        ConditionCase::Bind => format!("{work} (signal 'simple-condition) (setf (cdr hits) (1+ (cdr hits))) (values payload sum hits)"),
        ConditionCase::Case => format!("{work} (setf (cdr hits) (1+ (cdr hits))) (signal 'simple-condition) (values :wrong)"),
        ConditionCase::Restart | ConditionCase::Mixed => format!("{work} {} (setf (cdr hits) (1+ (cdr hits))) (invoke-restart 'condition-osr-done payload sum hits) (values :wrong)",
            if case == ConditionCase::Mixed { "(signal 'simple-condition)" } else { "nil" }),
    };
    let protected = match case {
        ConditionCase::Bind => format!("(handler-bind ((simple-condition (progn (setf (car setup) (1+ (car setup))) handler))) (setq handler nil) {finish})"),
        ConditionCase::Case => format!("(progn (setf (car setup) (1+ (car setup))) (handler-case (progn {finish}) (simple-condition (c) (declare (ignore c)) (setf (car hits) (1+ (car hits))) (values payload sum hits))))"),
        ConditionCase::Restart | ConditionCase::Mixed => {
            let restart = format!("(restart-case (progn {finish}) (condition-osr-done (answer total cell) (setf (car cell) (1+ (car cell))) (values answer total cell)))");
            if case == ConditionCase::Mixed {
                format!("(handler-bind ((simple-condition (progn (setf (car setup) (1+ (car setup))) handler))) (setq handler nil) {restart})")
            } else { format!("(progn (setf (car setup) (1+ (car setup))) {restart})") }
        }
    };
    let source =
        format!("((setf (car marker) (1+ (car marker))) (let ((sum {initial})) {protected}))");
    super::super::super::read_eval_all_env(
        &format!("(defun ordinary-condition-osr-observe () (%ordinary-condition-osr-observe-for-test))
            (defun condition-osr-make-handler (cell) (lambda (condition) (declare (ignore condition)) (setf (car cell) (1+ (car cell)))))
            (defun {name} (marker hits setup handler payload n sample) (progn . {source}))"), &mut env).unwrap();
    let observer = egcl_rt::symbols::intern("ORDINARY-CONDITION-OSR-OBSERVE");
    profile_pin(observer);
    let _fixture = ConditionFixtureScope(observer);
    egcl_rt::rooted!(
        params = reader::read_from_string("(marker hits setup handler payload n sample)")
            .unwrap()
            .0
    );
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let symbol = egcl_rt::symbols::intern(&name);
    let body = Arc::new(compile_function(&name, *params, *forms, &env, false, false).unwrap());
    let scopes = ScopeMap::analyze_function(&body).unwrap();
    let sum_sites: Vec<_> = body.code.windows(4).enumerate().filter_map(|(start, window)| {
        let [Instr::LoadLocal(sum), Instr::LoadLocal(index), Instr::CallNamed { sym, nargs: 2 }, Instr::StoreLocal(destination)] = window else { return None };
        let bcp = (start + 2) as u32;
        (sum == destination && sum != index && egcl_rt::symbols::symbol_name(*sym).as_deref() == Some("+")
            && scopes.before(bcp).is_some_and(|active| active.iter().any(|scope| case.matches_scope(scope)))).then_some(bcp)
    }).collect();
    let [sum_bcp] = sum_sites.as_slice() else {
        panic!("unique scoped sum site: {sum_sites:?}")
    };
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary mapped condition T1");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("mapped T1 required")
    };
    let input = snapshot_t2_input(symbol, 0).expect("real optimizing worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("worker must optimize local condition scope");
    assert!(
        artifact
            .speculations
            .iter()
            .any(|(bcp, kind)| bcp == sum_bcp && matches!(kind, SpecType::Fixnum)),
        "real scoped numeric speculation required"
    );
    let next = install_t2_completion(T2Completion {
                    panicked: false,
                    _permit: None, dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary mapped condition T2");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("mapped T2 required")
    };
    assert!(next.is_t2 && Arc::ptr_eq(next.body.as_ref().unwrap(), &body));
    assert!(
        next.osr_entries.keys().any(|bcp| scopes
            .before(*bcp)
            .is_some_and(|active| active.iter().any(|scope| case.matches_scope(scope)))),
        "OSR entry inside exact condition scope"
    );
    let next_owner = Rc::as_ptr(next_mapped) as usize;
    let next_code = Rc::clone(next_mapped);
    let before_deopts = next_code.deopt_count();
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&first)));
    mark_fresh_promotion(symbol);
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &first);
    let maker = egcl_rt::symbols::intern("CONDITION-OSR-MAKE-HANDLER");
    let parent = if nested_child {
        let parent_name = format!("{name}-PARENT");
        let parent_source = format!(
            "((dotimes (j 1) (setf (car held) (1+ (car held))))
              (multiple-value-bind (answer total cell)
                  ({name} marker hits setup handler payload n sample)
                (setf (car held) (+ (car held) 10))
                (values answer total cell)))"
        );
        super::super::super::read_eval_all_env(
            &format!("(defun {parent_name} (held marker hits setup handler payload n sample) (progn . {parent_source}))"), &mut env).unwrap();
        egcl_rt::rooted!(
            parent_params =
                reader::read_from_string("(held marker hits setup handler payload n sample)")
                    .unwrap()
                    .0
        );
        egcl_rt::rooted!(parent_forms = reader::read_from_string(&parent_source).unwrap().0);
        let parent_symbol = egcl_rt::symbols::intern(&parent_name);
        let parent_body = Arc::new(
            compile_function(
                &parent_name,
                *parent_params,
                *parent_forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        registry_put(parent_symbol, parent_body);
        let parent = try_promote_to_t1(parent_symbol).expect("ordinary mapped condition parent");
        assert!(!parent.is_t2 && matches!(parent._storage, NativeCodeStorage::Mapped(_)));
        egcl_rt::rooted!(warm_held = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
        egcl_rt::rooted!(
            warm_marker =
                super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
        );
        egcl_rt::rooted!(
            warm_hits =
                super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
        );
        egcl_rt::rooted!(
            warm_setup = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL)
        );
        egcl_rt::rooted!(
            warm_handler = super::super::super::apply_function(
                EgclVal::from_symbol_index(maker),
                &[*warm_hits],
                &mut env
            )
            .unwrap()
        );
        for _ in 0..2 {
            run_native(
                &parent,
                parent_symbol,
                &[
                    *warm_held,
                    *warm_marker,
                    *warm_hits,
                    *warm_setup,
                    *warm_handler,
                    NIL,
                    EgclVal::from_fixnum(0),
                    EgclVal::from_fixnum(0),
                ],
                &mut env,
            )
            .unwrap();
        }
        Some((parent_symbol, parent))
    } else {
        None
    };
    let parent_owner = parent.as_ref().map_or(0, |(_, parent)| {
        let NativeCodeStorage::Mapped(code) = &parent._storage else {
            unreachable!()
        };
        Rc::as_ptr(code) as usize
    });
    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(
        hits = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(setup = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    egcl_rt::rooted!(payload = super::super::super::arena_cons(EgclVal::from_fixnum(73), NIL));
    egcl_rt::rooted!(
        handler = super::super::super::apply_function(
            EgclVal::from_symbol_index(maker),
            &[*hits],
            &mut env
        )
        .unwrap()
    );
    egcl_rt::rooted!(
        held = if nested_child {
            super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL)
        } else {
            NIL
        }
    );
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp());
    let before_scopes = (
        env.handlers.len(),
        env.restarts.len(),
        env.catch_stack.len(),
    );
    let before_depth = NATIVE_DEPTH.with(Cell::get);
    let before_osr = osr_entry_count(symbol);
    CONDITION_OBSERVATION.with(|state| {
        *state.borrow_mut() = ConditionObservation {
            symbol,
            parent_owner,
            parent_root: nested_child.then(|| std::ptr::from_ref(&*held)),
            case: Some(case),
            replacement: Some(next),
            first_owner: Rc::as_ptr(first_mapped) as usize,
            next_owner,
            handler_base: before_scopes.0,
            restart_base: before_scopes.1,
            roots: Some([
                std::ptr::from_ref(&*marker),
                std::ptr::from_ref(&*hits),
                std::ptr::from_ref(&*setup),
                std::ptr::from_ref(&*payload),
            ]),
            handler_root: Some(std::ptr::from_ref(&*handler)),
            guard,
            ..ConditionObservation::default()
        }
    });
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(observe_condition_osr_poll)));
    egcl_rt::rooted!(
        result = if let Some((parent_symbol, parent)) = &parent {
            run_native(
                parent,
                *parent_symbol,
                &[
                    *held,
                    *marker,
                    *hits,
                    *setup,
                    *handler,
                    *payload,
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                ],
                &mut env,
            )
        } else {
            run_native(
                &first,
                symbol,
                &[
                    *marker,
                    *hits,
                    *setup,
                    *handler,
                    *payload,
                    EgclVal::from_fixnum(40),
                    EgclVal::from_single_float(1.25),
                ],
                &mut env,
            )
        }
        .unwrap()
    );
    assert_eq!(*result, *payload);
    assert!(env.mv_active);
    assert_eq!(
        env.mv.as_slice(),
        &[
            *payload,
            if guard {
                EgclVal::from_single_float(781.25)
            } else {
                EgclVal::from_fixnum(780)
            },
            *hits
        ]
    );
    assert_eq!(
        super::super::super::cp(*marker),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40))
    );
    assert_eq!(
        super::super::super::cp(*setup),
        (EgclVal::from_fixnum(1), NIL),
        "registration initializer must not replay"
    );
    assert_eq!(
        super::super::super::cp(*hits),
        (
            EgclVal::from_fixnum(if case == ConditionCase::Mixed { 2 } else { 1 }),
            EgclVal::from_fixnum(1)
        ),
        "selected handler/restart and continuation each execute once"
    );
    assert_eq!(
        super::super::super::cp(*payload),
        (EgclVal::from_fixnum(73), NIL)
    );
    if nested_child {
        assert_eq!(
            super::super::super::cp(*held),
            (EgclVal::from_fixnum(11), NIL),
            "parent resumes once and uses moved root after selected restart"
        );
    }
    assert_eq!(osr_entry_count(symbol), before_osr + 1);
    assert_eq!(next_code.deopt_count(), before_deopts + u32::from(guard));
    CONDITION_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert_eq!(state.steps_at_handoff, 4);
        assert_eq!(state.finishes, 1);
        assert!(state.replacement.is_none() && state.collected);
        assert!(state.cluster_suffix_moved);
        if nested_child {
            assert!(state.nested_seen && state.parent_moved);
            assert_ne!(state.parent_frame, 0);
            assert_ne!(state.parent_segment, 0);
            assert_ne!(state.parent_frame, state.frame);
        }
        if !guard {
            assert!(state.entered);
        }
        if case == ConditionCase::Case {
            assert_eq!(state.tokens.len(), 1);
        }
        if case.restarts() {
            assert_eq!(state.restart_ids.len(), 1);
        }
    });
    assert_eq!((stack.fp(), stack.sp()), before_stack);
    assert_eq!(
        (
            env.handlers.len(),
            env.restarts.len(),
            env.catch_stack.len()
        ),
        before_scopes
    );
    assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
}
