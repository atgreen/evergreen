// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{arena_cons, format_val, heap_test_lock, read_eval_all_env};

#[test]
fn deopt_scope_count_is_bounded_before_allocation() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        cold_guesses: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies { bodies: Vec::new() }),
        deopt_bodies: Default::default(),
    });
    let words = [0u64; 4];
    let stack = egcl_rt::current_stack();
    let before_fp = stack.fp();
    let before_sp = stack.sp();
    let mut panicked = Vec::new();
    for count in [0, 1, 2, u64::MAX] {
        // The slice always names real storage. MAX overflows vector capacity
        // before attempting an allocation in the old implementation; it does
        // not require exhausting memory or constructing an invalid raw slice.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            materialize_t2_deopt_scopes(
                count, words.len() as u64, words.as_ptr(), None, Some(Arc::clone(&metadata)),
            )
        }));
        match result {
            Ok(Err(_)) => {}
            Ok(Ok(_)) => panic!("an owner absent from metadata must never be accepted"),
            Err(_) => panicked.push(count),
        }
        assert_eq!(stack.fp(), before_fp);
        assert_eq!(stack.sp(), before_sp);
    }
    assert!(panicked.is_empty(), "scope counts must reject without panicking: {panicked:?}");
}

#[test]
fn deopt_outer_frame_has_room_for_the_resumed_body() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
    let owner = egcl_rt::symbols::intern("DEOPT-OUTER-CAPACITY");
    let body = Arc::new(
        compile_function("DEOPT-OUTER-CAPACITY", *params, *forms, &env, false, false)
            .expect("compile retained identity"),
    );
    assert_eq!(body.n_locals, 1);
    assert_eq!(body.max_stack, 1);
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        cold_guesses: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
            bodies: vec![Arc::clone(&body)],
        }),
        deopt_bodies: [(owner, Arc::clone(&body))].into_iter().collect(),
    });
    let stack = egcl_rt::current_stack();
    let before_fp = stack.fp();
    let before_sp = stack.sp();
    let mut mismatches = Vec::new();
    for (capacity, accepted) in [(1, false), (2, true), (3, true)] {
        let frame = stack.push_frame(
            EgclVal::from_symbol_index(owner), std::ptr::null(), capacity, FLAG_CALL,
        ).unwrap();
        let original = EgclVal::from_fixnum(17);
        unsafe { slot_set(frame, 0, original) };
        let frame_sp = stack.sp();
        // The saved local fits even in the undersized frame, so probing the
        // old acceptance is safe. Never execute that frame: LoadLocal needs
        // a second slot despite the empty saved operand stack.
        let words = [u64::from(owner), 0, 1, 0, EgclVal::from_fixnum(23).0];
        let result = materialize_t2_deopt_scopes(
            1, words.len() as u64, words.as_ptr(), Some(frame), Some(Arc::clone(&metadata)),
        );
        if result.is_ok() != accepted { mismatches.push(capacity); }
        if result.is_err() {
            assert_eq!(unsafe { slot_get(frame, 0) }, original, "rejection preserves locals");
        }
        assert_eq!(stack.fp(), frame);
        assert_eq!(stack.sp(), frame_sp);
        stack.pop_frame();
        assert_eq!(stack.fp(), before_fp);
        assert_eq!(stack.sp(), before_sp);
    }
    assert!(mismatches.is_empty(), "incorrect outer-frame capacity acceptance: {mismatches:?}");
}

#[test]
fn deopt_scope_chain_matches_call_continuations() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defun deopt-chain-child (x) (+ x 1))", &mut env).unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(child_forms = reader::read_from_string("((+ x 1))").unwrap().0);
    let child_symbol = egcl_rt::symbols::intern("DEOPT-CHAIN-CHILD");
    let child = Arc::new(
        compile_function("DEOPT-CHAIN-CHILD", *params, *child_forms, &env, false, false)
            .expect("compile retained child"),
    );
    let _child_root = egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
        bodies: vec![Arc::clone(&child)],
    });
    egcl_rt::rooted!(parent_forms = reader::read_from_string("((+ 3 (deopt-chain-child x)))").unwrap().0);
    let parent_symbol = egcl_rt::symbols::intern("DEOPT-CHAIN-PARENT");
    let parent = Arc::new(
        compile_function("DEOPT-CHAIN-PARENT", *params, *parent_forms, &env, false, false)
            .expect("compile retained parent"),
    );
    let call_pc = parent.code.iter().position(|op| {
        matches!(op, Instr::CallNamed { sym, nargs: 1 } if *sym == child_symbol)
    }).expect("parent must call the retained child") as u64;
    assert_eq!(parent.n_locals, 1);
    assert_eq!(child.n_locals, 1);
    assert!(parent.max_stack >= 2);
    let other_symbol = egcl_rt::symbols::intern("DEOPT-CHAIN-OTHER");
    let skipped_symbol = egcl_rt::symbols::intern("DEOPT-CHAIN-SKIPPED-CALL");
    let mut skipped = (*parent).clone();
    skipped.constants = vec![EgclVal::from_fixnum(3), EgclVal::from_fixnum(18)];
    skipped.code = vec![
        Instr::Const(0),
        Instr::Const(1),
        Instr::Br(4),
        Instr::CallNamed { sym: child_symbol, nargs: 1 },
        Instr::CallNamed { sym: egcl_rt::symbols::intern("+"), nargs: 2 },
        Instr::Return,
    ];
    skipped.max_stack = 2;
    let skipped = Arc::new(skipped);
    let depths = verified_operand_stack_depths(&skipped).expect("valid branch-over-call bytecode");
    assert_eq!(depths[3], None, "the alleged suspended call is unreachable");
    assert_eq!(depths[4], Some(2), "its successor is reachable by the branch");
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        cold_guesses: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
            bodies: vec![Arc::clone(&parent), Arc::clone(&child), Arc::clone(&skipped)],
        }),
        deopt_bodies: [
            (parent_symbol, Arc::clone(&parent)),
            (child_symbol, Arc::clone(&child)),
            (other_symbol, Arc::clone(&child)),
            (skipped_symbol, skipped),
        ].into_iter().collect(),
    });
    let scope = |owner: u32, bcp, values: &[EgclVal]| {
        let mut words = vec![u64::from(owner), bcp, 1, values.len() as u64];
        words.push(EgclVal::from_fixnum(17).0);
        words.extend(values.iter().map(|value| value.0));
        words
    };
    let three = EgclVal::from_fixnum(3);
    let mut valid = scope(parent_symbol, call_pc + 1, &[three]);
    valid.extend(scope(child_symbol, 0, &[]));
    let mut trailing_word = valid.clone();
    trailing_word.push(0);
    let mut wrong_target = scope(parent_symbol, call_pc + 1, &[three]);
    wrong_target.extend(scope(other_symbol, 0, &[]));
    let mut wrong_parent_pc = scope(parent_symbol, 0, &[three]);
    wrong_parent_pc.extend(scope(child_symbol, 0, &[]));
    let mut wrong_parent_depth = scope(parent_symbol, call_pc + 1, &[]);
    wrong_parent_depth.extend(scope(child_symbol, 0, &[]));
    let mut wrong_child_depth = scope(parent_symbol, call_pc + 1, &[three]);
    wrong_child_depth.extend(scope(child_symbol, 0, &[three]));
    let mut unreachable_call = scope(skipped_symbol, 4, &[three]);
    unreachable_call.extend(scope(child_symbol, 0, &[]));
    let cases = [
        ("valid retained chain", valid, 2, true),
        ("valid post-call root", scope(parent_symbol, call_pc + 1, &[three, EgclVal::from_fixnum(18)]), 1, true),
        ("wrong call target", wrong_target, 2, false),
        ("parent not after call", wrong_parent_pc, 2, false),
        ("wrong pending parent depth", wrong_parent_depth, 2, false),
        ("wrong child guard depth", wrong_child_depth, 2, false),
        ("wrong root guard depth", scope(child_symbol, 0, &[three]), 1, false),
        ("unreachable suspended call", unreachable_call, 2, false),
        ("trailing word after valid chain", trailing_word, 2, false),
    ];
    let stack = egcl_rt::current_stack();
    let before_fp = stack.fp();
    let before_sp = stack.sp();
    let mut mismatches = Vec::new();
    for (name, words, count, accepted) in cases {
        let result = materialize_t2_deopt_scopes(
            count, words.len() as u64, words.as_ptr(), None, Some(Arc::clone(&metadata)),
        );
        if result.is_ok() != accepted { mismatches.push(name); }
        if let Ok((scopes, retained)) = result {
            if accepted {
                assert_eq!(
                    resume_inlined_in_t0(scopes, retained, None, &mut env).unwrap(),
                    EgclVal::from_fixnum(21),
                    "{name}: resume exactly the pending computation",
                );
            } else {
                // Malformed activations are never executed.
                for _ in scopes { stack.pop_frame(); }
            }
        }
        assert_eq!(stack.fp(), before_fp, "{name}: frame cleanup");
        assert_eq!(stack.sp(), before_sp, "{name}: stack cleanup");
    }
    assert!(mismatches.is_empty(), "incorrect continuation acceptance: {mismatches:?}");
}

#[test]
fn deopt_scope_headers_match_retained_bodies() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env("(defun deopt-header-target (x) x)", &mut env).unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((deopt-header-target x))").unwrap().0);
    let symbol = egcl_rt::symbols::intern("DEOPT-HEADER-OWNER");
    let body = Arc::new(
        compile_function("DEOPT-HEADER-OWNER", *params, *forms, &env, false, false).unwrap(),
    );
    assert_eq!(body.n_locals, 1);
    let after_call = body.code.iter().position(|op| matches!(op, Instr::CallNamed { .. }))
        .expect("fixture must have a suspended call") as u64 + 1;
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        cold_guesses: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
            bodies: vec![Arc::clone(&body)],
        }),
        deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
    });
    let scope = |function, bcp, locals, depth| {
        let mut words = vec![function, bcp, locals, depth];
        words.resize(4 + (locals + depth) as usize, NIL.0);
        words
    };
    let owner = u64::from(symbol);
    let mut full_parent = scope(owner, after_call, 1, u64::from(body.max_stack));
    full_parent.extend(scope(owner, 0, 1, 0));
    let mut valid_parent = scope(owner, after_call, 1, 0);
    valid_parent.extend(scope(owner, 0, 1, 0));
    let cases = [
        ("valid root", scope(owner, 0, 1, 0), 1, true),
        ("header-shaped but mismatched callee", valid_parent, 2, false),
        ("missing local", scope(owner, 0, 0, 0), 1, false),
        ("extra local", scope(owner, 0, 2, 0), 1, false),
        ("truncated function id", scope(owner + (1u64 << 32), 0, 1, 0), 1, false),
        ("truncated resume position", scope(owner, 1u64 << 32, 1, 0), 1, false),
        ("resume past body", scope(owner, body.code.len() as u64, 1, 0), 1, false),
        ("excess operand depth", scope(owner, 0, 1, u64::from(body.max_stack) + 1), 1, false),
        ("no room for child result", full_parent, 2, false),
    ];
    let stack = egcl_rt::current_stack();
    let parent = stack.fp();
    let before_sp = stack.sp();
    let mut mismatches = Vec::new();
    for (name, words, count, accepted) in cases {
        // Never resume malformed activations. All writes are into fresh frames
        // allocated from the retained body, so this probes rejection safely.
        let result = materialize_t2_deopt_scopes(
            count, words.len() as u64, words.as_ptr(), None, Some(Arc::clone(&metadata)),
        );
        if result.is_ok() != accepted {
            mismatches.push(name);
        }
        if let Ok((scopes, _)) = result {
            for _ in scopes { stack.pop_frame(); }
        }
        assert_eq!(stack.fp(), parent, "{name}: frame rollback");
        assert_eq!(stack.sp(), before_sp, "{name}: stack rollback");
    }
    assert!(mismatches.is_empty(), "incorrect frame-header acceptance: {mismatches:?}");
}

#[test]
fn malformed_inner_deopt_scope_preserves_outer_frame() {
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
    let symbol = egcl_rt::symbols::intern("DEOPT-VALIDATE-BEFORE-WRITE");
    let body = Arc::new(
        compile_function("DEOPT-VALIDATE-BEFORE-WRITE", *params, *forms, &env, false, false)
            .unwrap(),
    );
    assert_eq!(body.n_locals, 1);
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: Vec::new(),
        cold_guesses: Vec::new(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
            bodies: vec![Arc::clone(&body)],
        }),
        deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
    });
    let stack = egcl_rt::current_stack();
    let parent = stack.fp();
    let frame = stack
        .push_frame(EgclVal::from_symbol_index(symbol), std::ptr::null(), body.num_slots(), FLAG_CALL)
        .unwrap();
    let original = EgclVal::from_fixnum(17);
    unsafe { slot_set(frame, 0, original) };
    // The second scope is structurally complete but has no retained body.
    let absent = egcl_rt::symbols::intern("DEOPT-MISSING-INNER-BODY");
    let words = [u64::from(symbol), 0, 1, 0, NIL.0, u64::from(absent), 0, 0, 0];
    let result = materialize_t2_deopt_scopes(
        2, words.len() as u64, words.as_ptr(), Some(frame), Some(metadata),
    );
    let after = unsafe { slot_get(frame, 0) };
    let restored_fp = stack.fp();
    stack.pop_frame();
    assert!(result.is_err(), "an unknown retained body must be rejected");
    assert_eq!(restored_fp, frame);
    assert_eq!(stack.fp(), parent);
    assert_eq!(after, original, "reject the whole stream before writing outer slots");
}

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
        assert_eq!(take_nested_entries(), 0, "ordinary child uses its own segment boundary");
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
    assert!(code.ordinary_entry.is_some(), "guarded child uses the ordinary register ABI");
    assert!(code.mapped_entry().is_none(), "no legacy child entry is available");
    let installed =
        super::super::super::native_transfer_entry::install_baseline_code(symbol, code).unwrap();
    mark_fresh_promotion(symbol);
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
    let installed = NATIVE_REGISTRY.with(|registry| registry.borrow()
        .get(&egcl_rt::symbols::find_index("CHILD-PHASE").unwrap()).cloned()).unwrap();
    let NativeCodeStorage::Mapped(owner) = &installed._storage else { unreachable!() };
    assert!(owner.ordinary_entry.is_some() && owner.mapped_entry().is_none());
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
    assert_eq!(take_nested_entries(), 0, "ordinary child uses its own segment boundary");
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
        cold_guesses: Vec::new(),
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
    // The installed version's fixnum guard was a cold guess, which the
    // single-float argument disproves, so it is replaced at its first such
    // deopt rather than after the blacklist threshold (bliss-9bw59).
    assert!(
        (1..deopt_blacklist_threshold()).contains(&old.deopt_count()),
        "future child entries must replace the failed installed version early: {} deopts",
        old.deopt_count()
    );
    let symbol = egcl_rt::symbols::intern("CHILD-INSTALLED-PHASE");
    // Guard retirement queues its replacement; wait for owner-side publication
    // without executing the Lisp body again.
    let replacement = background_t1::wait(symbol).expect("publish replacement after guard retirement");
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
    assert_eq!(take_nested_entries(), 0, "ordinary child uses its own segment boundary");
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
    let old_maps = Arc::downgrade(old.value_maps_for_test());
    assert!(old.value_maps_for_test().iter().any(|site| matches!(
        old.value_maps_for_test().value_map(site.return_offset),
        Some(egcl_compiler::t2::x64_calls::NativeCallValues::Deoptimizing { .. })
    )));
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
    assert_eq!(take_nested_entries(), 0, "ordinary child uses its own segment boundary");
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
    assert!(old_maps.upgrade().is_some(), "redefinition must retain old code maps");
    drop(old);
    assert!(old_maps.upgrade().is_none(), "maps retire with their code owner");
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
    mark_fresh_promotion(symbol);
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
