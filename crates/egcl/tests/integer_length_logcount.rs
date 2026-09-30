// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! INTEGER-LENGTH and LOGCOUNT reach their kernels directly (bliss-dvfk5).
//!
//! The bead came from Godbolt's AoCO2025 day 11, where a hand-written bit-counting
//! loop is recognised and replaced by the hardware popcount. CL needs no idiom
//! recogniser for that -- LOGCOUNT is a standard function, asked for by name --
//! and EGCL's fixnum kernels were ALREADY one instruction each,
//! `u64::count_ones` and `u64::leading_zeros`.
//!
//! So the cost was never the computation. Measured in a warmed, T2-compiled loop
//! against a 151 ns empty-loop floor:
//!
//!     (logand i 7)          152.00 ns/iter
//!     (abs (- i 500))       301.50
//!     (integer-length i)   1041.00      <- 6.9x the floor
//!     (logcount i)         1042.50
//!
//! ABS is six times cheaper than either despite doing comparable work, and the
//! reason is the DIRECT_* table in cli.rs: a native call to a builtin outside it
//! resolves the callee BY NAME on every single call, which profiling once put at
//! ~31% of a native loop. ABS is in that table; these two were not. Adding them
//! took both to 304 ns -- exactly ABS's cost.
//!
//! Which of the two matters is not what the bead guessed. Counting real uses:
//! LOGCOUNT appears 0 times in lib/*.lisp, 0 times in the ASDF/UIOP we load, and
//! 8 times across every vendored ocicl system. INTEGER-LENGTH appears 50 times.
//! The bead called this the lowest-value item because it asked about popcount;
//! the used function was the other one.
//!
//! The negative cases are the ones worth pinning down, because CL does not define
//! these the way C would: for a NEGATIVE integer, LOGCOUNT counts ZERO bits of
//! the two's-complement representation and INTEGER-LENGTH is the length of the
//! ones-complement magnitude. So `(logcount -1)` is 0, not 64, and
//! `(integer-length -1)` is 0, not 1.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const XS: [i64; 10] = [0, 1, 7, 8, 255, 256, -1, -8, -256, 12345];

const PROGRAM: &str = r#"
  (defun il (a) (integer-length a))
  (defun lc (a) (logcount a))
  (dotimes (i 3000) (il 7) (lc 7))
  (dolist (x '(0 1 7 8 255 256 -1 -8 -256 12345))
    (format t "~a ~a ~a~%" x (il x) (lc x)))
  ;; Past fixnum range, where the bignum limb path runs instead.
  (format t "big ~a ~a~%"
          (integer-length (ash 1 200)) (logcount (1- (ash 1 200))))
"#;

fn expected() -> Vec<String> {
    XS.iter()
        .map(|&x| {
            // CLHS: for negative n both are computed on ~n, so LOGCOUNT counts
            // the zero bits and INTEGER-LENGTH measures the ones complement.
            let u = if x < 0 { !x as u64 } else { x as u64 };
            format!("{x} {} {}", 64 - u.leading_zeros(), u.count_ones())
        })
        .collect()
}

#[test]
fn both_are_correct_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for line in expected() {
            assert!(
                stdout.lines().any(|l| l.trim() == line),
                "tier={tier}: missing {line:?} in:\n{stdout}\n{stderr}"
            );
        }
        // (ash 1 200) has 201 bits; (1- (ash 1 200)) is 200 one-bits.
        assert!(
            stdout.lines().any(|l| l.trim() == "big 201 200"),
            "tier={tier}: bignum path wrong in:\n{stdout}"
        );
    }
}

#[test]
fn neither_pays_for_name_resolution_any_more() {
    // The guard on the measurement: both must now cost about what ABS costs --
    // ABS having always been in the direct-call table -- rather than the ~3.4x
    // they cost outside it. Generous margin because this shares CI hardware; it
    // is checking that a table entry exists, not a nanosecond count.
    let program = r#"
      (defun k-abs (n) (let ((a 0)) (dotimes (i n) (setq a (abs (- i 500)))) a))
      (defun k-il  (n) (let ((a 0)) (dotimes (i n) (setq a (integer-length i))) a))
      (defun k-lc  (n) (let ((a 0)) (dotimes (i n) (setq a (logcount i))) a))
      (dolist (f '(k-abs k-il k-lc))
        (dotimes (w 200) (funcall f 50)) (funcall f 200000)
        (let* ((n 2000000) (s (get-internal-real-time)))
          (funcall f n)
          (format t "~a ~a~%" f (- (get-internal-real-time) s))))
    "#;
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let ms = |name: &str| -> f64 {
        stdout
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ")))
            .and_then(|v| v.trim().parse::<f64>().ok())
            .unwrap_or_else(|| panic!("no timing for {name} in:\n{stdout}"))
    };
    let (abs, il, lc) = (ms("K-ABS"), ms("K-IL"), ms("K-LC"));
    for (name, t) in [("integer-length", il), ("logcount", lc)] {
        assert!(
            t < abs * 2.0,
            "{name} should cost about what ABS does now that it is in the \
             direct-call table: abs={abs}ms il={il}ms lc={lc}ms"
        );
    }
}
