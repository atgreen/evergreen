// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defpackage alias-types (:use :cl)
  (:shadow #:fixnum #:symbol #:integer #:double-float)
  (:export #:fixnum #:symbol #:integer #:double-float))
(defpackage imported-types (:use)
  (:import-from :cl #:fixnum #:symbol #:integer #:double-float)
  (:export #:fixnum #:symbol #:integer #:double-float))
(deftype alias-types:fixnum () (list 'integer 0 '*))
(deftype alias-types:symbol () '(integer))
(deftype alias-types:integer () 'symbol)
(deftype alias-types:double-float () 'integer)
(defun alias-fixnum-p (value) (typep value 'alias-types:fixnum))
(defun alias-symbol-p (value) (typep value 'alias-types:symbol))
(defun alias-integer-p (value) (typep value 'alias-types:integer))
(defun alias-double-p (value) (typep value 'alias-types:double-float))
(defun imported-fixnum-p (value) (typep value 'imported-types:fixnum))
(defun imported-symbol-p (value) (typep value 'imported-types:symbol))
(defun imported-integer-p (value) (typep value 'imported-types:integer))
(defun imported-double-p (value) (typep value 'imported-types:double-float))
(defun alias-typecase (value)
  (typecase value (alias-types:fixnum :match) (single-float :single) (t :other)))
(defun alias-etypecase (value)
  (etypecase value (alias-types:fixnum :match) (single-float :single) (double-float :double)))
(defun checker-tier (name)
  #+egcl (eval (list 'egcl-ext::function-tier (list 'quote name)))
  #-egcl (declare (ignore name))
  #-egcl 1)
(defun warm-checker (name value)
  (dotimes (iteration 500) (funcall name value))
  #+egcl (loop repeat 5000
          until (>= (or (checker-tier name) 0) 1)
          do (funcall name value))
  (assert (>= (or (checker-tier name) 0) 1)))
"#;
const CHECKS: &str = r#"
(warm-checker 'alias-fixnum-p 42)
(warm-checker 'alias-symbol-p 42)
(warm-checker 'alias-integer-p :value)
(warm-checker 'alias-double-p 42)
(warm-checker 'imported-fixnum-p 42)
(warm-checker 'imported-symbol-p :value)
(warm-checker 'imported-integer-p 42)
(warm-checker 'imported-double-p 1.0d0)
(warm-checker 'alias-typecase 42)
(warm-checker 'alias-etypecase 42)
(let ((wide (expt 2 100)))
  (assert (typep wide 'alias-types:fixnum))
  (assert (alias-fixnum-p wide))
  (assert (not (alias-fixnum-p -1)))
  (assert (alias-symbol-p 42))
  (assert (not (alias-symbol-p :value)))
  (assert (alias-integer-p :value))
  (assert (not (alias-integer-p 42)))
  (assert (alias-double-p 42))
  (assert (not (alias-double-p 1.0d0)))
  (assert (imported-fixnum-p -1))
  (assert (imported-symbol-p :value))
  (assert (imported-integer-p wide))
  (assert (imported-double-p 1.0d0))
  (assert (eq (alias-typecase wide) :match))
  (assert (eq (alias-etypecase wide) :match)))
(write-line "TYPEP-IDENTITY-OK")
"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-native-type-script-{}-{serial}.lisp",
        std::process::id()
    ));
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--no-init")
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "4000000000")
        .env("EGCL_T1_T2_BACKEDGE_THRESHOLD", "4000000000")
        .env("EGCL_OSR_THRESHOLD", "4000000000")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .arg("--load")
        .arg(&script)
        .output()
        .unwrap();
    fs::remove_file(script).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("TYPEP-IDENTITY-OK"), "{stdout}");
    stdout
}

#[test]
fn warmed_type_predicates_preserve_foreign_aliases_and_imported_cl_types() {
    let program = format!(
        "{DEFINITIONS}
      (let ((function (compile nil '(lambda (value) (typep value 'alias-types:fixnum)))))
        (warm-checker function 42)
        (assert (funcall function (expt 2 100)))
        (assert (not (funcall function -1))))
      {CHECKS}"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn warmed_alias_predicates_survive_fresh_bytecode_loading() {
    let directory =
        std::env::temp_dir().join(format!("egcl-native-type-identity-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("predicates.lisp");
    let artifact = directory.join("predicates.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line \"TYPEP-IDENTITY-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..sections {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            assert_ne!(kind, 11, "unexpected source fallback");
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "expected portable BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
