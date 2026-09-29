use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_live_handlers_select_before_native_cleanup_and_outer_fallback() {
    use super::super::super::{
        RestartFunction, RetainedRestarts, invoke_restart_function_in, next_restart_id,
        restart_invoked_id, take_restart_args,
    };
    use super::super::native_transfer_entry::{
        TransferCode, take_native_cleanup_count, take_native_fallback_count,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for restart in [false, true] {
        let mut env = Env::new(false);
        torcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            "(setq *live-handler-calls* 0 *live-cleanups* 0 *live-seen* nil)
             (defun live-decline (c)
               (setq *live-at-signal* *live-cleanups*)
               (setq *live-handler-calls* (+ *live-handler-calls* 1))
               (setq *live-seen* (slot-value c 'datum))
               (%force-minor-gc-for-test))
             (defun live-restart (c)
               (live-decline c)
               (invoke-restart 'recover (slot-value c 'datum)))
             (defun live-recover (x)
               (setq *live-restart-cleanups* *live-cleanups*)
               (setq *live-restart-visible* (find-restart 'recover))
               (%force-minor-gc-for-test)
               (values x (list x)))
             (defun live-cleanup ()
               (%force-minor-gc-for-test)
               (setq *live-cleanups* (+ *live-cleanups* 1)))",
            &mut env,
        )
        .unwrap();
        let handler = resolve_sym(if restart {
            "LIVE-RESTART"
        } else {
            "LIVE-DECLINE"
        })
        .unwrap();
        env.handlers.push(HandlerCluster {
            entries: vec![HandlerEntry {
                type_name: "TYPE-ERROR".into(),
                handler: HandlerImpl::Function(handler),
            }],
        });
        torcl_rt::rooted!(restart_form = reader::read_from_string("#'live-recover").unwrap().0);
        let id = next_restart_id();
        if restart {
            env.restarts.push(RestartEntry {
                name: "RECOVER".into(),
                captured_blocks: Vec::new(),
                captured_tags: Vec::new(),
                function: RestartFunction::FunctionForm {
                    function_form: *restart_form,
                    captured_frame: Arc::clone(&env.frame),
                },
                interactive_function: None,
                test_function: None,
                unwind_on_invoke: true,
                group_base: 0,
                id,
                restart_obj: NIL,
                report: NIL,
            });
        }
        torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        torcl_rt::rooted!(
            forms = reader::read_from_string("((unwind-protect (symbol-value x) (live-cleanup)))")
                .unwrap()
                .0
        );
        let body = Arc::new(
            compile_function("LIVE-SIGNALING", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("native caller with live enclosing handlers");
        torcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
        let before = args[0].to_raw();
        take_native_cleanup_count();
        take_native_fallback_count();
        torcl_rt::rooted!(result = code.run(&args, &mut env));
        assert!(result.is_err());
        assert_ne!(args[0].to_raw(), before);
        assert_eq!(
            take_native_cleanup_count(),
            1,
            "cleanup must run natively; restart={restart}, result={:?}",
            &*result
        );
        assert_eq!(
            take_native_fallback_count(),
            1,
            "outer destination needs fallback"
        );
        assert_eq!(env.handlers.len(), 1);
        assert_eq!(
            env.lookup_var("*LIVE-AT-SIGNAL*"),
            Some(TorclVal::from_fixnum(0))
        );
        assert_eq!(env.restarts.len(), usize::from(restart));
        assert_eq!(
            env.lookup_var("*LIVE-HANDLER-CALLS*"),
            Some(TorclVal::from_fixnum(1))
        );
        assert_eq!(
            env.lookup_var("*LIVE-CLEANUPS*"),
            Some(TorclVal::from_fixnum(1))
        );
        assert_eq!(env.lookup_var("*LIVE-SEEN*"), Some(args[0]));
        let error = result.as_ref().unwrap_err();
        if restart {
            assert_eq!(restart_invoked_id(error), Some(id));
            torcl_rt::rooted!(restart_args = take_restart_args(id));
            assert_eq!(&*restart_args, &*args);
            // Consume the transfer using the same retained-entry and invocation
            // path as an enclosing RESTART-CASE, after its bindings retire.
            let mut retained = RetainedRestarts(env.restarts.split_off(0));
            torcl_rt::rooted_ref!(_retained = &mut retained);
            let entry = &retained.0[0];
            torcl_rt::rooted!(
                answer = invoke_restart_function_in(
                    &entry.function,
                    &restart_args,
                    &mut env,
                    Some((&entry.captured_blocks, &entry.captured_tags)),
                )
                .unwrap()
            );
            assert_eq!(*answer, args[0]);
            assert!(env.mv_active);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(env.mv[0], args[0]);
            assert_eq!(super::super::super::cp(env.mv[1]).0, args[0]);
            assert_eq!(
                env.lookup_var("*LIVE-RESTART-CLEANUPS*"),
                Some(TorclVal::from_fixnum(1))
            );
            assert_eq!(env.lookup_var("*LIVE-RESTART-VISIBLE*"), Some(NIL));
        } else {
            let TorclError::Signalled { condition, .. } = error else {
                panic!("declined error must not be signaled again: {error:?}");
            };
            let datum = resolve_sym("DATUM").unwrap();
            assert_eq!(
                super::super::super::read_slot_value(*condition, datum, &env).unwrap(),
                args[0]
            );
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_handler_replacement_signals_only_older_clusters_before_cleanup() {
    use super::super::native_transfer_entry::{
        TransferCode, take_native_cleanup_count, take_native_fallback_count,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    torcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *replace-inner* 0 *replace-outer* 0 *replace-cleanups* 0)
         (defun replace-inner (c)
           (setq *replace-inner* (+ *replace-inner* 1))
           (%force-minor-gc-for-test)
           (symbol-value 27))
         (defun replace-outer (c)
           (setq *replace-outer* (+ *replace-outer* 1))
           (setq *replace-at-signal* *replace-cleanups*)
           (setq *replace-datum* (slot-value c 'datum))
           (%force-minor-gc-for-test))
         (defun replace-cleanup ()
           (%force-minor-gc-for-test)
           (setq *replace-cleanups* (+ *replace-cleanups* 1)))",
        &mut env,
    )
    .unwrap();
    for name in ["REPLACE-OUTER", "REPLACE-INNER"] {
        env.handlers.push(HandlerCluster {
            entries: vec![HandlerEntry {
                type_name: "TYPE-ERROR".into(),
                handler: HandlerImpl::Function(resolve_sym(name).unwrap()),
            }],
        });
    }
    torcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    torcl_rt::rooted!(
        forms = reader::read_from_string("((unwind-protect (symbol-value x) (replace-cleanup)))")
            .unwrap()
            .0
    );
    let body = Arc::new(
        compile_function("REPLACING-HANDLER", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).unwrap();
    torcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
    let before = args[0].to_raw();
    take_native_cleanup_count();
    take_native_fallback_count();
    torcl_rt::rooted!(result = code.run(&args, &mut env));
    assert_ne!(args[0].to_raw(), before);
    assert_eq!(
        take_native_cleanup_count(),
        1,
        "replacement result: {:?}; inner={:?}, outer={:?}",
        &*result,
        env.lookup_var("*REPLACE-INNER*"),
        env.lookup_var("*REPLACE-OUTER*")
    );
    assert_eq!(take_native_fallback_count(), 1);
    assert_eq!(env.handlers.len(), 2);
    for name in ["*REPLACE-INNER*", "*REPLACE-OUTER*", "*REPLACE-CLEANUPS*"] {
        assert_eq!(
            env.lookup_var(name),
            Some(TorclVal::from_fixnum(1)),
            "{name}"
        );
    }
    assert_eq!(
        env.lookup_var("*REPLACE-AT-SIGNAL*"),
        Some(TorclVal::from_fixnum(0))
    );
    assert_eq!(
        env.lookup_var("*REPLACE-DATUM*"),
        Some(TorclVal::from_fixnum(27))
    );
    let TorclError::Signalled { condition, .. } = result.as_ref().unwrap_err() else {
        panic!("replacement must already be signaled: {:?}", &*result);
    };
    let datum = resolve_sym("DATUM").unwrap();
    assert_eq!(
        super::super::super::read_slot_value(*condition, datum, &env).unwrap(),
        TorclVal::from_fixnum(27)
    );
}
