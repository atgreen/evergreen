// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FUNCALL and APPLY of function objects whose installed T2 target is already
//! selected (bliss-vwgb0). Each call must reach the callee's native entry
//! through the cached target, not re-select it through `apply_function_impl`,
//! while argument-count errors, redefinition and collection keep their
//! semantics.

use super::super::native_transfer_entry::funcall_fast;
use super::*;

/// Compile `forms` as a bytecode caller taking `(n)`, run it once with 1 to
/// warm its callees, then again with `n`, returning the second result.
fn run_caller(name: &str, forms: &str, n: i64, env: &mut Env) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let caller = Arc::new(
        compile_function(name, *params, *forms, env, false, false)
            .unwrap_or_else(|| panic!("{name}: {:?}", last_bail_reason())),
    );
    run(Arc::clone(&caller), &[EgclVal::from_fixnum(1)], NIL, env)?;
    super::super::native_transfer_entry::take_lean_root_entries_for_test();
    funcall_fast::take_fast_calls_for_test();
    run(caller, &[EgclVal::from_fixnum(n)], NIL, env)
}

fn uninstall(symbols: &[u32]) {
    for &symbol in symbols {
        egcl_rt::symbols::set_symbol_function(symbol, NIL);
        registry_remove(symbol);
    }
}

/// A mapped T2 caller's FUNCALL runs the object's T2 code from the checked
/// call, through the lean root. An argument count the callee does not accept
/// takes the general route, which signals.
#[test]
fn a_mapped_caller_funcalls_an_installed_t2_object_through_its_fast_target() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fc-inner (a b) (+ a b)) (defun fc-kw (f a &key (b (1+ 0))) (funcall f a b)) \
         (defun fc-one (f a &key (b (1+ 0))) (funcall f a))",
        &mut env,
    )
    .unwrap();
    let (inner, inner_code) =
        super::ordinary_t2::install("FC-INNER", "(a b)", "((+ a b))", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &inner_code._storage else {
        panic!("the callee needs mapped T2 code")
    };
    assert!(
        mapped.lean_root_entry().is_some(),
        "the callee has a lean root entry"
    );
    let (outer, outer_code) =
        super::ordinary_t2::install("FC-KW", "(f a &key (b (1+ 0)))", "((funcall f a b))", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &outer_code._storage else {
        panic!("the caller needs mapped T2 code")
    };
    assert!(
        mapped.ordinary_entry.is_none(),
        "a &key body with an evaluated default keeps the mapped ABI"
    );
    let calls = 100;
    let result = run_caller(
        "FC-KW-CALLER",
        "((let ((s 0)) (dotimes (i n s) (setq s (fc-kw #'fc-inner s :b 2)))))",
        calls,
        &mut env,
    )
    .unwrap();
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let (fast, _) = funcall_fast::take_fast_calls_for_test();
    // One argument short: the fast target declines and the general route
    // signals the standard argument-count error.
    let (one, _) =
        super::ordinary_t2::install("FC-ONE", "(f a &key (b (1+ 0)))", "((funcall f a))", &mut env);
    funcall_fast::take_fast_calls_for_test();
    let error = run_caller("FC-ARITY-CALLER", "((fc-one #'fc-inner n))", 1, &mut env);
    let (declined, _) = funcall_fast::take_fast_calls_for_test();
    uninstall(&[inner, outer, one]);
    assert_eq!(result, EgclVal::from_fixnum(2 * calls));
    assert!(
        fast >= calls as usize,
        "only {fast} of {calls} FUNCALLs ran the fast target"
    );
    assert!(
        lean >= calls as usize,
        "only {lean} of {calls} FUNCALLs took the lean root entry"
    );
    let report = format!("{:?}", error.expect_err("a missing argument signals"));
    assert!(report.to_ascii_lowercase().contains("argument"), "{report}");
    assert_eq!(
        declined, 0,
        "an unaccepted argument count keeps the general route"
    );
}

/// APPLY of a function object forwards to the callable's own published entry,
/// whose adapter also runs a current fast target. A definition installed
/// afterwards is a different object, and the next call runs it.
#[test]
fn apply_runs_the_fast_target_and_sees_a_redefinition() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fa-inner (a b) (+ a b)) \
         (defun fa-kw (a &key (b (1+ 0))) (apply (symbol-function 'fa-inner) a b nil))",
        &mut env,
    )
    .unwrap();
    let (inner, _) = super::ordinary_t2::install("FA-INNER", "(a b)", "((+ a b))", &mut env);
    let (outer, _) = super::ordinary_t2::install(
        "FA-KW",
        "(a &key (b (1+ 0)))",
        "((apply (symbol-function 'fa-inner) a b nil))",
        &mut env,
    );
    let calls = 100;
    let result = run_caller(
        "FA-KW-CALLER",
        "((let ((s 0)) (dotimes (i n s) (setq s (fa-kw s :b 2)))))",
        calls,
        &mut env,
    )
    .unwrap();
    let (fast, _) = funcall_fast::take_fast_calls_for_test();
    super::super::super::read_eval_all_env("(defun fa-inner (a b) (* a b 10))", &mut env).unwrap();
    let redefined = run_caller("FA-REDEFINED-CALLER", "((fa-kw 3 :b 4))", 1, &mut env).unwrap();
    uninstall(&[inner, outer]);
    assert_eq!(result, EgclVal::from_fixnum(2 * calls));
    assert!(
        fast >= calls as usize,
        "only {fast} of {calls} APPLYs ran the fast target"
    );
    assert_eq!(
        redefined,
        EgclVal::from_fixnum(120),
        "APPLY must see the new definition"
    );
}

/// Entries name objects by identity without rooting them: a collection
/// retires every entry, and the next call selects its target again.
#[test]
fn a_collection_retires_fast_targets() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fg-inner (a b) (+ a b)) (defun fg-kw (f a &key (b (1+ 0))) (funcall f a b))",
        &mut env,
    )
    .unwrap();
    let (inner, _) = super::ordinary_t2::install("FG-INNER", "(a b)", "((+ a b))", &mut env);
    let (outer, _) =
        super::ordinary_t2::install("FG-KW", "(f a &key (b (1+ 0)))", "((funcall f a b))", &mut env);
    let first = run_caller("FG-CALLER", "((fg-kw #'fg-inner n :b 2))", 5, &mut env).unwrap();
    let function = egcl_rt::symbols::symbol_function(inner).unwrap();
    let cached = funcall_fast::is_cached_for_test(function);
    egcl_rt::gc::collect_t0_minor().unwrap();
    let retired = !funcall_fast::is_cached_for_test(function);
    let second = run_caller("FG-CALLER-2", "((fg-kw #'fg-inner n :b 2))", 6, &mut env).unwrap();
    let refilled = funcall_fast::is_cached_for_test(function);
    uninstall(&[inner, outer]);
    assert_eq!(
        (first, second),
        (EgclVal::from_fixnum(7), EgclVal::from_fixnum(8))
    );
    assert!(cached, "the warmed FUNCALL must have cached its target");
    assert!(retired, "a collection must retire every cached identity");
    assert!(refilled, "the next call selects its target again");
}

/// An argument passed through a fast target is owned by the callee's entry
/// roots: a collection during the call relocates it, and the callee returns
/// the relocated object. The probe checks that the argument really moved.
#[test]
fn a_fast_target_roots_arguments_that_move_during_the_call() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env("(defun fm-first (a b) (progn b a))", &mut env).unwrap();
    let (symbol, code) =
        super::ordinary_t2::install("FM-FIRST", "(a b)", "((progn b a))", &mut env);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("the callee needs mapped T2 code")
    };
    assert!(
        mapped.lean_root_entry().is_some(),
        "the callee has a lean root entry"
    );
    let function = egcl_rt::symbols::symbol_function(symbol).unwrap();
    // Select the target first, so the measured call is a cache hit.
    egcl_rt::rooted!(warm = vec![EgclVal::from_fixnum(1), NIL]);
    let warmed = funcall_fast::apply(function, &warm, &mut env, funcall_fast::Admit::Native);
    assert!(matches!(warmed, Some(Ok(value)) if value == EgclVal::from_fixnum(1)));
    // Allocate after a full collection so the argument starts young. The
    // collection retires the selection; the measured call selects again.
    egcl_rt::gc::full_gc().unwrap();
    egcl_rt::rooted!(args = vec![arena_cons(EgclVal::from_fixnum(73), NIL), NIL]);
    let original = args[0].0;
    funcall_fast::take_fast_calls_for_test();
    super::super::native_transfer_entry::take_lean_root_entries_for_test();
    let result = super::super::native_transfer_entry::with_ordinary_entry_hook_for_test(
        || egcl_rt::gc::collect_t0_minor().unwrap(),
        || funcall_fast::apply(function, &args, &mut env, funcall_fast::Admit::Native),
    );
    let (fast, _) = funcall_fast::take_fast_calls_for_test();
    let lean = super::super::native_transfer_entry::take_lean_root_entries_for_test();
    uninstall(&[symbol]);
    let value = result.expect("a selected target runs").unwrap();
    assert_eq!(fast, 1, "the call must take the fast target");
    assert_eq!(lean, 1, "the call must enter the callee's lean root");
    assert_ne!(
        args[0].0, original,
        "the probe must actually relocate the argument"
    );
    assert_eq!(value, args[0], "the callee returns the relocated argument");
    assert_eq!(cp(value).0, EgclVal::from_fixnum(73));
}

#[test]
fn ordinary_function_lookup_and_symbol_designators_avoid_per_call_runtime_entries() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fd-identity (x) x)
         (defun fd-local () (let ((f #'fd-identity) (r nil))
                              (dotimes (i 1000 r) (setq r (funcall f i)))))
         (defun fd-symbol (f) (let ((r nil))
                               (dotimes (i 1000 r) (setq r (funcall f i)))))",
        &mut env,
    ).unwrap();
    let (target, _) = super::ordinary_t2::install("FD-IDENTITY", "(x)", "(x)", &mut env);
    let (local, local_code) = super::ordinary_t2::install(
        "FD-LOCAL", "()",
        "((let ((f #'fd-identity) (r nil)) (dotimes (i 1000 r) (setq r (funcall f i)))))",
        &mut env,
    );
    let (symbol, symbol_code) = super::ordinary_t2::install(
        "FD-SYMBOL", "(f)",
        "((let ((r nil)) (dotimes (i 1000 r) (setq r (funcall f i)))))", &mut env,
    );
    for code in [&local_code, &symbol_code] {
        let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("T2 fixture") };
        assert!(mapped.ordinary_entry.is_some(), "both callers require an ordinary entry");
    }
    super::super::native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    egcl_rt::rooted!(result = run_caller("FD-CALLER", "((+ (fd-local) (fd-symbol 'fd-identity)))", 1, &mut env).unwrap());
    let entries = super::super::native_transfer_entry::ordinary_calls::take_named_entries_for_test();
    uninstall(&[target, local, symbol]);
    assert_eq!(*result, EgclVal::from_fixnum(1998));
    assert!(entries < 50, "{entries} runtime entries for 4000 indirect native calls");
}
