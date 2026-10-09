// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::{TransferCode, take_recursive_entries};
use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_optimized_recursive_overflow_resumes_without_replay() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let form = "(progn (segment-deopt-tick x)
        (if (= n 0) (+ x 1) (+ (segment-deopt-sum (- n 1) x) 1)))";
    super::super::super::read_eval_all_env(
        &format!(
            "(setq *segment-deopt-ticks* 0)
          (defun segment-deopt-tick (x)
            (setq *segment-deopt-ticks* (+ *segment-deopt-ticks* 1)))
          (defun segment-deopt-sum (n x) {form})"
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
    let body = Arc::new(
        compile_function("SEGMENT-DEOPT-SUM", *params, *forms, &env, false, false).unwrap(),
    );
    let symbol = super::super::super::resolve_sym("SEGMENT-DEOPT-SUM")
        .unwrap()
        .as_symbol_index();
    registry_put(symbol, Arc::clone(&body));
    let code = TransferCode::compile(body).expect("compile guarded segment");
    assert!(
        code.has_deopt,
        "arithmetic must use native speculative guards"
    );
    for overflow in [false, true] {
        super::super::super::read_eval_all_env("(setq *segment-deopt-ticks* 0)", &mut env).unwrap();
        let x = if overflow { (1_i64 << 60) - 1 } else { 10 };
        let stack = egcl_rt::current_stack();
        let fp = stack.fp();
        let sp = stack.sp();
        let depth = NATIVE_DEPTH.with(|d| d.get());
        take_recursive_entries();
        egcl_rt::rooted!(
            result = code
                .run(
                    &[EgclVal::from_fixnum(8), EgclVal::from_fixnum(x)],
                    &mut env
                )
                .unwrap()
        );
        assert_eq!(
            code.deopt_count(),
            if overflow { 9 } else { 0 },
            "each overflowing activation resumes its own continuation"
        );
        assert_eq!(
            take_recursive_entries(),
            8,
            "guard fallback must return through all native callers"
        );
        assert_eq!(stack.fp(), fp);
        assert_eq!(stack.sp(), sp);
        assert_eq!(NATIVE_DEPTH.with(|d| d.get()), depth);
        assert_eq!(
            super::super::super::read_eval_all_env("*segment-deopt-ticks*", &mut env).unwrap(),
            EgclVal::from_fixnum(9)
        );
        egcl_rt::rooted!(
            expected =
                super::super::super::read_eval_all_env(&format!("(+ {x} 9)"), &mut env).unwrap()
        );
        assert_eq!(
            super::super::super::format_val(*result),
            super::super::super::format_val(*expected)
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_optimized_deopt_moves_caller_roots_and_retires_errors() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for fail in [false, true] {
        let finish = if fail { "(car 1)" } else { "x" };
        let form = "(progn (segment-moving-tick v)
            (if (= n 0)
                (progn (+ v 1) (segment-moving-finish x))
                (progn (segment-moving (- n 1) v x) (segment-moving-car x))))";
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *segment-moving-ticks* 0)
             (defun segment-moving-tick (v)
               (setq *segment-moving-ticks* (+ *segment-moving-ticks* 1)))
             (defun segment-moving-finish (x) (%force-minor-gc-for-test) {finish})
             (defun segment-moving-car (x) (car x))
             (defun segment-moving (n v x) {form})"
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n v x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let symbol = super::super::super::resolve_sym("SEGMENT-MOVING")
            .unwrap()
            .as_symbol_index();
        let body = Arc::new(
            compile_function("SEGMENT-MOVING", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let code = TransferCode::compile(body).expect("compile guarded recursive source");
        assert!(code.has_deopt);
        egcl_rt::rooted!(
            args = vec![
                EgclVal::from_fixnum(8),
                EgclVal::from_fixnum((1_i64 << 60) - 1),
                super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL)
            ]
        );
        let address = args[2].to_raw();
        let stack = egcl_rt::current_stack();
        let fp = stack.fp();
        let sp = stack.sp();
        let depth = NATIVE_DEPTH.with(|d| d.get());
        take_recursive_entries();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(take_recursive_entries(), 8);
        assert_eq!(
            code.deopt_count(),
            1,
            "the deepest activation must resume T0"
        );
        assert_eq!(stack.fp(), fp);
        assert_eq!(stack.sp(), sp);
        assert_eq!(NATIVE_DEPTH.with(|d| d.get()), depth);
        assert_ne!(
            args[2].to_raw(),
            address,
            "GC must actually relocate the suspended caller's cons"
        );
        assert_eq!(
            super::super::super::cp(args[2]),
            (EgclVal::from_fixnum(42), NIL)
        );
        if fail {
            assert!(
                matches!(&*result, Err(EgclError::Signalled(details))
                if super::super::super::condition_matches_handler(&env, details.condition, "TYPE-ERROR")),
                "{:?}",
                &*result
            );
            let Err(EgclError::Signalled(details)) = &*result else {
                unreachable!()
            };
            assert_eq!(
                details
                    .backtrace
                    .iter()
                    .filter(|frame| frame
                        .function
                        .as_deref()
                        .is_some_and(|name| name.ends_with("SEGMENT-MOVING")))
                    .count(),
                9,
                "one logical frame per recursive activation"
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(42));
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*segment-moving-ticks*", &mut env).unwrap(),
            EgclVal::from_fixnum(9)
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_optimized_old_definition_deopts_with_multiple_values() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let form = "(progn (segment-redefine x) (segment-return-values (+ x 1)))";
    super::super::super::read_eval_all_env(
        &format!(
            "(setq *segment-redefines* 0)
         (defun segment-return-values (x) (values x (list x)))
         (defun segment-redefine (x)
           (setq *segment-redefines* (+ *segment-redefines* 1))
           (defun segment-retained (x) (+ x 900)))
         (defun segment-retained (x) {form})"
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
    let symbol = super::super::super::resolve_sym("SEGMENT-RETAINED")
        .unwrap()
        .as_symbol_index();
    let body = Arc::new(
        compile_function("SEGMENT-RETAINED", *params, *forms, &env, false, false).unwrap(),
    );
    registry_put(symbol, Arc::clone(&body));
    let code = TransferCode::compile(Arc::clone(&body)).unwrap();
    assert!(code.has_deopt);
    let value = code
        .run(&[EgclVal::from_single_float(1.5)], &mut env)
        .unwrap();
    assert_eq!(value, EgclVal::from_single_float(2.5));
    assert_eq!(
        code.deopt_count(),
        1,
        "guard must fail after replacement was installed"
    );
    assert!(!registry_get(symbol).is_some_and(|current| Arc::ptr_eq(&current, &body)));
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], value);
    assert_eq!(super::super::super::cp(env.mv[1]), (value, NIL));
    assert_eq!(
        super::super::super::read_eval_all_env("*segment-redefines*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
    assert_eq!(
        super::super::super::read_eval_all_env("(segment-retained 1.5)", &mut env).unwrap(),
        EgclVal::from_single_float(901.5)
    );
}
