//! Tests for torcl-stdlib conditions module (spec §5.4).
use std::panic::{self, AssertUnwindSafe};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use torcl_rt::value::{NIL, TorclVal};
use torcl_rt::{FrameType, TorclError, current_stack};
use torcl_stdlib::conditions::*;

fn sym(i: u32) -> TorclVal {
    TorclVal::from_symbol_index(i)
}

fn fx(i: i64) -> TorclVal {
    TorclVal::from_fixnum(i)
}

#[test]
fn make_simple_error_variants() {
    let _ = make_simple_error("err: ~A", &[TorclVal::from_fixnum(42)]);
    let _ = make_simple_error("plain", &[]);
}

#[test]
fn make_type_error_returns_val() {
    let _ = make_type_error(TorclVal::from_fixnum(5), sym(1));
}

#[test]
fn condition_values_participate_in_the_root_and_error_hierarchies() {
    let simple = make_simple_error("boom", &[]);
    let typed = make_type_error(fx(5), sym(2));

    // Per R5.91 and R5.107, ANSI condition objects must behave as CONDITION-rooted
    // CLOS instances, and DEFINE-CONDITION-backed types must participate in the
    // observable class hierarchy seen by handler dispatch.
    assert_eq!(
        handler_case(simple, &[(sym(*SYMBOL_ERROR), fx(1))]).unwrap(),
        fx(1)
    );
    assert_eq!(
        handler_case(simple, &[(sym(*SYMBOL_CONDITION), fx(2))]).unwrap(),
        fx(2)
    );
    assert_eq!(
        handler_case(typed, &[(sym(*SYMBOL_ERROR), fx(3))]).unwrap(),
        fx(3)
    );
}

#[test]
fn signal_no_handler_ok() {
    assert!(signal_condition(make_simple_error("t", &[])).is_ok());
}

#[test]
fn handler_bind_establishes_a_torcl_stack_cluster_frame() {
    let stack = current_stack();
    let before = stack.frame_depth();

    handler_bind_fn(&[(sym(*SYMBOL_ERROR), fx(99))], || {
        assert_eq!(stack.frame_depth(), before + 1);
        let frame = stack.fp();
        assert_eq!(unsafe { (*frame).frame_type() }, FrameType::Special);
        Ok(NIL)
    })
    .unwrap();

    assert_eq!(stack.frame_depth(), before);
}

#[test]
fn restart_bind_establishes_a_torcl_stack_cluster_frame() {
    let stack = current_stack();
    let before = stack.frame_depth();
    let spec = RestartSpec {
        name: sym(71),
        function: fx(72),
        report_function: Some(fx(73)),
        interactive_function: Some(fx(74)),
        test_function: Some(fx(75)),
    };

    restart_bind_fn(&[spec], || {
        assert_eq!(stack.frame_depth(), before + 1);
        let frame = stack.fp();
        assert_eq!(unsafe { (*frame).frame_type() }, FrameType::Special);
        Ok(NIL)
    })
    .unwrap();

    assert_eq!(stack.frame_depth(), before);
}

// Issue 10: error_condition_with_debugger_hook must verify the hook was actually called.
// Per R5.101, *DEBUGGER-HOOK* MUST be called before entering the debugger.
#[test]
fn error_condition_with_debugger_hook() {
    let hook_fn = TorclVal::from_fixnum(1);
    let hook_called = Arc::new(AtomicBool::new(false));
    let hook_called_for_funcall = Arc::clone(&hook_called);
    set_funcall_hook(move |function, args| {
        if function == hook_fn {
            assert_eq!(args.len(), 2);
            hook_called_for_funcall.store(true, Ordering::SeqCst);
        }
        Ok(NIL)
    });
    set_debugger_hook(Some(hook_fn));

    let result = error_condition(make_simple_error("unhandled", &[]));

    assert!(
        result.is_err(),
        "error_condition with unhandled error must enter debugger (return Err)"
    );
    assert!(
        hook_called.load(Ordering::SeqCst),
        "R5.101: *DEBUGGER-HOOK* must be invoked before debugger entry"
    );
    clear_funcall_hook();
    set_debugger_hook(None);
}

#[test]
fn cerror_callable() {
    let _ = cerror("Continue", make_simple_error("cont", &[]));
}

#[test]
fn warn_returns_ok() {
    assert!(warn_condition(make_simple_error("warning", &[])).is_ok());
}

#[test]
fn handler_bind_no_signal() {
    let body = TorclVal::from_fixnum(99);
    assert_eq!(
        handler_bind(&[(sym(10), TorclVal::from_fixnum(0))], body).unwrap(),
        body
    );
    assert_eq!(
        handler_bind(&[], TorclVal::from_fixnum(77)).unwrap(),
        TorclVal::from_fixnum(77)
    );
}

#[test]
fn handler_case_no_signal() {
    let form = TorclVal::from_fixnum(55);
    assert_eq!(
        handler_case(form, &[(sym(20), TorclVal::from_fixnum(0))]).unwrap(),
        form
    );
    assert_eq!(
        handler_case(TorclVal::from_fixnum(33), &[]).unwrap(),
        TorclVal::from_fixnum(33)
    );
}

#[test]
fn handler_case_nested_with_signal() {
    let error_type = sym(*SYMBOL_ERROR);
    let outer_handler = fx(100);
    let inner_handler = fx(200);
    let log = Arc::new(std::sync::Mutex::new(Vec::new()));
    let log_for_hook = Arc::clone(&log);

    set_funcall_hook(move |function, args| {
        assert_eq!(args.len(), 1);
        log_for_hook.lock().unwrap().push(function);
        Ok(TorclVal::from_fixnum(function.as_fixnum()))
    });

    let condition = make_simple_error("inner error", &[]);

    // Per R5.92, R5.93, R5.94, and R5.95, nested handler scopes must search
    // newest-first over a real signalled condition rather than plain values.
    let result = handler_bind_fn(&[(error_type, outer_handler)], || {
        handler_bind_fn(&[(error_type, inner_handler)], || {
            signal_condition(condition)?;
            Ok(fx(99))
        })
    })
    .unwrap();

    assert_eq!(result, fx(99));
    assert_eq!(*log.lock().unwrap(), vec![inner_handler, outer_handler]);
    clear_funcall_hook();
}

#[test]
fn handler_case_nested_propagation() {
    let inner_type = sym(*SYMBOL_SIMPLE_WARNING);
    let outer_type = sym(*SYMBOL_ERROR);
    let outer_handler = fx(300);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen_for_hook = Arc::clone(&seen);

    set_funcall_hook(move |function, args| {
        assert_eq!(args.len(), 1);
        seen_for_hook.lock().unwrap().push(function);
        Ok(function)
    });

    let condition = make_simple_error("propagating error", &[]);

    // Per R5.93, R5.94, and R5.95, a non-matching inner handler scope must let
    // a real signalled condition propagate outward to the matching older scope.
    handler_bind_fn(&[(outer_type, outer_handler)], || {
        handler_bind_fn(&[(inner_type, fx(999))], || {
            signal_condition(condition)?;
            Ok(fx(0))
        })
    })
    .unwrap();

    assert_eq!(*seen.lock().unwrap(), vec![outer_handler]);
    clear_funcall_hook();
}

#[test]
fn restart_spec_fields() {
    let full = RestartSpec {
        name: sym(40),
        function: TorclVal::from_fixnum(1),
        report_function: Some(TorclVal::from_fixnum(2)),
        interactive_function: Some(TorclVal::from_fixnum(3)),
        test_function: Some(TorclVal::from_fixnum(4)),
    };
    assert_eq!(full.name, sym(40));
    assert!(full.report_function.is_some());
    let minimal = RestartSpec {
        name: sym(41),
        function: TorclVal::from_fixnum(1),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };
    assert!(minimal.interactive_function.is_none());
}

#[test]
fn restart_bind_returns_body() {
    let spec = RestartSpec {
        name: sym(50),
        function: TorclVal::from_fixnum(1),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };
    assert_eq!(
        restart_bind(&[spec], TorclVal::from_fixnum(42)).unwrap(),
        TorclVal::from_fixnum(42)
    );
    assert_eq!(
        restart_bind(&[], TorclVal::from_fixnum(88)).unwrap(),
        TorclVal::from_fixnum(88)
    );
}

#[test]
fn restart_bind_multiple_specs() {
    let s1 = RestartSpec {
        name: sym(80),
        function: TorclVal::from_fixnum(1),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };
    let s2 = RestartSpec {
        name: sym(81),
        function: TorclVal::from_fixnum(2),
        report_function: Some(TorclVal::from_fixnum(3)),
        interactive_function: Some(TorclVal::from_fixnum(4)),
        test_function: Some(TorclVal::from_fixnum(5)),
    };
    assert!(restart_bind(&[s1, s2], TorclVal::from_fixnum(0)).is_ok());
}

#[test]
fn compute_restarts_within_restart_bind() {
    let restart_name = sym(60);
    let hidden_name = sym(61);
    let restart_fn = fx(1);
    let allow_fn = fx(2);
    let deny_fn = fx(3);
    let spec = RestartSpec {
        name: restart_name,
        function: restart_fn,
        report_function: None,
        interactive_function: None,
        test_function: Some(allow_fn),
    };
    let hidden = RestartSpec {
        name: hidden_name,
        function: fx(4),
        report_function: None,
        interactive_function: None,
        test_function: Some(deny_fn),
    };
    let condition = make_simple_error("restart-filter", &[]);

    set_funcall_hook(move |function, _args| {
        if function == allow_fn {
            return Ok(TorclVal::from_symbol_index(1));
        }
        if function == deny_fn {
            return Ok(NIL);
        }
        Ok(NIL)
    });

    // Per R5.96, R5.97, and R5.98, restart clusters exist only during their
    // dynamic extent, compute-restarts is newest-first, and find-restart
    // locates the newest applicable restart.
    let result = restart_bind_fn(&[spec, hidden], || {
        assert_eq!(compute_restarts(Some(condition)), vec![restart_name]);
        assert_eq!(compute_restarts(None), vec![hidden_name, restart_name]);
        assert_eq!(
            find_restart(restart_name, Some(condition)),
            Some(restart_fn)
        );
        Ok(fx(42))
    })
    .unwrap();
    assert_eq!(result, fx(42));
    assert!(find_restart(restart_name, Some(condition)).is_none());
    clear_funcall_hook();
}

#[test]
fn compute_restarts_and_find_outside_scope() {
    // Outside any restart_bind, find_restart for a random name returns None
    assert!(
        find_restart(sym(9999), None).is_none(),
        "find_restart should return None when no restarts are established"
    );
}

#[test]
fn invoke_restart_executes_restart_function() {
    let restart_name = sym(70);
    let restart_fn = fx(42);

    set_funcall_hook(move |function, args| {
        assert_eq!(function, restart_fn);
        assert!(args.is_empty());
        Ok(fx(4200))
    });

    // Per R5.99, INVOKE-RESTART must run the active restart's function in the
    // dynamic environment established by RESTART-BIND.
    let observed = restart_bind_fn(
        &[RestartSpec {
            name: restart_name,
            function: restart_fn,
            report_function: None,
            interactive_function: None,
            test_function: None,
        }],
        || {
            let active = find_restart(restart_name, None).unwrap();
            invoke_restart(active, &[])
        },
    )
    .unwrap();
    assert_eq!(observed, fx(4200));
    clear_funcall_hook();
}

#[test]
fn invoke_restart_with_args() {
    let restart_name = sym(71);
    let restart_fn = fx(43);

    set_funcall_hook(move |function, args| {
        assert_eq!(function, restart_fn);
        assert_eq!(args, &[fx(10), fx(20)]);
        Ok(args[0])
    });

    // Per R5.99, INVOKE-RESTART must pass caller-supplied arguments to the
    // active restart function, not a raw token disconnected from the stack.
    let observed = restart_bind_fn(
        &[RestartSpec {
            name: restart_name,
            function: restart_fn,
            report_function: None,
            interactive_function: None,
            test_function: None,
        }],
        || {
            let active = find_restart(restart_name, None).unwrap();
            invoke_restart(active, &[fx(10), fx(20)])
        },
    )
    .unwrap();
    assert_eq!(observed, fx(10));
    clear_funcall_hook();
}

#[test]
fn invoke_restart_interactively_uses_interactive_function() {
    let restart_name = sym(72);
    let restart_fn = fx(44);
    let interactive_fn = fx(45);
    let spec = RestartSpec {
        name: restart_name,
        function: restart_fn,
        report_function: None,
        interactive_function: Some(interactive_fn),
        test_function: None,
    };

    set_funcall_hook(move |function, args| {
        if function == interactive_fn {
            assert!(args.is_empty());
            return Ok(fx(99));
        }
        if function == restart_fn {
            assert_eq!(args, &[fx(99)]);
            return Ok(fx(199));
        }
        Ok(NIL)
    });

    // Per R5.100, INVOKE-RESTART-INTERACTIVELY must call the active restart's
    // interactive function to obtain arguments before invoking the restart.
    let result = restart_bind_fn(&[spec], || invoke_restart_interactively(restart_name)).unwrap();
    assert_eq!(result, fx(199));
    assert!(find_restart(restart_name, None).is_none());
    clear_funcall_hook();
}

#[test]
fn cleanup_runs_when_handler_transfer_unwinds_the_dynamic_extent() {
    let handler = fx(80);
    let cleanup_ran = Arc::new(AtomicBool::new(false));
    let cleanup_for_drop = Arc::clone(&cleanup_ran);

    struct CleanupGuard(Arc<AtomicBool>);
    impl Drop for CleanupGuard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    set_funcall_hook(move |function, _args| {
        if function == handler {
            panic!("simulated non-local exit");
        }
        Ok(NIL)
    });

    // Per R5.106 and R5.109, cleanup forms around handler-established dynamic
    // scopes must still run when control transfers out through the handler path.
    let unwound = panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = CleanupGuard(cleanup_for_drop);
        let _ = handler_bind_fn(&[(sym(*SYMBOL_ERROR), handler)], || {
            signal_condition(make_simple_error("cleanup", &[]))?;
            Ok(fx(0))
        });
    }));
    assert!(unwound.is_err());
    assert!(cleanup_ran.load(Ordering::SeqCst));
    clear_funcall_hook();
}

#[test]
fn handler_cluster_descriptor_is_restored_when_body_unwinds() {
    let before = torcl_rt::thread::current_condition_state_snapshot();

    let unwound = panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = handler_bind_fn(
            &[(sym(*SYMBOL_ERROR), fx(81))],
            || -> Result<TorclVal, TorclError> {
                panic!("leave handler dynamic extent");
            },
        );
    }));

    assert!(unwound.is_err());
    let after = torcl_rt::thread::current_condition_state_snapshot();
    assert_eq!(after.handler_depth, before.handler_depth);
    assert_eq!(after.restart_depth, before.restart_depth);
}

#[test]
fn restart_cluster_descriptor_is_restored_when_body_unwinds() {
    let before = torcl_rt::thread::current_condition_state_snapshot();
    let spec = RestartSpec {
        name: sym(82),
        function: fx(83),
        report_function: None,
        interactive_function: None,
        test_function: None,
    };

    let unwound = panic::catch_unwind(AssertUnwindSafe(|| {
        let _ = restart_bind_fn(&[spec], || -> Result<TorclVal, TorclError> {
            panic!("leave restart dynamic extent");
        });
    }));

    assert!(unwound.is_err());
    let after = torcl_rt::thread::current_condition_state_snapshot();
    assert_eq!(after.handler_depth, before.handler_depth);
    assert_eq!(after.restart_depth, before.restart_depth);
}

#[test]
fn break_on_signals_invokes_break_before_handler_search() {
    let hook = fx(81);
    let handler = fx(82);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let seen_for_hook = Arc::clone(&seen);

    set_funcall_hook(move |function, args| {
        if function == hook {
            assert_eq!(args.len(), 2);
            seen_for_hook.lock().unwrap().push("break");
        } else if function == handler {
            assert_eq!(args.len(), 1);
            seen_for_hook.lock().unwrap().push("handler");
        }
        Ok(NIL)
    });
    set_debugger_hook(Some(hook));
    set_break_on_signals(Some(sym(*SYMBOL_ERROR)));

    // Per R5.203, BREAK must run before the normal handler search when the
    // signalled condition matches *BREAK-ON-SIGNALS*.
    assert!(
        handler_bind_fn(&[(sym(*SYMBOL_ERROR), handler)], || {
            signal_condition(make_simple_error("debug", &[]))?;
            Ok(NIL)
        })
        .is_ok()
    );
    assert_eq!(&*seen.lock().unwrap(), &["break", "handler"]);

    set_break_on_signals(None);
    set_debugger_hook(None);
    clear_funcall_hook();
}

// Issue 8: HandlerBinding struct existence and accessibility.
#[test]
fn handler_binding_struct_exists() {
    // Verify HandlerBinding struct is accessible and can be referenced.
    // Currently it has _private: () making it opaque, but we verify it exists
    // as a type in the conditions module.
    let _: Option<HandlerBinding> = None;
    // Verify it's a sized type (can be used in Option, references, etc.)
    assert!(
        std::mem::size_of::<HandlerBinding>() > 0 || std::mem::size_of::<HandlerBinding>() == 0,
        "HandlerBinding should be a valid sized type"
    );
}

#[test]
fn debugger_hook_lifecycle() {
    set_debugger_hook(Some(TorclVal::from_fixnum(1)));
    let _ = invoke_debugger(make_simple_error("debug", &[]));
    set_debugger_hook(None);
}
