// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::TransferCode;
use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_named_calls_use_live_call_cells() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun native-cell-leaf (x) (values x (list :old x)))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((native-cell-leaf x))")
            .unwrap()
            .0
    );
    let body = Arc::new(
        compile_function("NATIVE-CELL-CALLER", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("mapped named caller");
    let symbol = egcl_rt::symbols::intern("NATIVE-CELL-LEAF");
    let cell = call_table::resolve(symbol).unwrap();
    let published = native_transfer_entry::published_entries().unwrap();
    for (index, slice) in [false, true].into_iter().enumerate() {
        let entry = unsafe { &*cell.entry_address(slice) }
            .load(std::sync::atomic::Ordering::Acquire);
        assert_eq!(entry, published[index], "cold cells expose the native contract");
        assert_ne!(cell.entry_address(slice), cell.checked_entry_address(slice));
    }
    assert!(cell.is_cold());
    assert_eq!(
        code.run(&[EgclVal::from_fixnum(41)], &mut env).unwrap(),
        EgclVal::from_fixnum(41)
    );
    assert!(
        !cell.is_cold(),
        "mapped named calls must enter and warm their published CallCell"
    );
    assert_eq!(env.mv.len(), 2);
    assert_eq!(cp(cp(env.mv[1]).1).0, EgclVal::from_fixnum(41));
    for _ in 0..30 {
        assert_eq!(
            code.run(&[EgclVal::from_fixnum(42)], &mut env).unwrap(),
            EgclVal::from_fixnum(42)
        );
        assert_eq!(env.mv.len(), 2);
    }
    super::super::super::read_eval_all_env(
        "(defun native-cell-leaf (x) (values :new x))",
        &mut env,
    )
    .unwrap();
    assert!(cell.is_cold(), "replacement invalidates the existing cell");
    let result = code.run(&[EgclVal::from_fixnum(43)], &mut env).unwrap();
    assert_eq!(result, super::super::super::resolve_sym(":NEW").unwrap());
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[1], EgclVal::from_fixnum(43));
    super::super::super::read_eval_all_env("(fmakunbound 'native-cell-leaf)", &mut env).unwrap();
    assert!(cell.is_cold());
    assert!(code.run(&[EgclVal::from_fixnum(44)], &mut env).is_err());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_call_cells_preserve_wide_optional_and_rest_values() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun native-cell-wide (a b &optional (c 17) &rest tail)
           (values a (list b c tail)))",
        &mut env,
    )
    .unwrap();
    for (source, expected) in [
        ("((native-cell-wide x 2))", "(2 17 nil)"),
        ("((native-cell-wide x 2 3 4 5))", "(2 3 (4 5))"),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function(
                "NATIVE-CELL-WIDE-CALLER",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = TransferCode::compile(body).expect("mapped wide caller");
        egcl_rt::rooted!(expected = reader::read_from_string(expected).unwrap().0);
        for primary in [NIL, EgclVal::from_fixnum(0), EgclVal::from_fixnum(42)] {
            for _ in 0..3 {
                assert_eq!(code.run(&[primary], &mut env).unwrap(), primary);
                assert!(env.mv_active);
                assert_eq!(env.mv.len(), 2);
                let mut actual_text = String::new();
                let mut expected_text = String::new();
                super::super::super::print_val(env.mv[1], &mut actual_text);
                super::super::super::print_val(*expected, &mut expected_text);
                assert_eq!(actual_text, expected_text);
            }
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_call_cells_capture_after_rust_reentry_and_run_cleanup_once() {
    use super::super::native_transfer_entry::{
        take_native_catch_count, take_native_cleanup_count, take_native_fallback_count,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *native-cell-cleanups* 0 *native-cell-after* 0)
         (defun native-cell-cleanup ()
           (setq *native-cell-cleanups* (+ *native-cell-cleanups* 1)))
         (defun native-cell-after ()
           (setq *native-cell-after* (+ *native-cell-after* 1)))
         (defun native-cell-throw (x)
           (%force-minor-gc-for-test) (throw :cell-tag (values x (list x))))
         (defun native-cell-rust (x) (if x (eval (list 'native-cell-throw (list 'quote x))) nil))",
        &mut env,
    )
    .unwrap();
    // Install a real legacy T2 target before entering the mapped caller. EVAL
    // reenters Rust and invokes another Lisp callee before its throw returns.
    let symbol = egcl_rt::symbols::intern("NATIVE-CELL-RUST");
    egcl_rt::rooted!(callee_params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        callee_forms = reader::read_from_string(
            "((if x (eval (list 'native-cell-throw (list 'quote x))) nil))"
        )
        .unwrap()
        .0
    );
    registry_put(
        symbol,
        Arc::new(
            compile_function(
                "NATIVE-CELL-RUST",
                *callee_params,
                *callee_forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        ),
    );
    let input = snapshot_t2_input(symbol, 0).unwrap();
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input.with_gc_stable(compile_t2_artifact).unwrap();
    assert!(
        install_t2_completion(T2Completion {
            sym: symbol,
            generation,
            artifact: Some(artifact),
            input,
        })
        .unwrap()
        .is_t2
    );
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((catch :cell-tag (unwind-protect (progn (native-cell-rust x) (native-cell-after)) (native-cell-cleanup))))"
    ).unwrap().0);
    let body = Arc::new(
        compile_function("NATIVE-CELL-UNWIND", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("mapped cleanup caller");
    // The successful call publishes its native cell entry; throwing calls
    // alone do not warm a cold cell. Reset the warmup's observable effects.
    code.run(&[NIL], &mut env).unwrap();
    assert!(!call_table::resolve(symbol).unwrap().is_cold());
    super::super::super::read_eval_all_env(
        "(setq *native-cell-cleanups* 0 *native-cell-after* 0)",
        &mut env,
    )
    .unwrap();
    for _ in 0..3 {
        take_native_catch_count();
        take_native_cleanup_count();
        take_native_fallback_count();
        egcl_rt::rooted!(
            args = vec![super::super::super::arena_cons(
                EgclVal::from_fixnum(42),
                NIL
            )]
        );
        let before = args[0].to_raw();
        egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
        assert_ne!(args[0].to_raw(), before, "probe must relocate its argument");
        assert_eq!(*result, args[0]);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(cp(env.mv[1]).0, args[0]);
        assert_eq!(take_native_catch_count(), 1);
        assert_eq!(take_native_cleanup_count(), 1);
        assert_eq!(take_native_fallback_count(), 0);
    }
    assert_eq!(
        super::super::super::read_eval_all_env("*native-cell-cleanups*", &mut env).unwrap(),
        EgclVal::from_fixnum(3)
    );
    assert_eq!(
        super::super::super::read_eval_all_env("*native-cell-after*", &mut env).unwrap(),
        EgclVal::from_fixnum(0)
    );
}
