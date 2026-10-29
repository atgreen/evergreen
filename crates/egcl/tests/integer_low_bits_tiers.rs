// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `LDB` of a constant byte specifier, and `LOGAND` against a constant mask,
//! must give the same answers at every tier, on every architecture.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `bitwise_shift_tiers.rs`: one program
//! runs at all four tiers and the answers are compared, so the file exercises
//! whichever emitter the test binary was built for.
//!
//! `integer_extract` rewrites the call to `IntegerLowBits` outright, so a
//! target without the arm must not be offered the pass at all — the opcode
//! would decline the whole function. The arm has a fixnum path and a bignum
//! path, and the bignum path is where this gets interesting:
//!
//!   * **The low bits of a NEGATIVE magnitude are the low bits of its
//!     NEGATION**, even across limbs, because every higher limb is a multiple
//!     of 2^64 and contributes nothing below bit 64. So limb 0 negated is the
//!     whole answer however long the magnitude is — which is only visibly
//!     correct on a MULTI-LIMB negative bignum, hence the cases past 2^64 and
//!     2^128 below.
//!   * **A bignum's `sign` is +1 or -1 and its limb count is at least one.**
//!     Zero is a fixnum, never a bignum, so a zero limb count must deopt rather
//!     than read limb 0 of nothing.
//!   * **The result must be masked before it is re-tagged**, and the fixnum
//!     path must untag first. Getting that order wrong gives an answer scaled
//!     by eight, which looks like a plausible number.
//!   * **A non-integer must signal**, through the deopt and the interpreter,
//!     with the error the ordinary call produces.

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
fn low_bit_extraction_agrees_at_every_tier() {
    let program = r#"
      (defun b8  (x) (ldb (byte 8 0) x))
      (defun b16 (x) (ldb (byte 16 0) x))
      (defun b1  (x) (ldb (byte 1 0) x))
      (defun m   (x) (logand x 255))
      (dotimes (i 3000) (b8 12345) (b16 12345) (b1 12345) (m 12345))
      (format t "FIX ~a ~a ~a ~a~%" (b8 0) (b8 255) (b8 256) (b8 12345))
      (format t "NEG ~a ~a ~a~%" (b8 -1) (b8 -256) (b8 -12345))
      (format t "MASK ~a ~a ~a~%" (m 0) (m 12345) (m -12345))
      (format t "ONE ~a ~a ~a ~a~%" (b1 0) (b1 1) (b1 2) (b1 -1))
      (format t "WIDE ~a ~a~%" (b16 65535) (b16 65536))
      ;; Single-limb and MULTI-LIMB bignums, positive and negative. The
      ;; negative multi-limb cases are the ones that prove limb 0 negated is
      ;; the whole answer.
      (let ((p1 (+ (expt 2 64) 12345))
            (n1 (- (+ (expt 2 64) 12345)))
            (p2 (+ (expt 2 128) 255))
            (n2 (- (+ (expt 2 128) 255)))
            (exact (expt 2 64)))
        (format t "BIG+ ~a ~a~%" (b8 p1) (b8 p2))
        (format t "BIG- ~a ~a~%" (b8 n1) (b8 n2))
        (format t "BIG16 ~a ~a~%" (b16 p1) (b16 n1))
        ;; A magnitude whose limb 0 is zero: 2^64 has low byte 0, and its
        ;; negation's low byte is 0 too.
        (format t "LIMB0 ~a ~a~%" (b8 exact) (b8 (- exact))))
      (format t "ERRORS ~a ~a~%"
              (nth-value 0 (ignore-errors (b8 "x")))
              (nth-value 0 (ignore-errors (m 1.5))))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "FIX 0 255 0 57",
            // (ldb (byte 8 0) -1) is 255: LDB is defined on the two's
            // complement, so a negative value's low bits are not "the
            // magnitude's low bits".
            "NEG 255 0 199",
            "MASK 0 57 199",
            "ONE 0 1 0 1",
            "WIDE 65535 0",
            // 2^64 + 12345 and 2^128 + 255: low byte from limb 0.
            "BIG+ 57 255",
            // Their negations, through the negate-limb-0 path.
            "BIG- 199 1",
            "BIG16 12345 53191",
            "LIMB0 0 0",
            "ERRORS NIL NIL",
        ],
    );
}

/// The bignum path dereferences a tagged pointer the collector moves, and the
/// deopt exits allocate. Allocating hard between the reads is what makes a
/// stale base visible — as a wrong number, so the values are checked rather
/// than only the exit status.
#[test]
fn low_bit_extraction_survives_collections_at_every_tier() {
    let program = r#"
      (defun b8 (x) (ldb (byte 8 0) x))
      (defun churn () (let ((x nil)) (dotimes (i 120) (setq x (cons i x))) (length x)))
      (let ((ok t))
        (dotimes (i 3000) (b8 12345))
        (dotimes (i 400)
          (churn)
          ;; Fresh nursery bignums each iteration, so they really relocate.
          (let* ((p (+ (expt 2 128) 255)) (n (- p)))
            (unless (and (eql (b8 p) 255) (eql (b8 n) 1)
                         (eql (b8 (+ i 0)) (logand i 255))
                         (null (nth-value 0 (ignore-errors (b8 "x")))))
              (setq ok nil))))
        (format t "STABLE ~s~%" ok))
    "#;
    agrees_at_every_tier(program, &["STABLE T"]);
}
