// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::TransferCode;
use super::*;
use std::cell::Cell;

#[test]
fn native_v2_protected_builder_refuses_cleanup_bypassing_direct_exits() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for source in [
        "((block exit (unwind-protect (return-from exit x) (identity x))))",
        "((tagbody (unwind-protect (go done) (identity x)) done))",
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = compile_function("PROTECTED-EXIT", *params, *forms, &env, false, false).unwrap();
        assert!(
            matches!(
                egcl_compiler::t2::build::build_from_bytecode_for_transfers(&body),
                Err(egcl_compiler::t2::build::BuildError::Unsupported(
                    "direct exit requires dynamic unwinding"
                ))
            ),
            "{source}"
        );
    }
}

#[test]
fn native_v2_eligible_local_exits_use_direct_native_branches() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for source in [
        "((block exit (return-from exit (values x (list x)))))",
        "((let ((y nil)) (tagbody (go done) (setq y :unreachable) done (setq y x)) (values y (list y))))",
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("DIRECT-LOCAL-EXIT", *params, *forms, &env, false, false)
                .expect("compile direct local exit"),
        );
        assert!(
            egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body).is_ok(),
            "native builder must admit an exit with no crossed dynamic scope: {source}"
        );
        let code = TransferCode::compile(body).expect("native direct local exit");
        egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(*result.as_ref().unwrap(), args[0], "{source}");
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(env.mv[0], args[0]);
        assert_eq!(super::super::super::cp(env.mv[1]).0, args[0]);
    }
}

#[test]
fn native_v2_fallback_runs_nested_caller_cleanups_and_replacing_transfer() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    for replace in [false, true] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            "(setq *caller-cleanups* nil *caller-ticks* 0)
             (defun caller-tick (x) (setq *caller-ticks* (+ *caller-ticks* 1)) x)
             (defun caller-cleanup (name x)
               (setq *caller-cleanups* (cons (cons name x) *caller-cleanups*)))",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        let cleanup_exit = if replace {
            "(throw :replacement (values x (list :second)))"
        } else {
            "nil"
        };
        egcl_rt::rooted!(
            forms = reader::read_from_string(&format!(
                "((let ((saved (list x)))
                (unwind-protect
                  (unwind-protect (progn (caller-tick x) (error x))
                    (caller-cleanup :inner saved) {cleanup_exit})
                  (caller-cleanup :outer saved))))"
            ))
            .unwrap()
            .0
        );
        let original = Arc::new(
            compile_function("PROTECTED-CALLER", *params, *forms, &env, false, false).unwrap(),
        );
        assert!(
            egcl_compiler::t2::build::build_from_bytecode(&original).is_err(),
            "legacy builder must still refuse protected code"
        );
        let ir = egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&original)
            .unwrap_or_else(|error| {
                panic!(
                    "cleanup build replace={replace}: {error:?}; {:?}",
                    original.code
                )
            });
        egcl_compiler::t2::emit::emit_framed_native_cleanups(
            &ir,
            1,
            original.num_slots(),
            2,
            3,
            4,
        )
        .unwrap_or_else(|error| {
            panic!(
                "cleanup emission replace={replace}: {error:?}; {:?}",
                original.code
            )
        });
        let code = TransferCode::compile(original)
            .expect("exceptional protected region has a mapped fallback");
        let token = super::super::super::next_control_token("REPLACEMENT");
        let tag = reader::read_from_string(":replacement").unwrap().0;
        env.catch_stack.push((tag, token.clone()));
        egcl_rt::rooted!(args = vec![super::super::super::arena_str("original error")]);
        let old_address = args[0].to_raw();
        let before = egcl_rt::current_stack().fp();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(args[0].to_raw(), old_address);
        assert_eq!(egcl_rt::current_stack().fp(), before);
        if replace {
            assert!(
                matches!(&*result, Err(EgclError::Internal(t)) if t == &token),
                "{:?}",
                &*result
            );
            assert_eq!(
                super::super::super::take_control_mv(&token, &mut env),
                args[0]
            );
            assert!(env.mv_active && env.mv.len() == 2);
            assert!(env.mv[1].is_cons());
        } else {
            assert!(
                matches!(&*result, Err(EgclError::Signalled { condition, report, backtrace })
                    if super::super::super::condition_matches_handler(&env, *condition, "SIMPLE-ERROR")
                        && report == "original error"
                        && !backtrace.is_empty()),
                "{:?}",
                &*result
            );
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*caller-ticks*", &mut env).unwrap(),
            EgclVal::from_fixnum(1)
        );
        egcl_rt::rooted!(
            log = super::super::super::read_eval_all_env("*caller-cleanups*", &mut env).unwrap()
        );
        egcl_rt::rooted!(entries = super::super::super::list_to_vec(*log));
        assert_eq!(entries.len(), 2);
        for (index, name) in [":OUTER", ":INNER"].into_iter().enumerate() {
            let expected = reader::read_from_string(name).unwrap().0;
            let (key, saved) = super::super::super::cp(entries[index]);
            assert_eq!(key, expected);
            assert_eq!(
                super::super::super::cp(saved).0,
                args[0],
                "cleanup-only local survived capture and GC"
            );
        }
        env.catch_stack.pop();
    }
}

#[test]
fn native_v2_fallback_propagates_without_replaying_the_original_definition() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    for (case, result_form) in [
        "(values x (list :secondary))",
        "(car 7)",
        "(throw :owned-tag (values x (list :secondary)))",
    ]
    .into_iter()
    .enumerate()
    {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *owned-ticks* 0 *owned-calls* 0 *owned-cleanups* 0)
                 (defun owned-tick (x)
                   (setq *owned-ticks* (+ *owned-ticks* 1)) x)
                 (defun owned-target (x)
                   (setq *owned-calls* (+ *owned-calls* 1))
                   (defun owned-caller (x) 99)
                   (unwind-protect {result_form}
                     (setq *owned-cleanups* (+ *owned-cleanups* 1))))"
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(
            body = reader::read_from_string("((owned-target (owned-tick x)))")
                .unwrap()
                .0
        );
        let original =
            Arc::new(compile_function("OWNED-CALLER", *params, *body, &env, false, false).unwrap());
        let code = TransferCode::compile(original).expect("tagged Invoke caller");
        let token = super::super::super::next_control_token("OWNED-CATCH");
        let tag = reader::read_from_string(":owned-tag").unwrap().0;
        env.catch_stack.push((tag, token.clone()));
        egcl_rt::rooted!(
            args = vec![super::super::super::arena_cons(
                EgclVal::from_fixnum(41),
                NIL
            )]
        );
        let old_address = args[0].to_raw();
        let stack = egcl_rt::current_stack();
        let before = stack.fp();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(
            stack.fp(),
            before,
            "invocation retired its frame exactly once"
        );
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(args[0].to_raw(), old_address, "argument actually relocated");
        match case {
            0 => {
                assert_eq!(result.as_ref().unwrap(), &args[0]);
                assert!(env.mv_active && env.mv.len() == 2);
                assert_eq!(env.mv[0], args[0]);
                assert!(env.mv[1].is_cons());
            }
            1 => assert!(matches!(&*result, Err(EgclError::Signalled { .. }))),
            2 => {
                assert!(
                    matches!(&*result, Err(EgclError::Internal(t)) if t == &token),
                    "{result:?}",
                    result = &*result
                );
                assert_eq!(
                    super::super::super::take_control_mv(&token, &mut env),
                    args[0]
                );
                assert!(env.mv_active && env.mv.len() == 2);
                assert!(env.mv[1].is_cons());
            }
            _ => unreachable!(),
        }
        for counter in ["*owned-ticks*", "*owned-calls*", "*owned-cleanups*"] {
            assert_eq!(
                super::super::super::read_eval_all_env(counter, &mut env).unwrap(),
                EgclVal::from_fixnum(1),
                "{counter} must not be replayed"
            );
        }
        assert_eq!(
            super::super::super::read_eval_all_env("(owned-caller nil)", &mut env).unwrap(),
            EgclVal::from_fixnum(99),
            "replacement exists, but recovery used the retained original"
        );
        env.catch_stack.pop();
        assert!(!native_error_pending());
        assert!(egcl_rt::native_transfer::current_segment().is_null());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_fallback_preserves_enclosing_execution_state() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(body = reader::read_from_string("((list x))").unwrap().0);
    let original =
        Arc::new(compile_function("OWNED-NESTED", *params, *body, &env, false, false).unwrap());
    let code = TransferCode::compile(original).unwrap();
    let _native = NativeEnvGuard::enter(&mut env);
    env.block_stack
        .push(("OUTER".into(), "outer-block-token".into()));
    env.tag_stack
        .push(("OUTER-TAG".into(), "outer-tag-token".into()));
    egcl_rt::rooted!(datum = super::super::super::arena_cons(EgclVal::from_fixnum(37), NIL));
    let old_address = datum.to_raw();
    NATIVE_ERROR.with(|slot| slot.set_first(EgclError::UnboundVariable(*datum)));
    let old_env = NATIVE_ENV.with(Cell::get);
    let depth = NATIVE_DEPTH.with(Cell::get);
    let before = egcl_rt::current_stack().fp();
    egcl_rt::rooted!(result = code.run(&[*datum], &mut env).unwrap());
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(super::super::super::cp(*result).0, *datum);
    assert_ne!(datum.to_raw(), old_address);
    assert_eq!(NATIVE_ENV.with(Cell::get), old_env);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
    assert_eq!(egcl_rt::current_stack().fp(), before);
    assert_eq!(
        env.block_stack,
        [("OUTER".into(), "outer-block-token".into())]
    );
    assert_eq!(
        env.tag_stack,
        [("OUTER-TAG".into(), "outer-tag-token".into())]
    );
    assert!(
        matches!(NATIVE_ERROR.with(|slot| slot.take()), Some(EgclError::UnboundVariable(value)) if value == *datum)
    );
    assert!(code.run(&[], &mut env).is_err());
    assert_eq!(egcl_rt::current_stack().fp(), before);
    assert_eq!(NATIVE_ENV.with(Cell::get), old_env);
}
