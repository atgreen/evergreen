// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A DEFINE-CONDITION `:report` is honoured wherever a condition is rendered.
//!
//! The report machinery existed but had no caller, so every `:report` form printed
//! as the bare class name and any library reporting errors idiomatically was
//! undiagnosable (bliss-e5eh6, bliss-l9tt; lib/egcl-jvm was the motivating case).
use std::process::Command;

#[test]
fn handled_errors_do_not_render_reports() {
    let setup = "(defvar *report-calls* 0)
        (define-condition quiet-error (error) ()
          (:report (lambda (c s) (declare (ignore c))
                     (incf *report-calls*) (write-string \"unexpected report\" s))))";
    for form in [
        "(list (handler-case (error 'quiet-error) (error () :caught)) *report-calls*)",
        "(list (handler-case (error 'type-error :datum '#1=(1 . #1#) :expected-type 'integer)
                 (type-error () :caught)) *report-calls*)",
        "(list (handler-case (error \"~S\" '#1=(1 . #1#)) (error () :caught)) *report-calls*)",
    ] {
        for interpreted in [false, true] {
            let expression = if interpreted {
                format!("(eval '{form})")
            } else {
                form.into()
            };
            let output = Command::new("timeout")
                .args([
                    "--kill-after=5",
                    "20",
                    env!("CARGO_BIN_EXE_egcl"),
                    "--no-init",
                    "--eval",
                    setup,
                    "--eval",
                    &expression,
                ])
                .output()
                .expect("run handled-error probe");
            assert!(
                output.status.success(),
                "handled error failed or hung: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .trim()
                    .lines()
                    .last(),
                Some("(:CAUGHT 0)"),
                "a handler transfer must precede reporting (interpreted={interpreted})"
            );
        }
    }
}

/// Evaluate `program` and assert `marker` appears in stdout+stderr combined.
///
/// Both streams, because the uncaught-error path reports on stderr and exits
/// non-zero, which is exactly the case bliss-e5eh6 names in its title.
/// `forms` are passed as SEPARATE `--eval` arguments, so each is read after the
/// previous one has run. One combined `(progn ...)` would be read in full before
/// any `in-package` took effect, interning a would-be REP-PKG symbol into CL-USER
/// and silently testing the wrong thing.
fn reports(forms: &[&str], marker: &str, stress: bool) {
    let mut command = Command::new("timeout");
    command
        .args([
            "--kill-after=5",
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
        ])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON");
    for form in forms {
        command.args(["--eval", form]);
    }
    if stress {
        command.env("EGCL_GC_STRESS", "1").env("EGCL_GC_POISON", "1");
    }
    let output = command.output().expect("run bounded EGCL");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        text.contains(marker),
        "expected {marker:?}{}\ngot: {text}",
        if stress { " (under GC stress)" } else { "" }
    );
}

/// All three `:report` designators ANSI allows: a literal string, a function name,
/// and a lambda. Every one of them printed the bare class name before.
#[test]
fn every_report_designator_form_is_honoured() {
    for (definition, signal, marker) in [
        (
            "(define-condition r-str (error) ((m :initarg :m)) (:report \"literal string report\"))",
            "(error 'r-str :m 1)",
            "literal string report",
        ),
        (
            "(defun r-fn (c s) (declare (ignore c)) (write-string \"named function ran\" s)) \
             (define-condition r-named (error) ((m :initarg :m)) (:report r-fn))",
            "(error 'r-named :m 1)",
            "named function ran",
        ),
        (
            "(define-condition r-lam (error) ((m :initarg :m :reader r-m)) \
             (:report (lambda (c s) (format s \"lambda ran m=~a\" (r-m c)))))",
            "(error 'r-lam :m 42)",
            "lambda ran m=42",
        ),
    ] {
        // Caught: the report is what ~A renders.
        reports(
            &[
                definition,
                &format!("(handler-case {signal} (error (e) (format t \"~a\" e)))"),
            ],
            marker,
            false,
        );
        // Uncaught: the report is the terminal message, not the type name.
        reports(&[definition, signal], marker, false);
    }
}

/// A condition defined in another package. The registry is keyed by
/// `condition_type_key`, so the report lookup must walk QUALIFIED names: a bare-name
/// walk found nothing and left every report outside CL/CL-USER unreachable.
#[test]
fn a_report_in_another_package_is_found() {
    reports(
        &[
            "(defpackage :rep-pkg (:use :cl) (:export :oops :oops-m))",
            "(in-package :rep-pkg)",
            "(define-condition oops (error) ((m :initarg :m :reader oops-m)) \
               (:report (lambda (c s) (format s \"qualified report ~a\" (oops-m c)))))",
            "(in-package :cl-user)",
            "(handler-case (error 'rep-pkg:oops :m 7) (error (e) (format t \"~a\" e)))",
        ],
        "qualified report 7",
        false,
    );
}

/// bliss-l9tt: a subclass of TYPE-ERROR must use its OWN report, not the built-in
/// "The value X is not of type Y." shape that the slot-derived reporter supplies.
#[test]
fn a_subclass_report_outranks_the_builtin_shape() {
    reports(
        &[
            "(define-condition my-te (type-error) () \
               (:report (lambda (c s) (format s \"custom te datum=~s\" (type-error-datum c)))))",
            "(handler-case (error 'my-te :datum 7 :expected-type 'string) \
               (error (e) (format t \"~a\" e)))",
        ],
        "custom te datum=7",
        false,
    );
}

/// The conditions that carry no `:report` must keep the messages they had: the
/// slot-derived shapes are the fallback, not something the report path replaced.
#[test]
fn conditions_without_a_report_are_unchanged() {
    reports(
        &["(handler-case (error 'type-error :datum 7 :expected-type 'string) \
             (error (e) (format t \"~a\" e)))"],
        "The value 7 is not of type STRING.",
        false,
    );
    reports(
        &["(handler-case (error \"simple ~a\" :control) (error (e) (format t \"~a\" e)))"],
        "simple CONTROL",
        false,
    );
}

/// Rendering a report allocates a string stream and may call arbitrary Lisp.
/// Render the caught condition inside its handler under GC stress, checking that
/// the condition and its slot values survive those allocations.
#[test]
fn rendering_a_report_does_not_orphan_the_condition() {
    let program: &[&str] = &[
        "(define-condition gc-r (error) ((m :initarg :m :reader gc-m)) \
           (:report (lambda (c s) (format s \"stress report ~a\" (gc-m c)))))",
        "(handler-case (error 'gc-r :m 11) (error (e) (format t \"~a\" e)))",
    ];
    reports(program, "stress report 11", false);
    reports(program, "stress report 11", true);
}
