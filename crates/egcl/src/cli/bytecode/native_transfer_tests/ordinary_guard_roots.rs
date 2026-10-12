// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Foreign guard recovery keeps older native homes and the original callable.

use super::*;
use std::cell::Cell;

fn mapped(code: &NativeCode) -> &native_transfer_entry::TransferCode {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("mapped owner") };
    mapped
}

fn install_chain(env: &mut Env) -> (u32, Rc<NativeCode>, Rc<NativeCode>) {
    let (_, child) = ordinary_t2::install("ORDINARY-GUARD-ROOTS-CHILD", "(x a b c)",
        "((+ (ordinary-guard-roots-effect x) 1))", env);
    let (symbol, caller) = ordinary_t2::install("ORDINARY-GUARD-ROOTS-CALLER", "(x)",
        "((ordinary-guard-roots-child x nil nil nil))", env);
    for code in [&child, &caller] {
        assert!(mapped(code).ordinary_entry.is_some(), "both owners must use ordinary entries");
        assert!(mapped(code).mapped_entry().is_none(), "no legacy adapter substitutes for this path");
    }
    // One incoming argument cannot supply the child's rounded overflow area;
    // this edge must suspend a real caller frame even with tail emission on.
    assert!(mapped(&caller).value_maps_for_test().iter().next().is_some());
    assert_eq!(run_native(&caller, symbol, &[EgclVal::from_fixnum(10)], env).unwrap(),
        EgclVal::from_fixnum(11));
    assert_eq!(mapped(&child).deopt_count(), 0);
    crate::cli::read_eval_all_env("(setq *ordinary-guard-roots-effects* 0)", env).unwrap();
    (symbol, caller, child)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_foreign_guard_error_preserves_caller_identity_after_collection() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-guard-roots-effects* 0 *ordinary-guard-roots-signals* 0)
         (defun ordinary-guard-roots-effect (x)
           (setq *ordinary-guard-roots-effects* (+ *ordinary-guard-roots-effects* 1)) x)
         (defun ordinary-guard-roots-child (x a b c) (+ (ordinary-guard-roots-effect x) 1))
         (defun ordinary-guard-roots-handler (condition)
           (setq *ordinary-guard-roots-signals* (+ *ordinary-guard-roots-signals* 1))
           (%force-minor-gc-for-test))", &mut env).unwrap();
    let (symbol, caller, child) = install_chain(&mut env);
    env.handlers.push(HandlerCluster { entries: vec![HandlerEntry {
        type_name: "TYPE-ERROR".into(),
        handler: HandlerImpl::Function(resolve_sym("ORDINARY-GUARD-ROOTS-HANDLER").unwrap()),
    }] });
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(713), NIL));
    // This host root makes the object live independently. Movement plus the
    // retained managed anchor identity checks recovery ownership, not that an
    // older native physical home was the sole root or was rewritten.
    let original = argument.to_raw();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    egcl_rt::rooted!(error = run_native(&caller, symbol, &[*argument], &mut env).unwrap_err());
    assert_eq!(mapped(&child).deopt_count(), 1, "foreign guard must resume generic arithmetic");
    assert_eq!(mapped(&caller).deopt_count(), 0, "older caller remains native");
    assert_ne!(argument.to_raw(), original, "condition handler must actually move the heap root");
    assert_eq!(crate::cli::cp(*argument), (EgclVal::from_fixnum(713), NIL));
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-guard-roots-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1), "completed effect must not replay");
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-guard-roots-signals*", &mut env).unwrap(),
        EgclVal::from_fixnum(1), "generic continuation signals once");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    // Inspect logical identity after retirement and another collection.
    // Ordinary argument recipes may conservatively be unavailable; managed
    // anchor enrichment copies that None too, so this is not a native-home
    // or debugger-argument assertion.
    HeapCollector::new().minor_gc().unwrap();
    let rows = error.backtrace().expect("guard error retains logical stack");
    let caller_rows: Vec<_> = rows.iter().filter(|row| row.function.as_deref()
        .is_some_and(|name| name.ends_with("ORDINARY-GUARD-ROOTS-CALLER"))).collect();
    assert_eq!(caller_rows.len(), 1, "{rows:?}");
    assert_eq!(caller_rows[0].origin, egcl_rt::debug_stack::FrameOrigin::Managed);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_foreign_guard_uses_retained_callable_after_redefinition() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-guard-roots-effects* 0 *ordinary-guard-redefine* nil)
         (defun ordinary-guard-roots-effect (x)
           (setq *ordinary-guard-roots-effects* (+ *ordinary-guard-roots-effects* 1))
           (if *ordinary-guard-redefine*
               (progn (defun ordinary-guard-roots-child (x a b c) 909)
                      (%force-minor-gc-for-test)))
           x)
         (defun ordinary-guard-roots-child (x a b c) (+ (ordinary-guard-roots-effect x) 1))",
        &mut env).unwrap();
    let (symbol, caller, child) = install_chain(&mut env);
    crate::cli::read_eval_all_env("(setq *ordinary-guard-redefine* t)", &mut env).unwrap();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    egcl_rt::rooted!(argument = egcl_rt::gc::alloc_double_float(2.5));
    egcl_rt::rooted!(answer = run_native(&caller, symbol, &[*argument], &mut env).unwrap());
    assert_eq!(answer.as_double_float(), 3.5, "active old callable resumes after its completed call");
    assert_eq!(mapped(&child).deopt_count(), 1);
    assert_eq!(mapped(&caller).deopt_count(), 0);
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-guard-roots-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1));
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(run_native(&caller, symbol, &[*argument], &mut env).unwrap(),
        EgclVal::from_fixnum(909), "the next invocation must use the new definition");
}

thread_local! {
    static RESUME_COLLECTIONS: Cell<u32> = const { Cell::new(0) };
}

fn collect_during_foreign_guard_resume() {
    RESUME_COLLECTIONS.with(|count| count.set(count.get() + 1));
    HeapCollector::new().minor_gc().unwrap();
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_foreign_guard_collection_updates_older_caller_return_home() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-guard-home-effects* 0)
         (defun ordinary-guard-home-effect (x)
           (setq *ordinary-guard-home-effects* (+ *ordinary-guard-home-effects* 1)) x)
         (defun ordinary-guard-home-child (x a b c) (+ (ordinary-guard-home-effect x) 1))",
        &mut env).unwrap();
    let (_, child) = ordinary_t2::install("ORDINARY-GUARD-HOME-CHILD", "(x a b c)",
        "((+ (ordinary-guard-home-effect x) 1))", &mut env);
    let (symbol, caller) = ordinary_t2::install("ORDINARY-GUARD-HOME-CALLER", "(x n held)",
        "((ordinary-guard-home-child x nil nil nil) (+ n 1) held)", &mut env);
    for code in [&child, &caller] {
        assert!(mapped(code).ordinary_entry.is_some());
        assert!(mapped(code).mapped_entry().is_none());
    }
    // A has no incoming overflow capacity for B's fourth argument. Its
    // post-call arithmetic/return also requires a genuine continuation.
    assert!(mapped(&caller).value_maps_for_test().iter().next().is_some());
    for _ in 0..2 {
        assert_eq!(run_native(&caller, symbol, &[
            EgclVal::from_fixnum(10), EgclVal::from_fixnum(7), EgclVal::from_fixnum(99),
        ], &mut env).unwrap(), EgclVal::from_fixnum(99));
    }
    crate::cli::read_eval_all_env("(setq *ordinary-guard-home-effects* 0)", &mut env).unwrap();
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(2.5));
    egcl_rt::rooted!(held = crate::cli::arena_cons(EgclVal::from_fixnum(941), NIL));
    let original = held.to_raw();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    RESUME_COLLECTIONS.with(|count| count.set(0));
    // The hook runs with B's precise guard payload rooted, before resuming
    // its generic continuation. A remains physically suspended throughout GC.
    let result = native_transfer_entry::with_ordinary_guard_resume_observer_for_test(
        collect_during_foreign_guard_resume,
        || run_native(&caller, symbol, &[*input, EgclVal::from_fixnum(7), *held], &mut env),
    ).unwrap();
    assert_eq!(RESUME_COLLECTIONS.with(Cell::get), 1);
    assert_ne!(held.to_raw(), original, "the observed guard collection must relocate held");
    // held is never passed to B. Its external root keeps it live but cannot
    // rewrite A's separate native return home. Check raw equality before
    // dereferencing result, so a missing native-root update fails cleanly.
    assert_eq!(result.to_raw(), held.to_raw(), "A must return its relocated native home");
    assert_eq!(crate::cli::cp(result), (EgclVal::from_fixnum(941), NIL));
    assert_eq!(mapped(&child).deopt_count(), 1);
    assert_eq!(mapped(&caller).deopt_count(), 0, "A must read held in native code, not T0");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-guard-home-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_caller_guard_resumes_after_native_child_completed_once() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-caller-guard-effects* 0)
         (defun ordinary-caller-guard-effect (x)
           (setq *ordinary-caller-guard-effects* (+ *ordinary-caller-guard-effects* 1)) x)
         (defun ordinary-caller-guard-child (x a b c) (ordinary-caller-guard-effect x))",
        &mut env).unwrap();
    let (_, child) = ordinary_t2::install("ORDINARY-CALLER-GUARD-CHILD", "(x a b c)",
        "((ordinary-caller-guard-effect x))", &mut env);
    let (symbol, caller) = ordinary_t2::install("ORDINARY-CALLER-GUARD-CALLER", "(x)",
        "((+ (ordinary-caller-guard-child x nil nil nil) 1))", &mut env);
    for code in [&child, &caller] {
        assert!(mapped(code).ordinary_entry.is_some());
        assert!(mapped(code).mapped_entry().is_none());
    }
    // A's smaller incoming overflow capacity requires a real CALL to B.
    // B may tail-forward its own terminal effect call; that cannot erase A.
    assert!(mapped(&caller).value_maps_for_test().iter().next().is_some());
    for _ in 0..2 {
        assert_eq!(run_native(&caller, symbol, &[EgclVal::from_fixnum(10)], &mut env).unwrap(),
            EgclVal::from_fixnum(11));
    }
    crate::cli::read_eval_all_env("(setq *ordinary-caller-guard-effects* 0)", &mut env).unwrap();
    egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(2.5));
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    // Drain ambient requests before measuring the generated A->B edge and
    // the terminal effect's named adapter independently.
    egcl_rt::poll_safepoint();
    assert_eq!(super::super::native_loop_should_exit(), 0);
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    egcl_rt::rooted!(answer = run_native(&caller, symbol, &[*input], &mut env).unwrap());
    assert_eq!(answer.as_double_float(), 3.5);
    assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 0,
        "all generated edges are admitted");
    assert_eq!(native_transfer_entry::ordinary_calls::take_named_entries_for_test(), 1,
        "only B's terminal effect uses the named adapter; A executes its warmed ordinary CALL");
    assert_eq!(mapped(&caller).deopt_count(), 1, "A fails only after B completed");
    assert_eq!(mapped(&child).deopt_count(), 0, "B completes using its ordinary forwarding entry");
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-caller-guard-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1), "A's post-call continuation must not execute B again");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_hoisted_entry_guard_resumes_before_child_effect_once() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-entry-guard-effects* 0)
         (defun ordinary-entry-guard-effect (x)
           (setq *ordinary-entry-guard-effects* (+ *ordinary-entry-guard-effects* 1)) x)
         (defun ordinary-entry-guard-child (x a b c) (+ (ordinary-entry-guard-effect x) 1))",
        &mut env).unwrap();
    let (_, child) = ordinary_t2::install("ORDINARY-ENTRY-GUARD-CHILD", "(x a b c)",
        "((+ (ordinary-entry-guard-effect x) 1))", &mut env);
    let (symbol, caller) = ordinary_t2::install("ORDINARY-ENTRY-GUARD-CALLER", "(x n held)",
        "((ordinary-entry-guard-child x nil nil nil) (+ n 1) held)", &mut env);
    for code in [&child, &caller] {
        assert!(mapped(code).ordinary_entry.is_some());
        assert!(mapped(code).mapped_entry().is_none());
    }
    for _ in 0..2 {
        assert_eq!(run_native(&caller, symbol, &[
            EgclVal::from_fixnum(10), EgclVal::from_fixnum(7), EgclVal::from_fixnum(99),
        ], &mut env).unwrap(), EgclVal::from_fixnum(99));
    }
    crate::cli::read_eval_all_env("(setq *ordinary-entry-guard-effects* 0)", &mut env).unwrap();
    egcl_rt::rooted!(number = egcl_rt::gc::alloc_double_float(2.5));
    egcl_rt::rooted!(held = crate::cli::arena_cons(EgclVal::from_fixnum(967), NIL));
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    // N's type guard is hoisted before CALL. Its own pre-call bytecode state
    // must execute B once, unlike a post-call failure which must skip B.
    let result = run_native(&caller, symbol,
        &[EgclVal::from_fixnum(10), *number, *held], &mut env).unwrap();
    assert_eq!(result, *held);
    assert_eq!(crate::cli::cp(result), (EgclVal::from_fixnum(967), NIL));
    assert_eq!(mapped(&caller).deopt_count(), 1);
    assert_eq!(mapped(&child).deopt_count(), 0);
    assert_eq!(crate::cli::read_eval_all_env("*ordinary-entry-guard-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1), "pre-call recovery must neither skip nor repeat the child effect");
    assert_eq!((stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)), before);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
}
