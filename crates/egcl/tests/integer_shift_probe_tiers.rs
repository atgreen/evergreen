// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `(ASH x -n)` must give the same answer at every tier, on every
//! architecture, for fixnums AND for bignums.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `bitwise_shift_tiers.rs`: one program
//! runs at all four tiers and the answers are compared, so the file exercises
//! whichever emitter the test binary was built for.
//!
//! `integer_shift` rewrites the call into a success-only `TryIntegerShr` probe
//! plus a `Brif` over the retained ASH call, so the probe never deopts and
//! never signals — it answers NIL and the slow arm runs the original call.
//! A fixnum right shift is already native everywhere, so the probe only earns
//! its keep on BIGNUM operands, and that is where it can go wrong:
//!
//!   * **CL's ASH FLOORS; the magnitude shift truncates.** A bignum's
//!     magnitude is unsigned and shifted logically, so for a NEGATIVE value the
//!     negated quotient must drop by one whenever any discarded bit was set.
//!     `(ash (- (+ (expt 2 62) 1)) -1)` is one past `(- (ash (+ (expt 2 62) 1)
//!     -1))`. Off-by-one only on inputs with a remainder, which positive-only
//!     or exact-only cases never reach.
//!   * **Only a one-limb magnitude is handled.** A wider bignum must take the
//!     NIL exit and still answer correctly through the retained call.
//!   * **A shift at or past the limb width** must answer 0 for a positive
//!     value and -1 for a negative one, not a wrapped shift.
//!   * **The result may leave fixnum range**, in which case the probe must
//!     decline rather than retag a value that is not a fixnum.
//!   * **Zero is a fixnum, never a bignum**, so a zero limb count must not be
//!     read as one.

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

/// The probe must actually be reached, or the rest of this file proves nothing
/// about the emitter arm — the lesson from `vector_element_tiers.rs`, where a
/// test passed on hardware while the arm it covered was never executed.
#[test]
fn the_shift_probe_reaches_the_optimising_tier() {
    let program = r#"
      (defun shr8 (x) (ash x -8))
      (let ((b (+ (expt 2 62) 12345)))
        (dotimes (i 3000) (shr8 i) (shr8 b))
        (format t "HOT ~a ~a~%" (shr8 256) (shr8 b)))
    "#;
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_T2_LOG", "stderr")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "HOT 1 18014398509482032"),
        "wrong answers:\n{stdout}\n{stderr}"
    );
    // Either ABI's admission marker: "T2 EMITTED" for the framed emitters,
    // "Lisp T2 ABI" for x86-64's mapped entry. Matching only one would make
    // this assertion silently vacuous on the other architecture.
    let reached = stderr.lines().any(|line| {
        line.contains("SHR8:") && (line.contains("T2 EMITTED") || line.contains("Lisp T2 ABI"))
    });
    assert!(
        reached,
        "SHR8 never reached T2, so the shift probe was never emitted natively \
         and the rest of this file proves nothing:\n{stderr}"
    );
}

#[test]
fn right_shifts_agree_at_every_tier() {
    let program = r#"
      (defun s1 (x) (ash x -1))
      (defun s8 (x) (ash x -8))
      (defun s64 (x) (ash x -64))
      (defun s100 (x) (ash x -100))
      (let ((p (+ (expt 2 62) 1))          ; one-limb bignum, odd
            (pe (expt 2 62))               ; one-limb bignum, exact
            (wide (expt 2 200)))           ; multi-limb: must take the NIL exit
        (dotimes (i 3000) (s1 i) (s8 i) (s1 p) (s8 p) (s64 p) (s100 p))
        ;; Fixnums, including negatives, where ASH floors.
        (format t "FIX ~a ~a ~a ~a~%" (s1 7) (s1 -7) (s8 1000) (s8 -1000))
        ;; One-limb bignums, positive then negative. The negative-odd case is
        ;; the flooring one.
        (format t "BIG+ ~a ~a~%" (s1 p) (s8 p))
        (format t "BIG- ~a ~a~%" (s1 (- p)) (s8 (- p)))
        ;; Exact: no flooring correction.
        (format t "EXACT ~a ~a~%" (s1 pe) (s1 (- pe)))
        ;; At and past the limb width.
        (format t "WIDE-SHIFT ~a ~a ~a ~a~%"
                (s64 p) (s64 (- p)) (s100 p) (s100 (- p)))
        ;; Multi-limb operands go through the retained call.
        (format t "MULTI ~a ~a~%" (s1 wide) (s8 (- wide)))
        ;; Zero is a fixnum, never a bignum.
        (format t "ZERO ~a ~a~%" (s1 0) (s8 0)))
    "#;
    agrees_at_every_tier(
        program,
        &[
            // (ash -7 -1) is -4: ASH floors, it does not truncate to -3.
            "FIX 3 -4 3 -4",
            "BIG+ 2305843009213693952 18014398509481984",
            // -(2^62+1) >> 1 floors to one below the negated positive result.
            "BIG- -2305843009213693953 -18014398509481985",
            "EXACT 2305843009213693952 -2305843009213693952",
            // Past the magnitude's width: 0 for positive, -1 for negative.
            "WIDE-SHIFT 0 -1 0 -1",
            "MULTI 803469022129495137770981046170581301261101496891396417650688 -6277101735386680763835789423207666416102355444464034512896",
            "ZERO 0 0",
        ],
    );
}
