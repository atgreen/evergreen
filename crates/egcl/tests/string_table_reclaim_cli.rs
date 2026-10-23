// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! bliss-4due: strings a program produces must be reclaimable.
//!
//! FORMAT NIL, GET-OUTPUT-STREAM-STRING and READ-LINE used to return strings
//! interned in an off-heap table that was never collected, and every string
//! literal the evaluator saw was registered in a pathname side table scanned as
//! a strong root. Any long-running program that produced strings grew without
//! bound, and a finalizer on such a string never fired. These tests assert the
//! side tables stay bounded (through the `%STRING-TABLE-COUNTS` diagnostic, not
//! a fragile RSS limit) and that string finalizers fire.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    stdout
        .trim()
        .lines()
        .last()
        .unwrap_or("")
        .trim()
        .to_string()
}

/// Producing thousands of distinct strings, through every path that used to
/// intern or register them, leaves the string side tables (nearly) unchanged.
#[test]
fn produced_strings_do_not_accumulate_in_string_tables() {
    let result = eval(
        "(progn
           (defun churn (n)
             (dotimes (i n)
               (format nil \"str-~d\" i)
               (princ-to-string (+ 1000000 i))
               (with-output-to-string (s) (format s \"out-~d\" i))
               (with-input-from-string (s (format nil \"line-~d~%next\" i))
                 (read-line s))
               (eval (read-from-string (format nil \"\\\"lit-~d\\\"\" i)))))
           (churn 50)
           (egcl-ext:gc :full t)
           (let ((before (egcl-ext::%string-table-counts)))
             (churn 3000)
             (egcl-ext:gc :full t)
             (egcl-ext:gc :full t)
             (let ((after (egcl-ext::%string-table-counts)))
               (if (every (lambda (b a) (< (- a b) 100)) before after)
                   :bounded
                   (list :grew before after)))))",
    );
    assert_eq!(result, ":BOUNDED");
}

/// The results are fresh strings, as CLHS requires, not shared interned ones.
#[test]
fn produced_strings_are_fresh() {
    assert_eq!(
        eval(
            "(list (eq (format nil \"same\") (format nil \"same\"))
                   (eq (get-output-stream-string
                         (let ((s (make-string-output-stream))) (write-string \"same\" s) s))
                       (format nil \"same\"))
                   (let ((a (with-input-from-string (s \"same\") (read-line s)))
                         (b (with-input-from-string (s \"same\") (read-line s))))
                     (setf (char a 0) #\\S)
                     (list (eq a b) a b)))"
        ),
        "(NIL NIL (NIL \"Same\" \"same\"))"
    );
}

/// A finalizer on a string fires once the string is unreachable -- for a
/// FORMAT result, an evaluated literal (registered in the pathname string
/// registry), and a string that was promoted before it died, which is only
/// collected by a major GC.
#[test]
fn string_finalizers_fire() {
    assert_eq!(
        eval(
            "(progn
               (defvar *log* nil)
               (defvar *old* nil)
               (defun note (tag) (lambda () (push tag *log*)))
               (defun doom-format ()
                 (egcl-ext:finalize (format nil \"doomed-~d\" 1) (note :format)) nil)
               (defun doom-literal ()
                 (egcl-ext:finalize (eval (read-from-string \"\\\"doomed literal\\\"\"))
                                    (note :literal))
                 nil)
               (defun doom-old ()
                 (setf *old* (eval (read-from-string \"\\\"old literal\\\"\")))
                 (egcl-ext:finalize *old* (note :old))
                 nil)
               (defun churn (n)
                 (let ((acc nil)) (dotimes (i n) (setf acc (list (vector i) acc))) (length acc)))
               (doom-format)
               (doom-literal)
               (doom-old)
               (egcl-ext:gc)
               (egcl-ext:gc)
               (egcl-ext:gc :full t)
               (setf *old* nil)
               (churn 1000)
               (egcl-ext:gc :full t)
               (egcl-ext:gc :full t)
               (sort (mapcar #'symbol-name *log*) #'string<))"
        ),
        "(\"FORMAT\" \"LITERAL\" \"OLD\")"
    );
}

/// A weak-key table entry whose key was promoted and then dropped is removed
/// by a major collection. The major-GC weak pass compared tagged references
/// against untagged addresses, so such entries were never removed.
#[test]
fn major_gc_drops_promoted_weak_keys() {
    assert_eq!(
        eval(
            "(progn
               (defvar *wk* (make-hash-table :weakness :key :test 'eq))
               (defvar *hold* nil)
               (dotimes (i 20)
                 (let ((k (vector i))) (push k *hold*) (setf (gethash k *wk*) i)))
               (egcl-ext:gc)
               (egcl-ext:gc)
               (egcl-ext:gc :full t)
               (let ((before (hash-table-count *wk*)))
                 (setf *hold* (subseq *hold* 0 5))
                 (let ((acc nil)) (dotimes (i 1000) (setf acc (list (vector i) acc))))
                 (egcl-ext:gc :full t)
                 (egcl-ext:gc :full t)
                 (list before (hash-table-count *wk*))))"
        ),
        "(20 5)"
    );
}
