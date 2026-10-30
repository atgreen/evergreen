// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A bytecode caller entering an installed T2 callee (GitHub #487). The call
//! must reach the mapped native entry on every iteration, without revisiting
//! tier dispatch and without allocating a fresh ordinary root identity.

use super::*;

#[test]
fn a_bytecode_caller_enters_an_installed_t2_leaf_directly() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env("(defun t0-t2-leaf (a b) (+ a b))", &mut env).unwrap();
    let (symbol, code) = super::ordinary_t2::install("T0-T2-LEAF", "(a b)", "((+ a b))", &mut env);
    assert!(code.is_t2, "the probe needs an installed T2 callee");
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("the probe needs mapped T2 code")
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "the leaf uses the ordinary ABI"
    );
    assert!(
        mapped.needs_callable_identity(),
        "each entry retains the leaf's root identity"
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_eq!(
        egcl_rt::function::tier(*function),
        2,
        "publication records the tier"
    );

    // The caller runs once, so it stays bytecode while its loop calls the leaf.
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(
        forms =
            reader::read_from_string("((let ((s 0)) (dotimes (i n s) (setq s (t0-t2-leaf s 1)))))")
                .unwrap()
                .0
    );
    let caller =
        Arc::new(compile_function("T0-T2-CALLER", *params, *forms, &env, false, false).unwrap());
    let calls = 300;
    // Warm once so the first-call root identity is not counted below.
    assert_eq!(
        run(
            Arc::clone(&caller),
            &[EgclVal::from_fixnum(1)],
            NIL,
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(1)
    );
    TIER_DISPATCHES.with(|count| count.set(0));
    super::super::call_table::take_target_lookups_for_test();
    super::super::call_table::take_root_identities_for_test();
    super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let result = run(caller, &[EgclVal::from_fixnum(calls)], NIL, &mut env).unwrap();
    let dispatches = TIER_DISPATCHES.with(std::cell::Cell::get);
    let entries = super::super::call_table::take_target_lookups_for_test();
    let identities = super::super::call_table::take_root_identities_for_test();
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(result, EgclVal::from_fixnum(calls));
    assert!(
        entries >= calls as usize,
        "only {entries} of {calls} calls entered native code"
    );
    assert_eq!(
        dispatches, 0,
        "an installed T2 callee needs no tier dispatch"
    );
    assert_eq!(
        identities, 0,
        "steady-state calls reuse their ordinary root identity"
    );
    assert!(
        lean >= calls as usize,
        "only {lean} of {calls} calls took the lean root entry"
    );
}

/// Compile `forms` as a bytecode caller taking `(n)`, run it once with 1 to
/// warm its callees' cells, then again with `n`, returning the second result.
fn run_caller(name: &str, forms: &str, n: i64, env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let caller = Arc::new(
        compile_function(name, *params, *forms, env, false, false)
            .unwrap_or_else(|| panic!("{name}: {:?}", last_bail_reason())),
    );
    run(Arc::clone(&caller), &[EgclVal::from_fixnum(1)], NIL, env).unwrap();
    super::super::native_transfer_entry::take_lean_root_entries_for_test();
    super::super::native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    super::super::call_table::take_resolves_for_test();
    run(caller, &[EgclVal::from_fixnum(n)], NIL, env)
}

#[test]
fn a_t2_leaf_reaches_a_builtin_through_its_cell_without_resolving_it() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env("(defun t0-t2-elt (s i) (elt s i))", &mut env).unwrap();
    let (symbol, code) = super::ordinary_t2::install("T0-T2-ELT", "(s i)", "((elt s i))", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("the probe needs mapped T2 code")
    };
    assert!(
        mapped.lean_root_entry().is_some(),
        "the leaf has a lean root entry"
    );
    let calls = 200;
    let result = run_caller(
        "T0-T2-ELT-CALLER",
        r#"((let ((s 0)) (dotimes (i n s) (setq s (+ s (t0-t2-elt '(1 2 3) 1))))))"#,
        calls,
        &mut env,
    )
    .unwrap();
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let named = super::super::native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    let resolves = super::super::call_table::take_resolves_for_test();
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(result, EgclVal::from_fixnum(2 * calls));
    assert!(
        lean >= calls as usize,
        "only {lean} of {calls} calls took the lean root entry"
    );
    assert!(
        named >= calls as usize,
        "only {named} of {calls} ELT calls used the named adapter"
    );
    assert_eq!(
        resolves, 0,
        "the named adapter carries its cell: no per-call resolution"
    );
}

#[test]
fn a_lean_root_entry_deoptimizes_and_signals_like_the_interpreter() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun t0-t2-guarded (a b) (+ a b)) (defun t0-t2-car (x) (car x))",
        &mut env,
    )
    .unwrap();
    let (guarded, _) = super::ordinary_t2::install("T0-T2-GUARDED", "(a b)", "((+ a b))", &mut env);
    let (car, _) = super::ordinary_t2::install("T0-T2-CAR", "(x)", "((car x))", &mut env);
    let deopts = deopt_count();
    // A fixnum guard fails inside the lean root on the last iteration; the
    // interpreter's result is the bignum one past the largest fixnum.
    let exact = run_caller(
        "T0-T2-DEOPT-CALLER",
        "((let ((s (if (= n 1) 0 (- most-positive-fixnum (1- n))))) \
            (dotimes (i n) (setq s (t0-t2-guarded s 1))) \
            (or (= n 1) (= s (1+ most-positive-fixnum)))))",
        50,
        &mut env,
    )
    .unwrap();
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    assert!(
        lean >= 50,
        "only {lean} of 50 calls took the lean root entry"
    );
    assert_eq!(exact, T, "the overflowing sum must be exact");
    assert!(
        deopt_count() > deopts,
        "the overflowing call must deoptimize"
    );
    // A TYPE-ERROR signalled inside the lean root leaves it as an error.
    let error = run_caller(
        "T0-T2-ERROR-CALLER",
        "((t0-t2-car (if (= n 1) (list n) n)))",
        7,
        &mut env,
    )
    .expect_err("CAR of a fixnum signals");
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    for symbol in [guarded, car] {
        egcl_rt::symbols::set_symbol_function(symbol, NIL);
        registry_remove(symbol);
    }
    let report = format!("{error:?}");
    assert!(report.contains("is not of type list"), "{report}");
    // The managed frame the entry pushed names the callee in the backtrace.
    assert!(report.contains("\"T0-T2-CAR\""), "{report}");
    assert_eq!(lean, 1, "the failing call took the lean root entry");
}

fn assert_mapped_bridge_backtrace() {
    let rows = egcl_rt::debug_stack::capture_current(32);
    let entries: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.function
                .as_deref()
                .is_some_and(|name| name.ends_with("T0-T2-INNER"))
        })
        .collect();
    assert_eq!(entries.len(), 1, "{rows:?}");
    assert_eq!(entries[0].origin, egcl_rt::debug_stack::FrameOrigin::Native);
}

#[test]
fn a_mapped_caller_enters_an_ordinary_t2_callee_through_reused_bridge() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun t0-t2-observe () (%ordinary-t2-entry-for-test)) (defun t0-t2-inner (a b) (t0-t2-observe) (+ a b)) (defun t0-t2-kw (a &key (b (1+ 0))) (t0-t2-inner a b))",
        &mut env,
    )
    .unwrap();
    let (inner, inner_code) = super::ordinary_t2::install(
        "T0-T2-INNER",
        "(a b)",
        "((t0-t2-observe) (+ a b))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &inner_code._storage else {
        panic!("the callee needs mapped T2 code")
    };
    assert!(
        mapped.lean_root_entry().is_some(),
        "the callee has a lean root entry"
    );
    let (outer, outer_code) = super::ordinary_t2::install(
        "T0-T2-KW",
        "(a &key (b (1+ 0)))",
        "((t0-t2-inner a b))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &outer_code._storage else {
        panic!("the caller needs mapped T2 code")
    };
    assert!(
        mapped.ordinary_entry.is_none(),
        "a &key body with an evaluated default keeps the mapped ABI"
    );
    let _observer_pin = super::ordinary_t2::RecursiveFixturePins::new(&["T0-T2-OBSERVE"]);
    super::ordinary_t2::expect_entry(&inner_code);
    // Cold linkage may still enter the callee through its managed root. Warm
    // it before the observer that requires the measured bridge's native frame.
    run_caller(
        "T0-T2-KW-BRIDGE-WARMUP",
        "((let ((s 0)) (dotimes (i n s) (setq s (t0-t2-kw s :b 2)))))",
        1,
        &mut env,
    )
    .unwrap();
    super::ordinary_t2::expect_entry(&inner_code);
    struct RestoreTrace(Option<fn()>);
    impl Drop for RestoreTrace {
        fn drop(&mut self) {
            super::ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(self.0));
        }
    }
    let _trace = RestoreTrace(
        super::ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|hook| hook.replace(Some(assert_mapped_bridge_backtrace))),
    );
    super::super::native_transfer_entry::mapped_ordinary::take_entries_for_test();
    let calls = 100;
    super::super::native_transfer_entry::take_nested_entries();
    super::super::native_transfer_entry::take_lean_target_calls_for_test();
    let result = run_caller(
        "T0-T2-KW-CALLER",
        "((let ((s 0)) (dotimes (i n s) (setq s (t0-t2-kw s :b 2)))))",
        calls,
        &mut env,
    )
    .unwrap();
    let bridges = super::super::native_transfer_entry::mapped_ordinary::take_entries_for_test();
    assert!(
        super::ordinary_t2::observed_entry(),
        "actual ordinary machine callee must execute"
    );
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let children = super::super::native_transfer_entry::take_nested_entries();
    let direct = super::super::native_transfer_entry::take_lean_target_calls_for_test();
    for symbol in [inner, outer] {
        egcl_rt::symbols::set_symbol_function(symbol, NIL);
        registry_remove(symbol);
    }
    assert_eq!(result, EgclVal::from_fixnum(2 * calls));
    assert_eq!(
        lean, 0,
        "bridge calls must not create lean root activations"
    );
    assert!(
        bridges >= calls as usize,
        "every hot mapped call uses the bridge"
    );
    assert_eq!(
        children, 0,
        "no mapped child activation is prepared for an ordinary callee"
    );
    assert!(
        direct >= calls as usize,
        "only {direct} of {calls} calls left the &key body directly"
    );
}

/// Calls that left an ordinary T2 body for the runtime: named builtin
/// adapters and cold calls.
fn take_ordinary_outcalls() -> usize {
    super::super::native_transfer_entry::ordinary_calls::take_named_entries_for_test()
        + super::super::native_transfer_entry::ordinary_calls::take_cold_calls_for_test()
}

/// `(ELT s i)` on a simple string is read inline by T2 code: neither an
/// ordinary nor a mapped (&key) body calls out for it. Any other operand still
/// reaches the real ELT, through the same calls as before (GitHub #487). ELT
/// stands in for CHAR here: CHAR is defined by the boot image, which these
/// tests do not load; `t0_native_call_cli` covers it.
#[test]
fn t2_elt_reads_simple_strings_inline_and_calls_elt_for_anything_else() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun t0-t2-str-elt (s i) (elt s i)) (defun t0-t2-kw-elt (s &key (start (1- 1))) (elt s start))",
        &mut env,
    ).unwrap();
    let (leaf, _) = super::ordinary_t2::install("T0-T2-STR-ELT", "(s i)", "((elt s i))", &mut env);
    let (keyed, keyed_code) = super::ordinary_t2::install(
        "T0-T2-KW-ELT",
        "(s &key (start (1- 1)))",
        "((elt s start))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(mapped) = &keyed_code._storage else {
        panic!("the &key probe needs mapped T2 code")
    };
    assert!(
        mapped.ordinary_entry.is_none(),
        "a &key body with an evaluated default keeps the mapped ABI"
    );
    let calls = 200;
    let sum = |text: &str| -> i64 {
        (0..calls)
            .map(|i| text.chars().nth((i % 5) as usize).unwrap() as i64)
            .sum()
    };
    let take_mapped_calls = || {
        super::super::native_transfer_entry::take_published_checked_calls_for_test()
            + super::super::native_transfer_entry::take_nested_entries()
    };
    // "hello" is a base string; the second literal needs 32-bit characters.
    for (text, callee) in [
        ("hello", "(t0-t2-str-elt s (mod i 5))"),
        ("hλllo", "(t0-t2-kw-elt s :start (mod i 5))"),
    ] {
        let forms = format!(
            "((let ((s \"{text}\") (sum 0)) (dotimes (i n sum) (setq sum (+ sum (char-code {callee}))))))"
        );
        take_ordinary_outcalls();
        take_mapped_calls();
        let result = run_caller("T0-T2-ELT-STRING-CALLER", &forms, calls, &mut env).unwrap();
        let outcalls = take_ordinary_outcalls();
        let mapped_calls = take_mapped_calls();
        assert_eq!(result, EgclVal::from_fixnum(sum(text)), "{callee}");
        assert_eq!(
            (outcalls, mapped_calls),
            (0, 0),
            "{callee} called out for a simple string"
        );
    }
    // A list, a vector, an index past the end and a non-sequence reach ELT.
    let mut outcalls = 0;
    for (callee, expected) in [
        ("(t0-t2-str-elt '(1 2 3) 1)", 2),
        ("(t0-t2-kw-elt '(1 2 3) :start 1)", 2),
        ("(t0-t2-str-elt (vector 4 5) 1)", 5),
        ("(t0-t2-str-elt \"ab\" 5)", -1),
        ("(t0-t2-kw-elt \"ab\" :start -1)", -1),
        ("(t0-t2-kw-elt 7)", -1),
    ] {
        let forms = format!(
            "((let ((r nil)) (dotimes (i n r) (setq r (handler-case {callee} (error () -1))))))"
        );
        take_ordinary_outcalls();
        take_mapped_calls();
        let value = run_caller("T0-T2-ELT-SLOW-CALLER", &forms, 50, &mut env).unwrap();
        outcalls += take_ordinary_outcalls() + take_mapped_calls();
        assert_eq!(value, EgclVal::from_fixnum(expected), "{callee}");
    }
    for symbol in [leaf, keyed] {
        egcl_rt::symbols::set_symbol_function(symbol, NIL);
        registry_remove(symbol);
    }
    assert!(
        outcalls >= 6 * 50,
        "only {outcalls} of 300 other operands called ELT"
    );
}

fn collect_at_ordinary_entry() {
    use egcl_rt::Collector;
    egcl_rt::HeapCollector::new().full_gc().unwrap();
}

/// The lean root links one host root for the callable, the copied actuals and
/// the rest of its activation. A collection after that link (here, just
/// before machine entry) must relocate the actuals the callee then receives.
#[test]
fn a_lean_root_entry_relocates_its_actuals_before_machine_entry() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env("(defun t0-t2-second (a b) (cdr b))", &mut env).unwrap();
    let (symbol, code) =
        super::ordinary_t2::install("T0-T2-SECOND", "(a b)", "((cdr b))", &mut env);
    egcl_rt::rooted!(
        args = vec![
            crate::cli::arena_cons(EgclVal::from_fixnum(1), NIL),
            crate::cli::arena_cons(EgclVal::from_fixnum(2), EgclVal::from_fixnum(3)),
        ]
    );
    let before = args[1].to_raw();
    super::super::native_transfer_entry::take_lean_root_entries_for_test();
    // The callee's only copy of the second actual is the entry's rooted one;
    // the returned CDR is the original fixnum only if that copy was updated.
    let actuals: Vec<EgclVal> = args.to_vec();
    egcl_rt::rooted!(
        result = super::super::native_transfer_entry::with_ordinary_entry_hook_for_test(
            collect_at_ordinary_entry,
            || run_native(&code, symbol, &actuals, &mut env),
        )
        .unwrap()
    );
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let moved = args[1].to_raw() != before;
    egcl_rt::symbols::set_symbol_function(symbol, NIL);
    registry_remove(symbol);
    assert_eq!(lean, 1, "the call took the lean root entry");
    assert!(moved, "the probe must relocate the actuals");
    assert_eq!(*result, EgclVal::from_fixnum(3));
}
