// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;

#[test]
fn parked_printer_errors_relocate_datums_conditions_and_backtraces() {
    const CHILD: &str = "EGCL_PRINTER_ERROR_RELOCATION_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "cli::printer_error_gc_tests::parked_printer_errors_relocate_datums_conditions_and_backtraces", "--nocapture"])
            .env(CHILD, "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_FORCE_TIER", "interp")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_AT")
            .output().unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let _lock = heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env_root = &mut env);
    egcl_rt::rooted!(class = ensure_condition_class_registered(&env, "SIMPLE-ERROR").unwrap());
    egcl_rt::collect_t0_minor().unwrap();
    let datum = arena_cons(EgclVal::from_fixnum(123), NIL);
    park_print_error(EgclError::TypeError {
        datum,
        expected: "STRING".into(),
    });
    park_print_error(EgclError::ProgramError(
        "later error must not replace the first".into(),
    ));
    // Unrelated evaluated builtins must not consume the parked printer error.
    assert!(
        evaluated_builtins::call("MAKE-STRING", &[EgclVal::from_fixnum(1)], &mut env)
            .unwrap()
            .is_ok()
    );
    egcl_rt::collect_t0_minor().unwrap();
    match resignal_print_error(Ok(NIL)).unwrap_err() {
        EgclError::TypeError {
            datum: relocated,
            expected,
        } => {
            assert_ne!(
                relocated, datum,
                "parked TypeError datum must physically move"
            );
            assert_eq!(cp(relocated), (EgclVal::from_fixnum(123), NIL));
            assert_eq!(expected, "STRING");
        }
        error => panic!("wrong error: {error:?}"),
    }
    let (before_condition, before_argument) = {
        egcl_rt::rooted!(condition = egcl_stdlib::allocate_instance(*class).unwrap());
        let argument = arena_cons(EgclVal::from_fixnum(456), NIL);
        park_print_error(EgclError::Signalled(Box::new(
            egcl_rt::error::SignalledError {
                condition: *condition,
                report: "parked condition".into(),
                backtrace: vec![egcl_rt::debug_stack::LogicalFrame {
                    function: Some("PRINTER-CALLBACK".into()),
                    origin: egcl_rt::debug_stack::FrameOrigin::Entry,
                    arguments: Some(vec![argument]),
                }],
            },
        )));
        (*condition, argument)
    };
    // Only the execution-local pending slot roots these objects now.
    egcl_rt::collect_t0_minor().unwrap();
    match resignal_print_error(Ok(NIL)).unwrap_err() {
        EgclError::Signalled(details) => {
            assert_ne!(
                details.condition, before_condition,
                "parked condition must physically move"
            );
            assert_eq!(egcl_stdlib::class_of(details.condition), *class);
            let argument = details.backtrace[0].arguments.as_ref().unwrap()[0];
            assert_ne!(
                argument, before_argument,
                "parked backtrace argument must physically move"
            );
            assert_eq!(cp(argument), (EgclVal::from_fixnum(456), NIL));
            assert_eq!(details.report, "parked condition");
        }
        error => panic!("wrong error: {error:?}"),
    }
    assert_eq!(resignal_print_error(Ok(T)).unwrap(), T);
}
