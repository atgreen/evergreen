// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_rt::Collector;

fn leaf_body(name: &str, forms: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap())
}

fn emit_leaf(body: Arc<BytecodeFunction>, tier: CompilationTier) -> TransferCode {
    let symbol = egcl_rt::symbols::intern(&body.name);
    let ir = egcl_compiler::t2::build::build_for_transfer_optimization(&body, symbol)
        .expect("identity fixture IR");
    TransferCode::emit_prepared_with_ordinary(
        Arc::clone(&body),
        retain_native_body(&body),
        &ir,
        Some(&ir),
        None,
        &[],
        false,
        tier,
    )
    .expect("identity fixture emits the ordinary leaf entry")
}

#[test]
fn audited_leaf_payload_retains_exact_code_maps_arity_and_root_token() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = leaf_body("ARTIFACT-RETAINED-LEAF", "(x)", &env);
    assert!(binding_free_leaf_body(&body));
    let code = emit_leaf(body, CompilationTier::T2);
    assert!(
        code.ordinary_entry.is_some(),
        "exercise actual ordinary code"
    );
    let ExecutableOwner::AuditedLeaf(payload) = &code.executable else {
        panic!("binding-free ordinary T2 fixture must receive an audited payload")
    };
    let first = Arc::clone(payload);
    let second = Arc::clone(payload);
    let weak_payload = Arc::downgrade(payload);
    let weak_roots = Arc::downgrade(&code._body_roots);
    assert!(Arc::ptr_eq(
        payload.retained_roots.as_ref().unwrap(),
        &code._body_roots,
    ));
    let entry = payload.code.as_ptr();
    let code_info = payload.code_info;
    let calls = Arc::as_ptr(&payload._native_calls);
    let bytes = code.code_bytes_for_test().to_vec();
    let arity = payload._ordinary_arity.as_ref().unwrap().as_ptr();
    // A payload handle permits inspecting frozen bytes/maps, never entering code
    // after its installation/PC lease has retired.
    drop(code);
    drop(first);
    assert_eq!(second.code.as_ptr(), entry);
    assert!(std::ptr::eq(second.code_info, code_info));
    assert_eq!(Arc::as_ptr(&second._native_calls), calls);
    assert_eq!(second._ordinary_arity.as_ref().unwrap().as_ptr(), arity);
    assert_eq!(
        unsafe { std::slice::from_raw_parts(second.code.as_ptr(), second.code_len) },
        bytes,
    );
    assert!(
        weak_roots.upgrade().is_some(),
        "a body Arc alone is insufficient"
    );
    drop(second);
    assert!(weak_payload.upgrade().is_none());
    assert!(
        weak_roots.upgrade().is_none(),
        "last payload releases the root token"
    );
}

#[test]
fn installation_feedback_and_retirement_stay_independent_of_payload_leases() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = leaf_body("ARTIFACT-INSTALLATION-IDENTITY", "(x)", &env);
    let first = Rc::new(emit_leaf(Arc::clone(&body), CompilationTier::T2));
    let second = Rc::new(emit_leaf(body, CompilationTier::T2));
    let first_revision = first.private_entry_revision();
    let second_revision = second.private_entry_revision();
    let second_before = second_revision.load(std::sync::atomic::Ordering::Acquire);
    first.request_recompile();
    assert!(first.needs_recompile());
    assert!(!second.needs_recompile());
    assert_eq!(first_revision.load(std::sync::atomic::Ordering::Acquire), 1);
    assert_eq!(
        second_revision.load(std::sync::atomic::Ordering::Acquire),
        second_before
    );
    assert!(!Rc::ptr_eq(&first.callable_cache, &second.callable_cache));
    assert_eq!(
        second.run(&[EgclVal::from_fixnum(43)], &mut env).unwrap(),
        EgclVal::from_fixnum(43),
    );
}

#[test]
fn execution_bound_variants_keep_installation_owned_payloads() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = leaf_body("ARTIFACT-T1-IDENTITY", "(x)", &env);
    let t1 = emit_leaf(Arc::clone(&body), CompilationTier::T1);
    assert!(t1.ordinary_entry.is_some());
    assert!(matches!(t1.executable, ExecutableOwner::Installation(_)));
    let mut changed = (*body).clone();
    changed.code.insert(
        0,
        Instr::LoadGlobal(egcl_rt::symbols::intern("ARTIFACT-DYNAMIC")),
    );
    assert!(!binding_free_leaf_body(&changed));
    for instruction in [
        Instr::CallNamed {
            sym: egcl_rt::symbols::intern("FUNCALL"),
            nargs: 1,
        },
        Instr::Br(0),
        Instr::LoadEnvVar(0),
    ] {
        changed = (*body).clone();
        changed.code.insert(0, instruction);
        assert!(!binding_free_leaf_body(&changed));
    }
    changed = (*body).clone();
    changed.code.insert(0, Instr::SetValues(1));
    assert!(!binding_free_leaf_body(&changed));
    changed = (*body).clone();
    egcl_rt::rooted!(heap_constant = crate::cli::arena_cons(EgclVal::from_fixnum(7), NIL));
    changed.constants.push(*heap_constant);
    assert!(
        !binding_free_leaf_body(&changed),
        "heap slots stay outside the audited cohort"
    );
}

#[test]
fn retained_installation_runs_original_native_code_after_redefinition_and_gc() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env("(defun artifact-retired-native (x) x)", &mut env).unwrap();
    let (symbol, native) = super::super::super::native_transfer_tests::ordinary_t2::install(
        "ARTIFACT-RETIRED-NATIVE",
        "(x)",
        "(x)",
        &mut env,
    );
    egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("fixture must own mapped ordinary T2 code")
    };
    assert!(matches!(&code.executable, ExecutableOwner::AuditedLeaf(_)));
    // Entry permission remains with this retained installation, never a bare
    // payload lease. Its exact maps and arity buffer survive replacement.
    let retained_installation = Rc::clone(code);
    drop(native);
    crate::cli::read_eval_all_env("(defun artifact-retired-native (x) 99)", &mut env).unwrap();
    egcl_rt::rooted!(value = crate::cli::arena_cons(EgclVal::from_fixnum(43), NIL));
    egcl_rt::HeapCollector::new().minor_gc().unwrap();
    take_lean_root_entries_for_test();
    assert_eq!(
        retained_installation
            .run_callable(*original, &[*value], &mut env)
            .unwrap(),
        *value,
    );
    assert!(
        take_lean_root_entries_for_test() > 0,
        "execute actual native ordinary entry"
    );
    assert_eq!(crate::cli::cp(*value), (EgclVal::from_fixnum(43), NIL));
    assert_eq!(
        crate::cli::read_eval_all_env("(artifact-retired-native nil)", &mut env).unwrap(),
        EgclVal::from_fixnum(99),
    );
}
