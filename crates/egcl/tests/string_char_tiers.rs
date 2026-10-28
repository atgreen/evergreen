// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `CHAR`, `SCHAR` and `ELT` on a string must give the same answers at every
//! tier, on every architecture — and must still signal for everything the
//! inline fast path does not handle.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, like `bitwise_shift_tiers.rs`: one program
//! runs at all four tiers and the answers are compared, so the file exercises
//! whichever emitter the test binary was built for.
//!
//! `StringCharOrNil` is unlike every guard in these emitters: it NEVER
//! deoptimizes and never signals. It answers NIL for anything it does not
//! handle, and the pass that inserts it branches on that to the ORIGINAL call,
//! which still carries its original frame state. So the cases below are
//! mostly about the NIL exits — each is a shape where a fast path that
//! answered a character instead of NIL would be silently wrong rather than
//! slow:
//!
//!   * **A fill-pointer or adjustable string is not a simple string.** Its
//!     layout is `COMPLEX_ARRAY`, so the type-id test must reject it and let
//!     the call read through the displacement. A fast path keyed only on "is a
//!     heap object" reads the wrong word entirely.
//!   * **Both simple layouts must work.** `SIMPLE_BASE_STRING` stores one byte
//!     per character and `SIMPLE_CHARACTER_STRING` a native-endian `u32`, so a
//!     wide string read with the byte path answers the low byte of the code —
//!     which is the *correct* character for every ASCII test case and wrong
//!     only above U+00FF. The non-ASCII case is what catches it.
//!   * **An out-of-range or negative index must signal**, not read out of
//!     bounds. The index is bounds-checked against the raw length at +8 after
//!     being untagged; getting the tagged/raw distinction wrong passes small
//!     indices and fails near the end of the string.
//!   * **A non-string, and a non-fixnum index, must signal** with the error the
//!     ordinary call produces.
//!   * **`ELT` also accepts lists and vectors**, which the string fast path
//!     must decline rather than mis-read.

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
fn string_character_access_agrees_at_every_tier() {
    let program = r#"
      (defun f-char  (s i) (char s i))
      (defun f-schar (s i) (schar s i))
      (defun f-elt   (s i) (elt s i))
      (dotimes (i 3000) (f-char "abc" 1) (f-schar "abc" 1) (f-elt "abc" 1))
      (let* ((simple "abcdef")
             (wide (make-array 3 :element-type 'character
                                 :initial-contents (list #\a (code-char 955) #\z)))
             (fp (make-array 6 :element-type 'character :fill-pointer 3
                               :initial-contents "abcdef"))
             (lst '(#\x #\y #\z))
             (vec (vector 10 20 30)))
        ;; Simple base string, including both ends.
        (format t "SIMPLE ~s ~s ~s~%"
                (f-char simple 0) (f-char simple 3) (f-char simple 5))
        (format t "SCHAR ~s ~s~%" (f-schar simple 0) (f-schar simple 5))
        (format t "ELT ~s ~s~%" (f-elt simple 0) (f-elt simple 5))
        ;; A wide string: the middle character is above U+00FF, so a byte-width
        ;; read would answer the low byte of its code instead.
        (format t "WIDE ~s ~s ~s~%" (f-char wide 0) (f-char wide 1) (f-char wide 2))
        (format t "WIDE-CODE ~s~%" (char-code (f-char wide 1)))
        ;; A fill-pointer string is a COMPLEX_ARRAY, not a simple layout.
        (format t "FILL ~s ~s~%" (f-char fp 0) (f-char fp 2))
        ;; ELT over a list and a vector, which the string path must decline.
        (format t "ELT-SEQ ~s ~s ~s~%" (f-elt lst 0) (f-elt lst 2) (f-elt vec 1))
        ;; Every NIL exit must still signal, with the ordinary call's error.
        (format t "ERRORS ~s ~s ~s ~s ~s~%"
                (nth-value 0 (ignore-errors (f-char simple 6)))
                (nth-value 0 (ignore-errors (f-char simple -1)))
                (nth-value 0 (ignore-errors (f-char simple 'x)))
                (nth-value 0 (ignore-errors (f-char 7 0)))
                (nth-value 0 (ignore-errors (f-char fp 5)))))
    "#;
    agrees_at_every_tier(
        program,
        &[
            "SIMPLE #\\a #\\d #\\f",
            "SCHAR #\\a #\\f",
            "ELT #\\a #\\f",
            "WIDE-CODE 955",
            // The fill pointer is 3, so index 2 is the last visible character.
            "FILL #\\a #\\c",
            "ELT-SEQ #\\x #\\z 20",
            // Past the end, negative, a non-fixnum index, a non-string, and
            // past a fill pointer: all five signal.
            "ERRORS NIL NIL NIL NIL NIL",
        ],
    );
}

/// The access reads through a tagged string pointer the collector moves.
/// Allocating hard between the reads is what makes a stale base visible — as a
/// wrong character, which is why the value is checked and not just the exit.
#[test]
fn string_access_survives_collections_at_every_tier() {
    let program = r#"
      (defun f-char (s i) (char s i))
      (defun churn () (let ((x nil)) (dotimes (i 200) (setq x (cons i x))) (length x)))
      (let ((ok t))
        (dotimes (i 3000) (f-char "abc" 1))
        (dotimes (i 1000)
          (churn)
          ;; A fresh nursery string each time, so the object really moves.
          (let ((s (copy-seq "abcdef")))
            (unless (and (eql (f-char s 0) #\a) (eql (f-char s 5) #\f))
              (setq ok nil))))
        (format t "STABLE ~s~%" ok))
    "#;
    agrees_at_every_tier(program, &["STABLE T"]);
}
