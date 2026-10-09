// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::native_transfer_entry::{
    TransferCode, take_native_fallback_count, take_recursive_entries,
};
use super::*;
use std::cell::Cell;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_recursive_frames_preserve_moving_roots_and_retire_on_error() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for fail in [false, true] {
        let name = if fail {
            "RECURSIVE-NATIVE-ERROR"
        } else {
            "RECURSIVE-NATIVE-ROOT"
        };
        let base = if fail {
            "(car 1)"
        } else {
            "(%force-minor-gc-for-test)"
        };
        let form = format!(
            "(if (= n 0) (progn (recursive-root-base) x)
               (progn ({name} (- n 1) x) (car x)))"
        );
        super::super::super::read_eval_all_env(
            &format!("(defun recursive-root-base () {base}) (defun {name} (n x) {form})"),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let body = Arc::new(compile_function(name, *params, *forms, &env, false, false).unwrap());
        registry_put(
            super::super::super::resolve_sym(name)
                .unwrap()
                .as_symbol_index(),
            Arc::clone(&body),
        );
        let code = TransferCode::compile(Arc::clone(&body)).expect("admit recursive source body");
        egcl_rt::rooted!(
            args = vec![
                EgclVal::from_fixnum(8),
                super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL),
            ]
        );
        let old_pointer = args[1].to_raw();
        let stack = egcl_rt::current_stack();
        let old_frame = stack.fp();
        let old_sp = stack.sp();
        let old_depth = NATIVE_DEPTH.with(Cell::get);
        take_recursive_entries();
        take_native_fallback_count();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        assert_eq!(
            take_recursive_entries(),
            8,
            "all recursive calls entered native code: {:?}; result={:?}",
            body.code,
            &*result
        );
        assert_eq!(
            take_native_fallback_count(),
            0,
            "no transfer-triggered bytecode fallback"
        );
        assert_eq!(stack.fp(), old_frame);
        assert_eq!(stack.sp(), old_sp);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), old_depth);
        if fail {
            assert!(
                matches!(&*result, Err(EgclError::Signalled(details))
                if super::super::super::condition_matches_handler(&env, details.condition, "TYPE-ERROR")
                    && details.backtrace.len() >= 8),
                "{:?}",
                &*result
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &EgclVal::from_fixnum(42));
            assert_ne!(
                args[1].to_raw(),
                old_pointer,
                "the suspended caller's heap value actually moved"
            );
            assert_eq!(
                super::super::super::cp(args[1]),
                (EgclVal::from_fixnum(42), NIL)
            );
        }
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_recursive_calls_observe_redefinition_and_unbinding() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for case in 0..3 {
        let unbind = case == 1;
        let name = if unbind {
            "RECURSIVE-UNBOUND"
        } else {
            "RECURSIVE-REPLACED"
        };
        let change = if unbind {
            format!("(fmakunbound '{name})")
        } else if case == 0 {
            format!("(defun {name} (n) 99)")
        } else {
            format!("(setf (symbol-function '{name}) #'recursive-replacement)")
        };
        let form = format!("(if (= n 0) 0 (progn (recursive-change-definition) ({name} (- n 1))))");
        super::super::super::read_eval_all_env(
            &format!("(defun recursive-replacement (n) 99) (defun recursive-change-definition () {change}) (defun {name} (n) {form})"),
            &mut env,
        ).unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let body = compile_recursive_fixture(name, *params, *forms, &env);
        let symbol = super::super::super::resolve_sym(name)
            .unwrap()
            .as_symbol_index();
        let code = TransferCode::compile(body).expect("admit recursive source body");
        let stack = egcl_rt::current_stack();
        let old_frame = stack.fp();
        let old_sp = stack.sp();
        let old_depth = NATIVE_DEPTH.with(Cell::get);
        take_recursive_entries();
        egcl_rt::rooted!(original_function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        egcl_rt::rooted!(result = code.run(&[EgclVal::from_fixnum(1)], &mut env));
        let entries = take_recursive_entries();
        assert_ne!(
            egcl_rt::symbols::symbol_function(symbol).unwrap(),
            *original_function,
            "helper must change the function cell; case={case}, native entries={entries}"
        );
        assert_eq!(stack.fp(), old_frame);
        assert_eq!(stack.sp(), old_sp);
        assert_eq!(NATIVE_DEPTH.with(Cell::get), old_depth);
        if unbind {
            assert!(
                matches!(&*result, Err(EgclError::UndefinedFunction(_)))
                    || matches!(&*result, Err(EgclError::Signalled(details))
                    if super::super::super::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")),
                "{:?}",
                &*result
            );
        } else {
            assert_eq!(
                result.as_ref().unwrap(),
                &EgclVal::from_fixnum(99),
                "case={case}"
            );
        }
        assert_eq!(entries, 0, "the old definition must not be entered");
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_recursive_depth_fallback_preserves_values_and_executes_once() {
    assert!(egcl_rt::native_transfer::is_supported());
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for fail in [false, true] {
        let base = if fail {
            "(car 1)"
        } else {
            "(values x (list x))"
        };
        let form = "(progn (recursive-bounded-tick)
            (if (= n 0) (recursive-bounded-base x)
                (recursive-bounded (- n 1) x)))";
        super::super::super::read_eval_all_env(
            &format!(
                "(setq *recursive-bounded-ticks* 0 *recursive-bounded-cleanups* 0)
             (defun recursive-bounded-tick ()
               (setq *recursive-bounded-ticks* (+ *recursive-bounded-ticks* 1)))
             (defun recursive-bounded-base (x)
               (unwind-protect {base}
                 (setq *recursive-bounded-cleanups* (+ *recursive-bounded-cleanups* 1))))
             (defun recursive-bounded (n x) {form})"
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({form})")).unwrap().0);
        let body = compile_recursive_fixture("RECURSIVE-BOUNDED", *params, *forms, &env);
        let code = TransferCode::compile(body).unwrap();
        egcl_rt::rooted!(
            args = vec![
                EgclVal::from_fixnum(200),
                super::super::super::arena_cons(EgclVal::from_fixnum(42), NIL)
            ]
        );
        let stack = egcl_rt::current_stack();
        let old_frame = stack.fp();
        let old_sp = stack.sp();
        let depth = NATIVE_DEPTH.with(|slot| slot.replace(native_depth_cap().saturating_sub(3)));
        take_recursive_entries();
        egcl_rt::rooted!(result = code.run(&args, &mut env));
        let restored_depth = NATIVE_DEPTH.with(|slot| slot.replace(depth));
        assert_eq!(
            take_recursive_entries(),
            2,
            "recursion must enter native code, then use bounded fallback"
        );
        assert_eq!(restored_depth, native_depth_cap().saturating_sub(3));
        assert_eq!(stack.fp(), old_frame);
        assert_eq!(stack.sp(), old_sp);
        if fail {
            assert!(
                matches!(&*result, Err(EgclError::Signalled(details))
                if super::super::super::condition_matches_handler(&env, details.condition, "TYPE-ERROR")),
                "{:?}",
                &*result
            );
        } else {
            assert_eq!(result.as_ref().unwrap(), &args[1]);
            assert!(env.mv_active);
            assert_eq!(env.mv.len(), 2);
            assert_eq!(env.mv[0], args[1]);
            assert_eq!(super::super::super::cp(env.mv[1]), (args[1], NIL));
        }
        assert_eq!(
            super::super::super::read_eval_all_env("*recursive-bounded-ticks*", &mut env).unwrap(),
            EgclVal::from_fixnum(201)
        );
        assert_eq!(
            super::super::super::read_eval_all_env("*recursive-bounded-cleanups*", &mut env)
                .unwrap(),
            EgclVal::from_fixnum(1)
        );
    }
}

/// Preserve real calls for the ABI tests: lowering an anonymous body prevents
/// the named self-tail-call pass from replacing the edges with a loop. Publish
/// it under the actual test definition only after lowering, without changing
/// the process-wide TCO setting or affecting other tests.
fn compile_recursive_fixture(
    name: &str,
    params: EgclVal,
    forms: EgclVal,
    env: &Env,
) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = params);
    egcl_rt::rooted!(forms = forms);
    let symbol = super::super::super::resolve_sym(name)
        .unwrap()
        .as_symbol_index();
    let mut body = compile_function("<lambda>", *params, *forms, env, false, false).unwrap();
    body.name = name.into();
    assert!(
        body.code
            .iter()
            .any(|op| matches!(op, Instr::CallNamed { sym, .. } if *sym == symbol)),
        "fixture must retain an actual recursive call"
    );
    let body = Arc::new(body);
    registry_put(symbol, Arc::clone(&body));
    body
}
