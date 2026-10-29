// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::{Command, Output};

const PROGRAM: &str = r#"
(in-package :cl-user)
(let ((scope-a 1))
  (defun scope-optional (&optional (x scope-a)) (declare (special scope-a)) x)
  (defun scope-key (&key (x scope-a)) (declare (special scope-a)) x)
  (defun scope-aux (&aux (x scope-a)) (declare (special scope-a)) x)
  (defun scope-body (&optional (x scope-a)) (declare (special scope-a)) (list x scope-a))
  (deftype scope-type (&optional (x scope-a)) (declare (special scope-a)) (list 'integer 0 x))
  (defmacro scope-macro (&optional (x scope-a)) (declare (special scope-a)) (list 'quote x)))
(defun scope-compiled-caller ()
  (let ((scope-a 2)) (declare (special scope-a))
    (list (scope-optional) (scope-key) (scope-aux) (scope-body))))
;; Both source-defined and EVAL-defined functions retain their definition's
;; lexical default while a free body declaration redirects only the body.
(eval '(let ((scope-a 1))
  (defun scope-eval-defined (&optional (x scope-a)) (declare (special scope-a)) x)))
(defvar *scope-effect* 0)
(let ((scope-a 1))
  (defun scope-error (&optional (x (progn (incf *scope-effect*) (error "default"))))
    (declare (special scope-a)) x))
;; A declaration CAPTURED from an enclosing definition scope still applies.
;; Such tree closures conservatively retain the interpreter's captured scope.
(let ((scope-a 1))
  (locally (declare (special scope-a))
    (defun scope-inherited (&optional (x scope-a)) x)))
(defparameter *scope-inherited-lambda*
  (let ((scope-a 1))
    (locally (declare (special scope-a))
      (lambda (&optional (x scope-a)) x))))
(defun scope-native (n) (declare (special scope-a)) (+ n scope-a))
(dotimes (pass 40)
  (assert (equal (scope-compiled-caller) '(1 1 1 (1 2))))
  (assert (equal
    (eval '(let ((scope-a 2)) (declare (special scope-a))
       (list (scope-optional) (scope-key) (scope-aux) (scope-body) (scope-eval-defined))))
    '(1 1 1 (1 2) 1)))
  (assert (equal
    (eval '(let ((scope-a 2)) (declare (special scope-a))
      (list (typep -1 'scope-type) (typep 0 'scope-type) (typep 1 'scope-type)
            (typep 2 'scope-type) (macroexpand-1 '(scope-macro)))))
    '(nil t t nil (quote 1))))
  (assert (= 2 (eval '(let ((scope-a 2)) (declare (special scope-a)) (scope-inherited)))))
  (assert (= 2 (eval '(let ((scope-a 2)) (declare (special scope-a)) (funcall *scope-inherited-lambda*)))))
  (assert (= 3 (progv '(scope-a) '(3) (scope-inherited))))
  (assert (= 3 (progv '(scope-a) '(3) (funcall *scope-inherited-lambda*))))
  (assert (= 9 (eval '(let ((scope-a 2)) (declare (special scope-a))
    (prog1 (scope-native 7) (assert (= scope-a 2))))))))
;; A non-local error must restore the caller's declaration extent, and each
;; omitted default runs exactly once. Explicit actuals skip the init form.
(assert (eq :caught (eval '(let ((scope-a 2)) (declare (special scope-a))
  (handler-case (scope-error) (error () (assert (= scope-a 2)) :caught))))))
(assert (= *scope-effect* 1))
(assert (= 19 (scope-error 19)))
(assert (= *scope-effect* 1))
;; SPECIAL PARAMETER bindings apply to subsequent initialization forms.
(defun scope-early (scope-p &optional (x (symbol-value 'scope-p)))
  (declare (special scope-p)) (list scope-p x))
(assert (equal (scope-early 7) '(7 7)))
(format *error-output* "SCOPE-OPTIONAL-TIER ~D~%" (egcl-ext:function-tier 'scope-optional))
(format *error-output* "SCOPE-NATIVE-TIER ~D~%" (egcl-ext:function-tier 'scope-native))
(format t "LAMBDA-INIT-DECLARATION-SCOPE-OK~%")
"#;

fn run(mode: &str, stress: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", PROGRAM]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_LAZY_COMPILE",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(key);
    }
    command.env("EGCL_T2_COMPILE_THREADS", "1");
    if mode != "default" {
        command.env("EGCL_FORCE_TIER", mode);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    command.output().expect("run declaration-scope regression")
}

#[test]
fn lambda_initforms_keep_definition_scope_through_eval_and_tiering() {
    let baseline = run("interp", false);
    assert!(
        baseline.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&baseline.stdout),
        String::from_utf8_lossy(&baseline.stderr)
    );
    assert!(String::from_utf8_lossy(&baseline.stdout).contains("LAMBDA-INIT-DECLARATION-SCOPE-OK"));
    for (mode, stress) in [
        ("default", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let output = run(mode, stress);
        assert!(
            output.status.success(),
            "mode={mode},stress={stress}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            output.stdout, baseline.stdout,
            "mode={mode},stress={stress}"
        );
        if mode == "t1" || mode == "t2" {
            let tier = if mode == "t1" { 1 } else { 2 };
            assert!(
                String::from_utf8_lossy(&output.stderr)
                    .contains(&format!("SCOPE-NATIVE-TIER {tier}")),
                "actual installed tier missing for {mode}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
