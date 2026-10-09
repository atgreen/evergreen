// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{arena_cons, format_val, heap_test_lock, read_eval_all_env};

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_optimized_child_guard_returns_and_escapes_without_replay() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let forms = "((child-deopt-tick) (child-deopt-finish x (+ n 1) fail))";
    read_eval_all_env(
        &format!(
            "(setq *child-deopt-ticks* 0 *child-deopt-cleanups* 0)
         (defun child-deopt-tick () (setq *child-deopt-ticks* (+ *child-deopt-ticks* 1)))
         (defun child-deopt-cleanup () (setq *child-deopt-cleanups* (+ *child-deopt-cleanups* 1)))
         (defun child-deopt-finish (x n fail)
           (%force-minor-gc-for-test)
           (if fail (throw :child-deopt (values x n (list x))) (values x n (list x))))
         (defun child-deopt (x n fail) {})",
            &forms[1..forms.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let mapped = install_guarded_child("CHILD-DEOPT", "(x n fail)", forms, &env);
    let caller = compile_caller(
        "(x n fail)",
        "((catch :child-deopt (unwind-protect (child-deopt x n fail) (child-deopt-cleanup))))",
        &env,
    );
    caller
        .run(&[NIL, EgclVal::from_fixnum(1), NIL], &mut env)
        .unwrap();
    for fail in [NIL, T] {
        read_eval_all_env(
            "(setq *child-deopt-ticks* 0 *child-deopt-cleanups* 0)",
            &mut env,
        )
        .unwrap();
        take_nested_entries();
        let deopts = mapped.deopt_count();
        let stack = egcl_rt::current_stack();
        let fp = stack.fp();
        let sp = stack.sp();
        let depth = NATIVE_DEPTH.with(|depth| depth.get());
        egcl_rt::rooted!(args = vec![arena_cons(T, NIL), EgclVal::from_single_float(1.5), fail]);
        let address = args[0].to_raw();
        egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
        assert_eq!(take_nested_entries(), 1);
        assert_eq!(mapped.deopt_count(), deopts + 1);
        assert_eq!(*result, args[0]);
        assert_ne!(args[0].to_raw(), address, "child T0 must move caller roots");
        assert_eq!(cp(args[0]), (T, NIL));
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 3);
        assert_eq!(env.mv[0], args[0]);
        assert_eq!(env.mv[1], EgclVal::from_single_float(2.5));
        assert_eq!(cp(env.mv[2]), (args[0], NIL));
        assert_eq!(stack.fp(), fp);
        assert_eq!(stack.sp(), sp);
        assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), depth);
        assert!(env.catch_stack.is_empty());
        assert!(env.handlers.is_empty());
        assert!(env.restarts.is_empty());
        assert_eq!(
            read_eval_all_env(
                "(list *child-deopt-ticks* *child-deopt-cleanups*)",
                &mut env
            )
            .map(format_val)
            .unwrap(),
            "(1 1)"
        );
    }
}

fn install_guarded_child(name: &str, params: &str, forms: &str, env: &Env) -> Rc<TransferCode> {
    egcl_rt::rooted!(params = reader::read_from_string(params).unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let symbol = egcl_rt::symbols::intern(name);
    let body = Arc::new(compile_function(name, *params, *forms, env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let code = TransferCode::compile(Arc::clone(&body)).expect("optimized mapped child");
    assert!(code.has_deopt, "{:?}", body.code);
    let installed =
        super::super::super::native_transfer_entry::install_baseline_code(symbol, code).unwrap();
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &installed,
    );
    let NativeCodeStorage::Mapped(code) = &installed._storage else {
        unreachable!()
    };
    Rc::clone(code)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_published_t2_child_uses_guards_and_relearns_numeric_phase() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defun child-phase (x) (+ x 1))", &mut env).unwrap();
    install_t2("CHILD-PHASE", "(x)", "((+ x 1))", &env);
    let caller = compile_caller("(x)", "((catch :phase (child-phase x)))", &env);
    caller.run(&[EgclVal::from_fixnum(1)], &mut env).unwrap();
    take_nested_entries();
    let before = DEOPT_COUNT.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(
        caller
            .run(&[EgclVal::from_single_float(1.5)], &mut env)
            .unwrap(),
        EgclVal::from_single_float(2.5)
    );
    assert_eq!(take_nested_entries(), 1);
    assert_eq!(
        DEOPT_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        before + 1,
        "published T2 child must really execute a speculative mapped guard"
    );
    let threshold = deopt_blacklist_threshold();
    for _ in 0..threshold + 2 {
        caller
            .run(&[EgclVal::from_single_float(1.5)], &mut env)
            .unwrap();
    }
    assert_eq!(
        DEOPT_COUNT.load(std::sync::atomic::Ordering::Relaxed),
        before + threshold as u64,
        "numeric phase must rebuild only the failed mapped version"
    );
}

#[test]
fn deopt_resume_keeps_original_callable_heat() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let source = "((+ x 1) (dotimes (i 5)) (values x (list x)))";
    read_eval_all_env(
        &format!(
            "(defun child-retained (x) {})",
            &source[1..source.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
    let symbol = egcl_rt::symbols::intern("CHILD-RETAINED");
    let body =
        Arc::new(compile_function("CHILD-RETAINED", *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
            bodies: vec![Arc::clone(&body)],
        }),
        deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
    });
    egcl_rt::rooted!(original = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let old_heat = egcl_rt::function::back_edge_count(*original);
    read_eval_all_env("(defun child-retained (x) (+ x 900))", &mut env).unwrap();
    let stack = egcl_rt::current_stack();
    let parent = stack.fp();
    let frame = stack
        .push_frame(*original, std::ptr::null(), body.num_slots(), FLAG_CALL)
        .unwrap();
    bind_params(&body, frame, &[EgclVal::from_single_float(1.5)], None);
    let scope = InlinedResumeScope {
        function: symbol,
        body,
        bcp: 0,
        sp_top: 0,
        frame,
    };
    assert_eq!(
        resume_inlined_in_t0(vec![scope], metadata, None, &mut env).unwrap(),
        EgclVal::from_single_float(1.5)
    );
    assert_eq!(stack.fp(), parent);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(cp(env.mv[1]), (EgclVal::from_single_float(1.5), NIL));
    egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert_ne!(*replacement, *original);
    assert!(
        egcl_rt::function::back_edge_count(*original) > old_heat,
        "resumed loop heat belongs to the retained callable"
    );
    assert_eq!(
        egcl_rt::function::back_edge_count(*replacement),
        0,
        "old child must not heat its replacement"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_installed_child_rebuilds_failed_guard_version() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defun child-installed-phase (x) (+ x 1))", &mut env).unwrap();
    let old = install_guarded_child("CHILD-INSTALLED-PHASE", "(x)", "((+ x 1))", &env);
    let caller = compile_caller("(x)", "((catch :phase (child-installed-phase x)))", &env);
    caller.run(&[EgclVal::from_fixnum(1)], &mut env).unwrap();
    let argument = EgclVal::from_single_float(1.5);
    for _ in 0..deopt_blacklist_threshold() + 2 {
        assert_eq!(
            caller.run(&[argument], &mut env).unwrap(),
            EgclVal::from_single_float(2.5)
        );
    }
    assert_eq!(
        old.deopt_count(),
        deopt_blacklist_threshold(),
        "future child entries must replace the failed installed version"
    );
    let symbol = egcl_rt::symbols::intern("CHILD-INSTALLED-PHASE");
    let replacement = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&symbol).cloned())
        .unwrap();
    let NativeCodeStorage::Mapped(mapped) = &replacement._storage else {
        panic!("mapped replacement")
    };
    assert!(!Rc::ptr_eq(&old, mapped));
    assert_eq!(mapped.deopt_count(), 0);
    // Retained old code can still finish after replacement; its feedback must
    // not mark the replacement for retirement, even for the identical body.
    old.run(&[argument], &mut env).unwrap();
    assert!(!mapped.needs_recompile());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_child_guard_signals_with_live_parent_restart() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(setq *guard-signals* 0 *guard-cleanups* 0)
         (defun guard-signal-child (x) (+ x 1))
         (defun guard-cleanup () (setq *guard-cleanups* (+ *guard-cleanups* 1)))
         (defun guard-handler (c)
           (setq *guard-signals* (+ *guard-signals* 1))
           (%force-minor-gc-for-test)
           (invoke-restart 'guard-resume (slot-value c 'datum)))",
        &mut env,
    )
    .unwrap();
    let mapped = install_guarded_child("GUARD-SIGNAL-CHILD", "(x)", "((+ x 1))", &env);
    let caller = compile_caller(
        "(x)",
        "((restart-case (handler-bind ((type-error #'guard-handler))
            (unwind-protect (guard-signal-child x) (guard-cleanup)))
            (guard-resume (v) (values v (list v)))))",
        &env,
    );
    caller.run(&[EgclVal::from_fixnum(1)], &mut env).unwrap();
    read_eval_all_env("(setq *guard-cleanups* 0)", &mut env).unwrap();
    egcl_rt::rooted!(args = vec![arena_cons(T, NIL)]);
    let before = args[0].to_raw();
    let stack = egcl_rt::current_stack();
    let fp = stack.fp();
    let sp = stack.sp();
    take_nested_entries();
    egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
    assert_eq!(*result, args[0]);
    assert_eq!(take_nested_entries(), 1);
    assert_eq!(mapped.deopt_count(), 1);
    assert_ne!(args[0].to_raw(), before);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(cp(env.mv[1]), (args[0], NIL));
    assert_eq!(stack.fp(), fp);
    assert_eq!(stack.sp(), sp);
    assert!(env.handlers.is_empty() && env.restarts.is_empty());
    assert_eq!(
        read_eval_all_env("(list *guard-signals* *guard-cleanups*)", &mut env)
            .map(format_val)
            .unwrap(),
        "(1 1)"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_child_redefinition_before_guard_keeps_original_result() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let source = "((guard-redefine active) (guard-old-values (+ x 1)))";
    read_eval_all_env(
        &format!(
            "(setq *guard-redefines* 0)
         (defun guard-redefine (active)
           (if active (progn (setq *guard-redefines* (+ *guard-redefines* 1))
             (defun guard-old-child (x active) (+ x 900))) nil))
         (defun guard-old-values (x) (%force-minor-gc-for-test) (values x (list x)))
         (defun guard-old-child (x active) {})",
            &source[1..source.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let old = install_guarded_child("GUARD-OLD-CHILD", "(x active)", source, &env);
    let caller = compile_caller(
        "(x active)",
        "((catch :old (guard-old-child x active)))",
        &env,
    );
    caller
        .run(&[EgclVal::from_fixnum(1), NIL], &mut env)
        .unwrap();
    take_nested_entries();
    assert_eq!(
        caller
            .run(&[EgclVal::from_single_float(1.5), T], &mut env)
            .unwrap(),
        EgclVal::from_single_float(2.5)
    );
    assert_eq!(take_nested_entries(), 1);
    assert_eq!(old.deopt_count(), 1);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(cp(env.mv[1]), (EgclVal::from_single_float(2.5), NIL));
    assert_eq!(
        read_eval_all_env("*guard-redefines*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
    assert_eq!(
        read_eval_all_env("(guard-old-child 1.5 nil)", &mut env).unwrap(),
        EgclVal::from_single_float(901.5)
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_retired_installed_guard_does_not_decay_new_version_profile() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defun guard-version (x) (+ x 1))", &mut env).unwrap();
    let old = install_guarded_child("GUARD-VERSION", "(x)", "((+ x 1))", &env);
    let symbol = egcl_rt::symbols::intern("GUARD-VERSION");
    let original = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&symbol).cloned())
        .unwrap();
    let body = Arc::clone(original.body.as_ref().unwrap());
    let bcp = body
        .code
        .iter()
        .position(|op| matches!(op, Instr::CallNamed { .. }))
        .unwrap() as u32;
    let replacement = super::super::super::native_transfer_entry::install_baseline_code(
        symbol,
        TransferCode::compile(Arc::clone(&body)).unwrap(),
    )
    .unwrap();
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &replacement,
    );
    let key = Arc::as_ptr(&body) as usize;
    for _ in 0..16 {
        record_type_profile(key, bcp, &[EgclVal::from_fixnum(1); 2]);
    }
    for _ in 0..deopt_blacklist_threshold() + 1 {
        assert_eq!(
            run_native(
                &original,
                symbol,
                &[EgclVal::from_single_float(1.5)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_single_float(2.5)
        );
    }
    assert!(old.deopt_count() > deopt_blacklist_threshold());
    assert_eq!(
        type_profile_at(key, bcp).unwrap().fixnum,
        16,
        "obsolete code may record observations but cannot decay its replacement's profile"
    );
    assert!(
        NATIVE_REGISTRY
            .with(|registry| Rc::ptr_eq(registry.borrow().get(&symbol).unwrap(), &replacement))
    );
    run_native(
        &replacement,
        symbol,
        &[EgclVal::from_single_float(1.5)],
        &mut env,
    )
    .unwrap();
    assert_eq!(
        type_profile_at(key, bcp).unwrap().fixnum,
        0,
        "the current failed version must apply its own feedback"
    );
}
