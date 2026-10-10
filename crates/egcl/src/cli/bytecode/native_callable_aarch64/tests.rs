// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{heap_test_lock, read_eval_all_env};

fn require_native_aarch64() {
    assert!(
        enabled(),
        "set EGCL_NATIVE_TRANSFER=1 for the AAPCS64 execution gate"
    );
    assert!(
        egcl_rt::native_transfer::is_supported(),
        "AAPCS64 callable execution gate requires supported segment boundary; refusal is not execution coverage"
    );
}

#[test]
#[ignore = "requires AArch64 with supported segment boundary"]
fn aapcs64_published_callables_execute_both_entries_and_retire_segments() {
    require_native_aarch64();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defun aapcs64-entry-fn (x) (+ x 1))
        (defun aapcs64-entry-compiled (x) (+ x 4))
        (compile 'aapcs64-entry-compiled)
        (defmethod aapcs64-entry-generic ((x t)) (+ x 2))
        (defclass aapcs64-entry-instance () () (:metaclass egcl-ext:funcallable-standard-class))
        (setq *aapcs64-entry-instance* (make-instance 'aapcs64-entry-instance))
        (egcl-ext:set-funcallable-instance-function *aapcs64-entry-instance* (lambda (x) (+ x 5)))",
        &mut env,
    )
    .unwrap();
    for (source, expected) in [
        ("#'aapcs64-entry-fn", 11),
        ("#'aapcs64-entry-compiled", 14),
        ("*aapcs64-entry-instance*", 15),
        ("#'aapcs64-entry-generic", 12),
        ("(let ((n 3)) (lambda (x) (+ n x)))", 13),
        ("#'1+", 11),
    ] {
        egcl_rt::rooted!(function = read_eval_all_env(source, &mut env).unwrap());
        for registers in [false, true] {
            ENTRY_COUNTS.with(|n| n.set((0, 0)));
            assert_eq!(
                invoke_entry(*function, &[EgclVal::from_fixnum(10)], &mut env, registers).unwrap(),
                EgclVal::from_fixnum(expected)
            );
            assert!(
                ENTRY_COUNTS.with(|n| n.get().0) > 0,
                "published entry was bypassed"
            );
            assert!(egcl_rt::native_transfer::current_segment().is_null());
        }
    }
    egcl_rt::rooted!(list = read_eval_all_env("#'list", &mut env).unwrap());
    for count in 0..=5 {
        let args: Vec<_> = (1..=count)
            .map(|n| EgclVal::from_fixnum(n as i64))
            .collect();
        egcl_rt::rooted!(result = invoke_entry(*list, &args, &mut env, count <= 3).unwrap());
        let mut tail = *result;
        for expected in 1..=count {
            let (head, next) = cp(tail);
            assert_eq!(head, EgclVal::from_fixnum(expected as i64));
            tail = next;
        }
        assert_eq!(tail, NIL);
    }
}

#[test]
#[ignore = "requires AArch64 with supported segment boundary"]
fn aapcs64_published_callable_preserves_error_reentry_and_multiple_values() {
    require_native_aarch64();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        function = read_eval_all_env(
            "(lambda () (let ((events nil))
          (let ((answer (multiple-value-list (catch 'aapcs64-exit
            (unwind-protect
              (funcall (lambda () (throw 'aapcs64-exit (values 7 8))))
              (setq events (cons :cleanup events)))))))
            (values answer events))))",
            &mut env
        )
        .unwrap()
    );
    ENTRY_COUNTS.with(|n| n.set((0, 0)));
    egcl_rt::rooted!(result = invoke(*function, &[], &mut env).unwrap());
    assert_eq!(crate::cli::format_val(*result), "(7 8)");
    assert_eq!(env.mv.len(), 2);
    assert_eq!(crate::cli::format_val(env.mv[1]), "(:CLEANUP)");
    assert!(
        ENTRY_COUNTS.with(|n| n.get().1) > 0,
        "callback did not enter a nested segment"
    );
    egcl_rt::rooted!(missing = read_eval_all_env("'aapcs64-missing-function", &mut env).unwrap());
    egcl_rt::rooted!(error = invoke(*missing, &[], &mut env).unwrap_err());
    assert!(matches!(&*error, EgclError::Signalled(details)
        if crate::cli::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")));
    assert!(egcl_rt::native_transfer::current_segment().is_null());
    assert!(NATIVE_ENV.with(|slot| slot.get()).is_null());
}

#[test]
#[ignore = "requires AArch64 with supported segment boundary"]
fn aapcs64_published_callable_reloads_relocated_roots() {
    require_native_aarch64();
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(prototype = read_eval_all_env("#'list", &mut env).unwrap());
    for registers in [false, true] {
        let (head, identity) = cp(*prototype);
        egcl_rt::rooted!(function = crate::cli::arena_cons(head, identity));
        egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL));
        let before = (function.to_raw(), argument.to_raw());
        COLLECT_NEXT.with(|flag| flag.set(true));
        egcl_rt::rooted!(
            result = invoke_entry(*function, &[*argument], &mut env, registers).unwrap()
        );
        assert_ne!(function.to_raw(), before.0, "callable did not relocate");
        assert_ne!(argument.to_raw(), before.1, "argument did not relocate");
        assert_eq!(cp(*result), (*argument, NIL));
    }
}
