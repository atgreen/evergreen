// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::{Cell, RefCell};

thread_local! {
    static ENTRY_OBSERVER: Cell<Option<fn()>> = const { Cell::new(None) };
    static EXPECTED_OWNER: Cell<usize> = const { Cell::new(0) };
    static OBSERVED_ENTRY: Cell<bool> = const { Cell::new(false) };
}

fn observed_native_capture() -> Option<(usize, bool)> {
    native_transfer_entry::child_capture_for_test().or_else(|| {
        native_callable::outer::suspended_callers_for_test()?.last()?.1
    })
}

pub(crate) fn recursive_heat_observe_for_test() -> EgclVal {
    let capture = observed_native_capture();
    if capture.is_some_and(|(owner, _)| owner == EXPECTED_OWNER.with(Cell::get)) {
        OBSERVED_ENTRY.with(|slot| slot.set(true));
    }
    if let Some(observe) = ENTRY_OBSERVER.with(Cell::get) { observe(); }
    NIL
}

#[derive(Default)]
struct Observation {
    owner: usize,
    symbol: u32,
    marker: Option<*mut EgclVal>,
    moved: bool,
    collection_depth: u32,
    minimum_depth: u32,
    queued_while_live: bool,
}

thread_local! {
    static OBSERVATION: RefCell<Observation> = RefCell::new(Observation::default());
}

fn observe_recursive_heat() {
    let Some((owner, _)) = observed_native_capture() else {
        return;
    };
    let collect = OBSERVATION.with(|state| {
        let mut state = state.borrow_mut();
        let function = egcl_rt::symbols::symbol_function(state.symbol).unwrap();
        if T2_QUEUED.with(|queued| queued.borrow().contains_key(&state.symbol))
            || egcl_rt::function::tier(function) == 2
        {
            state.queued_while_live = true;
        }
        if state.owner != owner { return None; }
        let depth = NATIVE_DEPTH.with(Cell::get);
        if !state.moved && depth >= state.minimum_depth {
            state.collection_depth = depth;
            Some(state.marker.unwrap())
        } else {
            None
        }
    });
    if let Some(marker) = collect {
        let before = unsafe { *marker }.to_raw();
        egcl_rt::collect_t0_minor().unwrap();
        assert_ne!(
            unsafe { *marker }.to_raw(),
            before,
            "marker must move while the root and at least two recursive frames are live"
        );
        OBSERVATION.with(|state| state.borrow_mut().moved = true);
    }
}

struct ObservationGuard;
impl Drop for ObservationGuard {
    fn drop(&mut self) {
        ENTRY_OBSERVER.with(|hook| hook.set(None));
        OBSERVATION.with(|state| *state.borrow_mut() = Observation::default());
    }
}

fn install_fib(name: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    let source = format!(
        "(progn (recursive-heat-observer) (if (< n 2) (+ n (car marker)) (+ ({name} (- n 1) marker) ({name} (- n 2) marker))))"
    );
    // The magic observer is an evaluator fixture. Its ordinary named adapter
    // interprets this tiny wrapper while the recursive body remains native.
    super::super::super::read_eval_all_env(&format!(
        "(defun recursive-heat-observer () (%recursive-heat-observe-for-test))
         (defun {name} (n marker) {source})"), env).unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n marker)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&format!("({source})")).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let code = native_transfer_entry::install_t1(symbol, body, true).expect("ordinary mapped T1");
    publish_native(symbol, egcl_rt::symbols::symbol_function(symbol), &code);
    assert!(!code.is_t2);
    assert!(matches!(&code._storage, NativeCodeStorage::Mapped(_)));
    let NativeCodeStorage::Mapped(mapped) = &code._storage else { unreachable!() };
    assert!(mapped.ordinary_entry.is_some());
    EXPECTED_OWNER.with(|slot| slot.set(Rc::as_ptr(mapped) as usize));
    OBSERVED_ENTRY.with(|slot| slot.set(false));
    (symbol, code)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_recursive_heat_counts_one_fib_and_queues_while_live() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install_fib("RECURSIVE-HEAT-FIB", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        unreachable!()
    };
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let before_count = egcl_rt::function::invoke_count(*function);
    assert!(
        t2_invoke_threshold() < 21891,
        "run with ordinary invocation threshold"
    );
    egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    OBSERVATION.with(|state| {
        *state.borrow_mut() = Observation {
            owner: Rc::as_ptr(mapped) as usize,
            symbol,
            marker: Some(&mut *marker),
            // Root + two recursive children + the observer adapter.
            minimum_depth: NATIVE_DEPTH.with(Cell::get) + 4,
            ..Observation::default()
        }
    });
    let _observation = ObservationGuard;
    ENTRY_OBSERVER
        .with(|hook| hook.set(Some(observe_recursive_heat)));
    native_transfer_entry::take_recursive_entries();
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    assert_eq!(
        super::super::super::apply_function(
            *function,
            &[EgclVal::from_fixnum(20), *marker],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(6765)
    );
    assert_eq!(
        native_transfer_entry::take_recursive_entries(),
        0,
        "ordinary recursion never allocates legacy recursive activations"
    );
    let counted = egcl_rt::function::invoke_count(*function) - before_count;
    assert!(counted >= t2_invoke_threshold().saturating_sub(before_count));
    assert!(counted <= 21891, "no logical invocation may count twice");
    if egcl_rt::function::tier(*function) < 2 {
        assert_eq!(counted, 21891, "all T1 invocations count, including the root");
    }
    assert!(OBSERVED_ENTRY.with(Cell::get), "native bodies really execute");
    OBSERVATION.with(|state| {
        let state = state.borrow();
        assert!(state.moved);
        assert!(state.collection_depth >= state.minimum_depth);
        assert!(
            state.queued_while_live,
            "T2 must be requested before this activation returns, not by a later tier query"
        );
    });
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before_stack
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_recursive_heat_does_not_double_count_depth_fallback() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, _code) = install_fib("RECURSIVE-HEAT-CAPPED", &mut env);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let before_count = egcl_rt::function::invoke_count(*function);
    egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    let initial = native_depth_cap().saturating_sub(2);
    struct RestoreDepth(u32);
    impl Drop for RestoreDepth {
        fn drop(&mut self) {
            NATIVE_DEPTH.with(|depth| depth.set(self.0));
        }
    }
    let _depth = RestoreDepth(NATIVE_DEPTH.with(|depth| depth.replace(initial)));
    native_transfer_entry::take_recursive_entries();
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp());
    assert_eq!(
        super::super::super::apply_function(
            *function,
            &[EgclVal::from_fixnum(8), *marker],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(21)
    );
    assert_eq!(
        native_transfer_entry::take_recursive_entries(),
        0,
        "depth fallback never uses the legacy recursive adapter"
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*function) - before_count,
        67,
        "fallback owns its own invocation count"
    );
    assert_eq!(NATIVE_DEPTH.with(Cell::get), initial);
    assert_eq!((stack.fp(), stack.sp()), before_stack);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_recursive_heat_obeys_profiling_policy() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, code) = install_fib("RECURSIVE-HEAT-PROFILE", &mut env);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let before_count = egcl_rt::function::invoke_count(*function);
    native_transfer_entry::take_recursive_entries();
    egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
    let stack = egcl_rt::current_stack();
    let before_stack = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    // This direct harness entry bypasses ordinary root-call accounting, even
    // with profiling disabled. Its 66 actual recursive admissions still run.
    assert_eq!(
        run_native_callable(
            &code,
            symbol,
            *function,
            &[EgclVal::from_fixnum(8), *marker],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(21)
    );
    assert_eq!(native_transfer_entry::take_recursive_entries(), 0);
    assert!(OBSERVED_ENTRY.with(Cell::get));
    assert_eq!(
        egcl_rt::function::invoke_count(*function) - before_count,
        if profiling_disabled() { 0 } else { 66 }
    );
    if profiling_disabled() {
        assert!(!T2_QUEUED.with(|queued| queued.borrow().contains_key(&symbol)));
        assert_ne!(egcl_rt::function::tier(*function), 2);
    }
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before_stack
    );
}

#[derive(Default)]
struct Mutation {
    owner: usize,
    symbol: u32,
    original: Option<*mut EgclVal>,
    replacement: Option<*mut EgclVal>,
    mutate_at_count: u32,
    done: bool,
}
thread_local! {
    static MUTATION: RefCell<Mutation> = RefCell::new(Mutation::default());
}
fn replace_during_recursion() {
    let Some((owner, _)) = observed_native_capture() else {
        return;
    };
    let action = MUTATION.with(|state| {
        let mut state = state.borrow_mut();
        if state.owner != owner
            || state.done
            || egcl_rt::function::invoke_count(unsafe { *state.original.unwrap() })
                < state.mutate_at_count
        {
            return None;
        }
        state.done = true;
        Some((
            state.symbol,
            state
                .replacement
                .map_or(egcl_rt::value::UNBOUND, |slot| unsafe { *slot }),
        ))
    });
    if let Some((symbol, replacement)) = action {
        // These are the ordinary function-cell and compilation invalidations,
        // performed while the old exact callable/code are still executing.
        egcl_rt::symbols::set_symbol_function(symbol, replacement);
        registry_remove(symbol);
        egcl_rt::collect_t0_minor().unwrap();
    }
}
struct MutationGuard;
impl Drop for MutationGuard {
    fn drop(&mut self) {
        ENTRY_OBSERVER.with(|hook| hook.set(None));
        MUTATION.with(|state| *state.borrow_mut() = Mutation::default());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_recursive_heat_attributes_replacement_and_unbound_fallback() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for unbind in [false, true] {
        let name = if unbind {
            "RECURSIVE-HEAT-UNBOUND"
        } else {
            "RECURSIVE-HEAT-REPLACED"
        };
        let (symbol, code) = install_fib(name, &mut env);
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            unreachable!()
        };
        egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
        super::super::super::read_eval_all_env(
            "(defun recursive-heat-replacement (n marker) 7)",
            &mut env,
        )
        .unwrap();
        let replacement_symbol = egcl_rt::symbols::intern("RECURSIVE-HEAT-REPLACEMENT");
        egcl_rt::rooted!(
            replacement = egcl_rt::symbols::symbol_function(replacement_symbol).unwrap()
        );
        assert_ne!(*original, *replacement);
        let original_before = egcl_rt::function::invoke_count(*original);
        let replacement_before = egcl_rt::function::invoke_count(*replacement);
        egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(0), NIL));
        let stack = egcl_rt::current_stack();
        let before_stack = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
        MUTATION.with(|state| {
            *state.borrow_mut() = Mutation {
                owner: Rc::as_ptr(mapped) as usize,
                symbol,
                original: Some(&mut *original),
                replacement: (!unbind).then_some(&mut *replacement),
                mutate_at_count: original_before + 2,
                done: false,
            }
        });
        let _mutation = MutationGuard;
        ENTRY_OBSERVER
            .with(|hook| hook.set(Some(replace_during_recursion)));
        native_transfer_entry::take_recursive_entries();
        egcl_rt::rooted!(
            result = super::super::super::apply_function(
                *original,
                &[EgclVal::from_fixnum(3), *marker],
                &mut env
            )
        );
        assert!(MUTATION.with(|state| state.borrow().done));
        assert_eq!(
            native_transfer_entry::take_recursive_entries(),
            0,
            "replacement does not reintroduce legacy recursive activations"
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*original) - original_before,
            2,
            "old root and its admitted child retain their original counter"
        );
        if unbind {
            assert!(
                matches!(&*result, Err(EgclError::UndefinedFunction(_)))
                    || matches!(&*result, Err(EgclError::Signalled(details))
                    if super::super::super::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")),
                "{:?}", &*result
            );
            assert_eq!(
                egcl_rt::function::invoke_count(*replacement) - replacement_before,
                0
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(21));
            assert_eq!(
                egcl_rt::function::invoke_count(*replacement) - replacement_before,
                3,
                "fallback calls count on the new selected callable only"
            );
        }
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before_stack
        );
    }
}
