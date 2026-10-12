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
fn ordinary_foreign_guard_error_moves_older_caller_root() {
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
    // Inspect after retirement and another collection: the trace owns its
    // relocated argument and cannot depend on the abandoned native stack.
    HeapCollector::new().minor_gc().unwrap();
    let rows = error.backtrace().expect("guard error retains logical stack");
    let caller_rows: Vec<_> = rows.iter().filter(|row| row.function.as_deref()
        .is_some_and(|name| name.ends_with("ORDINARY-GUARD-ROOTS-CALLER"))).collect();
    assert_eq!(caller_rows.len(), 1, "{rows:?}");
    assert_eq!(caller_rows[0].origin, egcl_rt::debug_stack::FrameOrigin::Managed);
    assert_eq!(caller_rows[0].arguments.as_ref().unwrap().as_slice(), &[*argument]);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_foreign_guard_uses_retained_callable_after_redefinition() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *ordinary-guard-roots-effects* 0)
         (defun ordinary-guard-roots-effect (x)
           (setq *ordinary-guard-roots-effects* (+ *ordinary-guard-roots-effects* 1))
           (if (floatp x)
               (progn (defun ordinary-guard-roots-child (x a b c) 909)
                      (%force-minor-gc-for-test)))
           x)
         (defun ordinary-guard-roots-child (x a b c) (+ (ordinary-guard-roots-effect x) 1))",
        &mut env).unwrap();
    let (symbol, caller, child) = install_chain(&mut env);
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
