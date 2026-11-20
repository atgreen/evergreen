// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Calls of a DEFSTRUCT reader, predicate or untyped writer that is still
//! DEFSTRUCT's own definition perform the structure access in place, and T2
//! answers an instance of exactly the structure's current layout with inline
//! loads and stores. Everything else must behave as the call did: a subtype
//! instance, a non-instance (TYPE-ERROR), a typed slot's check, evaluation
//! order, a later DEFUN of the accessor or writer, NOTINLINE, and heap values
//! stored into a structure that the collector then moves.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defstruct sia-base (a 1) (b 2 :type fixnum) (c nil))
  (defstruct (sia-kid (:include sia-base)) (d 3))
  (defstruct sia-other (a 10))
  (defvar *log* nil)
  (defun sia-read (x) (list (sia-base-a x) (sia-base-c x)))
  (defun sia-kid-d* (x) (sia-kid-d x))
  (defun sia-pred (x) (sia-base-p x))
  (defun sia-try (x) (handler-case (sia-base-a x) (type-error (e) (list :type-error (type-error-datum e)))))
  (defun sia-seta (x v) (setf (sia-base-a x) v))
  (defun sia-setb (x v) (setf (sia-base-b x) v))
  (defun sia-setc (x v) (setf (sia-base-c x) v))
  (defun sia-order (x)
    (setf (sia-base-a (progn (push :object *log*) x)) (progn (push :value *log*) 5)))
  (defun sia-loop (n)
    (let ((w (make-sia-base)) (k (make-sia-kid)) (sum 0))
      (dotimes (i n)
        (sia-seta w i)
        (sia-seta k (list i))
        (sia-setc w (cons i i))
        (sia-setb w (* 2 i))
        ;; Allocation between the store and the read moves young values.
        (make-array 20)
        (incf sum (+ (sia-base-a w) (car (sia-base-a k)) (cdr (sia-base-c w)))))
      (list sum (sia-read w) (sia-base-a k) (sia-base-b w) (sia-kid-d* k)
            (sia-pred w) (sia-pred k) (sia-pred 3) (sia-pred "s")
            (sia-pred (make-sia-other)) (sia-pred nil))))
  (let ((*print-pretty* nil))
    (format t "RESULT1 ~S~%" (sia-loop 20000))
    (format t "RESULT2 ~S ~S ~S~%"
            (sia-try 42) (sia-try (make-sia-other))
            (handler-case (sia-setb (make-sia-base) "no") (type-error () :type-error)))
    (setf *log* nil)
    (sia-order (make-sia-base))
    (format t "RESULT3 ~S~%" (reverse *log*)))
  ;; Redefining the reader and the writer reaches every later-compiled caller.
  (defun sia-base-b (x) (declare (ignore x)) :reader-redefined)
  (defun (setf sia-base-c) (v x) (declare (ignore x)) (list :writer-redefined v))
  (defun sia-get-b (x) (sia-base-b x))
  (defun sia-set-c (x v) (setf (sia-base-c x) v))
  (defun sia-notinline (x) (declare (notinline sia-base-a)) (sia-base-a x))
  (let ((w (make-sia-base :a 12)) (r nil))
    (dotimes (i 3000) (setf r (list (sia-get-b w) (sia-set-c w i) (sia-base-c w) (sia-notinline w))))
    (let ((*print-pretty* nil))
      (format t "RESULT4 ~S~%" r))))
"#;

// SBCL 2.6.9 prints the same four lines.
const EXPECTED: [&str; 4] = [
    "RESULT1 (599970000 (19999 (19999 . 19999)) (19999) 39998 3 T T NIL NIL NIL NIL)",
    "RESULT2 (:TYPE-ERROR 42) (:TYPE-ERROR #S(SIA-OTHER :A 10)) :TYPE-ERROR",
    "RESULT3 (:OBJECT :VALUE)",
    "RESULT4 (:READER-REDEFINED (:WRITER-REDEFINED 2999) NIL 12)",
];

fn run(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "600", BIN, "--no-init", "--eval", PROGRAM]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    cmd.envs(envs.iter().copied());
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check(envs: &[(&str, &str)]) {
    let out = run(envs);
    let lines: Vec<&str> = out.lines().filter(|l| l.starts_with("RESULT")).collect();
    assert_eq!(lines, EXPECTED, "{envs:?}\n{out}");
}

#[test]
fn inline_structure_access_matches_the_calls_at_every_tier() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        match tier {
            Some(tier) => check(&[("EGCL_FORCE_TIER", tier)]),
            None => check(&[]),
        }
    }
    check(&[("EGCL_DISABLE_STRUCT_INLINE", "1")]);
}

#[test]
fn inline_structure_stores_survive_moving_collections() {
    check(&[
        ("EGCL_FORCE_TIER", "t2"),
        ("EGCL_GC_STRESS", "50"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ("EGCL_GC_POISON", "1"),
    ]);
}
