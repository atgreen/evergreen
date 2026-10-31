// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Structure and class accessors called from compiled code (bliss-fskhm,
//! bliss-u31om).
//!
//! A compiled `(acc x)` or `(setf (acc x) v)` now reaches its slot through an
//! inline cache in the accessor generic's call cell, and a SETF of an accessor
//! place calls the writer by name instead of through FUNCALL. These programs
//! warm each accessor well past the tiering thresholds and then check that the
//! answers -- and the errors -- are the ones the tree-walker gives: typed slots
//! still reject a wrong value, :INCLUDE and subclasses read their own layout,
//! and user methods, redefinitions and lexical writers still take over.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [&str; 4] = ["interp", "t0", "t1", "t2"];

fn run(tier: &str, program: &str) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
    ] {
        cmd.env_remove(key);
    }
    cmd.env("EGCL_FORCE_TIER", tier);
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn check_all_tiers(program: &str, expected: &str) {
    for tier in TIERS {
        let out = run(tier, program);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| panic!("tier {tier}: no RESULT line in:\n{out}"));
        assert_eq!(line, expected, "tier {tier}:\n{out}");
    }
}

#[test]
fn struct_reads_writes_and_typed_slots() {
    check_all_tiers(
        r#"
(progn
  (defstruct dsa-ball (x 0) (y 0))
  (defstruct dsa-typed (n 0 :type fixnum) (f 0.0 :type float) (s nil))
  (defstruct (dsa-sub (:include dsa-ball)) (z 3))
  (defun dsa-read (b) (dsa-ball-x b))
  (defun dsa-write (b v) (setf (dsa-ball-x b) v))
  (defun dsa-write-n (b v) (setf (dsa-typed-n b) v))
  (defun dsa-write-f (b v) (setf (dsa-typed-f b) v))
  (defun dsa-make (i) (make-dsa-ball :x i :y (1+ i)))
  (let ((b (make-dsa-ball :x 1 :y 2))
        (s (make-dsa-sub :x 10 :y 20 :z 30))
        (ty (make-dsa-typed))
        (sum 0))
    (dotimes (i 3000)
      (dsa-write b i)
      (incf sum (dsa-read b))
      (dsa-write s (* 2 i))
      (incf sum (dsa-read s))
      (dsa-write-n ty i)
      (dsa-write-f ty 1.5)
      (incf sum (dsa-ball-y (dsa-make i))))
    (format t "RESULT ~S~%"
            (list sum (dsa-read b) (dsa-read s) (dsa-sub-z s)
                  (multiple-value-list (dsa-read b))
                  (multiple-value-list (dsa-write b :new))
                  (dsa-ball-x b)
                  (dsa-typed-n ty)
                  (handler-case (dsa-write-n ty "no") (type-error () :type-error))
                  (handler-case (dsa-write-n ty 2.5) (type-error () :type-error))
                  (handler-case (dsa-write-f ty 7) (type-error () :type-error))
                  (dsa-write-f ty 2.5)
                  (dsa-typed-n ty)
                  (handler-case (dsa-read nil) (error () :error))))))
"#,
        "RESULT (17997000 2999 5998 30 (2999) (:NEW) :NEW 2999 :TYPE-ERROR :TYPE-ERROR \
         :TYPE-ERROR 2.5 2999 :ERROR)",
    );
}

#[test]
fn class_accessors_follow_the_instance_layout() {
    check_all_tiers(
        r#"
(progn
  (defclass dsa-a () ((s :initarg :s :accessor dsa-s)))
  (defclass dsa-b () ((s :initarg :s :accessor dsa-s)))
  (defclass dsa-a2 (dsa-a) ((s2 :initarg :s2 :accessor dsa-s)))
  (defclass dsa-c () ((q :initarg :q)))
  (defun dsa-get (o) (dsa-s o))
  (defun dsa-put (o v) (setf (dsa-s o) v))
  (let ((a (make-instance 'dsa-a :s 1))
        (b (make-instance 'dsa-b :s 2))
        (a2 (make-instance 'dsa-a2 :s 3 :s2 4))
        (sum 0))
    (dotimes (i 2000)
      (dsa-put a i) (dsa-put b (1+ i)) (dsa-put a2 (+ 2 i))
      (incf sum (+ (dsa-get a) (dsa-get b) (dsa-get a2))))
    (let ((unbound (make-instance 'dsa-a)))
      (format t "RESULT ~S~%"
              (list sum (dsa-get a) (dsa-get b) (dsa-get a2)
                    (slot-value a2 's) (slot-value a2 's2)
                    (handler-case (dsa-get unbound) (unbound-slot () :unbound))
                    (handler-case (dsa-get (make-instance 'dsa-c :q 1))
                      (error () :no-method))
                    (progn (change-class a 'dsa-b) (dsa-get a))
                    (dsa-put a :changed) (slot-value a 's))))))
"#,
        "RESULT (6003000 1999 2000 2001 3 2001 :UNBOUND :NO-METHOD 1999 :CHANGED :CHANGED)",
    );
}

#[test]
fn user_methods_and_redefinitions_take_over_from_warm_accessors() {
    check_all_tiers(
        r#"
(progn
  (defstruct dsa-pt (x 0) (y 0))
  (defclass dsa-k () ((v :initform 1 :accessor dsa-v)))
  (defun dsa-pt-read (p) (dsa-pt-x p))
  (defun dsa-pt-write (p v) (setf (dsa-pt-x p) v))
  (defun dsa-k-read (k) (dsa-v k))
  (defun dsa-k-write (k v) (setf (dsa-v k) v))
  (defun dsa-local-write (p v)
    (flet (((setf dsa-pt-x) (nv q) (declare (ignore q)) (list :local nv)))
      (setf (dsa-pt-x p) v)))
  (let ((p (make-dsa-pt :x 1)) (k (make-instance 'dsa-k)) (log nil))
    (dotimes (i 2000)
      (dsa-pt-write p i) (dsa-pt-read p) (dsa-k-write k i) (dsa-k-read k)
      (dsa-local-write p i))
    (push (dsa-pt-read p) log)
    (push (dsa-local-write p 5) log)
    (defmethod dsa-v :around ((k dsa-k)) (list :around (call-next-method)))
    (push (dsa-k-read k) log)
    (defmethod (setf dsa-v) :before (new (k dsa-k)) (push (list :before new) log))
    (dsa-k-write k 77)
    (push (dsa-k-read k) log)
    (defun (setf dsa-pt-x) (v p) (declare (ignore p)) (list :writer v))
    (push (dsa-pt-write p 9) log)
    (defstruct dsa-pt (w :w) (x 0) (y 0))
    (push (dsa-pt-read (make-dsa-pt :x 55)) log)
    (defun dsa-pt-x (p) (declare (ignore p)) :reader)
    (push (dsa-pt-read p) log)
    (format t "RESULT ~S~%" (reverse log))))
"#,
        "RESULT (1999 (:LOCAL 5) (:AROUND 1999) (:BEFORE 77) (:AROUND 77) (:WRITER 9) 55 :READER)",
    );
}
