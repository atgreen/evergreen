use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_live_handlers_select_before_native_cleanup_and_outer_fallback() {
    use super::super::super::{
        RestartFunction, next_restart_id, restart_invoked_id, take_restart_args,
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
        let id = next_restart_id();
        if restart {
            env.restarts.push(RestartEntry {
                name: "RECOVER".into(),
                captured_blocks: Vec::new(),
                captured_tags: Vec::new(),
                function: RestartFunction::ContinueNil,
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
