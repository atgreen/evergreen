// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A global name bound to a captured closure enters the shared template word.
use super::*;

fn call(code: &NativeCode, symbol: u32, env: &mut Env) -> EgclVal {
    run_native(code, symbol, &[NIL], env).unwrap()
}

fn reset_routes() {
    native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    native_transfer_entry::ordinary_calls::take_cold_calls_for_test();
    native_transfer_entry::ordinary_calls::take_closure_misses_for_test();
}

fn assert_generated_routes() {
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_named_entries_for_test(),
        0,
        "a warmed named closure must not visit its named runtime adapter"
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_cold_calls_for_test(),
        0,
        "a warmed named closure must not take cold admission"
    );
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_closure_misses_for_test(),
        0,
        "the template stub must enter the exact installed body"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn named_heap_closure_words_survive_redefinition_relocation_and_foreign_claims() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(held)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((lambda (n) held))").unwrap().0);
    let maker = Arc::new(
        compile_function("NAMED-HEAP-MAKER", *params, *forms, &env, true, false)
            .unwrap_or_else(|| panic!("maker: {:?}", last_bail_reason())),
    );
    egcl_rt::rooted!(_maker = ActiveBytecodeRoot::new(&maker));
    let body = Arc::clone(&maker.nested_functions[0]);
    assert!(body.code.iter().any(|op| matches!(op, Instr::ClosureEnv)));
    egcl_rt::rooted!(seed = heap_env::make_closure(&body, NIL, &[NIL], &env).unwrap());
    let owner_symbol = egcl_rt::function::definition_index(*seed);
    let (_, template_native) = ordinary_t2::install_body(owner_symbol, Arc::clone(&body));
    assert!(template_native.is_t2);
    let NativeCodeStorage::Mapped(template) = &template_native._storage else {
        panic!("native template")
    };
    assert!(template.native_callable_slot.is_some());
    let entry = template.ordinary_lean_entry_address().unwrap();

    // The lowerer requires defined callees. Keep these placeholders out of
    // either caller with NOTINLINE, then replace their function cells below.
    crate::cli::read_eval_all_env(
        "(defun named-heap-leaf (n) n)
         (defun named-heap-retained (n) n)
         (defun named-heap-caller (n)
           (declare (notinline named-heap-leaf)) (named-heap-leaf n))
         (defun named-heap-retained-caller (n)
           (declare (notinline named-heap-retained)) (named-heap-retained n))",
        &mut env,
    )
    .unwrap();
    let (caller_symbol, caller) = ordinary_t2::install(
        "NAMED-HEAP-CALLER",
        "(n)",
        "((declare (notinline named-heap-leaf)) (named-heap-leaf n))",
        &mut env,
    );
    let (retained_symbol, retained_caller) = ordinary_t2::install(
        "NAMED-HEAP-RETAINED-CALLER",
        "(n)",
        "((declare (notinline named-heap-retained)) (named-heap-retained n))",
        &mut env,
    );
    for (code, target) in [
        (&caller, "NAMED-HEAP-LEAF"),
        (&retained_caller, "NAMED-HEAP-RETAINED"),
    ] {
        let target = egcl_rt::symbols::intern(target);
        assert_eq!(
            code.body
                .as_ref()
                .unwrap()
                .code
                .iter()
                .filter(|instruction| {
                    matches!(instruction, Instr::CallNamed { sym, .. } if *sym == target)
                })
                .count(),
            1,
            "NOTINLINE must retain the intended named call"
        );
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            panic!("native caller")
        };
        assert!(code.is_t2 && mapped.ordinary_entry.is_some() && mapped.mapped_entry().is_none());
    }
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(payload = arena_cons(EgclVal::from_fixnum(11), NIL));
    egcl_rt::rooted!(first = heap_env::make_closure(&body, NIL, &[*payload], &env).unwrap());
    assert!(!egcl_rt::function::is_pinned(*first));
    assert_eq!(
        egcl_rt::function::entry_word(*first),
        Some((0, 0)),
        "warming must teach the word"
    );
    let leaf = egcl_rt::symbols::intern("NAMED-HEAP-LEAF");
    egcl_rt::symbols::set_symbol_function(leaf, *first);
    for _ in 0..3 {
        assert_eq!(call(&caller, caller_symbol, &mut env), *payload);
    }
    let (stub, shared) = egcl_rt::function::entry_word(*first).unwrap();
    assert_eq!(shared, egcl_rt::function::SHARED_ENTRY_OWNER);
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let owner = unsafe { &*execution }.code_owner();
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((entry, owner))
    );
    reset_routes();
    for _ in 0..16 {
        assert_eq!(call(&caller, caller_symbol, &mut env), *payload);
    }
    assert_generated_routes();

    let before = (
        first.to_raw(),
        payload.to_raw(),
        egcl_rt::function::closure_env(*first).to_raw(),
    );
    HeapCollector::new().minor_gc().unwrap();
    assert_ne!(
        first.to_raw(),
        before.0,
        "the global callable must actually move"
    );
    assert_ne!(payload.to_raw(), before.1, "its capture must actually move");
    assert_ne!(egcl_rt::function::closure_env(*first).to_raw(), before.2);
    assert_eq!(egcl_rt::symbols::symbol_function(leaf), Some(*first));
    reset_routes();
    for _ in 0..16 {
        assert_eq!(call(&caller, caller_symbol, &mut env), *payload);
    }
    assert_generated_routes();

    // Shared words still enter a stub which checks ownership itself. A
    // different execution's claim must miss, and must never be stolen.
    let foreign = owner + 1_000_000;
    native_transfer_entry::closure_entry::set_record_for_test(stub, entry, foreign);
    reset_routes();
    for _ in 0..2 {
        assert_eq!(call(&caller, caller_symbol, &mut env), *payload);
    }
    assert_eq!(
        native_transfer_entry::ordinary_calls::take_closure_misses_for_test(),
        2
    );
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((entry, foreign))
    );
    native_transfer_entry::closure_entry::set_record_for_test(stub, entry, owner);

    let retained = egcl_rt::symbols::intern("NAMED-HEAP-RETAINED");
    egcl_rt::symbols::set_symbol_function(retained, *first);
    egcl_rt::rooted!(replacement_payload = arena_cons(EgclVal::from_fixnum(22), NIL));
    egcl_rt::rooted!(
        replacement = heap_env::make_closure(&body, NIL, &[*replacement_payload], &env).unwrap()
    );
    egcl_rt::symbols::set_symbol_function(leaf, *replacement);
    reset_routes();
    for _ in 0..8 {
        assert_eq!(call(&caller, caller_symbol, &mut env), *replacement_payload);
        assert_eq!(call(&retained_caller, retained_symbol, &mut env), *payload);
    }
    assert_generated_routes();
}
