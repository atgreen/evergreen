// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

fn require_compiled_callbacks(function: &BytecodeFunction) {
    assert!(
        !function.code.iter().any(|instruction| matches!(
            instruction,
            Instr::EvalHost(_) | Instr::MakeClosureEnv(_)
        )),
        "callbacks must not depend on host-evaluated source"
    );
    for nested in &function.nested_functions {
        require_compiled_callbacks(nested);
    }
}

fn compile_and_call(source: &str, portable: bool, expected: i64, env: &mut Env) {
    egcl_rt::rooted!(
        form = super::super::read_from_string_in_env(source, env)
            .unwrap()
            .0
    );
    let function = compile_thunk(*form, env, portable).expect("top-level thunk must compile");
    assert!(
        function.code.iter().any(|instruction| {
            matches!(
                instruction,
                Instr::MakeClosure { .. } | Instr::MakeEnvClosure { .. }
            )
        }),
        "an escaping callback must be emitted as nested bytecode"
    );
    require_compiled_callbacks(&function);
    let function = Arc::new(function);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&function));
    egcl_rt::rooted!(callback = run(function, &[], NIL, env).unwrap());
    assert!(egcl_rt::function::is_interpreted_function(*callback));
    // Invoke after the creating activation has returned. Both calls must use
    // the definition-site function namespace retained by the emitted closure.
    for _ in 0..2 {
        assert_eq!(
            super::super::apply_function(*callback, &[EgclVal::from_fixnum(2)], env).unwrap(),
            EgclVal::from_fixnum(expected)
        );
    }
}

#[test]
fn top_level_flet_callback_retains_its_compiled_function_namespace() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    for portable in [false, true] {
        compile_and_call(
            "(flet ((helper (value) (+ 40 value))) (lambda (value) (helper value)))",
            portable,
            42,
            &mut env,
        );
        compile_and_call(
            "(flet ((helper (value) (+ 40 value))) (function (lambda (value) (helper value))))",
            portable,
            42,
            &mut env,
        );
    }
}

#[test]
fn macro_hidden_callback_boxes_and_shares_its_helpers_outer_capture() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    // Native DEFMACRO plus QUOTE needs no boot Lisp or SETF expander fixture.
    super::super::read_eval_all_env(
        "(defmacro hidden-local-callback () '(lambda (value) (helper value)))",
        &mut env,
    )
    .unwrap();
    for portable in [false, true] {
        compile_and_call(
            "(let ((offset 40))
               (flet ((helper (value) (+ offset value)))
                 (let ((callback (hidden-local-callback)))
                   (setq offset 41)
                   callback)))",
            portable,
            43,
            &mut env,
        );
    }
}

const FRESH_CALLBACK: &str = r#"
(defmacro fresh-boxing-callback ()
  (let ((offset (gensym "BOXING-OFFSET")))
    (list 'let (list (list offset 40))
      (list 'flet (list (list 'helper '(value) (list '+ offset 'value)))
        (list 'let (list (list 'callback '(lambda (value) (helper value))))
          (list 'setq offset 41) 'callback)))))
"#;

#[test]
fn macro_generated_capture_identity_is_stable_across_boxing_retries() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    super::super::read_eval_all_env(FRESH_CALLBACK, &mut env).unwrap();
    for portable in [false, true] {
        compile_and_call("(fresh-boxing-callback)", portable, 43, &mut env);
    }
}

#[test]
fn retry_snapshot_expands_shared_syntax_in_its_occurrence_scope() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    super::super::read_eval_all_env(FRESH_CALLBACK, &mut env).unwrap();
    egcl_rt::rooted!(
        shared = super::super::read_from_string_in_env("(pick)", &mut env)
            .unwrap()
            .0
    );
    egcl_rt::rooted!(
        left = list_to_vec(
            super::super::read_from_string_in_env("(macrolet ((pick () 11)) (pick))", &mut env)
                .unwrap()
                .0
        )
    );
    egcl_rt::rooted!(
        right = list_to_vec(
            super::super::read_from_string_in_env("(macrolet ((pick () 22)) (pick))", &mut env)
                .unwrap()
                .0
        )
    );
    left[2] = *shared;
    right[2] = *shared;
    egcl_rt::rooted!(left = vec_to_list(&left));
    egcl_rt::rooted!(right = vec_to_list(&right));
    egcl_rt::rooted!(list = resolve_sym("LIST").unwrap());
    egcl_rt::rooted!(body = vec_to_list(&[*list, *left, *right]));
    egcl_rt::rooted!(lambda = resolve_sym("LAMBDA").unwrap());
    egcl_rt::rooted!(callback = vec_to_list(&[*lambda, NIL, *body]));
    egcl_rt::rooted!(
        fresh = super::super::read_from_string_in_env("(fresh-boxing-callback)", &mut env)
            .unwrap()
            .0
    );
    egcl_rt::rooted!(progn = resolve_sym("PROGN").unwrap());
    egcl_rt::rooted!(form = vec_to_list(&[*progn, *fresh, *callback]));
    for portable in [false, true] {
        let function = compile_thunk(*form, &env, portable).expect("shared syntax must compile");
        require_compiled_callbacks(&function);
        let function = Arc::new(function);
        egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&function));
        egcl_rt::rooted!(callback = run(function, &[], NIL, &mut env).unwrap());
        egcl_rt::rooted!(values = super::super::apply_function(*callback, &[], &mut env).unwrap());
        assert_eq!(
            list_to_vec(*values),
            vec![EgclVal::from_fixnum(11), EgclVal::from_fixnum(22)]
        );
    }
}

#[test]
fn retry_snapshot_declines_phase_boundaries_but_public_reentrant_walk_does_not() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    super::super::read_eval_all_env(
        "(setq *snapshot-later-count* 0)
         (defmacro snapshot-later () (setq *snapshot-later-count* (+ 1 *snapshot-later-count*)) 99)
         (defmacro snapshot-phase () '(eval-when (:execute) (snapshot-later)))
         (defmacro snapshot-bad () (error \"real expansion failure\"))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(menv = super::super::macroexpand_environment_from_cli(&env));
    for source in [
        "(progn (eval-when (:execute) (snapshot-later)) (snapshot-later))",
        "(progn (snapshot-phase) (snapshot-later))",
        "(progn (defun snapshot-defined () (snapshot-later)) (snapshot-later))",
        "(progn (load-time-value (snapshot-later)) (snapshot-later))",
    ] {
        egcl_rt::rooted!(
            form = super::super::read_from_string_in_env(source, &mut env)
                .unwrap()
                .0
        );
        assert!(
            compiler_macroexpand::macroexpand_all_for_boxing(*form, &menv)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(
        env.lookup_var("*SNAPSHOT-LATER-COUNT*"),
        Some(EgclVal::from_fixnum(0))
    );
    egcl_rt::rooted!(
        bad = super::super::read_from_string_in_env("(snapshot-bad)", &mut env)
            .unwrap()
            .0
    );
    assert!(
        compiler_macroexpand::macroexpand_all_for_boxing(*bad, &menv).is_err(),
        "a real expander error must not become a phase-boundary decline"
    );

    let key = compiler_macroexpand::next_registered_macro_key();
    compiler_macroexpand::register_macro_function(
        key,
        Arc::new(|_, environment| {
            egcl_rt::rooted!(execute = resolve_sym(":EXECUTE").unwrap());
            egcl_rt::rooted!(situations = arena_cons(*execute, NIL));
            egcl_rt::rooted!(operator = resolve_sym("EVAL-WHEN").unwrap());
            egcl_rt::rooted!(
                form = vec_to_list(&[*operator, *situations, EgclVal::from_fixnum(42)])
            );
            // This public call receives the caller's &ENVIRONMENT, but must not
            // inherit its private boxing-snapshot policy.
            compiler_macroexpand::macroexpand_all(*form, environment)?;
            Ok(EgclVal::from_fixnum(42))
        }),
    );
    struct Registration(EgclVal);
    impl Drop for Registration {
        fn drop(&mut self) {
            compiler_macroexpand::unregister_macro_function(self.0);
        }
    }
    let _registration = Registration(key);
    egcl_rt::rooted!(operator = resolve_sym("SNAPSHOT-REENTRANT").unwrap());
    egcl_rt::rooted!(
        menv = menv.augment_function(*operator, compiler_macroexpand::FunctionInfo::Macro(key))
    );
    egcl_rt::rooted!(form = arena_cons(*operator, NIL));
    assert_eq!(
        compiler_macroexpand::macroexpand_all_for_boxing(*form, &menv).unwrap(),
        Some(EgclVal::from_fixnum(42))
    );
}
