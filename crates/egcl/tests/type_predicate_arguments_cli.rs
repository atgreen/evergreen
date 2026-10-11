// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn eval(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("TYPE-ARGUMENTS-OK"), "{stdout}\n{stderr}");
}

const DEFINITIONS: &str = r#"
  (defvar *type-argument-order* nil)
  (defvar *type-argument-limit* 0)
  (deftype argument-range () (list 'integer 0 *type-argument-limit*))
  (defun remember-type-argument (number value)
    (push number *type-argument-order*)
    value)
  (defun type-argument-environment ()
    (push 3 *type-argument-order*)
    (setf *type-argument-limit* 10)
    (values nil :discarded))
  (defun direct-type-arguments ()
    (typep (remember-type-argument 1 7)
           (remember-type-argument 2 'argument-range)
           (type-argument-environment)))
  (defun direct-subtype-arguments ()
    (subtypep (remember-type-argument 1 (list 'integer 1 5))
              (remember-type-argument 2 'argument-range)
              (type-argument-environment)))
"#;

fn assert_order(call: &str, expected_values: &str) -> String {
    format!(
        r#"(setf *type-argument-order* nil *type-argument-limit* 0)
        (assert (equal (multiple-value-list {call}) '{expected_values}))
        (assert (equal *type-argument-order* '(3 2 1)))"#
    )
}

fn check_argument_order(operator: &str, first: &str, expected: &str) {
    let mut program = format!("(progn {DEFINITIONS}");
    let args = format!(
        "(remember-type-argument 1 {first}) (remember-type-argument 2 'argument-range) (type-argument-environment)"
    );
    for call in [
        format!("({operator} {args})"),
        format!("(eval '({operator} {args}))"),
        format!("(funcall #'{operator} {args})"),
        format!("(apply #'{operator} (list {args}))"),
    ] {
        program.push_str(&assert_order(&call, expected));
    }
    program.push_str("(format t \"TYPE-ARGUMENTS-OK~%\"))");
    eval(&program);
}

#[test]
fn typep_evaluates_all_arguments_before_expansion() {
    check_argument_order("typep", "7", "(t)");
}

#[test]
fn subtypep_evaluates_all_arguments_before_expansion() {
    check_argument_order("subtypep", "(list 'integer 1 5)", "(t t)");
}

#[test]
fn environment_argument_can_exit_before_type_expansion() {
    eval(
        r#"(progn
      (defvar *unexpected-type-expansion* 0)
      (deftype never-expanded () (incf *unexpected-type-expansion*) 'integer)
      (assert (eq :escaped (catch 'escape
        (eval '(typep 7 'never-expanded (throw 'escape :escaped))))))
      (assert (eq :escaped (catch 'escape
        (eval '(subtypep 'integer 'never-expanded (throw 'escape :escaped))))))
      (assert (= *unexpected-type-expansion* 0))
      (format t "TYPE-ARGUMENTS-OK~%"))"#,
    );
}

#[test]
fn typep_rejects_invalid_type_specifiers_through_function_calls() {
    eval(
        r#"(progn
      (dolist (spec '(values (values integer) (function (integer) integer)))
        (assert (handler-case (progn (funcall #'typep 7 spec) nil)
                  (error () t)))
        (assert (handler-case (progn (apply #'typep (list 7 spec nil)) nil)
                  (error () t))))
      (dolist (fn (list #'typep #'subtypep))
        (dolist (args (list nil (list 7) (list 7 'integer nil nil)))
          (assert (handler-case (progn (apply fn args) nil)
                    (program-error () t)))))
      (format t "TYPE-ARGUMENTS-OK~%"))"#,
    );
}

#[test]
fn type_argument_order_survives_compiled_file_loading() {
    let dir = std::env::temp_dir().join(format!("egcl-type-arguments-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let src = dir.join("arguments.lisp");
    let artifact = dir.join("arguments.bfasl");
    std::fs::write(&src, DEFINITIONS).unwrap();
    eval(&format!(
        "(progn (compile-file {src:?} :output-file {artifact:?}) (assert (probe-file {artifact:?})) (format t \"TYPE-ARGUMENTS-OK~%\"))"
    ));
    std::fs::remove_file(src).unwrap();
    let typep = assert_order("(direct-type-arguments)", "(t)");
    let subtypep = assert_order("(direct-subtype-arguments)", "(t t)");
    eval(&format!(
        "(progn (load {artifact:?}) (dotimes (i 15) {typep} {subtypep}) (format t \"TYPE-ARGUMENTS-OK~%\"))"
    ));
    std::fs::remove_dir_all(dir).unwrap();
}
