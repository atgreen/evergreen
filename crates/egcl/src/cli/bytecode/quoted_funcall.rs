// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Eliminate the applicator only for constant quoted global symbol designators.
//! No FUNCTION lookup is moved past actual argument evaluation, and no lexical
//! function binding of the target is consulted. The emitted named call owns all
//! normal dynamic binding, arity, multiple-value and recovery behavior.

use super::*;

pub(super) fn target(lowerer: &Lowerer<'_>, operator: EgclVal, rest: EgclVal) -> Option<u32> {
    let operator = operator.symbol_index()?;
    if egcl_rt::symbols::find_index("FUNCALL") != Some(operator)
        || super::super::installed_function(operator).is_some()
        || self_calls::notinline(operator, &lowerer.inline_policies)
        || !rest.is_cons()
    {
        return None;
    }
    let (designator, _) = cp(rest);
    if !designator.is_cons() {
        return None;
    }
    let (quote, value) = cp(designator);
    if quote.symbol_index() != egcl_rt::symbols::find_index("QUOTE") || !value.is_cons() {
        return None;
    }
    let (value, tail) = cp(value);
    if tail != NIL || value == NIL || value == T {
        return None;
    }
    value.symbol_index()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn compile(env: &Env, name: &str, body: &str) -> Arc<BytecodeFunction> {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(body = reader::read_from_string(body).unwrap().0);
        Arc::new(
            compile_function(name, *params, *body, env, true, false)
                .expect("quoted FUNCALL lowers"),
        )
    }

    fn named(body: &BytecodeFunction, name: &str, nargs: u16) -> bool {
        let symbol = resolve_sym(name).unwrap().as_symbol_index();
        body.code.iter().any(
            |op| matches!(op, Instr::CallNamed { sym, nargs: n } if *sym == symbol && *n == nargs),
        )
    }

    #[test]
    fn quoted_global_designator_becomes_named_and_keeps_target_notinline() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let body = compile(
            &env,
            "QUOTED-CALL-TEST",
            "((declare (notinline quoted-global-target)) (funcall 'quoted-global-target x))",
        );
        egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
        assert!(named(&body, "QUOTED-GLOBAL-TARGET", 1));
        assert!(!named(&body, "FUNCALL", 2));
        assert_eq!(body.notinline_sites.len(), 1);
        let ir = egcl_compiler::t2::build::build_from_bytecode(&body).unwrap();
        assert!(
            ir.block_order()
                .iter()
                .flat_map(|&b| &ir.block(b).insts)
                .any(|&i| matches!(
                    ir.inst(i).aux,
                    egcl_compiler::t2::ir::AuxData::OpaqueCallTarget(_)
                ))
        );
    }

    #[test]
    fn function_objects_and_notinline_funcall_keep_the_indirect_route() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for source in [
            "((funcall #'quoted-global-target x))",
            "((declare (notinline funcall)) (funcall 'quoted-global-target x))",
        ] {
            let body = compile(&env, "QUOTED-OPAQUE-TEST", source);
            egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
            assert!(named(&body, "FUNCALL", 2));
            assert!(!named(&body, "QUOTED-GLOBAL-TARGET", 1));
        }
    }

    #[test]
    fn quoted_target_ignores_flet_but_lexical_funcall_operator_is_not_rewritten() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        {
            let body = compile(
                &env,
                "QUOTED-FLET-TARGET",
                "((flet ((quoted-global-target (k) (+ k 100))) (funcall 'quoted-global-target x)))",
            );
            egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
            assert!(named(&body, "QUOTED-GLOBAL-TARGET", 1));
        }
        let body = compile(
            &env,
            "QUOTED-FLET-OPERATOR",
            "((flet ((funcall (f k) (declare (ignore f)) (+ k 100))) (funcall 'quoted-global-target x)))",
        );
        egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
        assert!(!named(&body, "QUOTED-GLOBAL-TARGET", 1));
    }

    #[test]
    fn package_local_funcall_is_not_the_standard_applicator() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            "(defpackage :quoted-call-shadow (:use :cl) (:shadow :funcall))",
            &mut env,
        )
        .unwrap();
        let body = compile(
            &env,
            "QUOTED-PACKAGE-OPERATOR",
            "((quoted-call-shadow::funcall 'quoted-global-target x))",
        );
        egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
        assert!(
            body.code
                .iter()
                .any(|op| matches!(op, Instr::CallNamed { nargs: 2, .. }))
        );
        assert!(!named(&body, "QUOTED-GLOBAL-TARGET", 1));
    }

    #[test]
    fn quoted_self_call_retains_the_same_generation_checked_site() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let body = compile(
            &env,
            "QUOTED-SELF-TARGET",
            "((funcall 'quoted-self-target (progn (setf (symbol-function 'quoted-self-target) (lambda (k) (+ k 100))) x)))",
        );
        egcl_rt::rooted!(_body_roots = ActiveBytecodeRoot::new(&body));
        assert!(named(&body, "QUOTED-SELF-TARGET", 1));
        assert_eq!(body.self_call_sites.len(), 1);
        assert!(matches!(
            body.code[body.self_call_sites[0] as usize],
            Instr::CallNamed { nargs: 1, .. }
        ));
    }
}
