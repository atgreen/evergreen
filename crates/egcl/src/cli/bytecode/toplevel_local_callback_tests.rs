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
        function
            .code
            .iter()
            .any(|instruction| matches!(instruction, Instr::MakeClosure { .. })),
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
