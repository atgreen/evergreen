// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::super::native_transfer_tests::ordinary_t2;
use super::*;

pub(super) fn install(
    env: &mut Env,
    name: &str,
    params: &str,
    body: &str,
) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {body})"), env).unwrap();
    ordinary_t2::install(name, params, &format!("({body})"), env)
}
pub(super) fn owner(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("native owner")
    };
    code
}
pub(super) fn run_mapped(
    symbol: u32,
    native: &NativeCode,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    let code = owner(native);
    assert!(
        code.ordinary_entry.is_none(),
        "fixture must really enter mapped native code"
    );
    let function = egcl_rt::symbols::symbol_function(symbol).unwrap();
    code.run_callable(function, args, env)
}
fn list(mut value: EgclVal) -> Vec<EgclVal> {
    let mut result = Vec::new();
    while !value.is_nil() {
        assert!(value.is_cons());
        let (head, tail) = cp(value);
        result.push(head);
        value = tail;
    }
    result
}

#[test]
fn mapped_bridge_reuses_state_and_preserves_zero_single_many_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // The ordinary cohort excludes SetValues itself. Keep the producer behind
    // a NOTINLINE call, as ordinary terminal-wrapper tests do, so the bridge
    // must transport the actual runtime tuple without widening admission.
    crate::cli::read_eval_all_env(
        "(defun bridge-mv-producer (mode x)
           (if (= mode 0) (values) (if (= mode 1) (values x) (values x 7 x))))",
        &mut env,
    )
    .unwrap();
    // Isolate tuple transport from producer tiering and asynchronous publication.
    let _producer_pin = ordinary_t2::RecursiveFixturePins::new(&["BRIDGE-MV-PRODUCER"]);
    let (_, child) = install(
        &mut env,
        "BRIDGE-MV-CHILD",
        "(mode x)",
        "(locally (declare (notinline bridge-mv-producer)) (bridge-mv-producer mode x))",
    );
    assert!(owner(&child).lean_root_entry().is_some());
    let (symbol, parent) = install(
        &mut env,
        "BRIDGE-MV-PARENT",
        "(n mode x &key (seed (1+ 0)))",
        "(let ((last nil)) (dotimes (i n last) (setq last (multiple-value-list (bridge-mv-child mode x)))))",
    );
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(43), NIL));
    for mode in 0..3 {
        run_mapped(
            symbol,
            &parent,
            &[
                EgclVal::from_fixnum(1),
                EgclVal::from_fixnum(mode),
                *payload,
            ],
            &mut env,
        )
        .unwrap();
        mapped_ordinary::take_entries_for_test();
        mapped_ordinary::take_prepares_for_test();
        take_lean_root_entries_for_test();
        egcl_rt::rooted!(
            result = run_mapped(
                symbol,
                &parent,
                &[
                    EgclVal::from_fixnum(64),
                    EgclVal::from_fixnum(mode),
                    *payload
                ],
                &mut env
            )
            .unwrap()
        );
        assert!(mapped_ordinary::take_entries_for_test() >= 64);
        assert_eq!(
            mapped_ordinary::take_prepares_for_test(),
            1,
            "one state for the entire mapped activation"
        );
        assert_eq!(take_lean_root_entries_for_test(), 0);
        let expected = match mode {
            0 => vec![],
            1 => vec![*payload],
            _ => vec![*payload, EgclVal::from_fixnum(7), *payload],
        };
        assert_eq!(list(*result), expected);
    }
}

#[test]
fn mapped_bridge_child_guard_does_not_replay_effects_or_mutate_parent_scopes() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env("(setq *bridge-effects* 0 *bridge-special* 91)", &mut env)
        .unwrap();
    let (_, child) = install(
        &mut env,
        "BRIDGE-GUARD-CHILD",
        "(x)",
        "(progn (setq *bridge-effects* (+ *bridge-effects* 1)) (+ x 1))",
    );
    assert!(owner(&child).lean_root_entry().is_some());
    let (symbol, parent) = install(
        &mut env,
        "BRIDGE-GUARD-PARENT",
        "(x &key (seed (1+ 0)))",
        "(let ((*bridge-special* 17)) (declare (special *bridge-special*)) (catch 'bridge-outer (bridge-guard-child 0) (setq *bridge-effects* 0) (bridge-guard-child x)))",
    );
    run_mapped(symbol, &parent, &[EgclVal::from_fixnum(2)], &mut env).unwrap();
    crate::cli::read_eval_all_env("(setq *bridge-effects* 0)", &mut env).unwrap();
    egcl_rt::rooted!(
        wide = reader::read_from_string("1267650600228229401496703205376")
            .unwrap()
            .0
    );
    egcl_rt::rooted!(
        expected = reader::read_from_string("1267650600228229401496703205377")
            .unwrap()
            .0
    );
    let before = owner(&child).deopt_count();
    mapped_generated::take_fast_entries_for_test();
    mapped_ordinary::take_entries_for_test();
    egcl_rt::rooted!(result = run_mapped(symbol, &parent, &[*wide], &mut env).unwrap());
    assert!(mapped_ordinary::take_entries_for_test() > 0);
    assert!(
        owner(&child).deopt_count() > before,
        "the physical child guard really failed"
    );
    assert!(
        mapped_generated::take_fast_entries_for_test() > 0,
        "guard failure must follow a generated hot call"
    );
    assert_eq!(
        egcl_rt::bignum::big_cmp(
            &egcl_rt::bignum::bigint_from_val(*result).unwrap(),
            &egcl_rt::bignum::bigint_from_val(*expected).unwrap()
        ),
        std::cmp::Ordering::Equal
    );
    assert_eq!(
        env.lookup_var("*BRIDGE-EFFECTS*"),
        Some(EgclVal::from_fixnum(1))
    );
    assert_eq!(
        env.lookup_var("*BRIDGE-SPECIAL*"),
        Some(EgclVal::from_fixnum(91))
    );
}

#[test]
fn mapped_bridge_propagates_throw_to_real_parent_and_rechecks_redefinitions() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *bridge-special* 91)
        (defun bridge-throw-leaf (x) (throw 'bridge-outer x))",
        &mut env,
    )
    .unwrap();
    let (_, child) = install(
        &mut env,
        "BRIDGE-THROW-CHILD",
        "(flag x)",
        "(if flag (bridge-throw-leaf x) x)",
    );
    assert!(owner(&child).lean_root_entry().is_some());
    let (symbol, parent) = install(
        &mut env,
        "BRIDGE-THROW-PARENT",
        "(flag x &key (seed (1+ 0)))",
        "(let ((*bridge-special* 17)) (declare (special *bridge-special*)) (catch 'bridge-outer (bridge-throw-child nil nil) (bridge-throw-child flag x)))",
    );
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(59), NIL));
    run_mapped(symbol, &parent, &[NIL, *payload], &mut env).unwrap();
    mapped_ordinary::take_entries_for_test();
    mapped_generated::take_fast_entries_for_test();
    egcl_rt::rooted!(
        result = run_mapped(symbol, &parent, &[egcl_rt::value::T, *payload], &mut env).unwrap()
    );
    assert_eq!(*result, *payload);
    assert!(
        mapped_generated::take_fast_entries_for_test() > 0,
        "THROW must escape generated hot call"
    );
    assert!(mapped_ordinary::take_entries_for_test() > 0);
    assert_eq!(
        env.lookup_var("*BRIDGE-SPECIAL*"),
        Some(EgclVal::from_fixnum(91))
    );
    install(&mut env, "BRIDGE-VERSION", "(x)", "(+ x 1)");
    install(&mut env, "BRIDGE-NEXT", "(x)", "(+ x 10)");
    crate::cli::read_eval_all_env("(defun bridge-change () (setf (symbol-function 'bridge-version) (symbol-function 'bridge-next)))", &mut env).unwrap();
    let (symbol, parent) = install(
        &mut env,
        "BRIDGE-REDEFINE-PARENT",
        "(x &key (seed (1+ 0)))",
        "(progn (bridge-version x) (let ((first (bridge-version x))) (bridge-change) (+ first (bridge-version x))))",
    );
    assert_eq!(
        run_mapped(symbol, &parent, &[EgclVal::from_fixnum(5)], &mut env).unwrap(),
        EgclVal::from_fixnum(21)
    );
}

#[test]
fn mapped_bridge_alternating_targets_prepare_once_per_distinct_owner() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    install(&mut env, "BRIDGE-ALTERNATE-A", "(x)", "(+ x 1)");
    install(&mut env, "BRIDGE-ALTERNATE-B", "(x)", "(+ x 2)");
    let (symbol, parent) = install(
        &mut env,
        "BRIDGE-ALTERNATE-PARENT",
        "(n &key (seed (1+ 0)))",
        "(let ((sum 0)) (dotimes (i n sum) (setq sum (+ sum (bridge-alternate-a i) (bridge-alternate-b i)))))",
    );
    run_mapped(symbol, &parent, &[EgclVal::from_fixnum(1)], &mut env).unwrap();
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    take_lean_root_entries_for_test();
    assert_eq!(
        run_mapped(symbol, &parent, &[EgclVal::from_fixnum(64)], &mut env).unwrap(),
        EgclVal::from_fixnum(4224)
    );
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 2);
    assert!(mapped_ordinary::take_entries_for_test() >= 128);
    assert_eq!(take_lean_root_entries_for_test(), 0);
}
