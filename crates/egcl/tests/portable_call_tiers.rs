// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A non-capturing `(lambda ...)` and `MULTIPLE-VALUE-LIST` must give the same
//! answers at every tier, on every architecture — and must not cost their
//! function its native code.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `bitwise_shift_tiers.rs`: one program
//! runs at all four tiers and the answers are compared, so the file exercises
//! whichever emitter the test binary was built for.
//!
//! These two shapes are unusual in that x86-64 services them through its
//! MAPPED TRANSFER PROTOCOL — `TRANSFER_MAKE_CLOSURE_REQUEST` and
//! `TRANSFER_VALUES_TO_LIST_REQUEST` — which only the mapped entry can answer.
//! A checked-ABI target (AArch64, System Z, …) therefore has to reach them
//! through a plain C adapter in `PortableCalls` instead. Until it does, the
//! emitter sees an `AuxData` its general `Call` arm rejects and declines the
//! WHOLE FUNCTION to T1, which is how this was found: a function returning
//! `(lambda (x) (+ x 1))` reported
//! `aarch64 emitter declined at emit_a64.rs:948` on real hardware.
//!
//! The cases below are the ones where getting the adapter wrong is a wrong
//! answer rather than a decline:
//!
//!   * **A returned closure must still be callable, and must behave.** Passing
//!     the wrong body address, or a capture argument where none belongs, yields
//!     a callable object that computes something else.
//!   * **Each call to the factory must yield a working closure**, not one
//!     aliasing a previous body.
//!   * **`MULTIPLE-VALUE-LIST` must collect every value**, in order, and leave
//!     the multiple-value state inactive so a following single-valued call is
//!     not polluted.
//!   * **A capturing lambda must keep working**, whatever route it takes: it is
//!     deliberately NOT this path (it needs an activation-owned `EnvFrame`),
//!     so it must still produce the right answer through whatever the builder
//!     and emitter do choose.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [&str; 4] = ["interp", "t0", "t1", "t2"];

fn agrees_at_every_tier(program: &str, expected: &[&str]) {
    for tier in TIERS {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for line in expected {
            assert!(
                stdout.lines().any(|l| l.trim() == *line),
                "tier={tier}: missing {line:?} in:\n{stdout}\n{stderr}"
            );
        }
    }
}

#[test]
fn non_capturing_closures_and_value_lists_agree_at_every_tier() {
    let program = r#"
      ;; A factory returning a non-capturing lambda: the shape that declined.
      (defun make-adder () (lambda (x) (+ x 1)))
      (defun make-doubler () (lambda (x) (* x 2)))
      (defun mvl (a b) (multiple-value-list (floor a b)))
      (defun mvl3 () (multiple-value-list (values 1 2 3)))
      ;; A CAPTURING lambda, deliberately not this path, must still be right.
      (defun make-counter (start) (let ((n start)) (lambda () (setq n (+ n 1)) n)))
      (dotimes (i 3000)
        (funcall (make-adder) 1) (funcall (make-doubler) 1)
        (mvl 7 2) (mvl3) (funcall (make-counter 0)))
      (format t "ADDER ~a ~a~%" (funcall (make-adder) 1) (funcall (make-adder) 41))
      ;; Two closures from different factories must not alias each other.
      (let ((a (make-adder)) (d (make-doubler)))
        (format t "BOTH ~a ~a ~a ~a~%" (funcall a 10) (funcall d 10)
                                        (funcall a 10) (funcall d 10)))
      ;; MAPCAR over a non-capturing lambda, the common form.
      (format t "MAPCAR ~a~%" (mapcar (lambda (x) (+ x 1)) '(1 2 3)))
      (format t "MVL ~a ~a~%" (mvl 7 2) (mvl -7 2))
      (format t "MVL3 ~a~%" (mvl3))
      ;; The multiple-value state must be inactive afterwards, so a following
      ;; single-valued call is not polluted by the previous tuple.
      (progn (mvl 7 2) (format t "AFTER ~a~%" (multiple-value-list (+ 1 1))))
      ;; The capturing closure keeps its own state across calls.
      (let ((c (make-counter 10)))
        (format t "COUNTER ~a ~a ~a~%" (funcall c) (funcall c) (funcall c)))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "ADDER 2 42",
            "BOTH 11 20 11 20",
            "MAPCAR (2 3 4)",
            // FLOOR returns quotient and remainder; the negative case floors.
            "MVL (3 1) (-4 1)",
            "MVL3 (1 2 3)",
            "AFTER (2)",
            "COUNTER 11 12 13",
        ],
    );
}

/// The closure body address the adapter is handed must stay valid, and the
/// closures it builds are heap objects the collector moves. Allocating hard
/// between the calls is what makes either mistake visible — as a wrong answer,
/// so the values are checked rather than only the exit status.
#[test]
fn portable_closures_survive_collections_at_every_tier() {
    let program = r#"
      (defun make-adder () (lambda (x) (+ x 1)))
      (defun mvl (a b) (multiple-value-list (floor a b)))
      (defun churn () (let ((x nil)) (dotimes (i 150) (setq x (cons i x))) (length x)))
      (let ((ok t))
        (dotimes (i 3000) (funcall (make-adder) 1) (mvl 7 2))
        (dotimes (i 500)
          (churn)
          ;; A fresh closure each iteration, so it really relocates.
          (let ((f (make-adder)))
            (unless (and (eql (funcall f i) (+ i 1))
                         (equal (mvl (+ i 7) 2) (list (floor (+ i 7) 2)
                                                      (mod (+ i 7) 2))))
              (setq ok nil))))
        (format t "STABLE ~s~%" ok))
    "#;
    agrees_at_every_tier(program, &["STABLE T"]);
}
