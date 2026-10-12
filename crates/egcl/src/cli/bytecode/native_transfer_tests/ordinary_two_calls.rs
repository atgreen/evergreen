// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! An empty active result must not force the next already-native call cold.
use super::*;
use super::ordinary_t2::{expect_entry, install, RECURSIVE_LEAF_OBSERVER};
use std::cell::Cell;

thread_local! { static COLLECTIONS: Cell<usize> = const { Cell::new(0) }; }
fn collect() {
    HeapCollector::new().full_gc().unwrap();
    COLLECTIONS.with(|count| count.set(count.get() + 1));
}
struct Restore(Option<fn()>);
impl Drop for Restore {
    fn drop(&mut self) { RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0)); }
}

fn assert_ordinary(code: &NativeCode) {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("mapped owner") };
    assert!(native_transfer_entry::installed_listing(mapped).contains("Ordinary x86-64 Lisp ABI"));
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn ordinary_two_calls_normalize_zero_and_relocate_a_live_argument_before_second_entry() {
    let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *two-call-zero-effects* 0 *two-call-many-effects* 0)
         (defun two-call-zero-terminal (x)
           (%ordinary-t2-entry-for-test)
           (setq *two-call-zero-effects* (+ *two-call-zero-effects* 1))
           (values))
         (defun two-call-many-terminal (x)
           (setq *two-call-many-effects* (+ *two-call-many-effects* 1))
           (values x 17 23))
         (defun two-call-zero (x) (two-call-zero-terminal x))
         (defun two-call-many (x) (two-call-many-terminal x))", &mut env).unwrap();
    let (zero_symbol, zero) = install("TWO-CALL-ZERO", "(x)", "((two-call-zero-terminal x))", &mut env);
    let (_, many) = install("TWO-CALL-MANY", "(x)", "((two-call-many-terminal x))", &mut env);
    let (symbol, caller) = install("TWO-CALL-CONTINUATION", "(x)",
        "((let ((ignored nil)) (setq ignored (two-call-zero x)) (two-call-many x)))", &mut env);
    for code in [&zero, &many, &caller] { assert_ordinary(code); }
    let NativeCodeStorage::Mapped(mapped) = &caller._storage else { unreachable!() };
    assert_eq!(mapped.value_maps_for_test().iter().count(), 4, "two actual direct/cold CALL pairs");
    expect_entry(&zero);
    assert_eq!(run_native(&zero, zero_symbol, &[NIL], &mut env).unwrap(), NIL);
    assert!(env.mv_active && env.mv.is_empty(), "first target really returns zero active values");
    // First cold publication still uses the zero target's root adapter.
    for iteration in 0..3 {
        expect_entry(if iteration == 0 { &zero } else { &caller });
        assert_eq!(run_native(&caller, symbol, &[NIL], &mut env).unwrap(), NIL);
    }
    for name in ["*TWO-CALL-ZERO-EFFECTS*", "*TWO-CALL-MANY-EFFECTS*"] {
        egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(name), EgclVal::from_fixnum(0)).unwrap();
    }
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(913), NIL));
    let before = payload.to_raw();
    let _restore = Restore(RECURSIVE_LEAF_OBSERVER.with(|hook| hook.replace(Some(collect))));
    COLLECTIONS.with(|count| count.set(0));
    expect_entry(&caller);
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    egcl_rt::rooted!(result = run_native(&caller, symbol, &[*payload], &mut env).unwrap());
    assert_eq!(native_transfer_entry::ordinary_calls::take_cold_calls_for_test(), 2,
        "only the two terminal helpers are cold; both continuation edges enter native targets");
    assert_eq!(COLLECTIONS.with(Cell::get), 1);
    assert_ne!(payload.to_raw(), before, "the first callee must relocate the caller's live argument");
    assert_eq!(*result, *payload);
    assert_eq!(crate::cli::cp(*result), (EgclVal::from_fixnum(913), NIL));
    assert!(env.mv_active);
    assert_eq!(env.mv, [*payload, EgclVal::from_fixnum(17), EgclVal::from_fixnum(23)]);
    for name in ["*TWO-CALL-ZERO-EFFECTS*", "*TWO-CALL-MANY-EFFECTS*"] {
        assert_eq!(egcl_rt::symbols::symbol_value(egcl_rt::symbols::intern(name)), Some(EgclVal::from_fixnum(1)));
    }
    assert_eq!(mapped.deopt_count(), 0);
}
