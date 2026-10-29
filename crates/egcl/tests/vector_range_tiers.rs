// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

//! A counted loop storing over a proven vector range must give the same
//! answers at every tier on x86-64 Linux, where vector-range versioning is
//! enabled, and must still signal wherever the range cannot be proven.
//!
//! AArch64 carries the same emitter arms, but its installer gate remains off
//! because the versioned loop currently regresses both measured shapes. Its
//! machine-code contracts are covered by cross-host emitter tests instead.
//!
//! `vector_ranges` clones the loop body behind a TOTAL runtime predicate
//! (`SmallVectorRangeP`) and lets the clone's stores (`VecSetInRange`) skip
//! their bounds checks, because the predicate proved the range. That makes this
//! the one place in the port where a store runs **unchecked**, so the cases
//! below are about the predicate being exactly right rather than approximately:
//!
//!   * **A range that is out of bounds must take the checked path and
//!     signal.** If the predicate answered T too eagerly the clone would write
//!     past the end — memory corruption, not an error.
//!   * **Only the SMALL simple-vector layout qualifies.** A header whose low
//!     halfword is `0xFFFF` selects the extended header, where the length is
//!     NOT at +8; a large vector must therefore fail the predicate rather than
//!     have its length read from the wrong word. Both sizes are exercised.
//!   * **A negative start, and `end` below `start`,** must fail the predicate.
//!   * **A non-simple vector** (fill-pointer, adjustable) and a non-vector must
//!     fail it.
//!   * **`end` exactly equal to the length** is in bounds, and `end` one past
//!     it is not — the boundary the comparison is easiest to get wrong by one.

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

/// The versioned loop must actually be produced, or this file proves nothing
/// about the two new arms — the lesson from #760, where a test passed on
/// hardware while the arm it covered was never executed.
#[test]
fn the_versioned_loop_is_produced() {
    let program = r#"
      (defun fill-range (vector end value)
        (declare (type fixnum end))
        (do ((i 0 (1+ i))) ((>= i end) vector)
          (setf (aref vector i) value)))
      (let ((v (make-array 16 :initial-element -1)))
        (dotimes (i 4000) (fill-range v 16 7))
        (format t "HOT ~a ~a~%" (aref v 0) (aref v 15)))
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
        stdout.lines().any(|l| l.trim() == "HOT 7 7"),
        "wrong answers:\n{stdout}\n{stderr}"
    );
    assert!(
        stderr.lines().any(|l| l.contains("vector range versions:")),
        "the loop was never versioned, so SmallVectorRangeP and VecSetInRange \
         were never emitted and the rest of this file proves nothing:\n{stderr}"
    );
}

#[test]
fn ranged_vector_stores_agree_at_every_tier() {
    let program = r#"
      (defun fill-range (vector start end value)
        (declare (type fixnum start end))
        (do ((i start (1+ i))) ((>= i end) vector)
          (setf (aref vector i) value)))
      (let ((small (make-array 8 :initial-element -1))
            ;; Large enough for the extended header, which the predicate must
            ;; refuse rather than read a length from the wrong word.
            (big (make-array 70000 :initial-element -1))
            (fp (make-array 8 :fill-pointer 4 :initial-element -1))
            (adj (make-array 8 :adjustable t :initial-element -1)))
        (dotimes (i 4000) (fill-range small 0 8 7) (fill-range big 0 16 7))
        ;; In bounds, including end exactly equal to the length.
        (format t "SMALL ~a ~a ~a~%"
                (aref (fill-range small 0 8 5) 0) (aref small 7) (length small))
        (format t "PART ~a ~a ~a~%"
                (aref (fill-range small 2 5 9) 1) (aref small 2) (aref small 5))
        (format t "BIG ~a ~a~%"
                (aref (fill-range big 0 70000 3) 0) (aref big 69999))
        ;; An empty range stores nothing.
        (format t "EMPTY ~a~%" (aref (fill-range small 3 3 77) 3))
        ;; Non-simple layouts still work, through the checked path.
        (format t "FP ~a~%" (aref (fill-range fp 0 4 6) 0))
        (format t "ADJ ~a~%" (aref (fill-range adj 0 8 6) 7))
        ;; Everything the predicate must refuse still signals rather than
        ;; writing out of bounds.
        (format t "ERRORS ~a ~a ~a ~a~%"
                (nth-value 0 (ignore-errors (fill-range small 0 9 1)))
                (nth-value 0 (ignore-errors (fill-range small -1 4 1)))
                (nth-value 0 (ignore-errors (fill-range small 0 100 1)))
                (nth-value 0 (ignore-errors (fill-range 7 0 1 1)))))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "SMALL 5 5 8",
            // start=2,end=5 writes 2,3,4 — index 1 keeps the previous 5.
            "PART 5 9 5",
            "BIG 3 3",
            // An empty range leaves index 3 as the 9 written by PART.
            "EMPTY 9",
            "FP 6",
            "ADJ 6",
            // end past the length, a negative start, a wildly out-of-range
            // end, and a non-vector: all four signal.
            "ERRORS NIL NIL NIL NIL",
        ],
    );
}
