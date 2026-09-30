// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_handler_bind_and_restart_case_preserve_fallback_context() {
    use super::super::native_transfer_entry::{take_native_fallback_count, TransferCode};
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((handler-bind ((type-error (lambda (c) (declare (ignore c)) (invoke-restart 'k))))\
          (restart-case (symbol-value x) (k () :ok))))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("NATIVE-HANDLER-BIND", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("native handler-bind/restart-case caller");
    egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
    take_native_fallback_count();
    egcl_rt::rooted!(result = code.run(&args, &mut env));
    egcl_rt::rooted!(ok = reader::read_from_string(":ok").unwrap().0);
    assert_eq!(*result.as_ref().unwrap(), *ok);
    assert_eq!(take_native_fallback_count(), 1);
    assert!(env.handlers.is_empty());
    assert!(env.restarts.is_empty());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_handler_case_enters_selected_clause_without_fallback() {
    use super::super::native_transfer_entry::{
        take_native_cleanup_count, take_native_fallback_count, take_native_handler_count,
        TransferCode,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *native-handler-calls* 0 *native-handler-cleanups* 0)
         (defun native-handler-fail (x)
           (setq *native-handler-calls* (+ *native-handler-calls* 1))
           (symbol-value x))
         (defun native-handler-cleanup ()
           (%force-minor-gc-for-test)
           (setq *native-handler-cleanups* (+ *native-handler-cleanups* 1)))
         (defun native-handler-pass (x)
           (%force-minor-gc-for-test) (values x (list x)))
         (defun native-handler-answer (condition)
           (%force-minor-gc-for-test)
           (values (slot-value condition 'datum) (list condition)))",
        &mut env,
    )
    .unwrap();
    for (source, calls, native_cleanups, cleanups) in [
        (
            "((handler-case (native-handler-fail x) (error (c) (native-handler-answer c))))",
            1,
            0,
            0,
        ),
        (
            "((handler-case (native-handler-fail x) (type-error () (native-handler-pass x))))",
            1,
            0,
            0,
        ),
        (
            "((handler-case (native-handler-fail x) (arithmetic-error (c) (values :wrong c)) (type-error (c) (native-handler-answer c))))",
            1,
            0,
            0,
        ),
        (
            "((handler-case (unwind-protect (native-handler-fail x) (native-handler-cleanup)) (type-error (c) (native-handler-answer c))))",
            1,
            1,
            1,
        ),
        (
            "((unwind-protect (handler-case (native-handler-fail x) (type-error (c) (native-handler-answer c))) (native-handler-cleanup)))",
            1,
            0,
            1,
        ),
        (
            "((handler-case (handler-case (native-handler-fail x) (arithmetic-error (c) (values :wrong c))) (type-error (c) (native-handler-answer c))))",
            1,
            0,
            0,
        ),
        (
            "((handler-case (handler-case (native-handler-fail x) (type-error (c) (native-handler-answer c))) (type-error (c) (values :wrong c))))",
            1,
            0,
            0,
        ),
        (
            "((handler-case (unwind-protect (native-handler-fail 19) (native-handler-fail x)) (type-error (c) (native-handler-answer c))))",
            2,
            1,
            0,
        ),
        (
            "((handler-case (native-handler-pass x) (type-error (c) (values :wrong c))))",
            0,
            0,
            0,
        ),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("NATIVE-HANDLER", *params, *forms, &env, false, false).unwrap(),
        );
        let code =
            TransferCode::compile(body).unwrap_or_else(|| panic!("native handler: {source}"));
        super::super::super::read_eval_all_env(
            "(setq *native-handler-calls* 0 *native-handler-cleanups* 0)",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
        let before = args[0].to_raw();
        take_native_fallback_count();
        take_native_handler_count();
        take_native_cleanup_count();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(
            *result
                .as_ref()
                .unwrap_or_else(|e| panic!("{source}: {e:?}")),
            args[0],
            "{source}"
        );
        assert_ne!(
            args[0].to_raw(),
            before,
            "condition datum must move: {source}"
        );
        assert_eq!(
            take_native_fallback_count(),
            0,
            "selected clause must stay native: {source}"
        );
        assert_eq!(
            take_native_handler_count(),
            usize::from(calls != 0),
            "{source}"
        );
        assert_eq!(take_native_cleanup_count(), native_cleanups, "{source}");
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(env.mv[0], args[0]);
        assert!(env.handlers.is_empty());
        assert_eq!(
            super::super::super::read_eval_all_env("*native-handler-calls*", &mut env).unwrap(),
            EgclVal::from_fixnum(calls),
            "{source}"
        );
        assert_eq!(
            super::super::super::read_eval_all_env("*native-handler-cleanups*", &mut env).unwrap(),
            EgclVal::from_fixnum(cleanups),
            "{source}"
        );
    }
}

#[test]
fn native_v2_handler_case_ir_tracks_clause_destinations() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((handler-case (identity x) (type-error (c) (identity c)) (error () x)))"
        )
        .unwrap()
        .0
    );
    let body =
        Arc::new(compile_function("HANDLER-CFG", *params, *forms, &env, false, false).unwrap());
    let _root = ActiveBytecodeRoot::new(&body);
    let ir = egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups(&body)
        .expect("handler exceptional CFG");
    egcl_compiler::t2::verify::verify(&ir).unwrap();
    use egcl_compiler::t2::{
        emit,
        ir::{AuxData, Opcode},
        verify,
    };
    assert_eq!(ir.handler_cases.len(), 1);
    let landings: Vec<_> = ir
        .block_order()
        .iter()
        .flat_map(|&b| &ir.block(b).insts)
        .copied()
        .filter(|&i| ir.inst(i).opcode == Opcode::HandlerLanding)
        .collect();
    assert!(!landings.is_empty());
    let mut clauses = std::collections::HashSet::new();
    for &landing in &landings {
        if let AuxData::HandlerDestination { clause_index, .. } = ir.inst(landing).aux {
            clauses.insert(clause_index);
        }
    }
    assert_eq!(clauses, std::collections::HashSet::from([0, 1]));
    let (code, sites) = emit::emit_framed_native_handlers(&ir, 1, body.num_slots(), 2, 3, 4, 5, 6)
        .expect("checked handler machine landings");
    assert!(!code.code.is_empty());
    assert!(sites.sites().count() > 0);
    for field in [0, 1, 2] {
        let mut broken = ir.clone();
        if let AuxData::HandlerDestination {
            push_bcp,
            table_index,
            clause_index,
        } = &mut broken.inst_mut(landings[0]).aux
        {
            match field {
                0 => *push_bcp = u32::MAX,
                1 => *table_index = u32::MAX,
                _ => *clause_index = u32::MAX,
            }
        }
        assert!(verify::verify(&broken).is_err());
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_unavailable_handler_clause_preserves_fallback_and_cleanup() {
    use super::super::native_transfer_entry::{
        take_native_cleanup_count, take_native_fallback_count, take_native_handler_count,
        TransferCode,
    };
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(setq *missing-handler-calls* 0 *missing-handler-cleanups* 0)
         (defun missing-handler-fail (x)
           (setq *missing-handler-calls* (+ *missing-handler-calls* 1)) (symbol-value x))
         (defun missing-handler-cleanup ()
           (%force-minor-gc-for-test)
           (setq *missing-handler-cleanups* (+ *missing-handler-cleanups* 1)))
         (defun missing-handler-answer (c)
           (%force-minor-gc-for-test) (values (slot-value c 'datum) (list c)))",
        &mut env,
    )
    .unwrap();
    for (source, cleanups) in [
        (
            "((handler-case (catch nil (missing-handler-fail x)) (type-error (c) (missing-handler-answer c))))",
            0,
        ),
        (
            "((handler-case (unwind-protect (catch nil (missing-handler-fail x)) (missing-handler-cleanup)) (type-error (c) (missing-handler-answer c))))",
            1,
        ),
        (
            "((handler-case (handler-case (missing-handler-fail x) (arithmetic-error (c) (values :wrong c))) (type-error (c) (missing-handler-answer c))))",
            0,
        ),
    ] {
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function("MISSING-HANDLER", *params, *forms, &env, false, false).unwrap(),
        );
        let target = body
            .code
            .iter()
            .position(|i| matches!(i, Instr::PushHandlerCase { .. }))
            .unwrap() as u32;
        let code = TransferCode::compile(body)
            .unwrap_or_else(|| panic!("{source}"))
            .without_handler_destination(target, 0);
        super::super::super::read_eval_all_env(
            "(setq *missing-handler-calls* 0 *missing-handler-cleanups* 0)",
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(args = vec![super::super::super::arena_cons(T, NIL)]);
        let before = args[0].to_raw();
        let frame = egcl_rt::current_stack().fp();
        take_native_fallback_count();
        take_native_handler_count();
        take_native_cleanup_count();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(
            *result
                .as_ref()
                .unwrap_or_else(|e| panic!("{source}: {e:?}")),
            args[0]
        );
        assert_ne!(args[0].to_raw(), before);
        assert_eq!(take_native_handler_count(), 0, "destination must be absent");
        assert_eq!(take_native_fallback_count(), 1);
        assert_eq!(take_native_cleanup_count(), cleanups as usize);
        assert_eq!(
            egcl_rt::current_stack().fp(),
            frame,
            "cluster frames retire once"
        );
        assert!(env.handlers.is_empty());
        assert!(env.catch_stack.is_empty());
        assert!(env.mv_active);
        assert_eq!(env.mv.len(), 2);
        assert_eq!(env.mv[0], args[0]);
        assert_eq!(
            super::super::super::read_eval_all_env("*missing-handler-calls*", &mut env).unwrap(),
            EgclVal::from_fixnum(1)
        );
        assert_eq!(
            super::super::super::read_eval_all_env("*missing-handler-cleanups*", &mut env).unwrap(),
            EgclVal::from_fixnum(cleanups)
        );
    }
}

static NEXT_HANDLER_FIBER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn handler_fiber() -> EgclVal {
    use super::super::native_transfer_entry::{
        take_native_fallback_count, take_native_handler_count, TransferCode,
    };
    let case = NEXT_HANDLER_FIBER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::read_eval_all_env(
        "(defun fiber-handler-fail (x) (symbol-value x))
         (defun fiber-handler-cleanup ()
           (egcl::%native-fiber :sleep 0.2d0) (%force-minor-gc-for-test))
         (defun fiber-handler-answer (c)
           (%force-minor-gc-for-test) (values (slot-value c 'datum) (list c)))",
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((handler-case (unwind-protect (fiber-handler-fail x) (fiber-handler-cleanup))
            (type-error (c) (fiber-handler-answer c))))"
        )
        .unwrap()
        .0
    );
    let body = Arc::new(
        compile_function("FIBER-NATIVE-HANDLER", *params, *forms, &env, false, false).unwrap(),
    );
    let code = TransferCode::compile(body).expect("fiber handler clause");
    egcl_rt::rooted!(
        args = vec![super::super::super::arena_cons(
            EgclVal::from_fixnum(case as i64),
            NIL
        )]
    );
    let before = args[0].to_raw();
    let frame = egcl_rt::current_stack().fp();
    let fiber = egcl_rt::current_fiber_id();
    let depth = NATIVE_DEPTH.with(|slot| slot.get());
    take_native_handler_count();
    take_native_fallback_count();
    egcl_rt::rooted!(result = code.run(&args, &mut env));
    assert_eq!(*result.as_ref().unwrap(), args[0]);
    assert_ne!(args[0].to_raw(), before);
    assert_eq!(
        super::super::super::cp(args[0]).0,
        EgclVal::from_fixnum(case as i64)
    );
    assert_eq!(take_native_handler_count(), 1);
    assert_eq!(take_native_fallback_count(), 0);
    assert!(env.handlers.is_empty());
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], args[0]);
    assert_eq!(egcl_rt::current_stack().fp(), frame);
    assert_eq!(egcl_rt::current_fiber_id(), fiber);
    assert_eq!(NATIVE_DEPTH.with(|slot| slot.get()), depth);
    assert!(egcl_rt::native_transfer::current_segment().is_null());
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_handler_conditions_survive_suspended_fiber_cleanups() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    super::super::super::read_eval_all_env(
        "(handler-case (symbol-value 19) (type-error () nil))",
        &mut startup,
    )
    .unwrap();
    for num_workers in [1, 4] {
        let group =
            egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers }).unwrap();
        let mut fibers = Vec::new();
        for _ in 0..8 {
            let entry =
                unsafe { EgclVal::from_function_ptr(handler_fiber as *const () as *mut u8) };
            let fiber = egcl_rt::thread::make_fiber(entry).unwrap();
            fibers.push(fiber);
            group.submit(fiber).unwrap();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        while fibers
            .iter()
            .filter(|&&id| {
                matches!(
                    egcl_rt::thread::fiber_state(id),
                    Some(
                        egcl_rt::thread::FiberState::Waiting
                            | egcl_rt::thread::FiberState::Blocked
                    )
                )
            })
            .count()
            < 2
        {
            egcl_rt::poll_safepoint();
            assert!(
                std::time::Instant::now() < deadline,
                "two selected conditions must suspend together"
            );
            std::thread::yield_now();
        }
        HeapCollector::new().minor_gc().unwrap();
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(1); 8]);
    }
}
