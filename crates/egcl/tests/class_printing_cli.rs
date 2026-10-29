// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A class prints its own name, not just its metaclass's (bliss-w53r3).
//!
//! Once a class became an object (bliss-vsjn1) it printed through the generic
//! instance path, which emits `#<class-of-the-object>` — so every standard
//! class in the image printed as the identical text `#<STANDARD-CLASS>`,
//! useless in a backtrace or a slot dump. SBCL, CCL and ECL all print
//! `#<STANDARD-CLASS FOO>`.
//!
//! **There are two printers**, and this test exists mostly to pin that. `~S`,
//! `~A`, `PRIN1-TO-STRING` and `PRINC-TO-STRING` go through
//! `instance_class_tag` in egcl-stdlib's format.rs; `WRITE`, `PRIN1`, `PRINC`,
//! `PRINT` and `WRITE-TO-STRING` go through `print_val_inner` in cli.rs.
//! Fixing one left the other printing the bare tag — measured, not supposed.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str) -> String {
    let output = Command::new("timeout")
        .args([
            "--kill-after=10",
            "180",
            BIN,
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

fn expect(text: &str, rows: &[&str]) {
    for row in rows {
        assert!(text.contains(row), "missing {row:?} in:\n{text}");
    }
}

/// Every printing entry point must agree. Both printers are covered here on
/// purpose; a fix to one is not a fix.
#[test]
fn every_printer_names_the_class() {
    let stdout = run(r#"
      (defclass uc () ())
      (let ((k (find-class 'uc)))
        (format t "FORMAT-S ~s~%" k)
        (format t "FORMAT-A ~a~%" k)
        (format t "PRIN1-TO-STRING ~a~%" (prin1-to-string k))
        (format t "PRINC-TO-STRING ~a~%" (princ-to-string k))
        (format t "WRITE-TO-STRING ~a~%" (write-to-string k))
        (write-string "WRITE ") (write k) (terpri)
        (write-string "PRIN1 ") (prin1 k) (terpri)
        (write-string "PRINC ") (princ k) (terpri)
        ;; and inside a structure, and through an error report
        (format t "IN-LIST ~s~%" (list k))
        (format t "IN-ERROR ~a~%"
                (handler-case (error "c: ~s" k) (error (e) (princ-to-string e)))))
      (format t "PRINTERS-OK~%")
    "#);
    for row in [
        "FORMAT-S #<STANDARD-CLASS UC>",
        "FORMAT-A #<STANDARD-CLASS UC>",
        "PRIN1-TO-STRING #<STANDARD-CLASS UC>",
        "PRINC-TO-STRING #<STANDARD-CLASS UC>",
        "WRITE-TO-STRING #<STANDARD-CLASS UC>",
        "WRITE #<STANDARD-CLASS UC>",
        "PRIN1 #<STANDARD-CLASS UC>",
        "PRINC #<STANDARD-CLASS UC>",
        "IN-LIST (#<STANDARD-CLASS UC>)",
        "IN-ERROR c: #<STANDARD-CLASS UC>",
        "PRINTERS-OK",
    ] {
        assert!(stdout.contains(row), "missing {row:?} in:\n{stdout}");
    }
}

/// Each kind of class names its own metaclass correctly, including a user
/// metaclass and the metaobject classes that are instances of STANDARD-CLASS.
#[test]
fn each_kind_of_class_prints_its_metaclass_and_name() {
    let stdout = run(r#"
      (defclass uc () ())
      (defstruct st x)
      (define-condition cc (error) ())
      (defclass meta (standard-class) ())
      (defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
      (defclass withmeta () () (:metaclass meta))
      (dolist (c '(uc st cc withmeta meta integer t hash-table
                   standard-class built-in-class structure-class standard-object))
        (format t "P ~s~%" (find-class c)))
      (format t "KINDS-OK~%")
    "#);
    expect(
        &stdout,
        &[
            "P #<STANDARD-CLASS UC>",
            "P #<STRUCTURE-CLASS ST>",
            "P #<STANDARD-CLASS CC>",
            // a user metaclass names itself in its instances' tags
            "P #<META WITHMETA>",
            "P #<STANDARD-CLASS META>",
            "P #<BUILT-IN-CLASS INTEGER>",
            "P #<BUILT-IN-CLASS T>",
            "P #<BUILT-IN-CLASS HASH-TABLE>",
            // the metaobject classes are instances of STANDARD-CLASS
            "P #<STANDARD-CLASS STANDARD-CLASS>",
            "P #<STANDARD-CLASS BUILT-IN-CLASS>",
            "P #<STANDARD-CLASS STRUCTURE-CLASS>",
            "P #<STANDARD-CLASS STANDARD-OBJECT>",
            "KINDS-OK",
        ],
    );
}

/// The printer is shared with structures, ordinary instances, conditions and
/// restarts. None of them may gain a second word.
#[test]
fn nothing_else_that_shares_the_printer_changes() {
    let stdout = run(r#"
      (defstruct pt x y)
      (defclass plain () ((a :initform 1)))
      (define-condition my-err (error) ((m :initform 7)))
      (format t "STRUCT ~s~%" (make-pt :x 1 :y 2))
      (format t "INSTANCE ~s~%" (make-instance 'plain))
      (format t "CONDITION ~s~%" (make-condition 'my-err))
      (write-string "STRUCT-WRITE ") (write (make-pt :x 1 :y 2)) (terpri)
      (write-string "INSTANCE-WRITE ") (write (make-instance 'plain)) (terpri)
      (format t "SHARED-OK~%")
    "#);
    expect(
        &stdout,
        &[
            "STRUCT #S(PT :X 1 :Y 2)",
            "INSTANCE #<PLAIN>",
            "CONDITION #<MY-ERR>",
            "STRUCT-WRITE #S(PT :X 1 :Y 2)",
            "INSTANCE-WRITE #<PLAIN>",
            "SHARED-OK",
        ],
    );
}
