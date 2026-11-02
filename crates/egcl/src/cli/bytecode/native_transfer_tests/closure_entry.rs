// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closures and FUNCALL through the entry word (bliss-gg3i1.7.15.10.3).
//!
//! An ordinary FUNCALL loads the function, checks for an interpreted function
//! whose entry word this execution owns or that carries its template's shared
//! dispatch stub, and calls that entry with the closure in RAX. The stub
//! enters the template's code when this execution claims it, and misses to
//! the runtime otherwise.

use super::ordinary_captured_context::{closure, install_automatic, template};
use super::*;

fn install_template(function: EgclVal) -> Rc<NativeCode> {
    native_transfer_entry::with_forced_captured_entry_for_test(|| install_automatic(function))
}

fn mapped(code: &NativeCode) -> &native_transfer_entry::TransferCode {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped owner")
    };
    mapped
}

fn owner() -> u64 {
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    unsafe { &*execution }.code_owner()
}

fn word(function: EgclVal) -> (usize, u64) {
    egcl_rt::function::entry_word(function).expect("interpreted function")
}

fn misses() -> usize {
    native_transfer_entry::ordinary_calls::take_closure_misses_for_test()
}

fn fast_calls() -> usize {
    native_transfer_entry::funcall_fast::take_fast_calls_for_test().0
}

fn caller(name: &str, forms: &str, tail: bool, env: &mut Env) -> (u32, Rc<NativeCode>) {
    let mut install = || ordinary_t2::install(name, "(f next)", forms, env);
    let (symbol, code) = if tail {
        install()
    } else {
        native_transfer_entry::with_ordinary_non_tail_emission_for_test(install)
    };
    assert!(mapped(&code).ordinary_entry.is_some(), "ordinary caller");
    (symbol, code)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn lean_funcall_enters_instances_through_their_template_claim() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(11), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
    let code = install_template(*first);
    let entry = mapped(&code)
        .ordinary_lean_entry_address()
        .expect("template code has a lean entry");
    for tail in [true, false] {
        let name = format!("CLOSURE-ENTRY-CALLER-{tail}");
        let (symbol, caller) = caller(&name, "((funcall f next))", tail, &mut env);
        // The first calls miss and teach the template's claim.
        for _ in 0..3 {
            for (function, expected) in [(*first, 11), (*second, 22)] {
                assert_eq!(
                    run_native(&caller, symbol, &[function, NIL], &mut env).unwrap(),
                    EgclVal::from_fixnum(expected)
                );
            }
        }
        let (stub, shared) = word(*first);
        assert_eq!(shared, egcl_rt::function::SHARED_ENTRY_OWNER);
        assert_eq!(word(*second), (stub, shared), "one stub per template");
        assert_eq!(
            native_transfer_entry::closure_entry::record_state_for_test(stub),
            Some((entry, owner())),
            "this execution claims the template for its code"
        );
        let prototype = egcl_rt::function::closure_prototype(*first).unwrap();
        assert_eq!(word(prototype).0, stub, "the prototype names the stub");
        misses();
        fast_calls();
        let _ = native_transfer_entry::take_lean_root_entries_for_test();
        for (function, expected) in [(*first, 11), (*second, 22)] {
            assert_eq!(
                run_native(&caller, symbol, &[function, NIL], &mut env).unwrap(),
                EgclVal::from_fixnum(expected)
            );
        }
        assert_eq!(misses(), 0, "no call missed the stub");
        assert_eq!(fast_calls(), 0, "no call took the Rust FUNCALL cache");
        // A collection retires no claim: the next calls enter directly.
        HeapCollector::new().minor_gc().unwrap();
        egcl_rt::gc::full_gc().unwrap();
        for (function, expected) in [(*first, 11), (*second, 22)] {
            assert_eq!(
                run_native(&caller, symbol, &[function, NIL], &mut env).unwrap(),
                EgclVal::from_fixnum(expected)
            );
        }
        assert_eq!(misses(), 0, "no cold miss after a collection");
        assert_eq!(fast_calls(), 0);
        assert_eq!(
            native_transfer_entry::take_lean_root_entries_for_test(),
            4,
            "only the outer callers construct Rust root activations"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn retiring_the_template_code_releases_its_claim_until_new_code_claims_it() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("((+ ordinary-native-cell next))", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(instance = closure(&body, EgclVal::from_fixnum(100), &mut env));
    let code = install_template(*instance);
    let (symbol, caller) = caller(
        "CLOSURE-ENTRY-RETIRE-CALLER",
        "((funcall f next))",
        false,
        &mut env,
    );
    let call = |next: i64, env: &mut Env| {
        run_native(
            &caller,
            symbol,
            &[*instance, EgclVal::from_fixnum(next)],
            env,
        )
        .unwrap()
    };
    for i in 0..3 {
        assert_eq!(call(i, &mut env), EgclVal::from_fixnum(100 + i));
    }
    let (stub, _) = word(*instance);
    let entry = mapped(&code).ordinary_lean_entry_address().unwrap();
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((entry, owner()))
    );
    // Removing the code from the registry (as deopt, a profile pin or a
    // replacement does) releases the pair at once.
    let template_symbol = egcl_rt::function::definition_index(*instance);
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().remove(&template_symbol));
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((0, 0)),
        "a retired version keeps no claim"
    );
    misses();
    for i in 0..3 {
        assert_eq!(call(i, &mut env), EgclVal::from_fixnum(100 + i));
    }
    assert!(misses() > 0, "calls miss to the runtime meanwhile");
    // New code for the template claims the same pair; the instance follows.
    drop(code);
    let code = install_template(*instance);
    let entry = mapped(&code).ordinary_lean_entry_address().unwrap();
    call(0, &mut env);
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((entry, owner())),
        "the replacement claims the template"
    );
    misses();
    for i in 0..3 {
        assert_eq!(call(i, &mut env), EgclVal::from_fixnum(100 + i));
    }
    assert_eq!(misses(), 0);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn another_executions_claim_or_a_non_function_takes_the_old_path() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("((cons ordinary-native-cell next))", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(instance = closure(&body, EgclVal::from_fixnum(5), &mut env));
    let code = install_template(*instance);
    let (symbol, caller) = caller(
        "CLOSURE-ENTRY-FOREIGN-CALLER",
        "((funcall f next))",
        false,
        &mut env,
    );
    for _ in 0..3 {
        run_native(&caller, symbol, &[*instance, NIL], &mut env).unwrap();
    }
    let (stub, _) = word(*instance);
    let entry = mapped(&code).ordinary_lean_entry_address().unwrap();
    // Another execution's claim: this one's calls miss and still complete.
    let foreign = owner() + 1_000_000;
    native_transfer_entry::closure_entry::set_record_for_test(stub, entry, foreign);
    misses();
    for i in 0..4 {
        egcl_rt::rooted!(
            result = run_native(
                &caller,
                symbol,
                &[*instance, EgclVal::from_fixnum(i)],
                &mut env
            )
            .unwrap()
        );
        assert_eq!(
            crate::cli::cp(*result),
            (EgclVal::from_fixnum(5), EgclVal::from_fixnum(i))
        );
    }
    assert_eq!(misses(), 4, "a foreign claim is never entered");
    assert_eq!(
        native_transfer_entry::closure_entry::record_state_for_test(stub),
        Some((entry, foreign)),
        "nor taken over"
    );
    native_transfer_entry::closure_entry::set_record_for_test(stub, 0, 0);
    // A non-function signals exactly as before, without touching the stub.
    misses();
    let error = run_native(&caller, symbol, &[EgclVal::from_fixnum(42), NIL], &mut env)
        .expect_err("a fixnum is not a function");
    assert_eq!(misses(), 0);
    let expected = apply_function(EgclVal::from_fixnum(42), &[NIL], &mut env)
        .expect_err("the general route signals too");
    let condition = |error: &EgclError| {
        let text = format!("{error:?}");
        text.split(", backtrace").next().unwrap().to_owned()
    };
    assert_eq!(condition(&error), condition(&expected), "the same error");
}

thread_local! {
    static COLLECTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Collect once, from inside the closure's published captured read.
fn collect_in_callee() {
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    egcl_rt::gc::collect_t0_minor().unwrap();
    COLLECTED.with(|done| done.set(true));
}

struct ResetObserver;
impl Drop for ResetObserver {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn a_collecting_closure_relocates_its_captures_and_callers_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("((cons ordinary-native-cell next))", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    let (symbol, caller) = caller(
        "CLOSURE-ENTRY-GC-CALLER",
        "((cons (funcall f next) next))",
        false,
        &mut env,
    );
    egcl_rt::rooted!(first = closure(&body, NIL, &mut env));
    let code = install_template(*first);
    let entry = mapped(&code).ordinary_lean_entry_address().unwrap();
    for _ in 0..3 {
        // Teach the template's claim.
        run_native(&caller, symbol, &[*first, NIL], &mut env).unwrap();
    }
    let _reset = ResetObserver;
    let mut moved = 0;
    for i in 0..6 {
        egcl_rt::gc::collect_t0_minor().unwrap();
        // Young captured and argument conses, both live across the call; the
        // closure made after them, armed at birth with its template's stub.
        egcl_rt::rooted!(captured = crate::cli::arena_cons(EgclVal::from_fixnum(i), NIL));
        egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(-i), NIL));
        egcl_rt::rooted!(instance = closure(&body, *captured, &mut env));
        let (stub, shared) = word(*instance);
        assert_eq!(
            shared,
            egcl_rt::function::SHARED_ENTRY_OWNER,
            "armed at birth"
        );
        assert_eq!(
            native_transfer_entry::closure_entry::record_state_for_test(stub),
            Some((entry, owner()))
        );
        misses();
        fast_calls();
        COLLECTED.with(|done| done.set(false));
        native_transfer_entry::root_publication::OBSERVE
            .with(|hook| hook.set(Some(collect_in_callee)));
        let (captured_before, argument_before) = (captured.to_raw(), argument.to_raw());
        let unrooted_instance = *instance;
        egcl_rt::rooted!(
            result = run_native(&caller, symbol, &[*instance, *argument], &mut env).unwrap()
        );
        assert!(
            COLLECTED.with(|done| done.get()),
            "a collection ran in the call"
        );
        assert_eq!(*instance, unrooted_instance, "the closure in RAX is pinned");
        assert_eq!(misses(), 0, "the call entered the template's code");
        assert_eq!(fast_calls(), 0);
        let (inner, held) = crate::cli::cp(*result);
        let (seen_capture, seen_argument) = crate::cli::cp(inner);
        assert_eq!(seen_capture, *captured, "the capture read after the GC");
        assert_eq!(seen_argument, *argument);
        assert_eq!(held, *argument, "the caller's value was updated");
        assert_eq!(crate::cli::cp(*captured).0, EgclVal::from_fixnum(i));
        assert_eq!(crate::cli::cp(*argument).0, EgclVal::from_fixnum(-i));
        if captured.to_raw() != captured_before && argument.to_raw() != argument_before {
            moved += 1;
        }
    }
    assert!(moved > 0, "the probe must observe a relocation");
}
