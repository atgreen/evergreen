// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::emit::TransferCallRequest;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};
use egcl_rt::{Collector, HeapCollector};
mod catches;
mod cleanup_ir;
mod compiled;
mod fallback;
mod fibers;
mod handlers;
mod live_signaling;
mod linkage;
mod nested;
mod payloads;
mod recursion;
mod deopt;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_enabled_entry_uses_cached_segment_path() {
    use super::native_transfer_entry::{take_segment_run_count, try_run_enabled};
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((+ x 1))").unwrap().0);
    let body = Arc::new(
        compile_function("NATIVE-ROLLOUT-PROBE", *params, *forms, &env, false, false)
            .expect("compile rollout probe"),
    );
    egcl_rt::rooted!(args = vec![EgclVal::from_fixnum(41)]);
    // Fresh-process CLI tests cover the process-wide opt-in switch.
    take_segment_run_count();
    let first = try_run_enabled(Arc::clone(&body), &args, &mut env)
        .expect("supported rollout must select the segment path")
        .expect("segment probe returns normally");
    let second = try_run_enabled(body, &args, &mut env)
        .expect("cached rollout must select the segment path")
        .expect("cached segment probe returns normally");
    assert_eq!(first, EgclVal::from_fixnum(42));
    assert_eq!(second, EgclVal::from_fixnum(42));
    assert_eq!(take_segment_run_count(), 2);
}

#[test]
fn native_v2_recursive_body_is_admitted_for_outer_segment() {
    let self_symbol = egcl_rt::symbols::intern("NATIVE-RECURSION-PROBE");
    let body = egcl_rt::bytecode::BytecodeFunction {
        code: vec![
            egcl_rt::bytecode::Instr::CallNamed {
                sym: self_symbol,
                nargs: 0,
            },
            egcl_rt::bytecode::Instr::Return,
        ],
        constants: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 0,
        max_stack: 1,
        arity: 0,
        name: "NATIVE-RECURSION-PROBE".into(),
        params_form: egcl_rt::value::NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    };
    assert!(
        native_transfer_entry::TransferCode::compile(Arc::new(body)).is_some(),
        "recursive bodies should be admitted for an outer segment; nested calls are gated by the active-segment check"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_osr_loop_poll_preserves_native_segment_state() {
    use super::native_transfer_entry::try_run_enabled;
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let body = Arc::new(egcl_rt::bytecode::BytecodeFunction {
        // The first pass through the loop changes a local from T to NIL; the
        // second pass exits.  This gives the T2 OSR body a real back-edge poll
        // without needing a bytecode call (the regression is stack alignment
        // for a poll-only native frame).
        code: vec![
            egcl_rt::bytecode::Instr::LoadLocal(0),
            egcl_rt::bytecode::Instr::Br(2),
            egcl_rt::bytecode::Instr::LoadLocal(0),
            egcl_rt::bytecode::Instr::BrIfFalse(7),
            egcl_rt::bytecode::Instr::Const(0),
            egcl_rt::bytecode::Instr::StoreLocal(0),
            egcl_rt::bytecode::Instr::Br(2),
            egcl_rt::bytecode::Instr::Const(1),
            egcl_rt::bytecode::Instr::Return,
        ],
        constants: vec![NIL, EgclVal::from_fixnum(42)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 1,
        arity: 1,
        name: "NATIVE-OSR-POLL-ALIGNMENT".into(),
        params_form: NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
    });
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(args = vec![egcl_rt::value::T]);
    let result = try_run_enabled(body, &args, &mut env)
        .expect("supported native segment should admit the OSR loop")
        .expect("OSR loop should return normally");
    assert_eq!(result, EgclVal::from_fixnum(42));
}

struct NativeEnvGuard(*mut Env);
impl NativeEnvGuard {
    fn enter(env: &mut Env) -> Self {
        assert!(!native_error_pending());
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnvGuard {
    fn drop(&mut self) {
        NATIVE_ERROR.with(|slot| {
            slot.take();
        });
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}

unsafe fn invoke_bridge(symbol: u32, args: *mut EgclVal, nargs: usize) -> NativeOutcome {
    let mut request = TransferCallRequest {
        symbol: u64::from(symbol),
        nargs,
        args,
        activation: args,
    };
    let mut outcome = NativeOutcome {
        value: NIL,
        exit: NativeExit::Deopt,
    };
    unsafe {
        c2i_call_legacy_v2(
            (&mut request as *mut TransferCallRequest).cast(),
            &mut outcome,
        );
    }
    outcome
}

#[test]
fn native_v2_bridge_preserves_real_lisp_values_errors_and_wide_calls() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(defun v2-zero () (values))
         (defun v2-many (x) (values x (list :second)))
         (defun v2-wide (a b c d e f g h) (list a b c d e f g h))
         (defun v2-error (x) (car x))",
        &mut env,
    )
    .unwrap();
    let zero = egcl_rt::symbols::intern("V2-ZERO");
    let many = egcl_rt::symbols::intern("V2-MANY");
    let wide = egcl_rt::symbols::intern("V2-WIDE");
    let error = egcl_rt::symbols::intern("V2-ERROR");
    let _native = NativeEnvGuard::enter(&mut env);
    let outcome = unsafe { invoke_bridge(zero, std::ptr::null_mut(), 0) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    assert_eq!(outcome.value, NIL);
    assert!(env.mv_active && env.mv.is_empty());

    egcl_rt::rooted!(args = vec![super::super::arena_cons(EgclVal::from_fixnum(19), NIL)]);
    let before = args[0].to_raw();
    let outcome = unsafe { invoke_bridge(many, args.as_mut_ptr(), args.len()) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    egcl_rt::rooted!(primary = outcome.value);
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(*primary, args[0]);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], *primary);
    assert!(env.mv[1].is_cons());
    assert_ne!(
        args[0].to_raw(),
        before,
        "real Lisp bridge preserved a relocated argument"
    );

    egcl_rt::rooted!(wide_args = (0..8).map(EgclVal::from_fixnum).collect::<Vec<_>>());
    let outcome = unsafe { invoke_bridge(wide, wide_args.as_mut_ptr(), wide_args.len()) };
    assert_eq!(outcome.exit, NativeExit::Returned);
    egcl_rt::rooted!(list = outcome.value);
    HeapCollector::new().minor_gc().unwrap();
    assert_eq!(super::super::list_to_vec(*list), *wide_args);

    let mut error_args = [EgclVal::from_fixnum(7)];
    let outcome = unsafe { invoke_bridge(error, error_args.as_mut_ptr(), error_args.len()) };
    assert_eq!(outcome.exit, NativeExit::Transfer);
    assert!(native_error_pending());
    assert!(NATIVE_ERROR.with(|slot| slot.take()).is_some());
    assert!(!native_error_pending());
}

#[test]
fn native_v2_bridge_preserves_throw_multiple_values_and_first_error() {
    let _lock = super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::read_eval_all_env(
        "(setq *v2-count* 0)
         (defun v2-side-effect () (setq *v2-count* (+ *v2-count* 1)))
         (defun v2-throw (x) (throw :v2-tag (values x (list :secondary))))",
        &mut env,
    )
    .unwrap();
    let throwing = egcl_rt::symbols::intern("V2-THROW");
    let side_effect = egcl_rt::symbols::intern("V2-SIDE-EFFECT");
    let tag = reader::read_from_string(":v2-tag").unwrap().0;
    let token = super::super::next_control_token("V2-CATCH");
    env.catch_stack.push((tag, token.clone()));
    let _native = NativeEnvGuard::enter(&mut env);
    egcl_rt::rooted!(args = vec![super::super::arena_cons(EgclVal::from_fixnum(23), NIL)]);
    let outcome = unsafe { invoke_bridge(throwing, args.as_mut_ptr(), args.len()) };
    assert_eq!(outcome.exit, NativeExit::Transfer);
    env.clear_mv();
    HeapCollector::new().minor_gc().unwrap();
    let error = NATIVE_ERROR.with(|slot| slot.take()).unwrap();
    assert!(matches!(error, EgclError::Internal(ref message) if message == &token));
    let primary = super::super::take_control_mv(&token, &mut env);
    assert_eq!(primary, args[0]);
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], primary);
    assert!(env.mv[1].is_cons());
    env.catch_stack.pop();

    NATIVE_ERROR.with(|slot| slot.set_first(EgclError::Interrupt));
    assert_eq!(
        unsafe { invoke_bridge(side_effect, std::ptr::null_mut(), 0) }.exit,
        NativeExit::Transfer
    );
    assert!(matches!(
        NATIVE_ERROR.with(|slot| slot.take()),
        Some(EgclError::Interrupt)
    ));
    let value = super::super::read_eval_all_env("*v2-count*", &mut env).unwrap();
    assert_eq!(value, EgclVal::from_fixnum(0));
}

pub(super) fn pause_child_for_test() -> Result<EgclVal, EgclError> {
    nested::fibers::pause()
}

pub(in crate::cli) use nested::reentry::{NativeEvalProbe, native_reentry_event_for_test};
