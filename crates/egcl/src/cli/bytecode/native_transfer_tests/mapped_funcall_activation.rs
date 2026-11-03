// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! An object call below one live mapped parent must reuse the existing native
//! ABI, rather than prepare/recycle a managed callee activation on every turn.
use super::super::native_transfer_entry::{funcall_fast, mapped_ordinary};
use super::*;
use std::cell::Cell;

thread_local! {
    static PROBE: Cell<(u32, usize)> = const { Cell::new((0, 0)) };
}
struct Probe(Option<fn()>);
impl Drop for Probe {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        native_transfer_entry::take_activation_counts_for_test();
        PROBE.with(|slot| slot.set((0, 0)));
    }
}
fn observe_counts_only() {
    let (owner, calls) = PROBE.with(Cell::get);
    assert_ne!(owner, 0);
    assert_eq!(
        native_transfer_entry::activation_counts_for_test(),
        (0, 0),
        "a real native callee entry must not prepare/recycle a per-call activation"
    );
    PROBE.with(|slot| slot.set((owner, calls + 1)));
}
fn observe_no_activation() {
    observe_counts_only();
    let frames = egcl_rt::debug_stack::capture_current(32);
    let callee: Vec<_> = frames
        .iter()
        .filter(|frame| {
            frame
                .function
                .as_deref()
                .is_some_and(|name| name.ends_with("MF-ACT-INNER"))
        })
        .collect();
    assert_eq!(callee.len(), 1, "{frames:?}");
    assert_eq!(callee[0].origin, egcl_rt::debug_stack::FrameOrigin::Native);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_mapped_funcall_reuses_one_bridge_without_per_call_activations() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun mf-act-observe () (%ordinary-t2-entry-for-test))
         (defun mf-act-inner (a b) (mf-act-observe) (+ a b))
         (defun mf-act-loop (f n &key (step (1+ 0)) &aux (unused 0))
           (let ((s 0)) (dotimes (i n s) (setq s (funcall f s step)))))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["MF-ACT-OBSERVE"]);
    let (inner, inner_code) = ordinary_t2::install(
        "MF-ACT-INNER",
        "(a b)",
        "((mf-act-observe) (+ a b))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(inner_mapped) = &inner_code._storage else {
        panic!("native callee");
    };
    assert!(inner_mapped.lean_root_entry().is_some());
    assert!(inner_code.body.as_ref().unwrap().self_call_sites.is_empty());
    let (_, outer_code) = ordinary_t2::install(
        "MF-ACT-LOOP",
        "(f n &key (step (1+ 0)) &aux (unused 0))",
        "((let ((s 0)) (dotimes (i n s) (setq s (funcall f s step)))))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(outer) = &outer_code._storage else {
        panic!("mapped parent");
    };
    assert!(
        outer.ordinary_entry.is_none(),
        "evaluated keyword default retains mapped caller ABI"
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(inner).unwrap());
    ordinary_t2::expect_entry(&inner_code);
    assert_eq!(
        funcall_fast::apply(
            *function,
            &[EgclVal::from_fixnum(0), EgclVal::from_fixnum(2)],
            &mut env,
            funcall_fast::Admit::Native
        )
        .unwrap()
        .unwrap(),
        EgclVal::from_fixnum(2)
    );
    assert!(ordinary_t2::observed_entry());
    let step = resolve_sym(":STEP").unwrap();
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_lean_root_entries_for_test();
    native_transfer_entry::take_nested_entries();
    funcall_fast::take_fast_calls_for_test();
    PROBE.with(|slot| slot.set((inner, 0)));
    native_transfer_entry::start_activation_counts_for_test(inner);
    ordinary_t2::expect_entry(&inner_code);
    let _probe = Probe(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(observe_no_activation))),
    );
    let calls = 100;
    assert_eq!(
        outer
            .run(
                &[
                    *function,
                    EgclVal::from_fixnum(calls),
                    step,
                    EgclVal::from_fixnum(2)
                ],
                &mut env
            )
            .unwrap(),
        EgclVal::from_fixnum(calls * 2)
    );
    assert_eq!(
        PROBE.with(|slot| slot.get().1),
        calls as usize,
        "every iteration executes the exact native callee, not only an installed tier"
    );
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 1);
    assert_eq!(mapped_ordinary::take_entries_for_test(), calls as usize);
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
    assert_eq!(native_transfer_entry::take_nested_entries(), 0);
    assert_eq!(funcall_fast::take_fast_calls_for_test().0, calls as usize);
    // The one native-child state is retired once with its live parent's lane;
    // no per-call prepare or recycle was hidden in that final teardown.
    assert_eq!(
        native_transfer_entry::take_activation_counts_for_test(),
        (0, 1)
    );
}

thread_local! {
    static MOVING_ARGUMENT: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static DID_MOVE: Cell<bool> = const { Cell::new(false) };
}
struct MovingProbe(Option<fn()>);
impl Drop for MovingProbe {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        MOVING_ARGUMENT.with(|slot| slot.set(std::ptr::null()));
    }
}
fn collect_native_argument() {
    let root = MOVING_ARGUMENT.with(Cell::get);
    assert!(!root.is_null());
    let before = unsafe { (*root).to_raw() };
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        unsafe { (*root).to_raw() },
        before,
        "the bridge argument really relocates"
    );
    DID_MOVE.with(|slot| slot.set(true));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_mapped_funcall_bridge_returns_argument_relocated_inside_native_callee() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun mf-move-observe () (%ordinary-t2-entry-for-test))
         (defun mf-move-inner (x) (mf-move-observe) x)
         (defun mf-move-outer (f x &key (k (1+ 0)) &aux (u 0)) (funcall f x))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["MF-MOVE-OBSERVE"]);
    let (inner, code) =
        ordinary_t2::install("MF-MOVE-INNER", "(x)", "((mf-move-observe) x)", &mut env);
    let (_, caller) = ordinary_t2::install(
        "MF-MOVE-OUTER",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "((funcall f x))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(parent) = &caller._storage else {
        panic!("mapped parent");
    };
    assert!(parent.ordinary_entry.is_none());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(inner).unwrap());
    ordinary_t2::expect_entry(&code);
    assert_eq!(
        funcall_fast::apply(*function, &[NIL], &mut env, funcall_fast::Admit::Native)
            .unwrap()
            .unwrap(),
        NIL
    );
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(781), NIL));
    MOVING_ARGUMENT.with(|slot| slot.set(&*argument));
    DID_MOVE.with(|slot| slot.set(false));
    let _probe = MovingProbe(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|slot| slot.replace(Some(collect_native_argument))),
    );
    ordinary_t2::expect_entry(&code);
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_lean_root_entries_for_test();
    egcl_rt::rooted!(result = parent.run(&[*function, *argument], &mut env).unwrap());
    assert!(ordinary_t2::observed_entry() && DID_MOVE.with(Cell::get));
    assert_eq!(
        *result, *argument,
        "native return reads the relocated callee home"
    );
    assert_eq!(cp(*result), (EgclVal::from_fixnum(781), NIL));
    assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 1);
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_mapped_funcall_bridge_keeps_two_captured_instances_distinct() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun mf-cap-observe () (%ordinary-t2-entry-for-test))
         (defun mf-cap-outer (first second &key (k (1+ 0)) &aux (u 0))
           (list (funcall first nil) (funcall second nil)
                 (funcall first nil) (funcall second nil)))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["MF-CAP-OBSERVE"]);
    let body = ordinary_captured_context::template("((mf-cap-observe) ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(
        first = ordinary_captured_context::closure(&body, EgclVal::from_fixnum(11), &mut env)
    );
    egcl_rt::rooted!(
        second = ordinary_captured_context::closure(&body, EgclVal::from_fixnum(22), &mut env)
    );
    let code = native_transfer_entry::with_forced_captured_entry_for_test(|| {
        ordinary_captured_context::install_automatic(*first)
    });
    let NativeCodeStorage::Mapped(callee) = &code._storage else {
        panic!("captured native callee");
    };
    assert!(callee.native_callable_slot.is_some() && callee.lean_root_entry().is_some());
    let (_, caller) = ordinary_t2::install(
        "MF-CAP-OUTER",
        "(first second &key (k (1+ 0)) &aux (u 0))",
        "((list (funcall first nil) (funcall second nil) (funcall first nil) (funcall second nil)))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(parent) = &caller._storage else {
        panic!("mapped parent");
    };
    assert!(parent.ordinary_entry.is_none());
    ordinary_t2::expect_entry(&code);
    for (use_first, expected) in [(true, 11), (false, 22)] {
        let function = if use_first { *first } else { *second };
        assert_eq!(
            funcall_fast::apply(function, &[NIL], &mut env, funcall_fast::Admit::Native)
                .unwrap()
                .unwrap(),
            EgclVal::from_fixnum(expected)
        );
    }
    let (owner, _, _, _) = funcall_fast::native_target(*first, 1).expect("captured target owner");
    PROBE.with(|slot| slot.set((owner, 0)));
    native_transfer_entry::start_activation_counts_for_test(owner);
    let _probe = Probe(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(observe_counts_only))),
    );
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    native_transfer_entry::take_lean_root_entries_for_test();
    egcl_rt::rooted!(result = parent.run(&[*first, *second], &mut env).unwrap());
    assert_eq!(
        list_to_vec(*result),
        [11, 22, 11, 22].map(EgclVal::from_fixnum)
    );
    assert_eq!(PROBE.with(|slot| slot.get().1), 4);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 4);
    assert_eq!(
        mapped_ordinary::take_prepares_for_test(),
        1,
        "one code lane keeps per-call receiver identity"
    );
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 0);
    assert_eq!(
        native_transfer_entry::take_activation_counts_for_test(),
        (0, 1)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_mapped_funcall_preserves_values_errors_cleanup_and_retained_definitions() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *mf-cleanups* 0)
         (defun mf-values (x) (values x (list x)))
         (defun mf-car (x) (car x))
         (defun mf-version (x) (+ x 1))
         (defun mf-values-parent (f x &key (k (1+ 0)) &aux (u 0))
           (multiple-value-list (funcall f x)))
         (defun mf-cleanup-parent (f x &key (k (1+ 0)) &aux (u 0))
           (unwind-protect (funcall f x) (setq *mf-cleanups* (1+ *mf-cleanups*))))
         (defun mf-version-parent (f x &key (k (1+ 0)) &aux (u 0)) (funcall f x))",
        &mut env,
    )
    .unwrap();
    let (values, _) = ordinary_t2::install("MF-VALUES", "(x)", "((values x (list x)))", &mut env);
    let (car, _) = ordinary_t2::install("MF-CAR", "(x)", "((car x))", &mut env);
    let (version, _) = ordinary_t2::install("MF-VERSION", "(x)", "((+ x 1))", &mut env);
    let (_, values_parent) = ordinary_t2::install(
        "MF-VALUES-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "((multiple-value-list (funcall f x)))",
        &mut env,
    );
    let (_, cleanup_parent) = ordinary_t2::install(
        "MF-CLEANUP-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "((unwind-protect (funcall f x) (setq *mf-cleanups* (1+ *mf-cleanups*))))",
        &mut env,
    );
    let (_, version_parent) = ordinary_t2::install(
        "MF-VERSION-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "((funcall f x))",
        &mut env,
    );
    let parent = |native: &NativeCode| {
        let NativeCodeStorage::Mapped(parent) = &native._storage else {
            panic!("mapped parent");
        };
        assert!(parent.ordinary_entry.is_none());
        Rc::clone(parent)
    };
    let values_parent = parent(&values_parent);
    let cleanup_parent = parent(&cleanup_parent);
    let version_parent = parent(&version_parent);
    egcl_rt::rooted!(values_function = egcl_rt::symbols::symbol_function(values).unwrap());
    egcl_rt::rooted!(car_function = egcl_rt::symbols::symbol_function(car).unwrap());
    egcl_rt::rooted!(old = egcl_rt::symbols::symbol_function(version).unwrap());
    // Only the object calls below use the new published ingress. Warming from
    // Rust still uses its own root entry; the cache is not an ABI capability.
    egcl_rt::rooted!(functions = [*values_function, *car_function, *old]);
    for index in 0..functions.len() {
        egcl_rt::rooted!(function = functions[index]);
        egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(1), NIL));
        // The version target needs an integer rather than a list.
        let input = if *function == *old {
            EgclVal::from_fixnum(1)
        } else {
            *argument
        };
        assert!(
            funcall_fast::apply(*function, &[input], &mut env, funcall_fast::Admit::Native)
                .unwrap()
                .is_ok()
        );
    }
    mapped_ordinary::take_entries_for_test();
    egcl_rt::rooted!(
        result = values_parent
            .run(&[*values_function, EgclVal::from_fixnum(71)], &mut env)
            .unwrap()
    );
    let tuple = list_to_vec(*result);
    assert_eq!(tuple.len(), 2);
    assert_eq!(tuple[0], EgclVal::from_fixnum(71));
    assert_eq!(cp(tuple[1]), (EgclVal::from_fixnum(71), NIL));
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        1,
        "MV producer executes the new native bridge"
    );
    let error = cleanup_parent
        .run(&[*car_function, EgclVal::from_fixnum(5)], &mut env)
        .expect_err("CAR must signal");
    assert!(format!("{error:?}").to_ascii_lowercase().contains("list"));
    assert_eq!(
        egcl_rt::symbols::symbol_value(symbol_index_of("*MF-CLEANUPS*").unwrap()),
        Some(EgclVal::from_fixnum(1))
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        1,
        "exceptional callee really entered through bridge"
    );
    assert_eq!(
        version_parent
            .run(&[*old, EgclVal::from_fixnum(3)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(4)
    );
    assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
    crate::cli::read_eval_all_env("(defun mf-version (x) (+ x 100))", &mut env).unwrap();
    ordinary_t2::install("MF-VERSION", "(x)", "((+ x 100))", &mut env);
    egcl_rt::rooted!(new = egcl_rt::symbols::symbol_function(version).unwrap());
    assert_ne!(*old, *new);
    assert_eq!(
        version_parent
            .run(&[*old, EgclVal::from_fixnum(3)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(4),
        "retained object uses its own source after public redefinition, including fallback"
    );
    mapped_ordinary::take_entries_for_test();
    assert_eq!(
        version_parent
            .run(&[*new, EgclVal::from_fixnum(3)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(103)
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        1,
        "current new object uses fresh exact native target"
    );
}
