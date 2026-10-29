// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_entry;
use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_protected_child_catch_and_cleanup_preserve_values() {
    use super::super::super::native_transfer_entry::root_publication::{
        describe_bails, take_adapter_crossings, take_completeness,
    };
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    take_adapter_crossings();
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for (local, fallback) in [(true, false), (false, false), (true, true)] {
        let body = if fallback {
            "((catch :protected (unwind-protect (if fail (protected-child-throw x) x) (protected-child-cleanup))) (protected-child-post x) (values x (list x)))"
        } else if local {
            "((catch :protected (unwind-protect (if fail (protected-child-throw x) (values x (list x))) (protected-child-cleanup))))"
        } else {
            "((unwind-protect (if fail (protected-child-throw x) (values x (list x))) (protected-child-cleanup)))"
        };
        super::super::super::super::read_eval_all_env(
            &format!(
                "(setq *protected-child-cleanups* 0)
             (defun protected-child-cleanup ()
               (setq *protected-child-cleanups* (+ *protected-child-cleanups* 1))
               (%force-minor-gc-for-test))
             (defun protected-child-post (x) (%force-minor-gc-for-test) (list x))
             (defun protected-child-throw (x) (throw :protected (values x (list x))))
             (defun protected-child (x fail) {body})",
                body = &body[1..body.len() - 1]
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x fail)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(body).unwrap().0);
        let symbol = egcl_rt::symbols::intern("PROTECTED-CHILD");
        let body = Arc::new(
            compile_function("PROTECTED-CHILD", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let installed = if fallback {
            let push = body
                .code
                .iter()
                .position(|i| matches!(i, Instr::PushCatch { .. }))
                .unwrap() as u32;
            let code =
                super::super::super::native_transfer_entry::TransferCode::compile_nested_protected(
                    body,
                )
                .unwrap()
                .without_catch_destination(push);
            super::super::super::native_transfer_entry::install_baseline_code(symbol, code)
        } else {
            super::super::super::native_transfer_entry::install_baseline(symbol, body)
        }
        .expect("install protected baseline");
        publish_native(
            symbol,
            egcl_rt::symbols::symbol_function(symbol),
            &installed,
        );
        assert_eq!(installed.transfer_abi_version, MAPPED_TRANSFER_ABI_VERSION);
        assert!(!native_transfer_abi_compatible(&installed));
        let code = compile_caller(
            "(x fail)",
            "((catch :protected (protected-child x fail)))",
            &env,
        );
        code.run(&[NIL, NIL], &mut env).unwrap();
        for fail in [NIL, T] {
            take_nested_entries();
            super::super::super::native_transfer_entry::take_native_fallback_count();
            let stack = egcl_rt::current_stack();
            let fp = stack.fp();
            let sp = stack.sp();
            egcl_rt::rooted!(args = vec![super::super::super::super::arena_cons(T, NIL), fail]);
            let before = args[0].to_raw();
            take_completeness();
            egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
            let (expected, resolved, bails) = take_completeness();
            assert!(expected > 0, "cleanup GC must reach published roots");
            assert_eq!(
                expected, resolved,
                "resolve every declared root across the child adapter"
            );
            assert_eq!(
                bails.iter().sum::<usize>(),
                0,
                "protected-child walk bailed: {}",
                describe_bails(bails)
            );
            assert_eq!(*result, args[0]);
            assert_ne!(args[0].to_raw(), before);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]), (args[0], NIL));
            assert_eq!(
                take_nested_entries(),
                1,
                "protected child must share its caller's segment"
            );
            if fallback && fail == T {
                assert_eq!(
                    super::super::super::native_transfer_entry::take_native_fallback_count(),
                    1,
                    "missing child landing must resume exactly one bytecode continuation"
                );
            }
            assert_eq!(stack.fp(), fp);
            assert_eq!(stack.sp(), sp);
            assert!(env.catch_stack.is_empty());
            assert!(env.handlers.is_empty());
            assert!(env.restarts.is_empty());
        }
        assert_eq!(
            super::super::super::super::read_eval_all_env("*protected-child-cleanups*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(3)
        );
    }
    assert!(
        take_adapter_crossings() > 0,
        "cleanup collection must cross a mapped child into its native caller"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_installed_baseline_records_loop_heat() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(defun protected-loop (n) (catch :loop (dotimes (i n))) n)",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((catch :loop (dotimes (i n))) n)")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("PROTECTED-LOOP");
    let body =
        Arc::new(compile_function("PROTECTED-LOOP", *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let installed =
        super::super::super::native_transfer_entry::install_baseline(symbol, body).unwrap();
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    publish_native(symbol, Some(*function), &installed);
    let before = egcl_rt::function::back_edge_count(*function);
    assert_eq!(
        run_native(&installed, symbol, &[EgclVal::from_fixnum(20)], &mut env).unwrap(),
        EgclVal::from_fixnum(20)
    );
    let heat = egcl_rt::function::back_edge_count(*function) - before;
    assert!(
        (20..=22).contains(&heat),
        "count loop headers, not every cyclic block: {heat}"
    );
    let hot = t2_backedge_threshold().saturating_add(1);
    run_native(
        &installed,
        symbol,
        &[EgclVal::from_fixnum(i64::from(hot))],
        &mut env,
    )
    .unwrap();
    assert!(
        T2_QUEUED.with(|queued| queued.borrow().contains_key(&symbol))
            || T2_DECLINED.with(|declined| declined.borrow().contains(&symbol))
            || NATIVE_REGISTRY.with(|registry| registry
                .borrow()
                .get(&symbol)
                .is_some_and(|code| code.is_t2)),
        "a loop-hot mapped baseline must request T2 compilation"
    );
    // The retained old entry may still run after redefinition, but its heat
    // belongs to neither the replacement body nor the replacement function.
    super::super::super::super::read_eval_all_env("(defun protected-loop (n) n)", &mut env)
        .unwrap();
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let replacement_heat = egcl_rt::function::back_edge_count(*replacement);
    run_native(&installed, symbol, &[EgclVal::from_fixnum(20)], &mut env).unwrap();
    assert_eq!(
        egcl_rt::function::back_edge_count(*replacement),
        replacement_heat
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_protected_child_handler_recovery_retires_clusters() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let clustered_source = "((handler-case (restart-case (handler-bind ((arithmetic-error #'protected-decline)) (unwind-protect (symbol-value x) (protected-handler-cleanup))) (protected-unused () :wrong)) (type-error (c) (protected-handler-answer c))))";
    for (fallback, clusters) in [(false, false), (true, false), (false, true), (true, true)] {
        let source = if clusters {
            clustered_source
        } else {
            "((handler-case (unwind-protect (symbol-value x) (protected-handler-cleanup)) (type-error (c) (protected-handler-answer c))))"
        };
        super::super::super::super::read_eval_all_env(&format!(
            "(setq *protected-handler-cleanups* 0)
             (defun protected-decline (c) (declare (ignore c)) nil)
             (defun protected-handler-cleanup () (%force-minor-gc-for-test) (setq *protected-handler-cleanups* (+ *protected-handler-cleanups* 1)))
             (defun protected-handler-answer (c) (%force-minor-gc-for-test) (values (slot-value c 'datum) (list (slot-value c 'datum))))
             (defun protected-handler-child (x) {})", &source[1..source.len()-1]), &mut env).unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let symbol = egcl_rt::symbols::intern("PROTECTED-HANDLER-CHILD");
        let body = Arc::new(
            compile_function(
                "PROTECTED-HANDLER-CHILD",
                *params,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let target = body
            .code
            .iter()
            .position(|op| matches!(op, Instr::PushHandlerCase { .. }))
            .unwrap() as u32;
        let mut code = TransferCode::compile_nested_protected(body).unwrap();
        if fallback {
            code = code.without_handler_destination(target, 0);
        }
        let installed =
            super::super::super::native_transfer_entry::install_baseline_code(symbol, code)
                .unwrap();
        publish_native(
            symbol,
            egcl_rt::symbols::symbol_function(symbol),
            &installed,
        );
        let caller = compile_mapped_caller("(x)", "((protected-handler-child x))", &env);
        caller.run(&[NIL], &mut env).unwrap();
        take_nested_entries();
        super::super::super::native_transfer_entry::take_native_fallback_count();
        let stack = egcl_rt::current_stack();
        let fp = stack.fp();
        let sp = stack.sp();
        egcl_rt::rooted!(args = vec![super::super::super::super::arena_cons(T, NIL)]);
        let before = args[0].to_raw();
        egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
        assert_eq!(*result, args[0]);
        assert_ne!(args[0].to_raw(), before);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(cp(env.mv[1]), (args[0], NIL));
        assert_eq!(take_nested_entries(), 1);
        assert_eq!(
            super::super::super::native_transfer_entry::take_native_fallback_count(),
            usize::from(fallback || clusters)
        );
        assert_eq!(stack.fp(), fp);
        assert_eq!(stack.sp(), sp);
        assert!(env.handlers.is_empty());
        assert!(env.restarts.is_empty());
        assert!(env.catch_stack.is_empty());
        assert_eq!(
            super::super::super::super::read_eval_all_env("*protected-handler-cleanups*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(2)
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_protected_child_restart_and_replacement_transfer() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for restart in [true, false] {
        let source = if restart {
            "((restart-case (handler-bind ((type-error #'protected-restart-handler)) (unwind-protect (if fail (symbol-value x) (values x (list x))) (protected-exit-cleanup x nil))) (protected-child-resume (v) (values v (list v)))))"
        } else {
            "((unwind-protect (if fail (throw :original x) (values x (list x))) (protected-exit-cleanup x fail)))"
        };
        super::super::super::super::read_eval_all_env(
            &format!(
                "(setq *protected-exit-cleanups* 0 *protected-exit-handlers* 0)
             (defun protected-restart-handler (c)
               (setq *protected-exit-handlers* (+ *protected-exit-handlers* 1))
               (%force-minor-gc-for-test)
               (invoke-restart 'protected-child-resume (slot-value c 'datum)))
             (defun protected-exit-cleanup (x replace)
               (setq *protected-exit-cleanups* (+ *protected-exit-cleanups* 1))
               (%force-minor-gc-for-test)
               (if replace (throw :replacement (values x (list x))) nil))
             (defun protected-exit-child (x fail) {})",
                &source[1..source.len() - 1]
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(x fail)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let symbol = egcl_rt::symbols::intern("PROTECTED-EXIT-CHILD");
        let body = Arc::new(
            compile_function("PROTECTED-EXIT-CHILD", *params, *forms, &env, false, false).unwrap(),
        );
        registry_put(symbol, Arc::clone(&body));
        let installed =
            super::super::super::native_transfer_entry::install_baseline(symbol, body).unwrap();
        publish_native(
            symbol,
            egcl_rt::symbols::symbol_function(symbol),
            &installed,
        );
        let caller = compile_caller(
            "(x fail)",
            "((catch :replacement (protected-exit-child x fail)))",
            &env,
        );
        caller.run(&[NIL, NIL], &mut env).unwrap();
        take_nested_entries();
        let stack = egcl_rt::current_stack();
        let fp = stack.fp();
        let sp = stack.sp();
        egcl_rt::rooted!(args = vec![super::super::super::super::arena_cons(T, NIL), T]);
        let before = args[0].to_raw();
        egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
        assert_eq!(*result, args[0]);
        assert_ne!(args[0].to_raw(), before);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(cp(env.mv[1]), (args[0], NIL));
        assert_eq!(take_nested_entries(), 1);
        assert_eq!(stack.fp(), fp);
        assert_eq!(stack.sp(), sp);
        assert!(env.handlers.is_empty());
        assert!(env.restarts.is_empty());
        assert!(env.catch_stack.is_empty());
        assert_eq!(
            super::super::super::super::read_eval_all_env("*protected-exit-cleanups*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(2)
        );
        assert_eq!(
            super::super::super::super::read_eval_all_env("*protected-exit-handlers*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(i64::from(restart))
        );
    }
}

#[derive(Clone, Copy)]
struct HeatMutation {
    symbol: u32,
    owner: usize,
    original: *mut EgclVal,
    replacement: *mut EgclVal,
}

thread_local! {
    static HEAT_MUTATION: std::cell::Cell<Option<HeatMutation>> = const { std::cell::Cell::new(None) };
    static HEAT_MUTATED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn replace_validated_heat_callable() {
    let Some(mutation) = HEAT_MUTATION.with(|state| state.take()) else {
        return;
    };
    let (owner, nested, frame) = native_transfer_entry::condition_capture_for_test()
        .expect("heat hook must execute inside a live mapped activation");
    assert_eq!(owner, mutation.owner);
    assert!(!nested);
    assert_eq!(unsafe { (*frame).function }, unsafe { *mutation.original });
    let replacement = if mutation.replacement.is_null() {
        egcl_rt::value::UNBOUND
    } else {
        unsafe { *mutation.replacement }
    };
    // Deliberately do not touch bytecode publication or its generation: model
    // the setter paused between updating the function cell and its registry.
    egcl_rt::symbols::set_symbol_function(mutation.symbol, replacement);
    HEAT_MUTATED.with(|done| done.set(true));
}

struct HeatMutationGuard {
    symbol: u32,
    original: *mut EgclVal,
}

impl Drop for HeatMutationGuard {
    fn drop(&mut self) {
        native_transfer_entry::AFTER_HEAT_VALIDATION.with(|hook| hook.set(None));
        HEAT_MUTATION.with(|state| state.set(None));
        egcl_rt::symbols::set_symbol_function(self.symbol, unsafe { *self.original });
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_loop_heat_retains_validated_callable_during_cell_replacement() {
    const CHILD: &str = "EGCL_TEST_HEAT_CELL_REPLACEMENT_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // The broken UNBOUND case can dereference a nonfunction. Keep it in a
        // child process so that a regression cannot abort the whole test suite.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cli::bytecode::native_transfer_tests::nested::protected::native_v2_loop_heat_retains_validated_callable_during_cell_replacement",
                "--ignored",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env("EGCL_T2", "0")
            .env_remove("EGCL_PROFILING_DISABLED")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "heat child failed: {:?}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("1 passed"));
        return;
    }
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(defun heat-cell-loop (n) (catch :heat-cell (dotimes (i n))) n)
         (defun heat-cell-replacement (n) n)",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((catch :heat-cell (dotimes (i n))) n)")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("HEAT-CELL-LOOP");
    let body =
        Arc::new(compile_function("HEAT-CELL-LOOP", *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let installed = native_transfer_entry::install_baseline(symbol, body).unwrap();
    let NativeCodeStorage::Mapped(mapped) = &installed._storage else {
        panic!("test must install a real mapped baseline");
    };
    egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let replacement_symbol = egcl_rt::symbols::intern("HEAT-CELL-REPLACEMENT");
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(replacement_symbol).unwrap());
    assert_ne!(*original, *replacement);
    publish_native(symbol, Some(*original), &installed);
    for unbind in [false, true] {
        let original_before = egcl_rt::function::back_edge_count(*original);
        let replacement_before = egcl_rt::function::back_edge_count(*replacement);
        let stack = egcl_rt::current_stack();
        let before_stack = (
            stack.fp(),
            stack.sp(),
            NATIVE_DEPTH.with(std::cell::Cell::get),
        );
        {
            let _restore = HeatMutationGuard {
                symbol,
                original: &mut *original,
            };
            HEAT_MUTATED.with(|done| done.set(false));
            HEAT_MUTATION.with(|state| {
                state.set(Some(HeatMutation {
                    symbol,
                    owner: Rc::as_ptr(mapped) as usize,
                    original: &mut *original,
                    replacement: if unbind {
                        std::ptr::null_mut()
                    } else {
                        &mut *replacement
                    },
                }))
            });
            native_transfer_entry::AFTER_HEAT_VALIDATION
                .with(|hook| hook.set(Some(replace_validated_heat_callable)));
            let result = run_native_callable(
                &installed,
                symbol,
                *original,
                &[EgclVal::from_fixnum(8)],
                &mut env,
            )
            .unwrap();
            assert_eq!(result, EgclVal::from_fixnum(8));
            assert!(HEAT_MUTATED.with(std::cell::Cell::get));
            assert_eq!(
                egcl_rt::symbols::symbol_function(symbol),
                Some(if unbind {
                    egcl_rt::value::UNBOUND
                } else {
                    *replacement
                })
            );
            assert_eq!(
                egcl_rt::function::back_edge_count(*original) - original_before,
                1,
                "the validated header belongs to the original; subsequent headers must decline (unbind={unbind})"
            );
            assert_eq!(
                egcl_rt::function::back_edge_count(*replacement),
                replacement_before,
                "replacement must receive no heat from the retained old body"
            );
            assert_eq!(
                (
                    stack.fp(),
                    stack.sp(),
                    NATIVE_DEPTH.with(std::cell::Cell::get)
                ),
                before_stack
            );
            assert!(env.catch_stack.is_empty());
        }
        assert_eq!(egcl_rt::symbols::symbol_function(symbol), Some(*original));
    }
}
