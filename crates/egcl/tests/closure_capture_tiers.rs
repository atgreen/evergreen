// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reading and writing a CAPTURED lexical variable must give the same answers
//! at every tier, on every architecture.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `equality_typep_tiers.rs`: one
//! program runs at all four tiers and the answers are compared, so the file
//! exercises whichever emitter the test binary was built for.
//!
//! `build.rs` turns every `LoadEnvVar`/`StoreEnvVar` into
//! `EnvironmentValue`/`SetEnvironmentValue` unconditionally — there is no gate
//! on it — so an emitter without those arms does not merely run the access
//! slowly. It declines the opcode, and the whole closure body loses its T2
//! code. That was the AArch64 position: the x86 framed emitter and the System Z
//! emitter both had the arm and `emit_a64.rs` did not, so a closure that merely
//! READ a captured variable stayed at T1.
//!
//! The cases are chosen for the ways the access goes wrong rather than for
//! coverage of CL scoping:
//!
//!   * **A write must be visible to the next read.** The access is a call to
//!     an adapter taking `(body, name_index)`; passing the index in the wrong
//!     argument register, or reading a stale copy instead of the cell, reads
//!     the right value the first time and then stops tracking it.
//!   * **Two closures over ONE binding must share it.** This is what
//!     distinguishes a real capture from a copy taken at closure creation: a
//!     backend that answers from a copy gets the single-closure case right and
//!     this one wrong.
//!   * **The captured value must survive a collection.** It lives in heap
//!     metadata the collector moves, and the adapter reaches it through the
//!     retained root body, so a stale body address answers from a moved-from
//!     object rather than crashing.
//!   * **Mixed reads and writes in a hot loop** is the shape that actually
//!     reaches T2 through the invocation threshold.

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
fn captured_variable_access_agrees_at_every_tier() {
    let program = r#"
      ;; Read and write one captured binding.
      (defun make-counter (start)
        (let ((n start))
          (lambda (k) (setq n (+ n k)) n)))
      ;; Two closures over the SAME binding: a copy taken at creation would
      ;; make the reader blind to the writer.
      (defun make-pair ()
        (let ((n 0))
          (list (lambda (k) (setq n (+ n k)) n)
                (lambda () n))))
      ;; Read-only capture, and a capture of something other than a fixnum.
      (defun make-reader (v) (lambda () v))
      (let* ((c (make-counter 100))
             (pair (make-pair))
             (bump (first pair))
             (peek (second pair))
             (r (make-reader "captured")))
        ;; Warm every closure body past the thresholds.
        (dotimes (i 3000)
          (funcall c 0) (funcall bump 0) (funcall peek) (funcall r))
        (format t "COUNT ~s ~s ~s~%"
                (funcall c 1) (funcall c 1) (funcall c -2))
        (funcall bump 7)
        (format t "SHARED ~s~%" (funcall peek))
        (funcall bump 5)
        (format t "SHARED ~s~%" (funcall peek))
        (format t "READER ~s~%" (funcall r)))
    "#;
    agrees_at_every_tier(
        program,
        &[
            // 100 after warmup (every warm call added 0), then +1, +1, -2.
            "COUNT 101 102 100",
            // The reader sees the writer's effect, twice.
            "SHARED 7",
            "SHARED 12",
            "READER \"captured\"",
        ],
    );
}

/// A captured variable read and written inside a hot counted loop: the shape
/// that reaches T2 through the invocation threshold, with the access on the
/// loop's critical path.
#[test]
fn captured_access_in_a_hot_loop_agrees_at_every_tier() {
    let program = r#"
      (defun make-accumulator ()
        (let ((total 0) (calls 0))
          (lambda (k)
            (setq calls (+ calls 1))
            (setq total (+ total k))
            (list total calls))))
      (let ((a (make-accumulator)))
        (dotimes (i 3000) (funcall a 0))
        (let ((seen nil))
          (dotimes (i 1000) (setq seen (funcall a 2)))
          (format t "ACC ~s ~s~%" (first seen) (second seen))))
    "#;
    // 3000 warm calls at 0 then 1000 at 2: total 2000, calls 4000.
    agrees_at_every_tier(program, &["ACC 2000 4000"]);
}

/// The captured binding lives in heap metadata the collector moves, and the
/// access reaches it through the retained root body. Allocating hard between
/// the reads is what makes a stale body address visible — as a wrong answer,
/// which is why the value is checked rather than only the exit status.
#[test]
fn captured_access_survives_collections_at_every_tier() {
    let program = r#"
      (defun make-counter (start)
        (let ((n start))
          (lambda (k) (setq n (+ n k)) n)))
      (defun churn () (let ((x nil)) (dotimes (i 200) (setq x (cons i x))) (length x)))
      (let ((c (make-counter 0)) (ok t))
        (dotimes (i 3000) (funcall c 0))
        (dotimes (i 2000)
          (churn)
          (unless (eql (funcall c 1) (+ i 1)) (setq ok nil)))
        (format t "STABLE ~s ~s~%" ok (funcall c 0)))
    "#;
    agrees_at_every_tier(program, &["STABLE T 2000"]);
}
