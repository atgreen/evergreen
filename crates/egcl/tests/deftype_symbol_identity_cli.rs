// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const SOURCE: &str = r#"
(defpackage :alias-identity-a (:use :cl)
  (:shadow :simple-vector) (:export :simple-vector :token :width :thread))
(defpackage :alias-identity-b (:use :cl) (:export :token :width :thread))
(defpackage :alias-identity-import (:use :cl)
  (:import-from :alias-identity-a :token :width) (:export :token :width))
(in-package :alias-identity-a)
(defun blocked-vector-p (value)
  (and (plusp (length value)) (eq (aref value 0) :blocked)))
(deftype simple-vector ()
  (let ((scratch (make-list 24)))
    (declare (ignore scratch))
    (list 'and 'cl:simple-vector '(not (satisfies blocked-vector-p)))))
(deftype token () '(integer 0 9))
(deftype thread () 'integer)
(deftype width (&optional (bits 8))
  (let ((scratch (make-list 24 :initial-element bits)))
    (declare (ignore scratch))
    (list 'unsigned-byte bits)))
(in-package :alias-identity-b)
(deftype token () '(integer 100 199))
(deftype width (&optional (bound 16)) (list 'integer (- bound) bound))
(defclass thread () ())
(in-package :alias-identity-import)
;; Defining through the imported symbol updates A's type, retaining its home.
(deftype token () '(integer 0 3))
(in-package :cl-user)
(defun verify-alias-identities ()
  (assert (typep (vector :blocked) 'cl:simple-vector))
  (assert (typep (vector :ok) 'alias-identity-a:simple-vector))
  (assert (not (typep (vector :blocked) 'alias-identity-a:simple-vector)))
  (assert (eq 'alias-identity-a:token 'alias-identity-import:token))
  (assert (eq 'alias-identity-a:width 'alias-identity-import:width))
  (assert (not (eq 'alias-identity-a:token 'alias-identity-b:token)))
  (assert (string= (package-name (symbol-package 'alias-identity-import:token)) "ALIAS-IDENTITY-A"))
  (assert (typep 3 'alias-identity-a:token))
  (assert (typep 3 'alias-identity-import:token))
  (assert (not (typep 7 'alias-identity-a:token)))
  (assert (not (typep 150 'alias-identity-a:token)))
  (assert (typep 150 'alias-identity-b:token))
  (assert (not (typep 3 'alias-identity-b:token)))
  (assert (typep 255 'alias-identity-a:width))
  (assert (not (typep 256 'alias-identity-a:width)))
  (assert (typep 7 '(alias-identity-import:width 3)))
  (assert (not (typep 8 '(alias-identity-a:width 3))))
  (assert (typep -16 'alias-identity-b:width))
  (assert (not (typep 17 'alias-identity-b:width)))
  (assert (typep -3 '(alias-identity-b:width 3)))
  (assert (not (typep -4 '(alias-identity-b:width 3))))
  (let ((instance (make-instance 'alias-identity-b:thread)))
    (assert (typep instance 'alias-identity-b:thread))
    (assert (not (typep instance 'alias-identity-a:thread)))
    (assert (typep 42 'alias-identity-a:thread))
    (assert (not (typep 42 'alias-identity-b:thread))))
  #+egcl
  (let ((condition-variable (egcl-thread:make-condition-variable))
        (name (find-symbol "CONDITION-VARIABLE" "EGCL-THREAD")))
    (import name "ALIAS-IDENTITY-IMPORT")
    (assert (eq name (find-symbol "CONDITION-VARIABLE" "ALIAS-IDENTITY-IMPORT")))
    (assert (typep condition-variable 'egcl-thread:condition-variable))
    (assert (typep condition-variable (find-symbol "CONDITION-VARIABLE" "ALIAS-IDENTITY-IMPORT"))))
  (dotimes (index 100)
    (assert (typep (vector index) 'alias-identity-a:simple-vector))
    (assert (typep (mod index 8) '(alias-identity-import:width 3))))
  (write-line "DEFTYPE-SYMBOL-IDENTITY-OK"))
"#;

const LEGACY: &str = r#"
#+egcl
(let ((left (intern "LEGACY" "ALIAS-IDENTITY-A"))
      (right (intern "LEGACY" "ALIAS-IDENTITY-B"))
      (old-expanders *type-expanders*)
      (old-definitions *type-definitions*))
  (unwind-protect
      (progn
        (import left "ALIAS-IDENTITY-IMPORT")
        (setq *type-expanders* (list (list left nil) (list right nil))
              *type-definitions* (list (list right 'string) (list left 'integer)))
        (assert (typep 42 left))
        (assert (not (typep "text" left)))
        (assert (typep "text" right))
        (assert (not (typep 42 right)))
        (assert (typep 42 (find-symbol "LEGACY" "ALIAS-IDENTITY-IMPORT"))))
    (setq *type-expanders* old-expanders *type-definitions* old-definitions)))
(write-line "DEFTYPE-LEGACY-IDENTITY-OK")
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
    let output = command
        .output()
        .expect("run DEFTYPE symbol identity regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}

#[test]
fn deftype_names_preserve_package_and_imported_symbol_identity() {
    let source = std::env::temp_dir().join(format!(
        "egcl-deftype-identity-source-{}.lisp",
        std::process::id()
    ));
    fs::write(&source, SOURCE).unwrap();
    let program = format!("(progn (load {source:?}) (verify-alias-identities))");
    run(&program, false, "DEFTYPE-SYMBOL-IDENTITY-OK");
    run(&program, true, "DEFTYPE-SYMBOL-IDENTITY-OK");
    fs::remove_file(source).unwrap();
}

#[test]
fn compiled_deftype_names_preserve_identity_without_source() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-deftype-identity-compiled-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("aliases.lisp");
    let artifact = directory.join("aliases.bfasl");
    fs::write(&source, SOURCE).unwrap();
    run(
        &format!(
            "(multiple-value-bind (path warnings failure)
         (compile-file {source:?} :output-file {artifact:?})
         (declare (ignore warnings)) (assert path) (assert (not failure))
         (write-line \"DEFTYPE-IDENTITY-COMPILED\"))"
        ),
        false,
        "DEFTYPE-IDENTITY-COMPILED",
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        assert!(
            bytes.windows(4).any(|window| window == b"BBU\0"),
            "expected portable bytecode rather than a source-only fallback"
        );
    }
    fs::remove_file(source).unwrap();
    run(
        &format!("(progn (load {artifact:?}) (verify-alias-identities))"),
        true,
        "DEFTYPE-SYMBOL-IDENTITY-OK",
    );
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn legacy_zero_argument_definitions_use_exact_symbol_keys() {
    let source = std::env::temp_dir().join(format!(
        "egcl-deftype-identity-legacy-{}.lisp",
        std::process::id()
    ));
    fs::write(&source, SOURCE).unwrap();
    run(
        &format!("(progn (load {source:?}) {LEGACY})"),
        true,
        "DEFTYPE-LEGACY-IDENTITY-OK",
    );
    fs::remove_file(source).unwrap();
}
