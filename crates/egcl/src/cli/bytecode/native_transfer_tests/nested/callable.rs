// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_funcall_and_apply_enter_exact_mapped_callable_and_preserve_caller_capture() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(setq *callable-cleanups* 0 *callable-after* 0)
         (defun callable-cleanup () (setq *callable-cleanups* (+ *callable-cleanups* 1)))
         (defun callable-after () (setq *callable-after* (+ *callable-after* 1)))
         (defun callable-collect (x) (%force-minor-gc-for-test) x)
         (defun callable-throw (x) (throw :callable (values x (list x))))
         (defun callable-small (x fail)
           (callable-collect x) (if fail (callable-throw x) (values x (list x))))
         (defun callable-wide (x fail a b)
           (callable-collect x) (if fail (callable-throw x) (values x (list x))))",
        &mut env,
    )
    .unwrap();
    for (name, params, invocation) in [
        ("CALLABLE-SMALL", "(x fail)", "(funcall function x fail)"),
        (
            "CALLABLE-WIDE",
            "(x fail a b)",
            "(funcall function x fail 1 2)",
        ),
        (
            "CALLABLE-SMALL",
            "(x fail)",
            "(apply function (list x fail))",
        ),
        (
            "CALLABLE-WIDE",
            "(x fail a b)",
            "(apply function x fail (list 1 2))",
        ),
    ] {
        install_t2(
            name,
            params,
            "((callable-collect x) (if fail (callable-throw x) (values x (list x))))",
            &env,
        );
        let code = compile_caller(
            "(function x fail)",
            &format!(
                "((catch :callable (unwind-protect (if fail (progn {invocation} (callable-after)) {invocation}) (callable-cleanup))))"
            ),
            &env,
        );
        egcl_rt::rooted!(
            function = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern(name)).unwrap()
        );
        code.run(&[*function, NIL, NIL], &mut env).unwrap();
        for fail in [NIL, T] {
            take_nested_entries();
            egcl_rt::rooted!(
                args = vec![
                    *function,
                    super::super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL),
                    fail
                ]
            );
            let before = args[1].to_raw();
            egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
            assert_eq!(*result, args[1]);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(cp(env.mv[1]).0, args[1]);
            assert_ne!(
                before,
                args[1].to_raw(),
                "the forwarded argument must relocate"
            );
            assert!(
                take_nested_entries() > 0,
                "{invocation} must enter its mapped target in the caller's segment"
            );
            assert!(egcl_rt::function::native_entries(*function).is_some());
        }
    }
    assert_eq!(
        super::super::super::super::read_eval_all_env("*callable-cleanups*", &mut env).unwrap(),
        EgclVal::from_fixnum(12)
    );
    assert_eq!(
        super::super::super::super::read_eval_all_env("*callable-after*", &mut env).unwrap(),
        EgclVal::from_fixnum(0)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_callable_slot_reloads_a_moving_builtin_wrapper() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for form in ["((funcall function x))", "((apply function x nil))"] {
        let code = compile_caller("(function x)", form, &env);
        egcl_rt::rooted!(
            prototype = super::super::super::super::read_eval_all_env("#'list", &mut env).unwrap()
        );
        assert!(
            prototype.is_cons(),
            "fixture must use the movable wrapper representation"
        );
        let (head, identity) = cp(*prototype);
        // Preserve the wrapper identity while giving this call a fresh nursery
        // object; the shared builtin-wrapper cache may already be tenured.
        egcl_rt::rooted!(function = super::super::super::super::arena_cons(head, identity));
        egcl_rt::rooted!(args = vec![*function, EgclVal::from_fixnum(42)]);
        native_callable::collect_next_entry();
        egcl_rt::rooted!(result = code.run(&args, &mut env).unwrap());
        assert!(
            native_callable::take_relocation(),
            "callable must move during entry preparation"
        );
        assert_eq!(cp(*result), (EgclVal::from_fixnum(42), NIL));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_callable_adapters_preserve_old_definitions_and_closure_captures() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(defun callable-old (x) (+ x 1))
         (setq *callable-old* #'callable-old)
         (defun callable-old (x) (+ x 100))
         (setq *callable-a* (let ((n 10)) (lambda (x) (+ n x))))
         (setq *callable-b* (let ((n 20)) (lambda (x) (+ n x))))
         (defmethod callable-generic ((x t)) (+ x 30))
         (defclass callable-instance () () (:metaclass egcl-ext:funcallable-standard-class))
         (setq *callable-instance* (make-instance 'callable-instance))
         (egcl-ext:set-funcallable-instance-function *callable-instance* (lambda (x) (+ x 40)))",
        &mut env,
    )
    .unwrap();
    let code = compile_caller("(function x)", "((funcall function x))", &env);
    for (source, expected) in [
        ("*callable-old*", 2),
        ("#'callable-old", 101),
        ("*callable-a*", 11),
        ("*callable-b*", 21),
        ("#'callable-generic", 31),
        ("*callable-instance*", 41),
    ] {
        egcl_rt::rooted!(
            function = super::super::super::super::read_eval_all_env(source, &mut env).unwrap()
        );
        for _ in 0..3 {
            assert_eq!(
                code.run(&[*function, EgclVal::from_fixnum(1)], &mut env)
                    .unwrap(),
                EgclVal::from_fixnum(expected)
            );
        }
    }
    super::super::super::super::read_eval_all_env(
        "(egcl-ext:set-funcallable-instance-function *callable-instance* (lambda (x) (+ x 50)))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(
        instance =
            super::super::super::super::read_eval_all_env("*callable-instance*", &mut env).unwrap()
    );
    assert_eq!(
        code.run(&[*instance, EgclVal::from_fixnum(1)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(51)
    );
    super::super::super::super::read_eval_all_env("(fmakunbound 'callable-old)", &mut env).unwrap();
    egcl_rt::rooted!(
        old = super::super::super::super::read_eval_all_env("*callable-old*", &mut env).unwrap()
    );
    assert_eq!(
        code.run(&[*old, EgclVal::from_fixnum(1)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(2)
    );
    let errors = compile_caller(
        "(function x)",
        "((handler-case (funcall function x) (undefined-function () :undefined) (type-error () :type)))",
        &env,
    );
    let missing = EgclVal::from_symbol_index(egcl_rt::symbols::intern("CALLABLE-OLD"));
    assert_eq!(
        errors.run(&[missing, NIL], &mut env).unwrap(),
        super::super::super::super::resolve_sym(":UNDEFINED").unwrap()
    );
    assert_eq!(
        errors
            .run(&[EgclVal::from_fixnum(7), NIL], &mut env)
            .unwrap(),
        super::super::super::super::resolve_sym(":TYPE").unwrap()
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_apply_releases_arguments_before_parent_continues() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(defun apply-check (x) (%native-reentry-event-for-test 0) x)
         (defun apply-zero () 42)
         (defun apply-fixnum (x) (declare (type fixnum x)) x)",
        &mut env,
    )
    .unwrap();
    install_t2("APPLY-ZERO", "()", "(42)", &env);
    // Declared parameter representations are not admitted by the mapped ABI
    // yet. Keep this type-error case in T0 while APPLY-ZERO exercises mapped T2.
    let typed_symbol = egcl_rt::symbols::intern("APPLY-FIXNUM");
    egcl_rt::rooted!(typed_params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        typed_forms = reader::read_from_string("((declare (type fixnum x)) x)")
            .unwrap()
            .0
    );
    let typed_body = Arc::new(
        compile_function(
            "APPLY-FIXNUM",
            *typed_params,
            *typed_forms,
            &env,
            false,
            false,
        )
        .unwrap(),
    );
    registry_put(typed_symbol, Arc::clone(&typed_body));
    egcl_rt::rooted!(typed_function = egcl_rt::symbols::symbol_function(typed_symbol).unwrap());
    assert!(
        native_for_dispatch(
            typed_symbol,
            Some(*typed_function),
            t1_threshold(),
            &typed_body
        )
        .is_none()
    );
    egcl_rt::rooted!(
        zero = egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("APPLY-ZERO")).unwrap()
    );
    let code = compile_caller(
        "(function args)",
        "((handler-case (apply-check (apply function args)) (error () (apply-check :failed))))",
        &env,
    );
    for _ in 0..3 {
        assert_eq!(
            code.run(&[*zero, NIL], &mut env).unwrap(),
            EgclVal::from_fixnum(42)
        );
    }
    let old_depth = NATIVE_DEPTH.with(|depth| depth.replace(native_depth_cap() - 1));
    let fallback = code.run(&[*zero, NIL], &mut env);
    NATIVE_DEPTH.with(|depth| depth.set(old_depth));
    assert_eq!(fallback.unwrap(), EgclVal::from_fixnum(42));
    let failed = super::super::super::super::resolve_sym(":FAILED").unwrap();
    egcl_rt::rooted!(one_arg = super::super::super::super::arena_cons(NIL, NIL));
    egcl_rt::rooted!(
        number =
            egcl_rt::symbols::symbol_function(egcl_rt::symbols::intern("APPLY-FIXNUM")).unwrap()
    );
    for (index, function) in [*zero, *number, EgclVal::from_fixnum(7)]
        .into_iter()
        .enumerate()
    {
        let args = if index == 2 { NIL } else { *one_arg };
        assert_eq!(code.run(&[function, args], &mut env).unwrap(), failed);
    }
    native_callable::fail_next_entry_poll();
    assert_eq!(code.run(&[*zero, NIL], &mut env).unwrap(), failed);
    assert_eq!(
        code.run(&[*zero, NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
    let repeated = compile_caller(
        "(function)",
        "((dotimes (i 40) (apply-check (apply function 1 2 (list 3)))) (apply-check 42))",
        &env,
    );
    egcl_rt::rooted!(
        list = super::super::super::super::read_eval_all_env("#'list", &mut env).unwrap()
    );
    assert_eq!(
        repeated.run(&[*list], &mut env).unwrap(),
        EgclVal::from_fixnum(42)
    );
}
