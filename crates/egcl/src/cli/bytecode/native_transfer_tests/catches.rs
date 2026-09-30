use super::*;

#[test]
fn native_v2_catch_ir_rejects_wrong_scope_identity() {
    use egcl_compiler::t2::{build, ir::AuxData, verify};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(tag x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((catch tag x))").unwrap().0);
    let body = Arc::new(
        compile_function("CATCH-SCOPE-CHECK", *params, *forms, &env, false, false).unwrap(),
    );
    let _root = ActiveBytecodeRoot::new(&body);
    for enter in [true, false] {
        let mut ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
        verify::verify(&ir).unwrap();
        let site = ir
            .block_order()
            .iter()
            .flat_map(|&b| &ir.block(b).insts)
            .copied()
            .find(|&i| matches!(ir.inst(i).aux, AuxData::CatchScope { enter: e, .. } if e == enter))
            .unwrap();
        if let AuxData::CatchScope { push_bcp, .. } = &mut ir.inst_mut(site).aux {
            *push_bcp += 1;
        }
        assert!(
            verify::verify(&ir).is_err(),
            "wrong catch identity enter={enter}"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_catch_registration_preserves_values_and_fallback_identity() {
    use super::super::native_transfer_entry::{
        TransferCode, take_native_catch_count, take_native_cleanup_count,
        take_native_fallback_count,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *catch-calls* 0)
         (defun catch-answer (x) (%force-minor-gc-for-test) (values x (list x)))
         (defun catch-first (first second)
           (%force-minor-gc-for-test) (values first (list first)))
         (defun catch-exit (tag x)
           (setq *catch-calls* (+ *catch-calls* 1))
           (%force-minor-gc-for-test) (throw tag (values x (list x))))",
        &mut env,
    )
    .unwrap();
    for (source, throws, native_cleanups) in [
        ("((catch tag (catch-answer x)))", 0, 0),
        ("((catch tag (catch-exit tag x)))", 1, 0),
        ("((catch tag (throw tag (catch-answer x))))", 0, 0),
        ("((catch tag (catch tag (catch-exit tag x))))", 1, 0),
        ("((catch tag (catch nil (catch-exit tag x))))", 1, 0),
        ("((catch-first x (catch tag (catch-exit tag nil))))", 1, 0),
        (
            "((let ((saved nil)) (catch tag (if tag (setq saved x) (setq saved tag)) (catch-exit tag nil)) (catch-answer saved)))",
            1,
            0,
        ),
        (
            "((let ((saved nil)) (catch tag (setq saved x) (catch-exit tag nil)) (catch-answer saved)))",
            1,
            0,
        ),
        (
            "((catch tag (unwind-protect (catch nil (catch-exit tag x)) (catch-answer x))))",
            1,
            1,
        ),
        (
            "((catch tag (unwind-protect (catch-exit tag x) (catch-answer x))))",
            1,
            1,
        ),
        (
            "((catch tag (unwind-protect (catch-exit tag nil) (catch-exit tag x))))",
            2,
            1,
        ),
        (
            "((unwind-protect (catch tag (catch-exit tag x)) (catch-answer x)))",
            1,
            0,
        ),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(tag x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("NATIVE-CATCH", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).unwrap_or_else(|| panic!("native catch: {source}"));
        egcl_rt::rooted!(answer = super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL));
        egcl_rt::rooted!(args = vec![NIL, *answer]);
        super::super::super::read_eval_all_env("(setq *catch-calls* 0)", &mut env).unwrap();
        // Allocate the tag last so stress during fixture setup cannot tenure it
        // before the collecting call whose relocation this assertion checks.
        args[0] = super::super::super::arena_cons(T, NIL);
        let before = args[0].to_raw();
        let enclosing = super::super::super::next_control_token("ENCLOSING-CATCH");
        env.catch_stack.push((args[0], enclosing.clone()));
        take_native_cleanup_count();
        take_native_fallback_count();
        take_native_catch_count();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(result.as_ref().unwrap(), &*answer, "{source}");
        assert_ne!(args[0].to_raw(), before, "{source}");
        assert_eq!(take_native_cleanup_count(), native_cleanups, "{source}");
        assert_eq!(
            take_native_catch_count(),
            usize::from(source.contains("catch-exit") || source.contains("(throw")),
            "{source}"
        );
        assert_eq!(
            take_native_fallback_count(),
            0,
            "catch must stay native: {source}"
        );
        assert_eq!(env.catch_stack.len(), 1, "native catches retired");
        assert_eq!(
            env.catch_stack[0],
            (args[0], enclosing),
            "outer catch retained"
        );
        env.catch_stack.pop();
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(super::super::super::cp(env.mv[1]).0, *answer);
        assert_eq!(
            super::super::super::read_eval_all_env("*catch-calls*", &mut env).unwrap(),
            EgclVal::from_fixnum(throws)
        );
    }
    for (source, count, expected) in [
        ("((catch tag (values)))", 0, NIL),
        ("((catch tag (values 9)))", 1, EgclVal::from_fixnum(9)),
        ("((catch tag (throw tag (values))))", 0, NIL),
        (
            "((catch tag (throw tag (values 9))))",
            1,
            EgclVal::from_fixnum(9),
        ),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(tag)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("CATCH-VALUE-COUNT", *params, *forms, &env, false, false).unwrap(),
        );
        let code = TransferCode::compile(body).expect("native catch value count");
        take_native_catch_count();
        take_native_fallback_count();
        assert_eq!(code.run(&[NIL], &mut env).unwrap(), expected);
        assert_eq!(
            take_native_catch_count(),
            usize::from(source.contains("throw"))
        );
        assert_eq!(take_native_fallback_count(), 0);
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), count);
        assert!(env.catch_stack.is_empty());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_missing_catch_destination_preserves_fallback_state() {
    use super::super::native_transfer_entry::{
        TransferCode, take_native_catch_count, take_native_cleanup_count,
        take_native_fallback_count,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *missing-catch-calls* 0 *missing-catch-cleanups* nil)
         (defun missing-catch-answer (x) (values x (list x)))
         (defun missing-catch-exit (tag x)
           (setq *missing-catch-calls* (+ *missing-catch-calls* 1))
           (%force-minor-gc-for-test)
           (throw tag (values x (list x))))
         (defun missing-catch-cleanup (name)
           (%force-minor-gc-for-test)
           (setq *missing-catch-cleanups* (cons name *missing-catch-cleanups*)))",
        &mut env,
    )
    .unwrap();
    for (source, native_cleanups, cleanup_names) in [
        (
            "((unwind-protect (catch tag (catch nil (missing-catch-exit tag x))) (missing-catch-cleanup :outer)))",
            0,
            "(:outer)",
        ),
        (
            "((unwind-protect (catch tag (unwind-protect (catch nil (missing-catch-exit tag x)) (missing-catch-cleanup :inner))) (missing-catch-cleanup :outer)))",
            1,
            "(:outer :inner)",
        ),
        (
            "((let ((saved nil)) (catch tag (setq saved x) (catch nil (missing-catch-exit tag nil))) (missing-catch-answer saved)))",
            0,
            "nil",
        ),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(tag x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("MISSING-CATCH", *params, *forms, &env, false, false).unwrap(),
        );
        let target = body
            .code
            .iter()
            .position(|i| matches!(i, Instr::PushCatch { .. }))
            .unwrap() as u32;
        let code = TransferCode::compile(body)
            .expect("native catch caller")
            .without_catch_destination(target);
        super::super::super::read_eval_all_env(
            "(setq *missing-catch-calls* 0 *missing-catch-cleanups* nil)",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(answer = super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL));
        egcl_rt::rooted!(args = vec![NIL, *answer]);
        args[0] = super::super::super::arena_cons(T, NIL);
        let before = args[0].to_raw();
        let enclosing = super::super::super::next_control_token("ENCLOSING-MISSING-CATCH");
        env.catch_stack.push((args[0], enclosing.clone()));
        take_native_catch_count();
        take_native_cleanup_count();
        take_native_fallback_count();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(result.as_ref().unwrap(), &*answer, "{source}");
        assert_ne!(args[0].to_raw(), before, "tag must actually move");
        assert_eq!(
            take_native_catch_count(),
            0,
            "destination must be unavailable"
        );
        assert_eq!(take_native_fallback_count(), 1, "exactly one fallback");
        assert_eq!(take_native_cleanup_count(), native_cleanups, "{source}");
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(env.mv[0], *answer);
        assert_eq!(super::super::super::cp(env.mv[1]).0, *answer);
        assert_eq!(
            env.catch_stack,
            vec![(args[0], enclosing)],
            "outer catch must remain live"
        );
        env.catch_stack.pop();
        assert_eq!(
            super::super::super::read_eval_all_env("*missing-catch-calls*", &mut env).unwrap(),
            EgclVal::from_fixnum(1),
            "failed call must not replay"
        );
        assert_eq!(
            super::super::super::read_eval_all_env(
                &format!("(equal *missing-catch-cleanups* '{cleanup_names})"),
                &mut env,
            )
            .unwrap(),
            T,
            "cleanup order and count: {source}",
        );
    }
}
