// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

#[derive(Default)]
struct SpecialObservation {
    symbol: u32,
    special: u32,
    replacement: Option<Rc<NativeCode>>,
    first_owner: usize,
    next_owner: usize,
    frame: usize,
    // Pointers into rooted fixture locals, not copies of movable values.
    roots: Option<[*const EgclVal; 8]>, // marker, setup, seen, cleanup, tag, current, previous, intermediate
    guard: bool,
    entered: bool,
    handoff_steps: i64,
    finishes: usize,
    collected: bool,
}
thread_local! {
    static SPECIAL_OBSERVATION: RefCell<SpecialObservation> = RefCell::new(SpecialObservation::default());
}

fn assert_special_binding() {
    SPECIAL_OBSERVATION.with(|state| {
        let state = state.borrow();
        let roots = state.roots.unwrap();
        assert_eq!(
            egcl_rt::symbols::symbol_value(state.special),
            Some(unsafe { *roots[5] }),
            "the original dynamic binding must remain installed"
        );
    });
}

fn collect_special_roots(native: Option<*mut Frame>) -> Result<(), EgclError> {
    let before = SPECIAL_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert!(!state.collected);
        state.roots.unwrap().map(|root| unsafe { (*root).to_raw() })
    });
    egcl_rt::collect_t0_minor()?;
    assert_special_binding();
    SPECIAL_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        let roots = state.roots.unwrap();
        for (index, root) in roots.into_iter().enumerate() {
            assert_ne!(
                unsafe { (*root).to_raw() },
                before[index],
                "root {index} must actually relocate; includes current and saved previous bindings"
            );
        }
        if let Some(frame) = native {
            for (slot, root) in roots[..5].iter().enumerate() {
                assert_eq!(unsafe { slot_get(frame, slot as u16) }, unsafe { **root });
            }
            assert_eq!(
                unsafe { slot_get(frame, 5) },
                NIL,
                "the bound value's original parameter was discarded before the loop"
            );
            assert_eq!(
                unsafe { slot_get(frame, 8) },
                NIL,
                "the intermediate binding's source parameter was also discarded"
            );
        }
        state.collected = true;
    });
    Ok(())
}

fn observe_special_osr_poll() {
    let Some((owner, nested, frame)) = native_transfer_entry::condition_capture_for_test() else {
        return;
    };
    assert!(!nested);
    assert!(!frame.is_null());
    let steps = super::super::super::cp(unsafe { slot_get(frame, 0) })
        .1
        .as_fixnum();
    let (publication, collect) = SPECIAL_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        assert!(owner == state.first_owner || owner == state.next_owner);
        if state.frame == 0 {
            state.frame = frame as usize;
        }
        assert_eq!(
            state.frame, frame as usize,
            "OSR retains the callable frame"
        );
        if owner == state.first_owner {
            if steps == 4 && state.replacement.is_some() {
                state.handoff_steps = steps;
                let symbol = state.symbol;
                return (state.replacement.take().map(|next| (symbol, next)), false);
            }
            return (None, false);
        }
        state.entered = true;
        (None, !state.guard && !state.collected)
    });
    if publication.is_some() || collect {
        assert_special_binding();
    }
    if let Some((symbol, code)) = publication {
        NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&code)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    }
    if collect {
        collect_special_roots(Some(frame)).unwrap();
    }
}

pub(in crate::cli) fn ordinary_special_osr_observe_for_test(
    _env: &mut Env,
) -> Result<EgclVal, EgclError> {
    if SPECIAL_OBSERVATION.with(|state| state.borrow().symbol == 0) {
        return Ok(NIL);
    }
    assert_special_binding();
    let guard = SPECIAL_OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        state.finishes += 1;
        assert_eq!(
            state.finishes, 1,
            "guard recovery must not replay the observer"
        );
        assert_eq!(state.handoff_steps, 4);
        if !state.guard {
            assert!(state.entered && state.collected);
        }
        state.guard
    });
    if guard {
        // A failing OSR entry guard may return to T0 before the first T2 poll.
        // Exact OSR/deopt counters below prove the attempt independently.
        collect_special_roots(None)?;
    }
    Ok(NIL)
}

struct SpecialFixtureScope(u32);
impl Drop for SpecialFixtureScope {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        SPECIAL_OBSERVATION.with(|state| *state.borrow_mut() = SpecialObservation::default());
        profile_unpin(self.0);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_special_binding_moves_and_unwinds_once() {
    exercise_special_osr(false, true, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_special_binding_guard_restores_moved_previous_value() {
    exercise_special_osr(true, true, false);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_special_binding_returns_normally_and_recovers_guards() {
    for guard in [false, true] {
        exercise_special_osr(guard, false, false);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_mapped_osr_repeated_special_binding_restores_in_lifo_order() {
    for guard in [false, true] {
        exercise_special_osr(guard, true, true);
    }
}

fn exercise_special_osr(guard: bool, throws: bool, repeated: bool) {
    use egcl_compiler::control_scope::{Ownership, ScopeKind, ScopeMap};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = format!(
        "SPECIAL-OSR-{}-{}-{}",
        if guard { "GUARD" } else { "NORMAL" },
        if throws { "THROW" } else { "RETURN" },
        if repeated { "REPEATED" } else { "SINGLE" }
    );
    let initial = if guard { "sample" } else { "0" };
    let result = "(values *special-osr-value* sum marker)";
    let finish = if throws {
        format!("(throw tag {result})")
    } else {
        result.to_owned()
    };
    let mut bound = format!(
        "(let ((*special-osr-value*
                 (progn (setf (car setup) (1+ (car setup))) current)))
           (setq current nil)
           (setq intermediate nil)
           (unwind-protect
               (let ((sum {initial}))
                 (dotimes (i n)
                   (setq sum (+ sum i))
                   (setf (cdr marker) (1+ (cdr marker))))
                 (ordinary-special-osr-observe)
                 {finish})
             (setf (car seen) *special-osr-value*)
             (setf (car cleanup) (1+ (car cleanup)))))"
    );
    if repeated {
        bound = format!(
            "(let ((*special-osr-value*
                                (progn (setf (car setup) (1+ (car setup))) intermediate)))
                           (unwind-protect {bound}
                             (setf (cdr setup) *special-osr-value*)))"
        );
    }
    let source = format!(
        "((setf (car marker) (1+ (car marker)))
          (catch tag
            (unwind-protect {bound}
              (setf (cdr seen) *special-osr-value*)
              (setf (cdr cleanup) (1+ (cdr cleanup))))))"
    );
    super::super::super::read_eval_all_env(
        &format!(
            "(defun ordinary-special-osr-observe () (%ordinary-special-osr-observe-for-test))
          (defun {name} (marker setup seen cleanup tag current n sample intermediate) (progn . {source}))"
        ),
        &mut env,
    )
    .unwrap();
    let observer = egcl_rt::symbols::intern("ORDINARY-SPECIAL-OSR-OBSERVE");
    profile_pin(observer);
    let _fixture = SpecialFixtureScope(observer);
    let special = egcl_rt::symbols::intern("*SPECIAL-OSR-VALUE*");
    egcl_rt::rooted!(
        params = reader::read_from_string(
            "(marker setup seen cleanup tag current n sample intermediate)"
        )
        .unwrap()
        .0
    );
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let symbol = egcl_rt::symbols::intern(&name);
    let body = Arc::new(compile_function(&name, *params, *forms, &env, false, false).unwrap());
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::BindSpecial(sym) if *sym == special)),
        "source LET must establish a real dynamic binding"
    );
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::LoadGlobal(sym) if *sym == special)),
        "exercise direct special access, not just a callback's lookup"
    );
    assert_eq!(
        body.code
            .iter()
            .filter(|op| matches!(op, Instr::BindSpecial(sym) if *sym == special))
            .count(),
        if repeated { 2 } else { 1 },
        "exercise the intended number of same-symbol bindings"
    );
    let scopes = ScopeMap::analyze_function(&body).unwrap();
    let in_binding = |bcp| {
        scopes.before(bcp).is_some_and(|active| active.iter().any(|scope|
        scope.ownership == Ownership::Local && matches!(scope.kind, ScopeKind::SpecialBinding { symbol } if symbol == special)))
    };
    let sums: Vec<_> = body
        .code
        .windows(4)
        .enumerate()
        .filter_map(|(start, window)| {
            let [
                Instr::LoadLocal(sum),
                Instr::LoadLocal(index),
                Instr::CallNamed { sym, nargs: 2 },
                Instr::StoreLocal(destination),
            ] = window
            else {
                return None;
            };
            let bcp = (start + 2) as u32;
            (sum == destination
                && sum != index
                && egcl_rt::symbols::symbol_name(*sym).as_deref() == Some("+")
                && in_binding(bcp))
            .then_some(bcp)
        })
        .collect();
    let [sum_bcp] = sums.as_slice() else {
        panic!("unique dynamically scoped sum: {sums:?}")
    };
    registry_put(symbol, Arc::clone(&body));
    let first = try_promote_to_t1(symbol).expect("ordinary mapped special-binding T1");
    let NativeCodeStorage::Mapped(first_mapped) = &first._storage else {
        panic!("mapped T1 required")
    };
    let input = snapshot_t2_input(symbol, 0).expect("real optimizing worker snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("worker must optimize special scope");
    assert!(
        artifact
            .speculations
            .iter()
            .any(|(bcp, kind)| bcp == sum_bcp && matches!(kind, SpecType::Fixnum)),
        "real dynamically scoped numeric speculation required"
    );
    let next = install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("ordinary mapped special-binding T2");
    let NativeCodeStorage::Mapped(next_mapped) = &next._storage else {
        panic!("mapped T2 required")
    };
    assert!(next.is_t2 && Arc::ptr_eq(next.body.as_ref().unwrap(), &body));
    assert!(
        next.osr_entries.keys().any(|bcp| in_binding(*bcp)),
        "OSR entry must be inside binding"
    );
    let next_code = Rc::clone(next_mapped);
    let next_owner = Rc::as_ptr(next_mapped) as usize;
    let before_deopts = next_code.deopt_count();
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().insert(symbol, Rc::clone(&first)));
    mark_fresh_promotion(symbol);
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &first);

    egcl_rt::collect_t0_minor().unwrap();
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(setup = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    egcl_rt::rooted!(seen = super::super::super::arena_cons(NIL, NIL));
    egcl_rt::rooted!(
        cleanup = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(tag = super::super::super::arena_cons(EgclVal::from_fixnum(79), NIL));
    egcl_rt::rooted!(current = super::super::super::arena_cons(EgclVal::from_fixnum(73), NIL));
    egcl_rt::rooted!(previous = super::super::super::arena_cons(EgclVal::from_fixnum(89), NIL));
    egcl_rt::rooted!(intermediate = super::super::super::arena_cons(EgclVal::from_fixnum(97), NIL));
    egcl_rt::rooted!(
        _previous_binding = DynBind::establish(EgclVal::from_symbol_index(special), *previous)
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
    SPECIAL_OBSERVATION.with(|state| {
        *state.borrow_mut() = SpecialObservation {
            symbol,
            special,
            replacement: Some(next),
            first_owner: Rc::as_ptr(first_mapped) as usize,
            next_owner,
            roots: Some([
                std::ptr::from_ref(&*marker),
                std::ptr::from_ref(&*setup),
                std::ptr::from_ref(&*seen),
                std::ptr::from_ref(&*cleanup),
                std::ptr::from_ref(&*tag),
                std::ptr::from_ref(&*current),
                std::ptr::from_ref(&*previous),
                std::ptr::from_ref(&*intermediate),
            ]),
            guard,
            ..SpecialObservation::default()
        };
    });
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(observe_special_osr_poll)));
    egcl_rt::rooted!(
        result = run_native(
            &first,
            symbol,
            &[
                *marker,
                *setup,
                *seen,
                *cleanup,
                *tag,
                *current,
                EgclVal::from_fixnum(40),
                EgclVal::from_single_float(1.25),
                *intermediate,
            ],
            &mut env
        )
        .unwrap()
    );
    assert_eq!(*result, *current);
    assert!(env.mv_active);
    assert_eq!(
        env.mv.as_slice(),
        &[
            *current,
            if guard {
                EgclVal::from_single_float(781.25)
            } else {
                EgclVal::from_fixnum(780)
            },
            *marker
        ]
    );
    assert_eq!(
        super::super::super::cp(*marker),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(40)),
        "committed effects must not replay"
    );
    assert_eq!(
        super::super::super::cp(*setup),
        (
            EgclVal::from_fixnum(if repeated { 2 } else { 1 }),
            if repeated { *intermediate } else { NIL },
        ),
        "initializers execute once; intermediate cleanup observes the relocated inner saved binding"
    );
    assert_eq!(
        super::super::super::cp(*seen),
        (*current, *previous),
        "inner cleanup sees current binding; outer cleanup sees restored previous binding"
    );
    assert_eq!(
        super::super::super::cp(*cleanup),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(1)),
        "both cleanups execute exactly once"
    );
    assert_eq!(
        egcl_rt::symbols::symbol_value(special),
        Some(*previous),
        "restore the relocated original binding"
    );
    assert_eq!(
        super::super::super::cp(*current),
        (EgclVal::from_fixnum(73), NIL)
    );
    assert_eq!(
        super::super::super::cp(*previous),
        (EgclVal::from_fixnum(89), NIL)
    );
    assert_eq!(osr_entry_count(symbol), before_osr + 1);
    assert_eq!(next_code.deopt_count(), before_deopts + u32::from(guard));
    SPECIAL_OBSERVATION.with(|state| {
        let state = state.borrow();
        assert_eq!(state.handoff_steps, 4);
        assert_eq!(state.finishes, 1);
        assert!(state.replacement.is_none() && state.collected);
        if !guard {
            assert!(state.entered);
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
    // The T1 code was the OSR source: its ledgers moved to the T2 activation.
    // Entering it again must not reuse that drained state (bliss-t9tzf review).
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    SPECIAL_OBSERVATION.with(|state| *state.borrow_mut() = SpecialObservation::default());
    egcl_rt::rooted!(
        marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(setup = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    egcl_rt::rooted!(seen = super::super::super::arena_cons(NIL, NIL));
    egcl_rt::rooted!(
        cleanup = super::super::super::arena_cons(EgclVal::from_fixnum(0), EgclVal::from_fixnum(0))
    );
    let again = run_native(
        &first,
        symbol,
        &[
            *marker,
            *setup,
            *seen,
            *cleanup,
            *tag,
            *current,
            EgclVal::from_fixnum(3),
            EgclVal::from_single_float(1.25),
            *intermediate,
        ],
        &mut env,
    );
    assert!(
        again.is_ok(),
        "re-entering T1 after it was an OSR source: {:?}",
        again.err()
    );
    assert_eq!(
        super::super::super::cp(*cleanup),
        (EgclVal::from_fixnum(1), EgclVal::from_fixnum(1)),
        "re-entered cleanups execute exactly once"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_global_access_honors_the_same_lexical_env_as_t0() {
    use super::super::native_transfer_entry::{TransferCode, take_native_fallback_count};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = "NATIVE-GLOBAL-ENV-SHADOW";
    let index = egcl_rt::symbols::intern(name);
    let symbol = EgclVal::from_symbol_index(index);
    egcl_rt::rooted!(
        original = egcl_rt::symbols::symbol_value(index).unwrap_or(egcl_rt::value::UNBOUND)
    );
    egcl_rt::symbols::set_symbol_value(index, EgclVal::from_fixnum(1));
    env.define_local_symbol(symbol, EgclVal::from_fixnum(41));
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((values native-global-env-shadow
                   (progn (setq native-global-env-shadow 42)
                          native-global-env-shadow)))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("NATIVE-GLOBAL-ENV-ACCESS", NIL, *forms, &env, false, false).unwrap(),
    );
    assert!(!body.has_env);
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::LoadGlobal(sym) if *sym == index))
    );
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::StoreGlobal(sym) if *sym == index))
    );
    let native = TransferCode::compile(Arc::clone(&body))
        .expect("direct global accesses must compile to published mapped helpers");
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp());
    let before_depth = NATIVE_DEPTH.with(Cell::get);
    let expected_values = [EgclVal::from_fixnum(41), EgclVal::from_fixnum(42)];
    for mapped in [false, true] {
        // Same input state and exact bytecode for both execution engines.
        env.define_local_symbol(symbol, EgclVal::from_fixnum(41));
        egcl_rt::symbols::set_symbol_value(index, EgclVal::from_fixnum(1));
        take_native_fallback_count();
        let result = if mapped {
            native.run(&[], &mut env)
        } else {
            run(Arc::clone(&body), &[], NIL, &mut env)
        }
        .unwrap();
        assert_eq!(
            result, expected_values[0],
            "mapped={mapped}: read lexical X=41, not cell X=1"
        );
        assert!(env.mv_active);
        assert_eq!(
            env.mv.as_slice(),
            expected_values,
            "mapped={mapped}: preserve read-before-store order"
        );
        assert_eq!(
            env.lookup_var_symbol(symbol),
            Some(expected_values[1]),
            "mapped={mapped}: update the lexical binding"
        );
        assert_eq!(
            egcl_rt::symbols::symbol_value(index),
            Some(EgclVal::from_fixnum(1)),
            "mapped={mapped}: leave the shadowed symbol value cell unchanged"
        );
        assert_eq!(
            take_native_fallback_count(),
            0,
            "mapped={mapped}: fallback must not supply correct semantics"
        );
        assert_eq!((stack.fp(), stack.sp()), before_stack);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
    }
    egcl_rt::symbols::set_symbol_value(index, *original);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_global_failures_keep_conditions_and_restore_specials_before_caller_cleanup() {
    use super::super::native_transfer_entry::take_native_fallback_count;
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    for store in [false, true] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let suffix = if store { "STORE" } else { "LOAD" };
        let name = format!("NATIVE-SPECIAL-FAILED-{suffix}");
        let cell_name = format!("NATIVE-SPECIAL-FAILED-{suffix}-CELL");
        let cell = egcl_rt::symbols::intern(&cell_name);
        let special = egcl_rt::symbols::intern("*NATIVE-SPECIAL-FAILURE-VALUE*");
        egcl_rt::symbols::set_symbol_value(cell, egcl_rt::value::UNBOUND);
        let failing = if store {
            format!("(setq {cell_name} 42)")
        } else {
            cell_name.clone()
        };
        let source = format!(
            "((let ((*native-special-failure-value* current))
                 (setf (car marker) (1+ (car marker)))
                 {failing}))"
        );
        super::super::super::read_eval_all_env(
            &format!("(defun {name} (marker current) (progn . {source}))"),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(marker current)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
        let symbol = egcl_rt::symbols::intern(&name);
        let body = Arc::new(compile_function(&name, *params, *forms, &env, false, false).unwrap());
        assert!(
            body.code
                .iter()
                .any(|op| matches!(op, Instr::BindSpecial(sym) if *sym == special))
        );
        assert!(body.code.iter().any(|op| if store {
            matches!(op, Instr::StoreGlobal(sym) if *sym == cell)
        } else {
            matches!(op, Instr::LoadGlobal(sym) if *sym == cell)
        }));
        assert!(
            !body
                .code
                .iter()
                .any(|op| matches!(op, Instr::PushUnwind { .. } | Instr::PushHandlerCase { .. })),
            "native callee has no local cleanup/handler landing to bypass full-frame fallback"
        );
        registry_put(symbol, Arc::clone(&body));
        let native = try_promote_to_t1(symbol).expect("mapped special/global helper body");
        assert!(matches!(native._storage, NativeCodeStorage::Mapped(_)));
        publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &native);
        // Compile the assignment while legal, then make it fail at execution.
        if store {
            egcl_rt::symbols::define_symbol_constant(cell, EgclVal::from_fixnum(7));
        }
        let caller = format!(
            "((handler-case
                 (unwind-protect ({name} marker current)
                   (setf (cdr marker) *native-special-failure-value*))
                (error (condition) (values condition *native-special-failure-value*))))"
        );
        egcl_rt::rooted!(caller_forms = reader::read_from_string(&caller).unwrap().0);
        let caller_body = Arc::new(
            compile_function(
                "INTERPRETED-SPECIAL-FAILURE-CALLER",
                *params,
                *caller_forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
        egcl_rt::rooted!(
            _previous_binding = DynBind::establish(
                EgclVal::from_symbol_index(special),
                EgclVal::from_fixnum(13),
            )
        );
        let stack = egcl_rt::current_stack();
        let before_stack = (stack.fp(), stack.sp());
        let before_depth = NATIVE_DEPTH.with(Cell::get);
        let before_handlers = env.handlers.len();
        take_native_fallback_count();
        // The outer handler and cleanup execute in T0. Only the callee has an
        // installed native body, and the exact fallback counter proves entry.
        egcl_rt::rooted!(
            condition = run(
                caller_body,
                &[*marker, EgclVal::from_fixnum(29)],
                NIL,
                &mut env
            )
            .unwrap_or_else(|error| panic!(
                "original {suffix} condition must reach caller handler: {error:?}"
            ))
        );
        let expected = if store {
            "PROGRAM-ERROR"
        } else {
            "UNBOUND-VARIABLE"
        };
        assert!(
            super::super::super::condition_matches_handler(&env, *condition, expected),
            "preserve original {expected}, not invalid_capture"
        );
        assert!(env.mv_active);
        assert_eq!(env.mv.as_slice(), &[*condition, EgclVal::from_fixnum(13)]);
        assert_eq!(
            super::super::super::cp(*marker),
            (EgclVal::from_fixnum(1), EgclVal::from_fixnum(13)),
            "callee effects occur once; outer cleanup sees restored special"
        );
        assert_eq!(
            egcl_rt::symbols::symbol_value(special),
            Some(EgclVal::from_fixnum(13))
        );
        assert_eq!(
            egcl_rt::symbols::symbol_value(cell),
            Some(if store {
                EgclVal::from_fixnum(7)
            } else {
                egcl_rt::value::UNBOUND
            }),
            "failed helper must leave the value cell unchanged"
        );
        assert_eq!(
            take_native_fallback_count(),
            1,
            "exercise exactly one native full-frame fallback"
        );
        assert_eq!((stack.fp(), stack.sp()), before_stack);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), before_depth);
        assert_eq!(env.handlers.len(), before_handlers);
    }
}
