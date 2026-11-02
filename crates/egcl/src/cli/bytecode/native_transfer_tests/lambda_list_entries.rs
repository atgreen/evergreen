// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary entries that bind `&rest`, `&key` and wide `&optional` lambda
//! lists in their frame (bliss-gg3i1.7.15.10.4). A `&rest` list is built by
//! the entry's noncollecting binder from the generated-code allocation
//! window; when it cannot, the call completes through the rooted decline
//! veneer, which may collect. Both must leave every caller value and every
//! bound value correct across a moving collection.

use super::ordinary_t2::install as install_ordinary;
use super::*;

fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        install_ordinary(name, params, forms, env)
    })
}

fn assert_ordinary(code: &NativeCode) {
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped ordinary owner")
    };
    assert!(
        native_transfer_entry::installed_listing(mapped).contains("Ordinary x86-64 Lisp ABI"),
        "the fixture must install an ordinary entry"
    );
}

fn native_rest_binds() -> u64 {
    super::super::variadic_plan::NATIVE_REST_BINDS.with(|binds| binds.replace(0))
}

/// A caller passes three fresh nursery conses to a `&rest` leaf, which
/// collects after its entry has bound them. Returns how many probes saw the
/// caller's own argument move.
fn rest_probe(caller: u32, caller_code: &NativeCode, env: &mut Env) -> usize {
    let mut moved = 0;
    for i in 0..4 {
        egcl_rt::HeapCollector::new().minor_gc().unwrap();
        egcl_rt::rooted!(
            args = vec![
                crate::cli::arena_cons(EgclVal::from_fixnum(i), NIL),
                crate::cli::arena_cons(EgclVal::from_fixnum(i + 10), NIL),
                crate::cli::arena_cons(EgclVal::from_fixnum(i + 20), NIL),
            ]
        );
        let before = args[0].to_raw();
        // A runtime T0 allocation republishes the generated-code window the
        // collection retired, as the next allocation of a running program would.
        egcl_rt::gc::alloc_double_float(0.0);
        egcl_rt::rooted!(result = run_native(caller_code, caller, &args, env).unwrap());
        // (ll-rest-leaf a b c a) => (a (b c a)), consed onto a.
        let (inner, tail) = crate::cli::cp(*result);
        assert_eq!(tail, args[0], "the caller's value was updated in place");
        let (a, rest) = crate::cli::cp(inner);
        assert_eq!(a, args[0]);
        let (rest, end) = crate::cli::cp(rest);
        assert_eq!(end, NIL);
        let elements: Vec<_> = std::iter::successors(Some(rest), |&cell| {
            let next = crate::cli::cp(cell).1;
            next.is_cons().then_some(next)
        })
        .map(|cell| crate::cli::cp(cell).0)
        .collect();
        assert_eq!(elements, vec![args[1], args[2], args[0]]);
        assert_eq!(crate::cli::cp(args[1]).0, EgclVal::from_fixnum(i + 10));
        if args[0].to_raw() != before {
            moved += 1;
        }
    }
    moved
}

fn rest_fixture(env: &mut Env) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(
        "(defun ll-gc () (%force-minor-gc-for-test))
         (defun ll-rest-leaf (a &rest r) (ll-gc) (list a r))
         (defun ll-rest-caller (a b c) (cons (ll-rest-leaf a b c a) a))",
        env,
    )
    .unwrap();
    let (_, leaf_code) = install("LL-REST-LEAF", "(a &rest r)", "((ll-gc) (list a r))", env);
    assert_ordinary(&leaf_code);
    let (caller, caller_code) = install(
        "LL-REST-CALLER",
        "(a b c)",
        "((cons (ll-rest-leaf a b c a) a))",
        env,
    );
    assert_ordinary(&caller_code);
    std::mem::forget(leaf_code);
    (caller, caller_code)
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn a_native_rest_list_and_the_callers_values_relocate_across_a_collecting_body() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (caller, caller_code) = rest_fixture(&mut env);
    native_rest_binds();
    let moved = rest_probe(caller, &caller_code, &mut env);
    // The first call may find the window retired and complete rooted.
    assert!(
        native_rest_binds() >= 3,
        "the entry must build the lists itself"
    );
    assert!(moved > 0, "the probe must observe a relocation");
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn a_declined_rest_binding_completes_rooted_across_a_collection() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (caller, caller_code) = rest_fixture(&mut env);
    // Every native binding declines, so each call reaches the rooted veneer
    // with the actual arguments, and the general binder allocates the list.
    super::super::variadic_plan::PLANS_DISABLED.with(|disabled| disabled.set(true));
    struct Restore;
    impl Drop for Restore {
        fn drop(&mut self) {
            super::super::variadic_plan::PLANS_DISABLED.with(|disabled| disabled.set(false));
        }
    }
    let _restore = Restore;
    native_rest_binds();
    let moved = rest_probe(caller, &caller_code, &mut env);
    assert_eq!(native_rest_binds(), 0, "every binding declined");
    assert!(moved > 0, "the probe must observe a relocation");
}

#[test]
#[ignore = "requires installed ordinary register entries"]
fn wide_optional_and_keyword_entries_bind_in_their_frame() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun ll-opt (a b c &optional (d (list a b)) (e (+ c 1) ep)) (list a b c d e ep))
         (defun ll-key (a &key b c d (e 5 ep)) (list a b c d e ep))
         (defun ll-opt-3 (x) (ll-opt x 2 3))
         (defun ll-opt-5 (x) (ll-opt x 2 3 4 5))
         (defun ll-key-3 (x) (ll-key x :e x :c 3 :b 2 :d 4))
         (defun ll-key-0 (x) (ll-key x))",
        &mut env,
    )
    .unwrap();
    let (_, opt) = install(
        "LL-OPT",
        "(a b c &optional (d (list a b)) (e (+ c 1) ep))",
        "((list a b c d e ep))",
        &mut env,
    );
    assert_ordinary(&opt);
    let (_, key) = install(
        "LL-KEY",
        "(a &key b c d (e 5 ep))",
        "((list a b c d e ep))",
        &mut env,
    );
    assert_ordinary(&key);
    for (name, params, forms, expected) in [
        ("LL-OPT-3", "(x)", "((ll-opt x 2 3))", "(7 2 3 (7 2) 4 NIL)"),
        ("LL-OPT-5", "(x)", "((ll-opt x 2 3 4 5))", "(7 2 3 4 5 T)"),
        (
            "LL-KEY-3",
            "(x)",
            "((ll-key x :e x :c 3 :b 2 :d 4))",
            "(7 2 3 4 7 T)",
        ),
        ("LL-KEY-0", "(x)", "((ll-key x))", "(7 NIL NIL NIL 5 NIL)"),
    ] {
        let (symbol, code) = install(name, params, forms, &mut env);
        assert_ordinary(&code);
        // The first keyword call teaches the binder; the rest bind natively.
        for _ in 0..3 {
            egcl_rt::rooted!(
                result = run_native(&code, symbol, &[EgclVal::from_fixnum(7)], &mut env).unwrap()
            );
            let mut printed = String::new();
            crate::cli::print_val(*result, &mut printed);
            assert_eq!(printed, expected, "{name}");
        }
    }
    // Counts outside the lambda list reach the rooted arity veneer.
    for form in ["(ll-opt 1 2)", "(ll-opt 1 2 3 4 5 6)", "(ll-key 1 :b)"] {
        let error = crate::cli::read_eval_all_env(form, &mut env);
        assert!(error.is_err(), "{form} must signal");
    }
}
