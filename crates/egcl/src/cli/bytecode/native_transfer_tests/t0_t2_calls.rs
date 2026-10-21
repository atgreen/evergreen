// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A bytecode caller entering an installed T2 callee (GitHub #487). The call
//! must reach the mapped native entry on every iteration, without revisiting
//! tier dispatch and without allocating a fresh ordinary root identity.

use super::*;

#[test]
fn a_bytecode_caller_enters_an_installed_t2_leaf_directly() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env("(defun t0-t2-leaf (a b) (+ a b))", &mut env).unwrap();
    let (symbol, code) = super::ordinary_t2::install("T0-T2-LEAF", "(a b)", "((+ a b))", &mut env);
    assert!(code.is_t2, "the probe needs an installed T2 callee");
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("the probe needs mapped T2 code")
    };
    assert!(mapped.ordinary_entry.is_some(), "the leaf uses the ordinary ABI");
    assert!(mapped.needs_callable_identity(), "each entry retains the leaf's root identity");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(egcl_rt::function::tier(*function), 2, "publication records the tier");

    // The caller runs once, so it stays bytecode while its loop calls the leaf.
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((let ((s 0)) (dotimes (i n s) (setq s (t0-t2-leaf s 1)))))").unwrap().0);
    let caller = Arc::new(
        compile_function("T0-T2-CALLER", *params, *forms, &env, false, false).unwrap(),
    );
    let calls = 300;
    // Warm once so the first-call root identity is not counted below.
    assert_eq!(run(Arc::clone(&caller), &[EgclVal::from_fixnum(1)], NIL, &mut env).unwrap(),
        EgclVal::from_fixnum(1));
    TIER_DISPATCHES.with(|count| count.set(0));
    super::super::call_table::take_target_lookups_for_test();
    super::super::call_table::take_root_identities_for_test();
    let result = run(caller, &[EgclVal::from_fixnum(calls)], NIL, &mut env).unwrap();
    let dispatches = TIER_DISPATCHES.with(std::cell::Cell::get);
    let entries = super::super::call_table::take_target_lookups_for_test();
    let identities = super::super::call_table::take_root_identities_for_test();
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(result, EgclVal::from_fixnum(calls));
    assert!(entries >= calls as usize, "only {entries} of {calls} calls entered native code");
    assert_eq!(dispatches, 0, "an installed T2 callee needs no tier dispatch");
    assert_eq!(identities, 0, "steady-state calls reuse their ordinary root identity");
}
