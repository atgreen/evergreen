// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Package and hash-table literals compile into the fasl (CLHS 3.2.4.2.2: a
//! package by name, a hash table by its test and entries) instead of making the
//! whole file source-only, and load in a fresh process as similar objects.
use std::process::{Command, Output};

fn egcl(dir: &std::path::Path, program: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .current_dir(dir)
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap()
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Compile `source` in one process, then load the fasl and run `checks` in
/// another, which prints `marker` on success. Returns the compile output.
fn compile_then_load(name: &str, source: &str, checks: &str, marker: &str) -> String {
    let dir = scratch(name);
    std::fs::write(dir.join("literals.lisp"), source).unwrap();
    let compiled = egcl(&dir, r#"(compile-file "literals.lisp")"#);
    let compile_out = String::from_utf8_lossy(&compiled.stdout).into_owned();
    assert!(
        compiled.status.success(),
        "{compile_out}\n{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = egcl(&dir, &format!(r#"(progn (load "literals.fasl") {checks})"#));
    let stdout = String::from_utf8_lossy(&loaded.stdout);
    assert!(
        loaded.status.success() && stdout.contains(marker),
        "{stdout}\n{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    compile_out
}

#[test]
fn package_literals_compile_and_load_by_name() {
    let compiled = compile_then_load(
        "fasl-package-literal",
        "(defun keyword-package () #.(find-package :keyword))\n\
         (defun cl-package () (load-time-value (find-package :common-lisp)))\n",
        r#"(assert (eq (keyword-package) (find-package :keyword)))
           (assert (eq (cl-package) (find-package :common-lisp)))
           (format t "PACKAGE-LITERAL-OK~%")"#,
        "PACKAGE-LITERAL-OK",
    );
    assert!(!compiled.contains("source-only"), "{compiled}");
}

#[test]
fn hash_table_literals_compile_and_load_with_their_test_and_entries() {
    let compiled = compile_then_load(
        "fasl-hash-table-literal",
        r#"(defparameter *by-name*
  #.(let ((table (make-hash-table :test 'equal)))
      (setf (gethash "alpha" table) 1
            (gethash '(1 2) table) "list"
            (gethash "nested" table) (let ((inner (make-hash-table)))
                                       (setf (gethash :k inner) :v)
                                       inner))
      table))
(defparameter *by-symbol*
  #.(let ((table (make-hash-table :test 'eq)))
      (setf (gethash :sha3 table) 32 (gethash :sha256 table) 32)
      table))
(defparameter *folded*
  #.(let ((table (make-hash-table :test 'equalp)))
      (setf (gethash "MiXeD" table) :found)
      table))
"#,
        r#"(assert (eq (hash-table-test *by-name*) 'equal))
           (assert (= (hash-table-count *by-name*) 3))
           (assert (eql (gethash "alpha" *by-name*) 1))
           (assert (equal (gethash (list 1 2) *by-name*) "list"))
           (assert (eq (gethash :k (gethash "nested" *by-name*)) :v))
           (assert (eq (hash-table-test *by-symbol*) 'eq))
           (assert (eql (gethash :sha3 *by-symbol*) 32))
           (assert (eq (hash-table-test *folded*) 'equalp))
           (assert (eq (gethash "mixed" *folded*) :found))
           (setf (gethash "beta" *by-name*) 2)
           (assert (eql (gethash "beta" *by-name*) 2))
           (format t "HASH-TABLE-LITERAL-OK~%")"#,
        "HASH-TABLE-LITERAL-OK",
    );
    assert!(!compiled.contains("source-only"), "{compiled}");
}

/// A table that contains itself cannot be written entry by entry; the file
/// falls back to source-only and still loads it.
#[test]
fn self_containing_hash_table_literal_still_loads() {
    let compiled = compile_then_load(
        "fasl-cyclic-hash-table-literal",
        "(defparameter *cyclic*\n\
           #.(let ((table (make-hash-table))) (setf (gethash :self table) table) table))\n",
        r#"(assert (eq (gethash :self *cyclic*) *cyclic*))
           (format t "CYCLIC-TABLE-OK~%")"#,
        "CYCLIC-TABLE-OK",
    );
    assert!(compiled.contains("source-only"), "{compiled}");
}
