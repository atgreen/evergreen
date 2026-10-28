// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `AREF`, `SVREF`, `ELT` and `(SETF AREF)` must give the same answers at every
//! tier, on every architecture — and must still signal for every operand the
//! inline layouts do not cover.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `bitwise_shift_tiers.rs`: one program
//! runs at all four tiers and the answers are compared, so the file exercises
//! whichever emitter the test binary was built for.
//!
//! `array_access` rewrites reads AND stores in the same function, so an emitter
//! that handles only some of them loses the whole function the first time the
//! other appears. That makes this the all-or-nothing family, and the cases
//! below are the ones where a mis-probed layout gives a WRONG ANSWER rather
//! than a crash:
//!
//!   * **A simple vector and a packed `(unsigned-byte 8)` array have different
//!     metadata offsets and element widths.** Reading a byte array with the
//!     vector path returns eight bytes of packed data as if it were one tagged
//!     word; reading a vector with the byte path returns the low byte of a
//!     tagged pointer. Both are plausible-looking fixnums.
//!   * **The header's low halfword selects the metadata offset.** `0xFFFF` means
//!     the extended header, whose metadata starts at +16 rather than +8.
//!     Assuming either one reads the length from the wrong word, which passes
//!     small indices and fails near the end — so every array here is exercised
//!     at index 0 AND at its last index.
//!   * **A negative index must be rejected separately from the upper bound**,
//!     because the length is compared signed: without its own test, -1 passes
//!     the upper comparison and addresses behind the array.
//!   * **A packed byte store must range-check the value.** Storing 256 or -1
//!     into a byte array must signal, not truncate to 0 or 255.
//!   * **A fill-pointer, adjustable or displaced array is not a simple
//!     layout** and must reach the ordinary call.
//!   * **A packed `double-float` array must be probed too.** On a target that
//!     deopted there instead, this case would still answer correctly — it is
//!     here so the layout keeps working, with the per-call cost tracked
//!     separately.

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

/// The accessors must actually reach the OPTIMISING tier, or this file proves
/// nothing about the emitter arm.
///
/// This is not a hypothetical: the first version of this test wrapped each
/// access in a one-line function called with a freshly built vector, and
/// `EGCL_T2_LOG` showed those functions were never even CONSIDERED for T2.
/// Both tests passed on hardware while the new arm had a bug that faulted the
/// moment a real loop reached it. Admission is asserted here so that cannot
/// recur.
#[test]
fn the_accessors_reach_the_optimising_tier() {
    let program = r#"
      (defvar *v* (make-array 64 :initial-element 3))
      (defvar *b* (make-array 64 :element-type '(unsigned-byte 8) :initial-element 3))
      (defun hot-read (n) (let ((s 0) (v *v*)) (dotimes (i n) (setq s (svref v 7))) s))
      (defun hot-aref (n) (let ((s 0) (v *v*)) (dotimes (i n) (setq s (aref v 7))) s))
      (defun hot-write (n) (let ((v *v*)) (dotimes (i n) (setf (aref v 7) 3)) 0))
      (defun hot-u8 (n) (let ((s 0) (v *b*)) (dotimes (i n) (setq s (aref v 7))) s))
      (defun hot-u8-write (n) (let ((v *b*)) (dotimes (i n) (setf (aref v 7) 3)) 0))
      (dotimes (i 3000) (hot-read 20) (hot-aref 20) (hot-write 20)
                        (hot-u8 20) (hot-u8-write 20))
      (format t "HOT ~a ~a ~a ~a ~a~%"
              (hot-read 100) (hot-aref 100) (hot-write 100)
              (hot-u8 100) (hot-u8-write 100))
    "#;
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .env("EGCL_T2_LOG", "stderr")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}
{stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "HOT 3 3 0 3 0"),
        "wrong answers:
{stdout}
{stderr}"
    );
    // The marker differs by ABI, which is the point of accepting either: the
    // framed emitters (AArch64 and the other checked ports) say "T2 EMITTED",
    // and x86-64's mapped entry says "installed with ordinary x86-64 Lisp T2
    // ABI". Matching only one of them makes this assertion silently vacuous on
    // the other architecture, which is the failure it exists to prevent.
    for name in ["HOT-READ", "HOT-AREF", "HOT-WRITE", "HOT-U8", "HOT-U8-WRITE"] {
        let reached = stderr.lines().any(|line| {
            line.contains(&format!("{name}:"))
                && (line.contains("T2 EMITTED") || line.contains("Lisp T2 ABI"))
        });
        assert!(
            reached,
            "{name} never reached T2, so its element access was never emitted \
             natively and the rest of this file proves nothing:\n{stderr}"
        );
    }
}

#[test]
fn vector_element_access_agrees_at_every_tier() {
    let program = r#"
      ;; Each accessor is driven through a HOT INNER LOOP over a stable array,
      ;; which is the shape `array_access` specialises -- see
      ;; `the_accessors_reach_the_optimising_tier`.
      (defun r-aref  (v i) (let ((x nil)) (dotimes (k 12) (setq x (aref v i))) x))
      (defun r-svref (v i) (let ((x nil)) (dotimes (k 12) (setq x (svref v i))) x))
      (defun r-elt   (v i) (let ((x nil)) (dotimes (k 12) (setq x (elt v i))) x))
      (defun w-aref  (v i x) (let ((r nil)) (dotimes (k 12) (setq r (setf (aref v i) x))) r))
      (let ((wv (vector 1 2 3))
            (wb (make-array 3 :element-type '(unsigned-byte 8) :initial-element 1)))
        (dotimes (i 3000)
          (r-aref wv 1) (r-svref wv 1) (r-elt wv 1) (w-aref wv 1 9)
          (r-aref wb 1) (w-aref wb 1 9)))
      (let* ((v (vector 10 20 30))
             ;; Big enough to use the extended header, so BOTH metadata
             ;; offsets are exercised.
             (big (make-array 40 :initial-element 7))
             (b (make-array 4 :element-type '(unsigned-byte 8)
                              :initial-contents '(1 2 3 4)))
             (bigb (make-array 40 :element-type '(unsigned-byte 8)
                                  :initial-element 5))
             (d (make-array 3 :element-type 'double-float
                              :initial-contents '(1.0d0 2.0d0 3.0d0)))
             (fp (make-array 4 :fill-pointer 2 :initial-contents '(1 2 3 4)))
             (adj (make-array 3 :adjustable t :initial-contents '(1 2 3))))
        ;; Reads at index 0 and at the LAST index of each layout.
        (format t "VEC ~a ~a ~a~%" (r-aref v 0) (r-aref v 2) (r-svref v 2))
        (format t "BIGVEC ~a ~a~%" (r-aref big 0) (r-aref big 39))
        (format t "U8 ~a ~a ~a~%" (r-aref b 0) (r-aref b 3) (r-elt b 3))
        (format t "BIGU8 ~a ~a~%" (r-aref bigb 0) (r-aref bigb 39))
        (format t "F64 ~a ~a~%" (r-aref d 0) (r-aref d 2))
        ;; Non-simple layouts must reach the ordinary call.
        (format t "FP ~a ~a~%" (r-aref fp 0) (r-aref fp 1))
        (format t "ADJ ~a ~a~%" (r-aref adj 0) (r-aref adj 2))
        ;; Stores, then read back.
        (w-aref v 1 99) (w-aref big 39 8) (w-aref b 3 200) (w-aref bigb 39 6)
        (format t "WROTE ~a ~a ~a ~a~%"
                (r-aref v 1) (r-aref big 39) (r-aref b 3) (r-aref bigb 39))
        ;; A simple vector takes any object; a byte array does not.
        (w-aref v 0 'sym)
        (format t "ANY ~a~%" (r-aref v 0))
        (format t "ERRORS ~a ~a ~a ~a ~a ~a~%"
                (nth-value 0 (ignore-errors (r-aref v 3)))
                (nth-value 0 (ignore-errors (r-aref v -1)))
                (nth-value 0 (ignore-errors (r-aref b 4)))
                (nth-value 0 (ignore-errors (w-aref b 0 256)))
                (nth-value 0 (ignore-errors (w-aref b 0 -1)))
                (nth-value 0 (ignore-errors (r-aref 7 0)))))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "VEC 10 30 30",
            "BIGVEC 7 7",
            "U8 1 4 4",
            "BIGU8 5 5",
            "F64 1.0d0 3.0d0",
            // The fill pointer is 2, so only indices 0 and 1 are visible.
            "FP 1 2",
            "ADJ 1 3",
            "WROTE 99 8 200 6",
            "ANY SYM",
            // Past the end, negative, past a byte array's end, a value above
            // 255, a negative value, and a non-array: all six signal.
            "ERRORS NIL NIL NIL NIL NIL NIL",
        ],
    );
}

/// The access dereferences a tagged array pointer the collector moves, and the
/// address it computes is an interior pointer that must not survive a
/// safepoint. Allocating hard between the accesses is what makes either
/// mistake visible — as a wrong element, so the values are checked rather than
/// only the exit status.
#[test]
fn vector_access_survives_collections_at_every_tier() {
    let program = r#"
      (defun r (v i) (aref v i))
      (defun w (v i x) (setf (aref v i) x))
      (defun churn () (let ((x nil)) (dotimes (i 200) (setq x (cons i x))) (length x)))
      (let ((ok t))
        (dotimes (i 3000)
          (let ((v (vector 1 2 3))) (r v 1) (w v 1 2)))
        (dotimes (i 500)
          (churn)
          ;; Fresh nursery arrays each iteration, so they really relocate.
          (let ((v (make-array 8 :initial-element 3))
                (b (make-array 8 :element-type '(unsigned-byte 8) :initial-element 4)))
            (w v 7 (+ i 1))
            (w b 7 (logand i 255))
            (unless (and (eql (r v 7) (+ i 1)) (eql (r v 0) 3)
                         (eql (r b 7) (logand i 255)) (eql (r b 0) 4)
                         (null (nth-value 0 (ignore-errors (r v 8)))))
              (setq ok nil))))
        (format t "STABLE ~s~%" ok))
    "#;
    agrees_at_every_tier(program, &["STABLE T"]);
}
