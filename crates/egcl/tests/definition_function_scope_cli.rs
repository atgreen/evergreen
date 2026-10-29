// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    process::{Command, Output},
};

const DEFINITIONS: &str = r#"
(in-package :cl-user)
(defun scope-local () :global)
(flet ((scope-local () 1))
  (defun scope-function () #'scope-local)
  (defun scope-call () (scope-local))
  (defun scope-default (&optional (function #'scope-local)) (funcall function))
  (setf (symbol-function 'scope-installed) (lambda () (scope-local))))
(flet ((scope-local () (scope-local)))
  (defun scope-nonrecursive-parent () (scope-local)))
(defun scope-definition-maker (value)
  (flet ((read-value () value)) (defun scope-made () (read-value))))
(scope-definition-maker 31)
(eval '(flet ((eval-local () 23)) (defun scope-eval-made () (eval-local))))
(let ((cell nil))
  (flet ((read-cell () cell) (write-cell (value) (setq cell value)))
    (defstruct function-scope-struct
      (initialized (progn (setq cell :initial) nil))
      (reader #'read-cell) (writer #'write-cell))))
(let ((base 7))
  (labels ((walk-even (n) (if (zerop n) base (walk-odd (1- n))))
           (walk-odd (n) (if (zerop n) base (walk-even (1- n)))))
    (defun scope-recursive (n) (walk-even n))
    (defun scope-recursive-function () #'walk-odd)
    (defun scope-recursive-set (value) (setq base value))))
(let ((scope-special 1))
  (locally (declare (special scope-special))
    (defun scope-defining-special (&optional (x scope-special)) x)))
(defun scope-ordinary (x) (+ x 1))
(defun scope-known () 17)
(defvar *scope-lookup-effects* 0)
(defun scope-function-lookup (known)
  (if known #'scope-known
      (let ((function #'scope-undefined))
        (incf *scope-lookup-effects*) function)))
"#;

const CHECKS: &str = r#"
(assert (functionp (scope-function)))
(assert (eq (scope-function) (scope-function)))
(assert (= 1 (funcall (scope-function))))
(assert (= 1 (scope-call)))
(assert (= 1 (scope-default)))
(assert (= 1 (scope-installed)))
(assert (eq :global (scope-local)))
(assert (eq :global (scope-nonrecursive-parent)))
(assert (= 31 (scope-made)))
(assert (= 23 (scope-eval-made)))
(assert (= 7 (scope-recursive 11)))
(assert (= 7 (funcall (scope-recursive-function) 12)))
(assert (= 8 (scope-recursive-set 8)))
(assert (= 8 (scope-recursive 11)))
(assert (= 8 (funcall (scope-recursive-function) 12)))
(assert (= 7 (scope-recursive-set 7)))
(let* ((first (make-function-scope-struct))
       (reader (function-scope-struct-reader first))
       (writer (function-scope-struct-writer first)))
  (assert (functionp reader)) (assert (functionp writer))
  (assert (eq :initial (funcall reader)))
  (assert (eq :updated (funcall writer :updated)))
  (assert (eq :updated (funcall reader)))
  (let ((second (make-function-scope-struct)))
    (assert (eq reader (function-scope-struct-reader second)))
    (assert (eq writer (function-scope-struct-writer second)))
    (assert (eq :initial (funcall reader)))))
(assert (= 2 (eval '(let ((scope-special 2)) (declare (special scope-special)) (scope-defining-special)))))
(assert (= 3 (progv '(scope-special) '(3) (scope-defining-special))))
"#;

fn run(program: &str, mode: &str, stress: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_LAZY_COMPILE",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(name);
    }
    if mode != "default" {
        command.env("EGCL_FORCE_TIER", mode);
    }
    command.env("EGCL_T2_COMPILE_THREADS", "1");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    command.output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn global_definitions_retain_lexical_function_context_through_escape_and_tiering() {
    let program = format!(
        "{DEFINITIONS}\n(dotimes (pass 80) {CHECKS} (assert (= 18 (scope-ordinary 17))) (assert (functionp (scope-function-lookup t))))\n\
        (when (equal (egcl-ext:getenv \"EGCL_FORCE_TIER\") \"t2\")\n\
          (loop repeat 400 until (and (= 2 (egcl-ext:function-tier 'scope-ordinary)) (= 2 (egcl-ext:function-tier 'scope-function-lookup)))\n\
            do (scope-ordinary 17) (scope-function-lookup t) (sleep 0.002)))\n\
        (egcl-ext:gc :full t)\n{CHECKS}\n\
        (format *error-output* \"SCOPE-LOOKUP-TIER ~D~%\" (egcl-ext:function-tier 'scope-function-lookup))\n\
        (assert (handler-case (progn (scope-function-lookup nil) (incf *scope-lookup-effects*) nil) (undefined-function (c) (eq (cell-error-name c) 'scope-undefined))))\n\
        (assert (= *scope-lookup-effects* 0))\n\
        (assert (handler-case (progn (eval '(function scope-never-defined)) nil) (undefined-function (c) (eq (cell-error-name c) 'scope-never-defined))))\n\
        (assert (functionp #'car))\n\
        (format *error-output* \"SCOPE-ORDINARY-TIER ~D~%\" (egcl-ext:function-tier 'scope-ordinary))\n\
        (format t \"DEFINITION-FUNCTION-SCOPE-OK~%\")"
    );
    let baseline = run(&program, "interp", false);
    success(&baseline);
    for (mode, stress) in [
        ("default", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let output = run(&program, mode, stress);
        success(&output);
        assert_eq!(
            output.stdout, baseline.stdout,
            "mode={mode} stress={stress}"
        );
        if mode == "t2" {
            assert!(String::from_utf8_lossy(&output.stderr).contains("SCOPE-ORDINARY-TIER 2"));
            assert!(String::from_utf8_lossy(&output.stderr).contains("SCOPE-LOOKUP-TIER 2"));
        }
    }
}

#[test]
fn saved_global_definitions_retain_function_namespaces_and_special_scope() {
    let dir = std::env::temp_dir().join(format!("egcl-definition-scope-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let core = dir.join("scope.core");
    let built = run(
        &format!("{DEFINITIONS}\n(egcl-ext:save-lisp-and-die {core:?})"),
        "interp",
        false,
    );
    success(&built);
    let restored = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "interp")
        .args(["--no-init", "--image"])
        .arg(&core)
        .args([
            "--eval",
            &format!("{CHECKS}\n(format t \"RESTORED-DEFINITION-SCOPE-OK~%\")"),
        ])
        .output()
        .unwrap();
    let _ = fs::remove_dir_all(dir);
    success(&restored);
    assert!(String::from_utf8_lossy(&restored.stdout).contains("RESTORED-DEFINITION-SCOPE-OK"));
}
