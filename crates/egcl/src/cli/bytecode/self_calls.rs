// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Positive source proofs for calls bound to the executing definition.
use super::*;

pub(super) fn body_inline_policies(mut body: EgclVal) -> Vec<(u32, bool)> {
    // A function's optional documentation string precedes its declarations.
    // A sole string is an executable return value, not a docstring.
    if body.is_cons() {
        let (first, rest) = cp(body);
        if first.is_string() && rest.is_cons() {
            body = rest;
        }
    }
    let mut policies = Vec::new();
    while body.is_cons() {
        let (form, rest) = cp(body);
        body = rest;
        if !form.is_cons() {
            break;
        }
        let (head, mut declarations) = cp(form);
        if !head.is_symbol() || sym_bare_name_rc(head).as_ref() != "DECLARE" {
            break;
        }
        while declarations.is_cons() {
            let (declaration, rest) = cp(declarations);
            declarations = rest;
            policies.extend(declaration_inline_policies(declaration));
        }
    }
    policies
}

pub(super) fn declaration_inline_policies(declaration: EgclVal) -> Vec<(u32, bool)> {
    if !declaration.is_cons() {
        return Vec::new();
    }
    let (kind, mut names) = cp(declaration);
    if !kind.is_symbol() {
        return Vec::new();
    }
    let policy = match sym_bare_name_rc(kind).as_ref() {
        "NOTINLINE" => true,
        "INLINE" => false,
        _ => return Vec::new(),
    };
    let mut policies = Vec::new();
    while names.is_cons() {
        let (name, rest) = cp(names);
        names = rest;
        if name.is_symbol() {
            policies.push((name.as_symbol_index(), policy));
        }
    }
    policies
}

pub(super) fn notinline(symbol: u32, local: &[(u32, bool)]) -> bool {
    if let Some((_, policy)) = local.iter().rev().find(|(name, _)| *name == symbol) {
        return *policy;
    }
    // Symbol properties are traced and saved in images with their symbol.
    let Some(key) = resolve_sym("EGCL-INTERNAL::INLINE-POLICY") else {
        return false;
    };
    egcl_rt::symbols::symbol_plist(symbol)
        .and_then(|plist| super::super::plist_lookup(plist, key))
        .is_some_and(|policy| {
            policy.is_symbol() && sym_bare_name_rc(policy).as_ref() == "NOTINLINE"
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn lower(name: &str, source: &str, env: &Env) -> BytecodeFunction {
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(body = reader::read_from_string(source).unwrap().0);
        compile_function(name, *params, *body, env, true, false).expect("self-call source lowers")
    }
    #[test]
    fn source_proof_respects_local_notinline_and_lexical_shadowing() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "SELF-PROOF-LEXICAL";
        let body = lower(
            name,
            "((+ (self-proof-lexical n) (locally (declare (notinline self-proof-lexical)) (self-proof-lexical n))))",
            &env,
        );
        assert_eq!(body.self_call_sites.len(), 1);
        assert!(
            matches!(body.code[body.self_call_sites[0] as usize], Instr::CallNamed { sym, .. } if sym == resolve_sym(name).unwrap().as_symbol_index())
        );
        let shadowed = lower(
            name,
            "((flet ((self-proof-lexical (x) (+ x 1))) (self-proof-lexical n)))",
            &env,
        );
        assert!(shadowed.self_call_sites.is_empty());
        let tail = lower(
            name,
            "((declare (notinline self-proof-lexical)) (self-proof-lexical n))",
            &env,
        );
        assert!(tail.self_call_sites.is_empty());
        assert!(
            tail.code
                .iter()
                .any(|instruction| matches!(instruction, Instr::CallNamed { .. })),
            "NOTINLINE forbids bytecode self-tail rewriting too"
        );
    }
    #[test]
    fn proclaimed_notinline_and_lexical_inline_override() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let name = "SELF-PROOF-PROCLAIMED";
        let symbol = resolve_sym(name).unwrap();
        let key = resolve_sym("EGCL-INTERNAL::INLINE-POLICY").unwrap();
        let policy = resolve_sym("NOTINLINE").unwrap();
        super::super::super::symbol_plist_put(symbol, key, policy);
        assert!(
            lower(name, "((+ 1 (self-proof-proclaimed n)))", &env)
                .self_call_sites
                .is_empty()
        );
        assert_eq!(lower(name, "((locally (declare (inline self-proof-proclaimed)) (+ 1 (self-proof-proclaimed n))))", &env).self_call_sites.len(), 1);
        egcl_rt::symbols::set_symbol_plist(symbol.as_symbol_index(), NIL);
    }
    #[test]
    fn enclosing_locally_policy_survives_delayed_defun_compilation() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        super::super::super::read_eval_all_env(
            "(locally (declare (notinline self-proof-delayed)) (defun self-proof-delayed (n) (+ 1 (self-proof-delayed n))))", &mut env).unwrap();
        assert!(env.active_declarations.is_empty());
        let symbol = resolve_sym("SELF-PROOF-DELAYED").unwrap();
        egcl_rt::rooted!(
            function = egcl_rt::symbols::symbol_function(symbol.as_symbol_index()).unwrap()
        );
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(body = egcl_rt::function::body(*function));
        let compiled =
            compile_function("SELF-PROOF-DELAYED", *params, *body, &env, true, false).unwrap();
        assert!(compiled.self_call_sites.is_empty());
    }

    #[test]
    fn docstring_does_not_hide_function_notinline() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let body = lower(
            "SELF-PROOF-DOC",
            r#"("A recursive function." (declare (notinline self-proof-doc)) (self-proof-doc n))"#,
            &env,
        );
        assert!(body.self_call_sites.is_empty());
        assert!(
            body.code
                .iter()
                .any(|instruction| matches!(instruction, Instr::CallNamed { .. })),
            "docstring must not enable the self-tail rewrite"
        );
    }

    #[test]
    fn docstring_keeps_inner_policy_after_inherited_declarations() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for (outer, inner, expected) in [("inline", "notinline", 0), ("notinline", "inline", 1)] {
            let source = format!(
                r#"(locally (declare ({outer} self-proof-doc-override)) (defun self-proof-doc-override (n) "Keep this documentation first." (declare ({inner} self-proof-doc-override)) (+ 1 (self-proof-doc-override n))))"#
            );
            super::super::super::read_eval_all_env(&source, &mut env).unwrap();
            assert!(env.active_declarations.is_empty());
            let symbol = resolve_sym("SELF-PROOF-DOC-OVERRIDE").unwrap();
            egcl_rt::rooted!(
                function = egcl_rt::symbols::symbol_function(symbol.as_symbol_index()).unwrap()
            );
            egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
            egcl_rt::rooted!(body = egcl_rt::function::body(*function));
            assert!(
                cp(super::super::super::body_through_implicit_block(*body))
                    .0
                    .is_string(),
                "capturing policy must preserve the initial docstring"
            );
            let compiled =
                compile_function("SELF-PROOF-DOC-OVERRIDE", *params, *body, &env, true, false)
                    .unwrap();
            assert_eq!(
                compiled.self_call_sites.len(),
                expected,
                "inner {inner} overrides outer {outer} after docstring"
            );
        }
    }

    #[test]
    fn macrolet_declarations_keep_call_policy_scoped_after_preexpansion() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for (operator, bindings, argument) in [
            ("macrolet", "((local-identity (x) x))", "(local-identity n)"),
            ("symbol-macrolet", "((local-value n))", "local-value"),
        ] {
            let source = format!(
                "((+ (self-proof-macro n) ({operator} {bindings} (declare (notinline self-proof-macro)) (self-proof-macro {argument})) (self-proof-macro n)))"
            );
            let compiled = lower("SELF-PROOF-MACRO", &source, &env);
            assert_eq!(
                compiled.self_call_sites.len(),
                2,
                "{operator} NOTINLINE is local to its body"
            );
            let source = format!(
                "((declare (notinline self-proof-macro)) (+ ({operator} {bindings} (declare (inline self-proof-macro)) (self-proof-macro {argument})) (self-proof-macro n)))"
            );
            let compiled = lower("SELF-PROOF-MACRO", &source, &env);
            assert_eq!(
                compiled.self_call_sites.len(),
                1,
                "{operator} INLINE overrides outer NOTINLINE without leaking"
            );
            let nested = if operator == "macrolet" {
                "symbol-macrolet"
            } else {
                "macrolet"
            };
            let source = format!(
                "(({operator} {bindings} (+ (self-proof-macro n) ({nested} () (declare (notinline self-proof-macro)) (self-proof-macro {argument})) (self-proof-macro n))))"
            );
            let compiled = lower("SELF-PROOF-MACRO", &source, &env);
            assert_eq!(
                compiled.self_call_sites.len(),
                2,
                "nested {nested} inside {operator} preserves NOTINLINE without affecting surrounding calls"
            );
        }
    }

    #[test]
    fn lexical_self_identity_requires_resolved_gensym() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let self_symbol = resolve_sym("SELF-PROOF-GENSYM").unwrap().as_symbol_index();
        let other_symbol = resolve_sym("SELF-PROOF-OTHER").unwrap().as_symbol_index();
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(
            body = reader::read_from_string("((+ 1 (self-proof-local n)))")
                .unwrap()
                .0
        );
        let lower = |target| {
            let locals = HashMap::from([("SELF-PROOF-LOCAL".to_string(), target)]);
            compile_local_function(
                self_symbol,
                &[],
                *params,
                *body,
                &env,
                &HashMap::new(),
                &locals,
                true,
            )
            .unwrap()
        };
        assert_eq!(lower(self_symbol).self_call_sites.len(), 1);
        assert!(lower(other_symbol).self_call_sites.is_empty());
    }
    #[test]
    fn legacy_bbu_has_no_self_proof_and_optional_table_validates_call_identity() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let name = resolve_sym("SELF-PROOF-BBU").unwrap();
        let other = resolve_sym("SELF-PROOF-BBU-OTHER").unwrap();
        let code = vec![0x02, 0, 0, 0x0d, 0, 0, 0, 0, 1, 0, 0x10, 1, 0];
        let encoded = BbuFunction {
            self_call_sites: vec![3],
            name_ref: 0,
            lambda_list_ref: BBU_NO_INDEX,
            flags: 0,
            arity_min: 1,
            arity_max: 1,
            n_locals: 1,
            max_stack: 1,
            code,
            literal_refs: Vec::new(),
            param_layout: Vec::new(),
            has_env: false,
            variadic: false,
            restart_tables: Vec::new(),
            handler_cases: Vec::new(),
        };
        let mut record = Vec::new();
        serialize_bbu_function_record(&mut record, &encoded);
        let mut legacy = parse_bbu_function(&mut BbuCursor::new(&record)).unwrap();
        assert!(
            decode_bbu_function(&legacy, &[name, other], &[])
                .unwrap()
                .self_call_sites
                .is_empty()
        );
        let table = serialize_bbu_self_call_sites(&[encoded]);
        legacy.self_call_sites = parse_bbu_self_call_sites(&table, 1).unwrap().remove(0);
        assert_eq!(
            decode_bbu_function(&legacy, &[name, other], &[])
                .unwrap()
                .self_call_sites,
            vec![1]
        );
        legacy.name_ref = 1;
        assert!(decode_bbu_function(&legacy, &[name, other], &[]).is_err());
        legacy.name_ref = 0;
        legacy.self_call_sites = vec![0];
        assert!(decode_bbu_function(&legacy, &[name, other], &[]).is_err());
    }
}
