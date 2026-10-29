// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_foreign_guard_resumes_after_completed_effect_once() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *ordinary-guard-effects* 0)
         (defun ordinary-guard-effect (x)
           (setq *ordinary-guard-effects* (+ *ordinary-guard-effects* 1)) x)
         (defun ordinary-guard-child (x a b c) (+ (ordinary-guard-effect x) 1))",
        &mut env,
    )
    .unwrap();
    let (_, child) = ordinary_t2::install(
        "ORDINARY-GUARD-CHILD",
        "(x a b c)",
        "((+ (ordinary-guard-effect x) 1))",
        &mut env,
    );
    let (symbol, caller) = ordinary_t2::install(
        "ORDINARY-GUARD-CALLER",
        "(x)",
        "((ordinary-guard-child x nil nil nil))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(child_code) = &child._storage else {
        panic!("mapped child")
    };
    let NativeCodeStorage::Mapped(caller_code) = &caller._storage else {
        panic!("mapped caller")
    };
    assert!(
        child_code.ordinary_entry.is_some(),
        "guarded child must use ordinary ABI, not mapped fallback"
    );
    assert!(
        child_code.mapped_entry().is_none(),
        "guarded ordinary artifact has no legacy entry"
    );
    assert!(caller_code.ordinary_entry.is_some());
    // The one-argument caller lacks the child's overflow capacity: retain a
    // real CALL and distinct suspended owner rather than a tail transfer.
    assert!(caller_code.value_maps_for_test().iter().next().is_some());
    assert_eq!(
        run_native(&caller, symbol, &[EgclVal::from_fixnum(10)], &mut env).unwrap(),
        EgclVal::from_fixnum(11)
    );
    assert_eq!(child_code.deopt_count(), 0);
    super::super::super::read_eval_all_env("(setq *ordinary-guard-effects* 0)", &mut env).unwrap();
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(2.5));
    egcl_rt::rooted!(result = run_native(&caller, symbol, &[*input], &mut env).unwrap());
    assert_eq!(result.as_double_float(), 3.5);
    assert_eq!(
        child_code.deopt_count(),
        1,
        "foreign owner must execute its guard continuation"
    );
    assert_eq!(
        caller_code.deopt_count(),
        0,
        "suspended caller stays native"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(
        super::super::super::read_eval_all_env("*ordinary-guard-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1),
        "completed call must not replay"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_zero_argument_root_guard_uses_empty_native_argument_range() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *ordinary-guard-zero-input* 2.5d0 *ordinary-guard-zero-effects* 0)
         (defun ordinary-guard-zero-effect ()
           (setq *ordinary-guard-zero-effects* (+ *ordinary-guard-zero-effects* 1))
           *ordinary-guard-zero-input*)",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = ordinary_t2::install(
        "ORDINARY-GUARD-ZERO",
        "()",
        "((+ (ordinary-guard-zero-effect) 1))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped root")
    };
    assert!(mapped.ordinary_entry.is_some());
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    egcl_rt::rooted!(result = run_native(&code, symbol, &[], &mut env).unwrap());
    assert_eq!(result.as_double_float(), 3.5);
    assert_eq!(mapped.deopt_count(), 1);
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(
        super::super::super::read_eval_all_env("*ordinary-guard-zero-effects*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_guard_shared_code_rejects_a_second_callable_before_publication() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun ordinary-guard-identity-effect (x) x)",
        &mut env,
    )
    .unwrap();
    let (_, code) = ordinary_t2::install(
        "ORDINARY-GUARD-IDENTITY",
        "(x)",
        "((+ (ordinary-guard-identity-effect x) 1))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped guarded owner")
    };
    assert!(mapped.ordinary_entry.is_some() && mapped.has_deopt);
    let body = code.body.as_ref().unwrap();
    egcl_rt::rooted!(first = make_bytecode_closure(body, None));
    egcl_rt::rooted!(second = make_bytecode_closure(body, None));
    assert_ne!(*first, *second);
    call_table::assert_guarded_callable_publication_for_test(Rc::clone(mapped), *first, *second);
}
