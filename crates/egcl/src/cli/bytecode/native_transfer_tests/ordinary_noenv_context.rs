// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::cell::Cell;

fn template(name: &str, params: &str, forms: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    assert!(!body.has_env);
    body
}

fn install_owner(symbol: u32) -> Rc<NativeCode> {
    let input = snapshot_t2_input(symbol, 0).expect("NoEnv T2 snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("NoEnv T2 artifact");
    install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("NoEnv T2 publication")
}

fn install(function: EgclVal) -> Rc<NativeCode> {
    let symbol = egcl_rt::function::definition_index(function);
    closure_compilation_owner(symbol, 1);
    install_owner(symbol)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn private_noenv_probe_shares_code_and_preserves_distinct_actual_identities() {
    use std::sync::atomic::Ordering;
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // The ordinary-looking display name must not select a named/public ABI.
    let body = template("PRIVATE-NOENV-DISPLAY", "(next)", "((1+ next))", &env);
    egcl_rt::rooted!(first = make_bytecode_closure(&body, None));
    egcl_rt::rooted!(second = make_bytecode_closure(&body, None));
    let first_id = egcl_rt::function::definition_index(*first);
    let native = install(*first);
    let NativeCodeStorage::Mapped(callee) = &native._storage else {
        panic!("mapped NoEnv owner")
    };
    assert!(
        native.is_t2 && callee.ordinary_entry.is_some() && callee.native_callable_slot.is_some()
    );
    assert!(closure_captured_env(*first).is_none() && closure_captured_env(*second).is_none());
    assert!(egcl_rt::call_table::resolve_ordinal(first_id).is_none());
    for function in [*first, *second] {
        let (owner, target, actual, _) =
            native_transfer_entry::funcall_fast::native_target(function, 1)
                .expect("NoEnv native target");
        assert_eq!(owner, first_id);
        assert!(Rc::ptr_eq(&target, callee));
        assert_eq!(actual, function);
        assert!(callee.accepts_explicit_callable(actual));
    }
    let (symbol, caller) = ordinary_t2::install(
        "NOENV-PROBE-CALLER",
        "(f next)",
        "((funcall f next))",
        &mut env,
    );
    for _ in 0..4 {
        for function in [*first, *second] {
            assert_eq!(
                run_native(
                    &caller,
                    symbol,
                    &[function, EgclVal::from_fixnum(8)],
                    &mut env
                )
                .unwrap(),
                EgclVal::from_fixnum(9)
            );
        }
    }
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    for function in [*first, *second] {
        assert_eq!(
            run_native(
                &caller,
                symbol,
                &[function, EgclVal::from_fixnum(9)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(10)
        );
    }
    assert_eq!(
        native_transfer_entry::take_lean_root_entries_for_test(),
        2,
        "only outer caller roots, no Rust child root"
    );
    assert_eq!(
        native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
        0,
        "both calls must use the generated child probe"
    );
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(!closure_bodies().borrow().contains_key(&first_id));
    assert!(NATIVE_REGISTRY.with(|codes| {
        codes
            .borrow()
            .get(&first_id)
            .is_some_and(|code| Rc::ptr_eq(code, &native))
    }));
    assert_eq!(
        run_native(
            &caller,
            symbol,
            &[*second, EgclVal::from_fixnum(12)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(13)
    );
    let revision = callee.private_entry_revision();
    let before = revision.load(Ordering::Acquire);
    *second = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(!NATIVE_REGISTRY.with(|codes| codes.borrow().contains_key(&first_id)));
    assert_ne!(
        revision.load(Ordering::Acquire),
        before,
        "retirement revokes probes even while native Rc is retained"
    );
}

thread_local! {
    static YOUNG_BITS: Cell<u64> = const { Cell::new(0) };
    static OBSERVED: Cell<(u64, u64, u64, bool)> = const { Cell::new((0, 0, 0, false)) };
}
fn collect_during_noenv_guard(words: *const u64, count: usize, _: bool) {
    let wanted = YOUNG_BITS.with(Cell::get);
    let before_callable =
        native_transfer_entry::current_guard_callable_for_test().map_or(0, |value| value.0);
    let slot = (0..count).find(|&index| unsafe { *words.add(index) == wanted });
    let ok = egcl_rt::gc::collect_t0_minor().is_ok();
    let moved = slot.map_or(0, |index| unsafe { *words.add(index) });
    let after_callable =
        native_transfer_entry::current_guard_callable_for_test().map_or(0, |value| value.0);
    OBSERVED.with(|seen| seen.set((before_callable, after_callable, moved, ok)));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn private_noenv_child_guard_relocates_argument_and_resumes_actual_instance() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("PRIVATE-NOENV-GUARD", "(next)", "((1+ next))", &env);
    egcl_rt::rooted!(first = make_bytecode_closure(&body, None));
    egcl_rt::rooted!(second = make_bytecode_closure(&body, None));
    let native = install(*first);
    let NativeCodeStorage::Mapped(callee) = &native._storage else {
        panic!("mapped NoEnv guard")
    };
    assert!(callee.has_deopt && callee.native_callable_slot.is_some());
    let (symbol, caller) = ordinary_t2::install(
        "NOENV-GUARD-CALLER",
        "(f next)",
        "((funcall f next))",
        &mut env,
    );
    for _ in 0..4 {
        assert_eq!(
            run_native(
                &caller,
                symbol,
                &[*second, EgclVal::from_fixnum(2)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(3)
        );
    }
    *first = NIL;
    // Empty the nursery before the sole young argument is constructed. No
    // Lisp allocation occurs between this allocation and the guard callback.
    egcl_rt::gc::collect_t0_minor().unwrap();
    // Every GC retires the weak FUNCALL probe. Refill it with nonallocating
    // fixnum calls before constructing the young argument, so the failing
    // call below enters the child through generated code rather than Rust.
    for _ in 0..4 {
        assert_eq!(
            run_native(
                &caller,
                symbol,
                &[*second, EgclVal::from_fixnum(2)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(3)
        );
    }
    egcl_rt::rooted!(young = egcl_rt::gc::alloc_double_float(2.5));
    let before = young.0;
    YOUNG_BITS.with(|bits| bits.set(before));
    OBSERVED.with(|seen| seen.set((0, 0, 0, false)));
    let prior = callee.deopt_count();
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    let result = native_transfer_entry::with_ordinary_guard_payload_observer_for_test(
        collect_during_noenv_guard,
        || run_native(&caller, symbol, &[*second, *young], &mut env),
    );
    assert_eq!(result.unwrap().as_double_float(), 3.5);
    assert_eq!(
        native_transfer_entry::take_lean_root_entries_for_test(),
        1,
        "the failing generated child must not create a Rust root activation"
    );
    assert_eq!(
        native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
        0,
        "the failing call must hit the generated FUNCALL probe"
    );
    assert_eq!(callee.deopt_count(), prior + 1);
    let (actual_before, actual_after, payload, collected) = OBSERVED.with(Cell::get);
    assert!(collected);
    assert_eq!(
        actual_before, second.0,
        "precise home names B, not compilation owner A"
    );
    assert_eq!(actual_after, second.0);
    assert_ne!(
        payload, 0,
        "outgoing payload contains the original argument"
    );
    assert_ne!(
        payload, before,
        "real minor relocation must occur during recovery"
    );
    assert_eq!(payload, young.0);
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn private_noenv_admission_preserves_named_binding_and_exact_body_checks() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("NOENV-PUBLIC-CONTROL", "(next)", "((1+ next))", &env);
    egcl_rt::rooted!(first = make_bytecode_closure(&body, None));
    let private = install(*first);
    let NativeCodeStorage::Mapped(callee) = &private._storage else {
        panic!("NoEnv code")
    };
    assert!(callee.native_callable_slot.is_some());
    let different = template("NOENV-DIFFERENT-BODY", "(next)", "((1+ next))", &env);
    egcl_rt::rooted!(other = make_bytecode_closure(&different, None));
    assert!(
        !callee.accepts_explicit_callable(*other),
        "identical instructions are not exact body ownership"
    );
    let public = resolve_sym("NOENV-PUBLIC-CONTROL")
        .unwrap()
        .as_symbol_index();
    registry_put(public, Arc::clone(&body));
    egcl_rt::symbols::set_symbol_function(public, *first);
    let named = install_owner(public);
    let NativeCodeStorage::Mapped(named) = &named._storage else {
        panic!("public ordinary code")
    };
    assert!(
        named.ordinary_entry.is_some() && named.native_callable_slot.is_none(),
        "named/public ABI must remain unchanged"
    );
    assert_eq!(
        named
            .run_callable(*first, &[EgclVal::from_fixnum(4)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(5)
    );
    egcl_rt::symbols::set_symbol_function(public, egcl_rt::value::UNBOUND);
    registry_remove(public);
    let (symbol, wrong) =
        ordinary_t2::install("NOENV-WRONG-ARITY", "(f)", "((funcall f))", &mut env);
    let error = run_native(&wrong, symbol, &[*first], &mut env).unwrap_err();
    assert!(
        matches!(error.without_backtrace(), EgclError::ProgramError(_)),
        "wrong arity must retain PROGRAM-ERROR: {error:?}"
    );
    let broad = template("NOENV-FOUR-ARGUMENT-CONTROL", "(a b c d)", "(a)", &env);
    egcl_rt::rooted!(broad = make_bytecode_closure(&broad, None));
    let broad = install(*broad);
    let NativeCodeStorage::Mapped(broad) = &broad._storage else {
        panic!("four argument control")
    };
    assert!(
        broad.native_callable_slot.is_none(),
        "overflow arity stays on existing ABI"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn private_noenv_probe_transports_zero_two_and_three_actuals() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for (count, params, forms, caller_params, caller_forms, expected) in [
        (0, "()", "(41)", "(f)", "((funcall f))", 41),
        (
            2,
            "(a b)",
            "((declare (type fixnum a b)) (+ a b))",
            "(f a b)",
            "((funcall f a b))",
            3,
        ),
        (
            3,
            "(a b c)",
            "((declare (type fixnum a b c)) (+ (+ a b) c))",
            "(f a b c)",
            "((funcall f a b c))",
            6,
        ),
    ] {
        let body = template("NOENV-ARGUMENT-BOUNDARY", params, forms, &env);
        egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
        let native = install(*function);
        let NativeCodeStorage::Mapped(callee) = &native._storage else {
            panic!("NoEnv argument owner")
        };
        assert!(
            callee.ordinary_entry.is_some() && callee.native_callable_slot.is_some(),
            "arity {count} must install the leaf callable-home ABI"
        );
        let name = format!("NOENV-ARGUMENT-CALLER-{count}");
        let (symbol, caller) = ordinary_t2::install(&name, caller_params, caller_forms, &mut env);
        let mut args = vec![*function];
        args.extend((1..=count).map(EgclVal::from_fixnum));
        egcl_rt::rooted_ref!(_args = &mut args);
        for _ in 0..4 {
            assert_eq!(
                run_native(&caller, symbol, &args, &mut env).unwrap(),
                EgclVal::from_fixnum(expected)
            );
        }
        let _ = native_transfer_entry::take_lean_root_entries_for_test();
        let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
        assert_eq!(
            run_native(&caller, symbol, &args, &mut env).unwrap(),
            EgclVal::from_fixnum(expected)
        );
        assert_eq!(
            native_transfer_entry::take_lean_root_entries_for_test(),
            1,
            "only the outer root at arity {count}"
        );
        assert_eq!(
            native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
            0,
            "native child probe at arity {count}"
        );
    }
}
