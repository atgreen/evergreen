// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T1 FUNCTION lookups use the ordinary ABI and read the current function cell.

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t1_function_lookup_preserves_redefinition_and_undefined_function() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun t1-lookup-target (x) (+ x 1))
         (defun t1-lookup-reader (x) (list #'t1-lookup-target x))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string("((list #'t1-lookup-target x))")
            .unwrap()
            .0
    );
    let symbol = egcl_rt::symbols::intern("T1-LOOKUP-READER");
    let body = Arc::new(
        compile_function("T1-LOOKUP-READER", *params, *forms, &env, false, false).unwrap(),
    );
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::LoadFunction(_)))
    );
    registry_put(symbol, Arc::clone(&body));
    let code = try_promote_to_t1(symbol).expect("T1 function lookup installation");
    assert!(!code.is_t2, "exercise baseline T1, not a T2 worker result");
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("T1 uses the native transfer representation");
    };
    assert!(
        mapped.ordinary_entry.is_some(),
        "lookup must admit an ordinary entry"
    );
    assert!(
        mapped.ordinary_root_allowed(),
        "the actual invocation must enter it"
    );
    assert!(
        mapped.mapped_entry().is_none(),
        "the fixture must not have a mapped fallback entry"
    );
    let target = egcl_rt::symbols::intern("T1-LOOKUP-TARGET");
    egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(target).unwrap());
    egcl_rt::rooted!(
        first = run_native(&code, symbol, &[EgclVal::from_fixnum(7)], &mut env).unwrap()
    );
    assert_eq!(crate::cli::cp(*first).0, *original);
    assert_eq!(
        crate::cli::cp(crate::cli::cp(*first).1),
        (EgclVal::from_fixnum(7), NIL)
    );

    crate::cli::read_eval_all_env("(defun t1-lookup-target (x) (+ x 10))", &mut env).unwrap();
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(target).unwrap());
    assert_ne!(*original, *replacement);
    egcl_rt::rooted!(
        second = run_native(&code, symbol, &[EgclVal::from_fixnum(8)], &mut env).unwrap()
    );
    assert_eq!(
        crate::cli::cp(*second).0,
        *replacement,
        "lookup reads the new binding"
    );
    assert_eq!(
        crate::cli::cp(*first).0,
        *original,
        "retained result keeps the old function"
    );
    assert_eq!(
        crate::cli::apply_function_impl(*original, &[EgclVal::from_fixnum(4)], &mut env).unwrap(),
        EgclVal::from_fixnum(5)
    );
    assert_eq!(
        crate::cli::apply_function_impl(*replacement, &[EgclVal::from_fixnum(4)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(14)
    );

    crate::cli::read_eval_all_env("(fmakunbound 't1-lookup-target)", &mut env).unwrap();
    egcl_rt::rooted!(
        error = run_native(&code, symbol, &[EgclVal::from_fixnum(9)], &mut env).unwrap_err()
    );
    assert!(
        matches!(error.without_backtrace(), EgclError::UndefinedFunction(name)
            if *name == EgclVal::from_symbol_index(target)),
        "{:?}",
        &*error
    );
    assert_eq!(
        crate::cli::read_eval_all_env(
            "(handler-case (t1-lookup-reader 10) (undefined-function () :lookup-missing))",
            &mut env
        )
        .unwrap(),
        resolve_sym(":LOOKUP-MISSING").unwrap(),
        "a Lisp handler receives the Rust lookup error as UNDEFINED-FUNCTION"
    );
}
