// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::{
    TransferCode, take_native_catch_count, take_native_cleanup_count, take_nested_entries,
};
use super::*;

pub(super) mod fibers;
mod protected;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_distinct_callees_share_segment_and_unwind_into_caller() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *distinct-cleanups* 0 *distinct-after* 0)
         (defun distinct-cleanup () (setq *distinct-cleanups* (+ *distinct-cleanups* 1)))
         (defun distinct-after () (setq *distinct-after* (+ *distinct-after* 1)))
         (defun distinct-throw (x) (throw :distinct (values x (list x))))
         (defun distinct-collect (x) (%force-minor-gc-for-test) x)
         (defun distinct-leaf (x fail)
           (distinct-collect x)
           (if fail (distinct-throw x) (values x (list x))))",
        &mut env,
    )
    .unwrap();
    install_t2(
        "DISTINCT-LEAF",
        "(x fail)",
        "((distinct-collect x) (if fail (distinct-throw x) (values x (list x))))",
        &env,
    );
    egcl_rt::rooted!(params = reader::read_from_string("(x fail)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((catch :distinct (unwind-protect (if fail (progn (distinct-leaf x fail) (distinct-after)) (distinct-leaf x fail)) (distinct-cleanup))))"
    ).unwrap().0);
    let body =
        Arc::new(compile_function("DISTINCT-CALLER", *params, *forms, &env, false, false).unwrap());
    let code = TransferCode::compile(body).expect("mapped caller");
    code.run(&[NIL, NIL], &mut env).unwrap(); // cold cell resolution
    for fail in [NIL, T] {
        take_nested_entries();
        take_native_cleanup_count();
        take_native_catch_count();
        egcl_rt::rooted!(
            args = vec![
                super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL),
                fail
            ]
        );
        let before = args[0].to_raw();
        egcl_rt::rooted!(
            function = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("DISTINCT-LEAF"))
                .unwrap()
        );
        let calls = egcl_rt::function::invoke_count(*function);
        egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
        assert_eq!(*result, args[0]);
        assert_eq!(egcl_rt::function::invoke_count(*function), calls + 1);
        assert_ne!(
            args[0].to_raw(),
            before,
            "callee must collect and relocate caller's root"
        );
        assert_eq!(env.mv.len(), 2);
        assert_eq!(cp(env.mv[1]).0, args[0]);
        assert!(
            take_nested_entries() > 0,
            "distinct callee must execute as a mapped activation in the caller's segment"
        );
        assert_eq!(take_native_cleanup_count(), usize::from(fail == T));
        assert_eq!(take_native_catch_count(), usize::from(fail == T));
    }
    assert_eq!(
        super::super::super::read_eval_all_env("*distinct-cleanups*", &mut env).unwrap(),
        EgclVal::from_fixnum(3)
    );
    assert_eq!(
        super::super::super::read_eval_all_env("*distinct-after*", &mut env).unwrap(),
        EgclVal::from_fixnum(0)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_distinct_callee_preserves_live_handler_cluster() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *distinct-signals* 0)
         (defun distinct-signal-handler (c)
           (declare (ignore c))
           (setq *distinct-signals* (+ *distinct-signals* 1))
           (invoke-restart 'distinct-resume))
         (defun distinct-symbol-value (x) (symbol-value x))",
        &mut env,
    )
    .unwrap();
    install_t2("DISTINCT-SYMBOL-VALUE", "(x)", "((symbol-value x))", &env);
    let code = compile_caller(
        "(x)",
        "((handler-bind ((type-error #'distinct-signal-handler))
            (restart-case (distinct-symbol-value x) (distinct-resume () :resumed))))",
        &env,
    );
    code.run(&[NIL], &mut env).unwrap(); // warm the callee cell with a valid symbol
    let stack = egcl_rt::current_stack();
    let old_frame = stack.fp();
    let old_sp = stack.sp();
    for _ in 0..2 {
        take_nested_entries();
        egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
        egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
        assert_eq!(*result, reader::read_from_string(":resumed").unwrap().0);
        assert_eq!(take_nested_entries(), 1);
        assert_eq!(stack.fp(), old_frame);
        assert_eq!(stack.sp(), old_sp);
        assert!(env.handlers.is_empty());
        assert!(env.restarts.is_empty());
    }
    assert_eq!(
        super::super::super::read_eval_all_env("*distinct-signals*", &mut env).unwrap(),
        EgclVal::from_fixnum(2)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_distinct_mutual_calls_bound_depth_and_retire_crossed_frames() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let a = "((distinct-mutual-tick) (if (= n 0) (distinct-mutual-base x fail) (distinct-mutual-b (- n 1) x fail)))";
    let b = "((distinct-mutual-tick) (if (= n 0) (distinct-mutual-base x fail) (distinct-mutual-a (- n 1) x fail)))";
    super::super::super::read_eval_all_env(&format!(
        "(setq *distinct-mutual-ticks* 0)
         (defun distinct-mutual-tick () (setq *distinct-mutual-ticks* (+ *distinct-mutual-ticks* 1)))
         (defun distinct-mutual-base (x fail)
           (%force-minor-gc-for-test)
           (if fail (throw :mutual (values x (list x))) (values x (list x))))
         (defun distinct-mutual-a (n x fail) {a})
         (defun distinct-mutual-b (n x fail) {b})",
         a=&a[1..a.len()-1], b=&b[1..b.len()-1]), &mut env).unwrap();
    install_t2("DISTINCT-MUTUAL-A", "(n x fail)", a, &env);
    install_t2("DISTINCT-MUTUAL-B", "(n x fail)", b, &env);
    let code = compile_caller(
        "(n x fail)",
        "((catch :mutual (distinct-mutual-a n x fail)))",
        &env,
    );
    for bounded in [false, true] {
        for fail in [NIL, T] {
            // Legacy fallback can install helpers and invalidate call cells.
            // Resolve both mapped bodies before each measured warm-entry case.
            for _ in 0..4 {
                code.run(&[EgclVal::from_fixnum(4), NIL, NIL], &mut env)
                    .unwrap();
            }
            super::super::super::read_eval_all_env("(setq *distinct-mutual-ticks* 0)", &mut env)
                .unwrap();
            take_nested_entries();
            let stack = egcl_rt::current_stack();
            let old_frame = stack.fp();
            let old_sp = stack.sp();
            let initial_depth = if bounded {
                native_depth_cap().saturating_sub(3)
            } else {
                0
            };
            let depth = NATIVE_DEPTH.with(|slot| slot.replace(initial_depth));
            egcl_rt::rooted!(
                args = vec![
                    EgclVal::from_fixnum(8),
                    super::super::super::arena_cons(T, NIL),
                    fail
                ]
            );
            let before = args[1].to_raw();
            egcl_rt::rooted!(result = code.run(&args, &mut env));
            let restored_depth = NATIVE_DEPTH.with(|slot| slot.replace(depth));
            assert_eq!(restored_depth, initial_depth);
            assert_eq!(result.as_ref().unwrap(), &args[1]);
            assert_ne!(args[1].to_raw(), before);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]), (args[1], NIL));
            assert_eq!(
                take_nested_entries(),
                if bounded { 2 } else { 9 },
                "bounded={bounded} fail={fail:?}"
            );
            assert_eq!(stack.fp(), old_frame);
            assert_eq!(stack.sp(), old_sp);
            assert_eq!(
                super::super::super::read_eval_all_env("*distinct-mutual-ticks*", &mut env)
                    .unwrap(),
                EgclVal::from_fixnum(9)
            );
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_distinct_active_callee_survives_redefinition_and_unbinding() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let form = "((if mutate (distinct-change) nil) (if fail (distinct-changing-throw x) (values x (list x))))";
    for unbind in [false, true] {
        for fail in [NIL, T] {
            let change = if unbind {
                "(fmakunbound 'distinct-changing)"
            } else {
                "(setf (symbol-function 'distinct-changing) #'distinct-replacement)"
            };
            super::super::super::read_eval_all_env(
                &format!(
                "(defun distinct-replacement (x mutate fail) (declare (ignore x mutate fail)) 99)
                 (defun distinct-change () {change} (%force-minor-gc-for-test))
                 (defun distinct-changing-throw (x) (throw :changing (values x (list x))))
                 (defun distinct-changing (x mutate fail) {body})", body=&form[1..form.len()-1]),
                &mut env,
            )
            .unwrap();
            install_t2("DISTINCT-CHANGING", "(x mutate fail)", form, &env);
            let code = compile_caller(
                "(x mutate fail)",
                "((catch :changing (distinct-changing x mutate fail)))",
                &env,
            );
            code.run(&[NIL, NIL, NIL], &mut env).unwrap();
            take_nested_entries();
            egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL), T, fail]);
            let before = args[0].to_raw();
            egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
            assert_eq!(*result, args[0]);
            assert_ne!(args[0].to_raw(), before);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]), (args[0], NIL));
            assert_eq!(take_nested_entries(), 1);
            let next = code.run(&[NIL, NIL, NIL], &mut env);
            if unbind {
                assert!(
                    next.is_err(),
                    "unbound definition must not reuse retired code"
                );
            } else {
                assert_eq!(next.unwrap(), EgclVal::from_fixnum(99));
            }
            assert_eq!(take_nested_entries(), 0);
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_distinct_callee_returns_to_recursive_parent_activation() {
    use super::super::native_transfer_entry::take_recursive_entries;
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let form = "((if (= n 0) (distinct-recursive-leaf x fail) (distinct-recursive-parent (- n 1) x fail)))";
    let leaf =
        "((distinct-recursive-collect) (if fail (distinct-recursive-error) (values x (list x))))";
    super::super::super::read_eval_all_env(
        &format!(
            "(defun distinct-recursive-collect () (%force-minor-gc-for-test))
         (defun distinct-recursive-error () (car 1))
         (defun distinct-recursive-leaf (x fail) {leaf})
         (defun distinct-recursive-parent (n x fail) {form})",
            leaf = &leaf[1..leaf.len() - 1],
            form = &form[1..form.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    install_t2("DISTINCT-RECURSIVE-LEAF", "(x fail)", leaf, &env);
    egcl_rt::rooted!(params = reader::read_from_string("(n x fail)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(form).unwrap().0);
    // Preserve real self calls instead of the named tail-call rewrite.
    let mut body = compile_function("<lambda>", *params, *forms, &env, false, false).unwrap();
    body.name = "DISTINCT-RECURSIVE-PARENT".into();
    let body = Arc::new(body);
    registry_put(
        egcl_rt::symbols::intern("DISTINCT-RECURSIVE-PARENT"),
        Arc::clone(&body),
    );
    let code = TransferCode::compile(body).unwrap();
    code.run(&[EgclVal::from_fixnum(0), NIL, NIL], &mut env)
        .unwrap();
    for (fail, invalid_capture) in [(NIL, false), (T, false), (T, true), (NIL, false)] {
        if invalid_capture {
            super::super::native_transfer_entry::fail_next_nested_capture();
        }
        take_nested_entries();
        take_recursive_entries();
        let stack = egcl_rt::current_stack();
        let old_frame = stack.fp();
        let old_sp = stack.sp();
        let depth = NATIVE_DEPTH.with(|depth| depth.get());
        egcl_rt::rooted!(
            args = vec![
                EgclVal::from_fixnum(4),
                super::super::super::arena_cons(T, NIL),
                fail
            ]
        );
        let before = args[1].to_raw();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        if invalid_capture {
            assert!(
                matches!(&*result, Err(EgclError::Internal(message)) if message == "invalid native transfer capture"),
                "{:?}", &*result
            );
        } else if fail == T {
            assert!(
                matches!(&*result, Err(EgclError::Signalled(_))),
                "{:?}", &*result
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &args[1]);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]), (args[1], NIL));
        }
        assert_ne!(args[1].to_raw(), before);
        assert_eq!(take_nested_entries(), 1);
        assert_eq!(take_recursive_entries(), 4);
        assert_eq!(stack.fp(), old_frame);
        assert_eq!(stack.sp(), old_sp);
        assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), depth);
    }
}

fn compile_caller(params: &str, forms: &str, env: &Env) -> TransferCode {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    TransferCode::compile(Arc::new(
        compile_function("DISTINCT-CALLER", *params, *forms, env, false, false).unwrap(),
    ))
    .unwrap()
}

fn install_t2(name: &str, params: &str, forms: &str, env: &Env) {
    let symbol = egcl_rt::symbols::intern(name);
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    registry_put(
        symbol,
        Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap()),
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
}
