// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The cached variadic binder must bind exactly what the general binder does,
//! and must actually serve the common `&key` call (bliss-nkzx6, GitHub #474).

use super::variadic_plan::{GENERAL_BINDS, PLANS_DISABLED};
use super::*;

fn general_binds() -> u64 {
    GENERAL_BINDS.with(std::cell::Cell::get)
}

/// Compile `(lambda <params> (list <vars>))` and call it with each argument
/// list in `calls`, returning the printed result or `ERROR` for each.
fn outcomes(source: &str, vars: &str, calls: &[&str], env: &mut Env) -> Vec<String> {
    egcl_rt::rooted!(params = reader::read_from_string(source).unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(&format!("((list {vars}))"))
            .unwrap()
            .0
    );
    let body = Arc::new(
        compile_function("VARIADIC-PLAN-PROBE", *params, *forms, env, false, false)
            .expect("probe compiles to bytecode"),
    );
    assert!(body.variadic, "{source} is variadic");
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    calls
        .iter()
        .map(|call| {
            egcl_rt::rooted!(list = reader::read_from_string(call).unwrap().0);
            egcl_rt::rooted!(args = list_to_vec(*list));
            match run(Arc::clone(&body), &args, NIL, env) {
                Ok(value) => super::super::format_val(value),
                Err(_) => "ERROR".to_string(),
            }
        })
        .collect()
}

/// Bind every call through the plan and through the general binder alone.
fn both(params: &str, vars: &str, calls: &[&str], env: &mut Env) -> (Vec<String>, u64) {
    PLANS_DISABLED.with(|disabled| disabled.set(true));
    let general = outcomes(params, vars, calls, env);
    PLANS_DISABLED.with(|disabled| disabled.set(false));
    let before = general_binds();
    let planned = outcomes(params, vars, calls, env);
    let fallbacks = general_binds() - before;
    assert_eq!(planned, general, "{params} bound differently by its plan");
    (planned, fallbacks)
}

#[test]
fn a_keyword_call_binds_without_the_general_binder() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (results, fallbacks) = both(
        "(x &key a)",
        "x a",
        &["(1 :a 2)", "(1)", "(1 :a 2 :a 3)"],
        &mut env,
    );
    assert_eq!(results, ["(1 2)", "(1 NIL)", "(1 2)"]);
    assert_eq!(
        fallbacks, 0,
        "an ordinary &key call must not re-parse its lambda list"
    );
    let (_, fallbacks) = both(
        "(x &optional (y 3 y-p) &rest r)",
        "x y y-p r",
        &["(1)", "(1 2)", "(1 2 3 4)"],
        &mut env,
    );
    assert_eq!(
        fallbacks, 0,
        "constant &optional defaults and &rest are planned"
    );
}

#[test]
fn the_plan_matches_the_general_binder() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    // Unknown, odd, non-symbol and :ALLOW-OTHER-KEYS tails and wrong counts
    // fall back, so the general binder still reports them.
    let (results, fallbacks) = both(
        "(x &key a)",
        "x a",
        &[
            "(1 :b 2)",
            "(1 :a)",
            "(1 2 3)",
            "()",
            "(1 :allow-other-keys t :b 2)",
        ],
        &mut env,
    );
    assert_eq!(results[..4], ["ERROR", "ERROR", "ERROR", "ERROR"]);
    assert_eq!(results[4], "(1 NIL)");
    assert_eq!(fallbacks, 5);
    both(
        "(x &key a (b 7) (c 'q c-p) ((:dee d) #\\z d-p) (e :k) &allow-other-keys)",
        "x a b c c-p d d-p e",
        &[
            "(1)",
            "(1 :c 2 :dee 3)",
            "(1 :zz 9 :b 8)",
            "(1 :dee 4 :dee 5 :e 6)",
            "(1 :allow-other-keys nil :zz 1)",
        ],
        &mut env,
    );
    both(
        "(&rest r &key a b)",
        "r a b",
        &["()", "(:b 1)", "(:a 1 :b 2)", "(:c 1)"],
        &mut env,
    );
    both(
        "(x &optional y (z 5 z-p) &key (k 1.5) &aux (w 'aux) v)",
        "x y z z-p k w v",
        &["(1)", "(1 2)", "(1 2 3)", "(1 2 3 :k 4)", "(1 2 3 :k)"],
        &mut env,
    );
    // Defaults that must be evaluated decline only when they are needed.
    let (results, fallbacks) = both(
        "(x &key (a (+ x 1)) b)",
        "x a b",
        &["(1 :a 5)", "(1 :b 2)"],
        &mut env,
    );
    assert_eq!(results, ["(1 5 NIL)", "(1 2 2)"]);
    assert_eq!(fallbacks, 1);
    both(
        "(x &optional (y x) &rest r)",
        "x y r",
        &["(1)", "(1 2)", "(1 2 3)"],
        &mut env,
    );
    both(
        "(&optional a b &key c)",
        "a b c",
        &["()", "(1)", "(1 2)", "(1 2 :c)", "(1 2 :c 3)", "(1 2 3 4)"],
        &mut env,
    );
}
