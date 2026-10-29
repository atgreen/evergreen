// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::PathBuf, process::Command};

const DEFINITIONS: &str = r#"(defpackage :attribute-definition (:use :cl) (:export :block-delete-p))
(defpackage :attribute-use (:use :cl))
(in-package :attribute-definition)
(eval-when (:compile-toplevel :load-toplevel :execute)
(defun symbolicate (&rest things)
 (intern (apply #'concatenate 'string (mapcar #'string things)) *package*)))
(defmacro block-attributep (attributes &rest names) `(list ,attributes ',names))
(defun block-flags (value) value)
(macrolet ((defattr (block-slot)
 `(defmacro ,block-slot (block)
   `(block-attributep (block-flags ,block)
     ,(symbolicate (subseq (string ',block-slot) 6))))))
 (defattr block-delete-p))
(defmacro do-uses ((value values) &body body)
 `(let ((uses ,values))
   (block nil
    (flet ((do-1-use (,value) ,@body))
     (if (listp uses)
      (dolist (node uses) (do-1-use node))
      (do-1-use uses))))))
(defun run-direct (value) (block-delete-p value))
(defun run-nested (values)
 (let ((result nil))
  (do-uses (value values) (push (block-delete-p value) result))
  (nreverse result)))
(defun run-fallback (items)
 (declare (special items))
 (let ((result nil))
  (dolist (value items) (push (block-delete-p value) result))
  (nreverse result)))
(defun run-macrolet (items)
 (declare (special items))
 (let ((result nil))
  (macrolet ((add-result (value) `(push ,value result)))
   (dolist (value items) (add-result (block-delete-p value))))
  (nreverse result)))
(defun run-symbolmacrolet (items)
 (declare (special items))
 (let ((result nil))
  (symbol-macrolet ((alias items))
   (dolist (value alias) (push (block-delete-p value) result)))
  (nreverse result)))

(defun run-shadow (items)
 (declare (special items))
 (let ((value (first items)))
  (macrolet ((marker () :local))
   (flet ((block-delete-p (item) (list item :shadowed)))
    (symbol-macrolet ((alias value))
     (list (block-delete-p alias) (marker) '(block-delete-p delete-p)
           (the (or integer null) value)))))))
"#;
const CHECKS: &str = r#"(in-package :attribute-definition)
(dolist (row (run-macrolet '(42 43)))
 (assert (eq (first (second row)) 'delete-p)))
(in-package :attribute-use)
(dolist (name '(attribute-definition::run-nested attribute-definition::run-fallback
                attribute-definition::run-macrolet attribute-definition::run-symbolmacrolet))
 (dolist (row (funcall name '(42 43)))
  (assert (eq (first (second row)) (find-symbol "DELETE-P" "ATTRIBUTE-DEFINITION")))))
(assert (equal (attribute-definition::run-shadow '(42))
 (list '(42 :shadowed) :local
       '(attribute-definition::block-delete-p attribute-definition::delete-p) 42)))
(let ((expansion (macroexpand-1 '(attribute-definition:block-delete-p 42))))
 (assert (eq (symbol-package (third expansion)) *package*)))
#+egcl
(dolist (name '(attribute-definition::run-macrolet attribute-definition::run-symbolmacrolet))
 (assert (egcl::%fn-body (symbol-function name))))
(in-package :attribute-definition)
(defmacro block-delete-p (value) `(list ,value '(late-redefinition)))
(in-package :attribute-use)
(dolist (name '(attribute-definition::run-macrolet attribute-definition::run-symbolmacrolet))
 (dolist (row (funcall name '(7 8)))
  (assert (eq (first (second row)) (find-symbol "DELETE-P" "ATTRIBUTE-DEFINITION")))))
"#;

fn scratch(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "egcl-fallback-{name}-{}-{serial}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn run(program: &str, stress: bool) -> String {
    let directory = scratch("runner");
    let script = directory.join("run.lisp");
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&script)
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("FALLBACK-CONTEXT-OK"), "{stdout}");
    stdout
}
fn require_bytecode(artifact: &std::path::Path) {
    let bytes = fs::read(artifact).unwrap();
    assert!(bytes.starts_with(b"BFASL\0"));
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut cursor = 24;
    let mut bbu = false;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 2..cursor + 6].try_into().unwrap()) as usize;
        cursor += 6;
        assert_ne!(
            kind, 11,
            "whole-source fallback loses compile-time expansion"
        );
        if kind == 12 {
            bbu = bytes[cursor..cursor + size].starts_with(b"BBU\0");
        }
        cursor += size;
    }
    assert!(bbu);
}
#[test]
fn retained_function_scopes_freeze_in_compile_package_before_fresh_load() {
    let directory = scratch("scopes");
    let source = directory.join("definitions.lisp");
    let artifact = directory.join("definitions.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            r#"(multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line "FALLBACK-CONTEXT-OK")"#
        ),
        false,
    );
    require_bytecode(&artifact);
    fs::remove_file(source).unwrap();
    let program = format!(r#"(load {artifact:?}) {CHECKS} (write-line "FALLBACK-CONTEXT-OK")"#);
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
#[test]
fn final_retained_function_macro_error_is_reported_and_deferred_to_call() {
    let directory = scratch("error");
    let source = directory.join("error.lisp");
    let artifact = directory.join("error.bfasl");
    fs::write(
        &source,
        r#"(defun failed-fallback (value)
      (declare (special value))
      (macrolet ((broken () (error "SCOPED-FREEZE-ERROR"))) (broken)))"#,
    )
    .unwrap();
    run(
        &format!(
            r#"(multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?})
        (assert file) (assert warnings) (assert failure))
        (write-line "FALLBACK-CONTEXT-OK")"#
        ),
        false,
    );
    fs::remove_file(source).unwrap();
    let program = format!(
        r#"(load {artifact:?})
      (let ((caught nil))
        (handler-case (failed-fallback 1)
          (program-error (condition)
            (setf caught t)
            (assert (search "SCOPED-FREEZE-ERROR" (princ-to-string condition)))))
        (assert caught))
      (write-line "FALLBACK-CONTEXT-OK")"#
    );
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
#[test]
fn unexternalizable_frozen_function_cannot_silently_retain_original_source() {
    let directory = scratch("literal");
    let source = directory.join("literal.lisp");
    let artifact = directory.join("literal.bfasl");
    fs::write(
        &source,
        r#"(defun unsupported-frozen-literal (value)
      (declare (special value))
      (macrolet ((payload ()
        (list 'quote (let ((state 0)) (lambda () (incf state))))))
        (payload)))"#,
    )
    .unwrap();
    run(
        &format!(
            r#"(let ((rejected nil))
      (handler-case
        (multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?})
          (declare (ignore file warnings)) (setf rejected failure))
        (error () (setf rejected t)))
      (assert rejected))
      (write-line "FALLBACK-CONTEXT-OK")"#
        ),
        false,
    );
    fs::remove_dir_all(directory).unwrap();
}
