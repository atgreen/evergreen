use super::super::native_transfer_entry::TransferCode;
use super::*;
use std::cell::Cell;

#[test]
fn native_v2_protected_builder_refuses_cleanup_bypassing_direct_exits() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    for source in [
        "((block exit (unwind-protect (return-from exit x) (identity x))))",
        "((tagbody (unwind-protect (go done) (identity x)) done))",
    ] {
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = compile_function("PROTECTED-EXIT", *params, *forms, &env, false, false).unwrap();
        assert!(
            matches!(
                torcl_compiler::t2::build::build_from_bytecode_for_transfers(&body),
                Err(torcl_compiler::t2::build::BuildError::Unsupported(
                    "direct exit requires dynamic unwinding"
                ))
            ),
            "{source}"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_fallback_runs_nested_caller_cleanups_and_replacing_transfer() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(torcl_rt::native_transfer::is_supported());
    for replace in [false, true] {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            "(setq *caller-cleanups* nil *caller-ticks* 0)
             (defun caller-tick (x) (setq *caller-ticks* (+ *caller-ticks* 1)) x)
             (defun caller-cleanup (name x)
               (setq *caller-cleanups* (cons (cons name x) *caller-cleanups*)))",
            &mut env,
        )
        .unwrap();
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        let cleanup_exit = if replace {
            "(throw :replacement (values x (list :second)))"
        } else {
            "nil"
        };
        torcl_rt::rooted!(
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
            torcl_compiler::t2::build::build_from_bytecode(&original).is_err(),
            "legacy builder must still refuse protected code"
        );
        let ir = torcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&original)
            .unwrap_or_else(|error| {
                panic!(
                    "cleanup build replace={replace}: {error:?}; {:?}",
                    original.code
                )
            });
        torcl_compiler::t2::emit::emit_framed_native_cleanups(
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
        env.catch_stack
            .push((super::super::super::val_as_str(tag), token.clone()));
        torcl_rt::rooted!(args = vec![super::super::super::arena_str("original error")]);
        let old_address = args[0].to_raw();
        let before = torcl_rt::current_stack().fp();
        torcl_rt::rooted!(result = code.run(&args, &mut env));
        HeapCollector::new().minor_gc().unwrap();
        assert_ne!(args[0].to_raw(), old_address);
        assert_eq!(torcl_rt::current_stack().fp(), before);
        if replace {
            assert!(
                matches!(&*result, Err(TorclError::Internal(t)) if t == &token),
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
                matches!(&*result, Err(TorclError::Internal(message)) if message == "ERROR: original error"),
                "{:?}",
                &*result
            );
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*caller-ticks*", &mut env).unwrap(),
            TorclVal::from_fixnum(1)
        );
        torcl_rt::rooted!(
            log = super::super::super::read_eval_all_env("*caller-cleanups*", &mut env).unwrap()
        );
        torcl_rt::rooted!(entries = super::super::super::list_to_vec(*log));
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
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_fallback_propagates_without_replaying_the_original_definition() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(torcl_rt::native_transfer::is_supported());
    for (case, result_form) in [
        "(values x (list :secondary))",
        "(car 7)",
        "(throw :owned-tag (values x (list :secondary)))",
    ]
    .into_iter()
    .enumerate()
    {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
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
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            body = reader::read_from_string("((owned-target (owned-tick x)))")
                .unwrap()
                .0
        );
        let original =
            Arc::new(compile_function("OWNED-CALLER", *params, *body, &env, false, false).unwrap());
        let code = TransferCode::compile(original).expect("tagged Invoke caller");
        let token = super::super::super::next_control_token("OWNED-CATCH");
        let tag = reader::read_from_string(":owned-tag").unwrap().0;
        env.catch_stack
            .push((super::super::super::val_as_str(tag), token.clone()));
        torcl_rt::rooted!(
            args = vec![super::super::super::arena_cons(
                TorclVal::from_fixnum(41),
                NIL
            )]
        );
        let old_address = args[0].to_raw();
        let stack = torcl_rt::current_stack();
        let before = stack.fp();
        torcl_rt::rooted!(result = code.run(&args, &mut env));
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
            1 => assert!(matches!(&*result, Err(TorclError::Signalled { .. }))),
            2 => {
                assert!(
                    matches!(&*result, Err(TorclError::Internal(t)) if t == &token),
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
                TorclVal::from_fixnum(1),
                "{counter} must not be replayed"
            );
        }
        assert_eq!(
            super::super::super::read_eval_all_env("(owned-caller nil)", &mut env).unwrap(),
            TorclVal::from_fixnum(99),
            "replacement exists, but recovery used the retained original"
        );
        env.catch_stack.pop();
        assert!(!native_error_pending());
        assert!(torcl_rt::native_transfer::current_segment().is_null());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_fallback_preserves_enclosing_execution_state() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(torcl_rt::native_transfer::is_supported());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    torcl_rt::rooted!(body = reader::read_from_string("((list x))").unwrap().0);
    let original =
        Arc::new(compile_function("OWNED-NESTED", *params, *body, &env, false, false).unwrap());
    let code = TransferCode::compile(original).unwrap();
    let _native = NativeEnvGuard::enter(&mut env);
    env.block_stack
        .push(("OUTER".into(), "outer-block-token".into()));
    env.tag_stack
        .push(("OUTER-TAG".into(), "outer-tag-token".into()));
    torcl_rt::rooted!(datum = super::super::super::arena_cons(TorclVal::from_fixnum(37), NIL));
    let old_address = datum.to_raw();
    NATIVE_ERROR.with(|slot| slot.set_first(TorclError::UnboundVariable(*datum)));
    let old_env = NATIVE_ENV.with(Cell::get);
    let depth = NATIVE_DEPTH.with(Cell::get);
    let before = torcl_rt::current_stack().fp();
    torcl_rt::rooted!(result = code.run(&[*datum], &mut env).unwrap());
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(super::super::super::cp(*result).0, *datum);
    assert_ne!(datum.to_raw(), old_address);
    assert_eq!(NATIVE_ENV.with(Cell::get), old_env);
    assert_eq!(NATIVE_DEPTH.with(Cell::get), depth);
    assert_eq!(torcl_rt::current_stack().fp(), before);
    assert_eq!(
        env.block_stack,
        [("OUTER".into(), "outer-block-token".into())]
    );
    assert_eq!(
        env.tag_stack,
        [("OUTER-TAG".into(), "outer-tag-token".into())]
    );
    assert!(
        matches!(NATIVE_ERROR.with(|slot| slot.take()), Some(TorclError::UnboundVariable(value)) if value == *datum)
    );
    assert!(code.run(&[], &mut env).is_err());
    assert_eq!(torcl_rt::current_stack().fp(), before);
    assert_eq!(NATIVE_ENV.with(Cell::get), old_env);
}
