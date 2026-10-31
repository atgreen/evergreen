// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Leaf builtin helpers (bliss-gg3i1.7.3): compiled calls to EQL, LENGTH,
//! GETHASH, the integer operators and the other builtins with a leaf helper
//! answer exactly as the general path does, including every miss -- a wrong
//! type, a bignum result, a synchronized or EQUAL table, an unbound special --
//! the multiple values the call leaves, and a redefinition of the builtin.

use std::process::Command;

const PROGRAM: &str = r#"
(defvar *lb-special* 10)
(defvar *lb-unbound*)
(makunbound '*lb-unbound*)
(defun lb-ops (a b)
  (list (eql a b) (equal a b) (equalp a b) (consp a) (endp b) (car b) (cdr b)
        (length b) (elt b 1) (zerop a) (1+ a) (1- a) (+ a 1) (- a 1) (* a 3)
        (= a 2) (/= a 2) (< a 2) (<= a 2) (> a 2) (>= a 2)
        (logand a 6) (logior a 8) (logxor a 3) (ash a 3) (ash a -1) (logbitp 1 a)))
(defun lb-vec (v i) (list (svref v i) (aref v i) (elt v i) (length v)))
(defun lb-str (s i) (list (aref s i) (elt s i) (length s)))
(defun lb-gethash (k h) (gethash k h))
(defun lb-gethash-default (k h) (gethash k h :none))
(defun lb-gethash-values (k h) (multiple-value-list (gethash k h)))
(defun lb-length-values (x) (multiple-value-list (length x)))
(defun lb-special () *lb-special*)
(defun lb-special-bound (v) (let ((*lb-special* v)) (lb-special)))
(defun lb-unbound () *lb-unbound*)
(defun lb-logbitp (i x) (logbitp i x))
(defun lb-loop (n) (let ((r nil)) (dotimes (i n r) (setq r (lb-logbitp 1 3)))))
(defun lb-try (f &rest args)
  (handler-case (apply f args) (error (e) (list :error (type-of e)))))
(defun lb-run ()
  (let ((eq-table (make-hash-table :test 'eq))
        (equal-table (make-hash-table :test 'equal))
        (synchronized (make-hash-table :test 'eql :synchronized t))
        (big (expt 2 62))
        (result nil))
    (setf (gethash 'a eq-table) 1 (gethash 'n eq-table) nil
          (gethash "s" equal-table) 2 (gethash 5 synchronized) 3)
    (dotimes (i 60)
      (setf result
            (list (lb-try #'lb-ops 2 '(1 2 3)) (lb-try #'lb-ops 5 '(1 . 2))
                  (lb-try #'lb-ops big '(1 2)) (lb-try #'lb-ops most-positive-fixnum nil)
                  (lb-try #'lb-ops 1.5 (vector 1 2)) (lb-try #'lb-ops "a" "A")
                  (lb-try #'lb-vec (vector 1 2 3) 2) (lb-try #'lb-vec (vector 1 2 3) 3)
                  (lb-try #'lb-vec "abc" 1)
                  (lb-try #'lb-vec (make-array 3 :element-type 'double-float
                                                  :initial-element 1d0) 1)
                  (lb-try #'lb-str "abc" 2) (lb-try #'lb-str (make-array 4 :fill-pointer 2
                                                                  :element-type 'character
                                                                  :initial-element #\x) 1)
                  (lb-try #'lb-gethash 'a eq-table) (lb-try #'lb-gethash 'zz eq-table)
                  (lb-try #'lb-gethash "s" equal-table) (lb-try #'lb-gethash 5 synchronized)
                  (lb-try #'lb-gethash 'a 7) (lb-try #'lb-gethash-default 'zz eq-table)
                  (lb-try #'lb-gethash-values 'n eq-table)
                  (lb-try #'lb-gethash-values 'zz eq-table)
                  (lb-try #'lb-length-values "xy") (lb-try #'lb-length-values 3)
                  (lb-try #'lb-special) (lb-try #'lb-special-bound i) (lb-try #'lb-unbound)
                  (lb-loop 3))))
    result))
(format t "RESULT ~S~%" (lb-run))
(dotimes (i 40) (lb-loop 50))
;; LOGBITP is not one of the standard primitives compiled code may open-code,
;; so compiled callers must see a new definition.
(setf (symbol-function 'logbitp) (lambda (i x) (list :redefined i x)))
(format t "REDEFINED ~S~%" (lb-loop 2))
"#;

fn run(tier: &str, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .arg("--no-init")
        .args(["--eval", &format!("(progn {PROGRAM})")]);
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_DISABLE_T2",
        "EGCL_NO_LEAF_BUILTINS",
    ] {
        command.env_remove(name);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    if tier.is_empty() {
        command.env_remove("EGCL_FORCE_TIER");
    } else {
        command.env("EGCL_FORCE_TIER", tier);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier {tier:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
        .lines()
        .filter(|line| line.starts_with("RESULT ") || line.starts_with("REDEFINED "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn leaf_builtins_agree_with_the_interpreter_in_every_tier() {
    let expected = run("interp", false);
    assert!(
        expected.contains("REDEFINED (:REDEFINED 1 3)"),
        "{expected}"
    );
    assert!(expected.contains("(:ERROR UNBOUND-VARIABLE)"), "{expected}");
    for tier in ["t0", "t1", "t2", ""] {
        assert_eq!(run(tier, false), expected, "tier {tier:?}");
    }
    assert_eq!(run("t2", true), expected, "t2 under GC stress");
}
