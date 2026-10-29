// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, path::PathBuf, process::Command};

const SCOPES: &str = r#"
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defvar *scope-state* :before)
  (defvar *mode-events* nil))
(symbol-macrolet ((payload :symbol-macro))
  (macrolet ((alias (name) `(snapshot ,name))
             (local-value () :local-macro)
             (snapshot (name)
               (let ((value *scope-state*))
                 `(progn
                    (eval-when (:compile-toplevel) (setq *scope-state* :after))
                    (defun ,name () (list payload ',value (local-value)))))))
    (locally (declare (optimize (debug 2))) (alias scope-value))))
(symbol-macrolet ((default-value 42))
  (macrolet ((initial () 'default-value)
             (report-text () "local-report"))
    (define-condition scoped-condition (condition)
      ((value :initform (initial) :initarg :value :reader scoped-condition-value))
      (:default-initargs :value default-value)
      (:report (lambda (condition stream)
                 (declare (ignore condition))
                 (write-string (report-text) stream))))))
(eval-when (:compile-toplevel :load-toplevel)
  (push :both *mode-events*)
  (eval-when (:load-toplevel :execute) (push :inherit-ctt *mode-events*))
  (eval-when (:load-toplevel) (push :load-only-nct *mode-events*))
  (eval-when (:compile-toplevel) (push :inner-evaluate *mode-events*))
  (eval-when (:execute) (push :execute-ctt *mode-events*))
  (eval-when (:compile-toplevel)
    (eval-when (:compile-toplevel) (error "nested CT-only must not execute"))))
(eval-when (:execute) (error "top-level execute-only must be discarded"))
(defun verify-scopes ()
  (assert (equal (scope-value) '(:symbol-macro :before :local-macro)))
  (assert (eq *scope-state* :before))
  (assert (equal *mode-events* '(:load-only-nct :inherit-ctt :both)))
  (assert (= (scoped-condition-value (make-condition 'scoped-condition)) 42))
  (assert (string= (princ-to-string (make-condition 'scoped-condition)) "local-report"))
  (assert (null (macro-function 'alias)))
  (multiple-value-bind (expansion expanded-p) (macroexpand 'payload)
    (declare (ignore expansion)) (assert (not expanded-p)))
  (write-line "COMPILE-SCOPES-OK"))
"#;

fn directory(label: &str) -> PathBuf {
    let directory =
        std::env::temp_dir().join(format!("egcl-toplevel-{label}-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    directory
}

fn run(program: &str, stress: bool, marker: &str) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    let output = command
        .output()
        .expect("run top-level processing regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

fn portable_unit(artifact: &std::path::Path) {
    let bytes = fs::read(artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        assert!(
            bytes.windows(4).any(|window| window == b"BBU\0"),
            "expected portable bytecode, not a source-only fallback"
        );
    }
}

#[test]
fn computed_expansions_keep_lexical_scopes_and_eval_when_modes() {
    let directory = directory("scopes");
    let source = directory.join("scopes.lisp");
    let artifact = directory.join("scopes.bfasl");
    fs::write(&source, SCOPES).unwrap();
    run(
        &format!(
            "(progn
          (multiple-value-bind (path warnings failure)
              (compile-file {source:?} :output-file {artifact:?})
            (declare (ignore warnings)) (assert path) (assert (not failure)))
          (assert (eq *scope-state* :after))
          (assert (equal *mode-events* '(:execute-ctt :inner-evaluate :inherit-ctt :both)))
          (assert (null (macro-function 'alias)))
          (write-line \"COMPILE-SCOPES-COMPILED\"))"
        ),
        true,
        "COMPILE-SCOPES-COMPILED",
    );
    portable_unit(&artifact);
    fs::remove_file(source).unwrap();
    run(
        &format!("(progn (load {artifact:?}) (verify-scopes))"),
        true,
        "COMPILE-SCOPES-OK",
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn top_level_effects_preserve_dynamic_handlers_and_recursive_compile() {
    let directory = directory("recursive");
    let source = directory.join("outer.lisp");
    let artifact = directory.join("outer.bfasl");
    let nested_source = directory.join("inner.lisp");
    let nested_artifact = directory.join("inner.bfasl");
    fs::write(
        &source,
        r#"
      (eval-when (:compile-toplevel :load-toplevel :execute) (defvar *compile-hook* nil))
      (macrolet ((with-effect ()
                   '(progn (eval-when (:compile-toplevel) (funcall *compile-hook*))
                           (defun outer-retained () 42))))
        (with-effect))"#,
    )
    .unwrap();
    fs::write(
        &nested_source,
        r#"
      (eval-when (:compile-toplevel) (assert (eq *dynamic-marker* :inside)))
      (defun inner-retained () 7)"#,
    )
    .unwrap();
    run(
        &format!(
            "(progn
      (define-condition compiler-order-note (condition) ())
      (defvar *dynamic-marker* :outside) (defvar *compile-hook* nil)
      (defvar *handler-seen* 0) (defvar *hook-calls* 0)
      (let ((*dynamic-marker* :inside)
            (*compile-hook* (lambda ()
                (incf *hook-calls*) (signal 'compiler-order-note)
                (multiple-value-bind (path warnings failure)
                    (compile-file {nested_source:?} :output-file {nested_artifact:?})
                  (declare (ignore warnings)) (assert path) (assert (not failure))))))
        (handler-bind ((compiler-order-note (lambda (condition)
                         (declare (ignore condition)) (incf *handler-seen*))))
          (multiple-value-bind (path warnings failure)
              (compile-file {source:?} :output-file {artifact:?})
            (declare (ignore warnings)) (assert path) (assert (not failure)))))
      (assert (= *hook-calls* 1)) (assert (= *handler-seen* 1))
      (assert (eq *dynamic-marker* :outside)) (assert (null *compile-hook*))
      (write-line \"RECURSIVE-COMPILE-OK\"))"
        ),
        false,
        "RECURSIVE-COMPILE-OK",
    );
    portable_unit(&artifact);
    portable_unit(&nested_artifact);
    fs::remove_file(source).unwrap();
    fs::remove_file(nested_source).unwrap();
    run(
        &format!(
            "(progn (load {artifact:?}) (load {nested_artifact:?})
           (assert (= (outer-retained) 42)) (assert (= (inner-retained) 7))
           (write-line \"RECURSIVE-LOAD-OK\"))"
        ),
        true,
        "RECURSIVE-LOAD-OK",
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn declared_package_scopes_preserve_residual_package_actions() {
    let directory = directory("package");
    let source = directory.join("package.lisp");
    let artifact = directory.join("package.bfasl");
    fs::write(
        &source,
        r#"
      (defpackage :order-cm-target (:use :cl) (:export :target))
      (defpackage :order-cm-definition (:use :cl))
      (locally (declare (optimize (debug 2)))
        (in-package :order-cm-definition)
        (defun order-cm-target:target () :ordinary)
        (define-compiler-macro order-cm-target:target ()
          (list 'quote (package-name *package*))))
      (in-package :cl-user)"#,
    )
    .unwrap();
    run(
        &format!(
            "(progn (multiple-value-bind (path warnings failure)
           (compile-file {source:?} :output-file {artifact:?})
           (declare (ignore warnings)) (assert path) (assert (not failure)))
           (assert (string= (package-name *package*) \"COMMON-LISP-USER\"))
           (write-line \"PACKAGE-COMPILE-OK\"))"
        ),
        false,
        "PACKAGE-COMPILE-OK",
    );
    portable_unit(&artifact);
    fs::remove_file(source).unwrap();
    run(
        &format!(
            "(progn
           (let ((printed (with-output-to-string (stream)
                            (let ((*standard-output* stream)) (load {artifact:?} :print t)))))
             (assert (search \"TARGET\" printed)))
           (assert (compiler-macro-function (find-symbol \"TARGET\" \"ORDER-CM-TARGET\")))
           #+egcl
           (assert (string= (eval (read-from-string \"(order-cm-target:target)\"))
                            \"ORDER-CM-DEFINITION\"))
           (assert (string= (package-name *package*) \"COMMON-LISP-USER\"))
           (write-line \"PACKAGE-LOAD-OK\"))"
        ),
        true,
        "PACKAGE-LOAD-OK",
    );
    fs::remove_dir_all(directory).unwrap();
}
