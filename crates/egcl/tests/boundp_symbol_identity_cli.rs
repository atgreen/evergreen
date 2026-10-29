// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const SOURCE: &str = r#"
(defpackage :boundp-identity-probe
  (:use :cl) (:shadow :*read-default-float-format* :single-float))
(in-package :boundp-identity-probe)
(assert (not (boundp '*read-default-float-format*)))
(defvar *initializations* 0)
(defvar *not-a-symbol* 42)
(defvar *read-default-float-format* (progn (incf *initializations*) 'single-float))
(defvar *read-default-float-format* (error "DEFVAR initialized twice"))
(defun read-own-format () *read-default-float-format*)
(defun unbound-p (thunk)
  (handler-case (progn (funcall thunk) nil) (unbound-variable () t)))
(defun verify ()
  (assert (= *initializations* 1))
  (assert (boundp '*read-default-float-format*))
  (assert (eq (symbol-value '*read-default-float-format*) 'single-float))
  (assert (eq (read-own-format) 'single-float))
  (assert (not (eq 'single-float 'cl:single-float)))
  (let ((lexical-only 73))
    (assert (= lexical-only 73))
    (assert (not (boundp 'lexical-only))))
  (let ((local-special 7))
    (declare (special local-special))
    (assert (boundp 'local-special))
    (assert (= (symbol-value 'local-special) 7)))
  (assert (not (boundp 'local-special)))
  (let ((cl:*read-default-float-format* 'cl:double-float)
        (*read-default-float-format* 'single-float))
    (assert (boundp '*read-default-float-format*))
    (assert (eq (symbol-value '*read-default-float-format*) 'single-float))
    (assert (eq (read-own-format) 'single-float))
    (assert (typep (read-from-string "1.25") 'cl:double-float)))
  (progv '(*read-default-float-format*) nil
    (assert (not (boundp '*read-default-float-format*)))
    (assert (unbound-p (lambda () (symbol-value '*read-default-float-format*))))
    (assert (unbound-p #'read-own-format))
    (assert (unbound-p (lambda () (eval '*read-default-float-format*)))))
  (assert (eq (read-own-format) 'single-float))
  (makunbound '*read-default-float-format*)
  (assert (not (boundp '*read-default-float-format*)))
  (assert (unbound-p (lambda () (symbol-value '*read-default-float-format*))))
  (assert (unbound-p #'read-own-format))
  (assert (unbound-p (lambda () (eval '*read-default-float-format*))))
  (let ((*read-default-float-format* 'single-float))
    (assert (boundp '*read-default-float-format*))
    (assert (eq (symbol-value '*read-default-float-format*) 'single-float)))
  (assert (not (boundp '*read-default-float-format*)))
  (assert (unbound-p #'read-own-format))
  (setf (symbol-value '*read-default-float-format*) 'single-float)
  (let ((left (make-symbol "*READ-DEFAULT-FLOAT-FORMAT*"))
        (right (make-symbol "*READ-DEFAULT-FLOAT-FORMAT*")))
    (assert (not (boundp left)))
    (assert (not (boundp right)))
    (setf (symbol-value left) 7)
    (assert (boundp left))
    (assert (not (boundp right)))
    (progv (list left) '(9)
      (assert (boundp left))
      (assert (= (symbol-value left) 9))
      (assert (not (boundp right))))
    (assert (= (symbol-value left) 7))
    (makunbound left)
    (assert (not (boundp left))))
  (assert (boundp nil))
  (assert (boundp t))
  (assert (boundp :keyword))
  (assert (handler-case (progn (boundp *not-a-symbol*) nil) (type-error () t)))
  (dotimes (i 500) (assert (eq (read-own-format) 'single-float)))
  (write-line "BOUNDP-SYMBOL-IDENTITY-OK"))
"#;

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
    let output = command.output().expect("run BOUNDP identity regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn boundp_and_unbound_special_reads_keep_symbol_identity() {
    let source =
        std::env::temp_dir().join(format!("egcl-boundp-source-{}.lisp", std::process::id()));
    fs::write(&source, SOURCE).unwrap();
    let program = format!(
        "(progn (load {source:?}) (funcall (find-symbol \"VERIFY\" \"BOUNDP-IDENTITY-PROBE\")))"
    );
    run(&program, false, "BOUNDP-SYMBOL-IDENTITY-OK");
    run(&program, true, "BOUNDP-SYMBOL-IDENTITY-OK");
    fs::remove_file(source).unwrap();
}

#[test]
fn boundp_identity_survives_compile_file_and_source_free_load() {
    let directory =
        std::env::temp_dir().join(format!("egcl-boundp-compiled-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("bindings.lisp");
    let artifact = directory.join("bindings.bfasl");
    fs::write(&source, SOURCE).unwrap();
    run(
        &format!(
            "(multiple-value-bind (path warnings failure)
                (compile-file {source:?} :output-file {artifact:?})
                (declare (ignore warnings)) (assert path) (assert (not failure))
                (write-line \"BOUNDP-COMPILE-OK\"))"
        ),
        false,
        "BOUNDP-COMPILE-OK",
    );
    fs::remove_file(source).unwrap();
    run(&format!("(progn (load {artifact:?}) (funcall (find-symbol \"VERIFY\" \"BOUNDP-IDENTITY-PROBE\")))"), true, "BOUNDP-SYMBOL-IDENTITY-OK");
    fs::remove_dir_all(directory).unwrap();
}
