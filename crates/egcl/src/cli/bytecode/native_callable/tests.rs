// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::cli::{heap_test_lock, read_eval_all_env};

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn universal_outer_entry_executes_published_adapter_for_all_callable_kinds() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    read_eval_all_env(
        "(defun outer-entry-interpreted (x) (+ x 1))
         (defun outer-entry-compiled (x) (+ x 2))
         (compile 'outer-entry-compiled)
         (defmethod outer-entry-generic ((x t)) (+ x 3))
         (defclass outer-entry-instance () () (:metaclass egcl-ext:funcallable-standard-class))
         (setq *outer-entry-instance* (make-instance 'outer-entry-instance))
         (egcl-ext:set-funcallable-instance-function *outer-entry-instance* (lambda (x) (+ x 4)))",
        &mut env,
    )
    .unwrap();
    for (source, expected) in [
        ("#'outer-entry-interpreted", 11),
        ("#'outer-entry-compiled", 12),
        ("#'outer-entry-generic", 13),
        ("*outer-entry-instance*", 14),
        ("(let ((n 5)) (lambda (x) (+ x n)))", 15),
        ("#'1+", 11),
    ] {
        egcl_rt::rooted!(function = read_eval_all_env(source, &mut env).unwrap());
        ENTRY_COUNTS.with(|counts| counts.set((0, 0)));
        assert_eq!(
            apply_function(*function, &[EgclVal::from_fixnum(10)], &mut env).unwrap(),
            EgclVal::from_fixnum(expected)
        );
        assert!(
            ENTRY_COUNTS.with(|counts| counts.get().0) > 0,
            "{source} bypassed its published entry"
        );
        assert!(egcl_rt::native_transfer::current_segment().is_null());
    }
    egcl_rt::rooted!(list = read_eval_all_env("#'list", &mut env).unwrap());
    ENTRY_COUNTS.with(|counts| counts.set((0, 0)));
    egcl_rt::rooted!(wide = vec![EgclVal::from_fixnum(1); 5]);
    egcl_rt::rooted!(result = apply_function(*list, &wide, &mut env).unwrap());
    assert_eq!(crate::cli::format_val(*result), "(1 1 1 1 1)");
    assert!(ENTRY_COUNTS.with(|counts| counts.get().0) > 0);
    egcl_rt::rooted!(missing = read_eval_all_env("'outer-entry-undefined", &mut env).unwrap());
    ENTRY_COUNTS.with(|counts| counts.set((0, 0)));
    egcl_rt::rooted!(error = apply_function(*missing, &[], &mut env).unwrap_err());
    assert!(
        matches!(
            &*error,
            EgclError::Signalled(details)
                if crate::cli::condition_matches_handler(&env, details.condition, "UNDEFINED-FUNCTION")
        ),
        "undefined callable must signal its condition before leaving: {:?}",
        &*error
    );
    assert!(
        ENTRY_COUNTS.with(|counts| counts.get().0) > 0,
        "undefined callable bypassed its published entry"
    );
    assert!(egcl_rt::native_transfer::current_segment().is_null());
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn universal_outer_entry_reloads_moving_callable_and_arguments() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(prototype = read_eval_all_env("#'list", &mut env).unwrap());
    let (head, identity) = cp(*prototype);
    egcl_rt::rooted!(function = crate::cli::arena_cons(head, identity));
    egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(42), NIL));
    let before = argument.to_raw();
    collect_next_entry();
    egcl_rt::rooted!(result = apply_function(*function, &[*argument], &mut env).unwrap());
    assert!(
        take_relocation(),
        "callable must actually relocate inside the published entry"
    );
    assert_ne!(argument.to_raw(), before, "argument must actually relocate");
    assert_eq!(cp(*result), (*argument, NIL));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn universal_outer_entry_preserves_live_restart_callbacks_and_multiple_values() {
    let _lock = heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        function = read_eval_all_env(
            "(lambda ()
           (let ((events nil))
             (let ((result
                     (multiple-value-list
                       (catch 'outer-entry-exit
                         (unwind-protect
                           (handler-bind ((simple-error
                                            (lambda (condition)
                                              (declare (ignore condition))
                                              (setq events (cons :handler events))
                                              (invoke-restart 'continue))))
                             (cerror \"Continue\" \"outer entry\")
                             (setq events (cons :continued events))
                             (funcall (lambda () (throw 'outer-entry-exit (values 7 8)))))
                           (setq events (cons :cleanup events)))))))
               (values result (reverse events)))))",
            &mut env
        )
        .unwrap()
    );
    ENTRY_COUNTS.with(|counts| counts.set((0, 0)));
    egcl_rt::rooted!(result = apply_function(*function, &[], &mut env).unwrap());
    assert_eq!(crate::cli::format_val(*result), "(7 8)");
    assert_eq!(env.mv.len(), 2);
    assert_eq!(
        crate::cli::format_val(env.mv[1]),
        "(:HANDLER :CONTINUED :CLEANUP)"
    );
    assert!(
        ENTRY_COUNTS.with(|counts| counts.get().1) > 0,
        "callbacks must create nested native segments"
    );
    assert!(egcl_rt::native_transfer::current_segment().is_null());
}
