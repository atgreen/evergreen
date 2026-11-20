// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::bytecode::*;

fn clause(params: &str, source: &str, env: &Env) -> BytecodeFunction {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(body = reader::read_from_string(source).unwrap().0);
    let captured = var_name(resolve_sym("CAPTURED").unwrap());
    let captures = std::collections::HashSet::from([captured]);
    let blocks = std::collections::HashSet::from([var_name(resolve_sym("OUTSIDE").unwrap())]);
    let tags = std::collections::HashSet::from([var_name(resolve_sym("END").unwrap())]);
    compile_restart_clause(
        *params,
        *body,
        env,
        &captures,
        &HashMap::new(),
        &Default::default(),
        &blocks,
        &tags,
        false,
    )
    .expect("lower the actual restart clause")
}

#[test]
fn literal_restart_frame_elision_uses_the_actual_lowered_body() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let function = clause("()", "(:report \"Retry assertion.\" nil)", &env);
    assert!(!function.has_env);
    assert!(matches!(
        function.code.as_slice(),
        [
            Instr::Const(0),
            Instr::Pop,
            Instr::Const(1),
            Instr::Pop,
            Instr::Const(2),
            Instr::ClearMv,
            Instr::Return
        ]
    ));
    assert_eq!(function.constants.len(), 3);
    assert!(function.constants[2].is_nil());
    assert!(literal_without_frame(&function));
}

#[test]
fn restart_frame_proof_rejects_real_macro_hidden_defaults_and_control() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        definition = reader::read_from_string("(defmacro restart-frame-hidden () 'captured)")
            .unwrap()
            .0
    );
    crate::cli::eval_form(*definition, &mut env).unwrap();
    for (params, body) in [
        ("()", "(captured)"),
        ("()", "((restart-frame-hidden))"),
        ("(&optional (value captured))", "(value)"),
        ("()", "((return-from outside :escaped))"),
        ("()", "((go end))"),
        ("()", "((lambda () captured))"),
        ("()", "((unwind-protect nil (setq captured 1)))"),
        ("()", "(:report (lambda (stream) captured) nil)"),
        ("()", "(:interactive (lambda () captured) nil)"),
        ("()", "(:test (lambda (condition) captured) nil)"),
    ] {
        let function = clause(params, body, &env);
        assert!(function.has_env, "{params} {body}: {:?}", function.code);
        assert!(!literal_without_frame(&function), "{params} {body}");
        if body.contains("restart-frame-hidden") {
            assert!(
                function
                    .code
                    .iter()
                    .any(|i| matches!(i, Instr::LoadEnvVar(_))),
                "the macro must actually reveal the boxed lexical: {:?}",
                function.code
            );
        }
    }
}

#[test]
fn restart_frame_proof_refuses_special_and_slot_bindings() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let fixed = clause("(value)", "(value)", &env);
    assert!(fixed.has_env);
    assert!(fixed.code.iter().any(|i| matches!(i, Instr::LoadLocal(_))));
    // Even an unused slot does not broaden this literal, zero-argument proof.
    let unused = clause("(value)", "(nil)", &env);
    assert!(unused.has_env);
    let special = clause(
        "()",
        "((locally (declare (special captured)) captured))",
        &env,
    );
    assert!(special.has_env);
    assert!(
        special
            .code
            .iter()
            .any(|i| matches!(i, Instr::LoadGlobal(_)))
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn literal_restart_no_longer_blocks_shared_heap_environment_rewrite() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(body = reader::read_from_string(
        "((let ((captured 41)) (list (lambda () (setq captured 42)) (restart-case nil (finish () :report \"Retry assertion.\" nil)))))").unwrap().0);
    let function =
        compile_function("RESTART-LITERAL-HEAP-UNIT", NIL, *body, &env, true, false).unwrap();
    assert_eq!(function.restart_cases.len(), 1);
    assert!(!function.restart_cases[0].restarts[0].function.has_env);
    assert!(!function.has_env);
    assert!(
        function
            .code
            .iter()
            .any(|i| matches!(i, Instr::MakeEnvClosure { .. })),
        "the actual creator must use the shared heap environment: {:?}",
        function.code
    );
    assert!(!function.code.iter().any(|i| matches!(
        i,
        Instr::LoadEnvVar(_)
            | Instr::StoreEnvVar(_)
            | Instr::DefineEnvVar(_)
            | Instr::PushEnvChild
            | Instr::PopEnvChild
    )));
}
