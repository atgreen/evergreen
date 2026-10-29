// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! TRUNCATE / FLOOR / REM / MOD / MIN / MAX on fixnums, and the single-float
//! comparisons, must give the same answers at every tier on every
//! architecture.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `equality_typep_tiers.rs`: one
//! program is run at all four tiers and the answers compared, so the file
//! exercises whichever emitter the test binary was built for.
//!
//! These shapes are easy to get wrong in ways a smaller test misses, and each
//! case below is here for a specific wrong answer rather than for coverage of
//! CL semantics:
//!
//!   * **The two division families disagree only on negative operands.**
//!     TRUNCATE and REM round toward zero; FLOOR and MOD round toward negative
//!     infinity. They coincide for every pair of positive operands, so a
//!     backend that implements one and labels it the other passes any
//!     positive-only test. `(mod -7 2)` is 1 and `(rem -7 2)` is -1; `(floor
//!     -7 2)` is -4 and `(truncate -7 2)` is -3.
//!   * **The sign rule is about the DIVISOR, not the dividend.** The floored
//!     and truncated results differ exactly when the division is inexact and
//!     the remainder's sign disagrees with the divisor's, so `(mod 7 -2)` must
//!     be -1 while `(mod -7 -2)` is -1 as well and `(rem 7 -2)` is 1. A
//!     backend that tests the dividend's sign instead gets three of these four
//!     right.
//!   * **An exact division must not be adjusted.** `(floor -8 2)` is -4, not
//!     -5: the correction applies only when the remainder is nonzero.
//!   * **MIN-FIXNUM divided by -1 is not a fixnum.** It is 2^60, one past the
//!     maximum, so a native quotient has to be range-checked and the operation
//!     handed back. AArch64 compounds this: its `SDIV` neither traps on that
//!     case nor on a zero divisor — it answers `i64::MIN` and 0 respectively —
//!     so both checks are explicit branches rather than a fault, and dropping
//!     either produces a wrong answer rather than a crash.
//!   * **Dividing by zero must still signal.** It reaches the interpreter
//!     through a deopt; a native path that let `SDIV`'s zero answer through
//!     would quietly return 0.
//!   * **A NaN comparison must not be decided natively.** The interpreter's
//!     NaN ordering is not IEEE's, and on AArch64 an unordered `FCMP` sets the
//!     flags so that `LT` and `LE` both read TRUE — so a comparison that does
//!     not take the unordered exit first answers `(< nan 1.0)` as T.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [&str; 4] = ["interp", "t0", "t1", "t2"];

/// Run `program` at every tier and require each line of `expected` to appear.
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

/// Warmed past the speculation thresholds, and warmed on the SAME SIGNS the
/// assertions use: a function warmed only on positive operands speculates, but
/// a later negative call takes whichever path the emitter actually produced,
/// which is the case this file is about.
const DIVISION: &str = r#"
  (defun f-trunc (a b) (truncate a b))
  (defun f-floor (a b) (floor a b))
  (defun f-rem   (a b) (rem a b))
  (defun f-mod   (a b) (mod a b))
  (defun f-min   (a b) (min a b))
  (defun f-max   (a b) (max a b))
  (dotimes (i 3000)
    (f-trunc 7 2) (f-trunc -7 2) (f-floor 7 2) (f-floor -7 2)
    (f-rem 7 2) (f-rem -7 2) (f-mod 7 2) (f-mod -7 2)
    (f-min 1 2) (f-max 1 2) (f-min -1 -2) (f-max -1 -2))
  (format t "TRUNC ~s ~s ~s ~s~%"
          (f-trunc 7 2) (f-trunc -7 2) (f-trunc 7 -2) (f-trunc -7 -2))
  (format t "FLOOR ~s ~s ~s ~s~%"
          (f-floor 7 2) (f-floor -7 2) (f-floor 7 -2) (f-floor -7 -2))
  (format t "REM ~s ~s ~s ~s~%"
          (f-rem 7 2) (f-rem -7 2) (f-rem 7 -2) (f-rem -7 -2))
  (format t "MOD ~s ~s ~s ~s~%"
          (f-mod 7 2) (f-mod -7 2) (f-mod 7 -2) (f-mod -7 -2))
  ;; Exact divisions: the floor/mod correction must NOT fire.
  (format t "EXACT ~s ~s ~s ~s~%"
          (f-floor -8 2) (f-mod -8 2) (f-trunc -8 2) (f-rem -8 2))
  (format t "MINMAX ~s ~s ~s ~s~%"
          (f-min 3 -4) (f-max 3 -4) (f-min -4 -4) (f-max 1152921504606846975 0))
  ;; MIN-FIXNUM / -1 is 2^60, one past MOST-POSITIVE-FIXNUM, so a native
  ;; quotient must be range-checked and the operation handed back.
  (format t "OVERFLOW ~s ~s~%"
          (f-trunc -1152921504606846976 -1) (f-floor -1152921504606846976 -1))
  (format t "ZERO ~s ~s ~s ~s~%"
          (nth-value 0 (ignore-errors (f-trunc 1 0)))
          (nth-value 0 (ignore-errors (f-floor 1 0)))
          (nth-value 0 (ignore-errors (f-rem 1 0)))
          (nth-value 0 (ignore-errors (f-mod 1 0))))
"#;

#[test]
fn fixnum_division_agrees_at_every_tier() {
    agrees_at_every_tier(
        DIVISION,
        &[
            // Toward zero.
            "TRUNC 3 -3 -3 3",
            // Toward negative infinity: two of the four differ from TRUNC.
            "FLOOR 3 -4 -4 3",
            // The remainder takes the DIVIDEND's sign.
            "REM 1 -1 1 -1",
            // The remainder takes the DIVISOR's sign.
            "MOD 1 1 -1 -1",
            // Exact: no correction, so both families agree and both
            // remainders are zero.
            "EXACT -4 0 -4 0",
            "MINMAX -4 3 -4 1152921504606846975",
            // 2^60 is a bignum here, not a wrapped fixnum.
            "OVERFLOW 1152921504606846976 1152921504606846976",
            // All four signal rather than answering 0.
            "ZERO NIL NIL NIL NIL",
        ],
    );
}

/// Division inside a hot counted loop, which is where the opcode matters: this
/// is the shape that reaches T2 through the invocation threshold and keeps the
/// quotient and remainder in registers across a back edge.
#[test]
fn division_in_a_hot_loop_agrees_at_every_tier() {
    let program = r#"
      (defun digit-sum (n)
        (let ((s 0))
          (loop while (> n 0)
                do (setq s (+ s (mod n 10))
                         n (floor n 10)))
          s))
      (defun collatz (n)
        (let ((steps 0))
          (loop until (= n 1)
                do (setq n (if (= 0 (mod n 2)) (floor n 2) (+ (* 3 n) 1))
                         steps (+ steps 1)))
          steps))
      (dotimes (i 3000) (digit-sum 123456789) (collatz 27))
      (format t "DIGITS ~s ~s ~s~%"
              (digit-sum 0) (digit-sum 123456789) (digit-sum 999999999))
      (format t "COLLATZ ~s ~s ~s~%" (collatz 1) (collatz 27) (collatz 97))
    "#;
    agrees_at_every_tier(program, &["DIGITS 0 45 81", "COLLATZ 0 111 118"]);
}

/// Single-float comparisons, including the unordered case that must deopt
/// rather than be decided natively.
#[test]
fn single_float_comparisons_agree_at_every_tier() {
    let program = r#"
      (defun f-lt (a b) (< a b))
      (defun f-le (a b) (<= a b))
      (defun f-gt (a b) (> a b))
      (defun f-ge (a b) (>= a b))
      (defun f-eq (a b) (= a b))
      (dotimes (i 3000)
        (f-lt 1.0 2.0) (f-le 1.0 2.0) (f-gt 1.0 2.0)
        (f-ge 1.0 2.0) (f-eq 1.0 2.0))
      (format t "LT ~s ~s ~s~%" (f-lt 1.0 2.0) (f-lt 2.0 1.0) (f-lt 1.0 1.0))
      (format t "LE ~s ~s ~s~%" (f-le 1.0 2.0) (f-le 2.0 1.0) (f-le 1.0 1.0))
      (format t "GT ~s ~s ~s~%" (f-gt 1.0 2.0) (f-gt 2.0 1.0) (f-gt 1.0 1.0))
      (format t "GE ~s ~s ~s~%" (f-ge 1.0 2.0) (f-ge 2.0 1.0) (f-ge 1.0 1.0))
      (format t "EQ ~s ~s~%" (f-eq 1.0 1.0) (f-eq 1.0 2.0))
      ;; Negative zero compares EQUAL to positive zero, and both orderings of
      ;; a negative pair must stay consistent.
      (format t "ZEROS ~s ~s ~s~%" (f-eq -0.0 0.0) (f-lt -0.0 0.0) (f-le -0.0 0.0))
      (format t "NEG ~s ~s ~s~%" (f-lt -2.0 -1.0) (f-gt -2.0 -1.0) (f-ge -1.0 -2.0))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "LT T NIL NIL",
            "LE T NIL T",
            "GT NIL T NIL",
            "GE NIL T T",
            "EQ T NIL",
            "ZEROS T NIL T",
            "NEG T NIL T",
        ],
    );
}

/// A NaN operand is the case native code must NOT decide. On AArch64 an
/// unordered `FCMP` leaves the flags reading TRUE for both `LT` and `LE`, so an
/// arm that selects on those codes without first taking the unordered exit
/// answers `(< nan 1.0)` as T, disagreeing with every other tier.
#[test]
fn an_unordered_float_comparison_agrees_at_every_tier() {
    let program = r#"
      (defun f-lt (a b) (< a b))
      (defun f-le (a b) (<= a b))
      (defun f-gt (a b) (> a b))
      (defun f-ge (a b) (>= a b))
      (defun f-eq (a b) (= a b))
      (dotimes (i 3000)
        (f-lt 1.0 2.0) (f-le 1.0 2.0) (f-gt 1.0 2.0)
        (f-ge 1.0 2.0) (f-eq 1.0 2.0))
      (let ((nan (nth-value 0 (ignore-errors (/ 0.0 0.0)))))
        (format t "NANP ~s~%" (and nan (/= nan nan)))
        (when (and nan (/= nan nan))
          (format t "NAN ~s ~s ~s ~s ~s~%"
                  (nth-value 0 (ignore-errors (f-lt nan 1.0)))
                  (nth-value 0 (ignore-errors (f-le nan 1.0)))
                  (nth-value 0 (ignore-errors (f-gt nan 1.0)))
                  (nth-value 0 (ignore-errors (f-ge nan 1.0)))
                  (nth-value 0 (ignore-errors (f-eq nan nan))))))
    "#;
    // Whatever the engine decides for a NaN, every tier must decide it the
    // same way, so the answers are compared ACROSS tiers rather than against a
    // literal. A build where `(/ 0.0 0.0)` signals instead of producing a NaN
    // prints only NANP and still exercises that agreement.
    let mut answers: Vec<(String, String)> = Vec::new();
    for tier in TIERS {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        let relevant: String = stdout
            .lines()
            .filter(|l| l.starts_with("NAN"))
            .collect::<Vec<_>>()
            .join("|");
        assert!(
            !relevant.is_empty(),
            "tier={tier}: no NaN result at all:\n{stdout}\n{stderr}"
        );
        answers.push((tier.to_string(), relevant));
    }
    let (first_tier, first) = &answers[0];
    for (tier, answer) in &answers[1..] {
        assert_eq!(
            answer, first,
            "tier={tier} disagrees with tier={first_tier} on an unordered compare"
        );
    }
}
