// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A counted integer-to-octet loop must write exactly the same bytes at every
//! tier, on every architecture.
//!
//! ARCHITECTURE-AGNOSTIC ON PURPOSE, unlike `octet_word_fusion_cli.rs` and
//! `bounded_octet_fallback.rs`, which are both `#![cfg(target_arch =
//! "x86_64")]`. Those gave the AArch64 kernel **zero** coverage — they report
//! "0 passed" there — which is why this file exists: one program runs at all
//! four tiers and the bytes are compared, so it exercises whichever emitter the
//! test binary was built for.
//!
//! `octet_word_fusion` replaces the loop with a single unaligned 64-bit write
//! into an untraced byte payload. That makes a bug here memory corruption
//! rather than a wrong answer, so the cases are chosen for the ways the kernel
//! can write the wrong bytes or the wrong number of them:
//!
//!   * **A full eight-byte span and a partial one take different paths.** The
//!     partial path walks bytes while streaming the sign extension in from
//!     above (`EXTR` on AArch64, `SHRD` on x86). This fixture samples widths
//!     1, 3 and 7 in addition to the full eight-byte case.
//!   * **Negative integers are the delicate half.** A negative one-limb bignum
//!     is negated, which modulo 2^64 gives the low two's-complement octets; a
//!     negative fixnum sign-extends. Both must write the same bytes `LDB`
//!     would; negative values are checked in the sampled cases.
//!   * **A bignum magnitude must be CANONICAL.** A positive one is at least
//!     2^60 and a negative one strictly greater than `abs(MIN-FIXNUM)`, because
//!     anything smaller would have been a fixnum. Values straddling that
//!     boundary are included.
//!   * **A multi-limb bignum must take the NIL exit** and still write correctly
//!     through the retained scalar loop.
//!   * Valid spans exercise exact-length and nonzero-start boundaries.
//!     Out-of-bounds and short-array failure cases need separate coverage.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [&str; 4] = ["interp", "t0", "t1", "t2"];

/// The loop the pass fuses, in the shape it actually matches: a `tagbody` that
/// stores `(ldb (byte 8 0) lane)` and shifts `lane` by -8 each iteration.
/// Returns the bytes written plus the final lane, so a wrong shift or a wrong
/// byte count both show up in the compared output.
const DRIVER: &str = r#"
  (defun octets (array start end value)
    (let ((index start) (lane value))
      (tagbody
        again
        (if (>= index end) (go done))
        (setf (aref array index) (ldb (byte 8 0) lane))
        (setq lane (ash lane -8))
        (setq index (1+ index))
        (go again)
        done)
      lane))
  (defun run (len start end value)
    (let ((a (make-array len :element-type '(unsigned-byte 8) :initial-element 99)))
      (let ((final (octets a start end value)))
        (list (coerce a 'list) final))))
"#;

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

/// Assert candidate recognition. This diagnostic precedes final emission and
/// does not independently prove native installation or kernel execution.
#[test]
fn octet_loop_is_recognized_for_fusion() {
    let program = format!(
        "{DRIVER}
         (dotimes (i 4000) (run 16 0 8 123456789))
         (format t \"HOT ~a~%}}\" (second (run 16 0 8 123456789)))"
    )
    .replace("}}", "");
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", &program])
        .env("EGCL_T2_LOG", "stderr")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    assert!(
        stderr.lines().any(|l| l.contains("octet word stores:")),
        "the loop was not recognized for octet word fusion:\n{stderr}"
    );
}

#[test]
fn octet_spans_agree_at_every_tier() {
    let program = format!(
        r#"{DRIVER}
      ;; Warm on both the full-eight and a partial span so each path compiles.
      (dotimes (i 4000) (run 16 0 8 123456789) (run 16 0 3 123456789))
      ;; A full eight-byte span, positive and negative, fixnum and bignum.
      (format t "FULL8+ ~a~%" (run 8 0 8 123456789))
      (format t "FULL8- ~a~%" (run 8 0 8 -123456789))
      (format t "BIG8+ ~a~%" (run 8 0 8 (+ (expt 2 62) 12345)))
      (format t "BIG8- ~a~%" (run 8 0 8 (- (+ (expt 2 62) 12345))))
      ;; Sampled partial widths, where sign extension streams in from above.
      (format t "P1 ~a~%" (run 8 0 1 -1))
      (format t "P3 ~a~%" (run 8 0 3 -123456789))
      (format t "P7 ~a~%" (run 8 0 7 -123456789))
      ;; A non-zero start, so the byte cursor arithmetic is exercised.
      (format t "OFF ~a~%" (run 12 3 11 123456789))
      ;; Canonicality boundaries: 2^60 is the smallest bignum magnitude.
      (format t "MINBIG ~a~%" (run 8 0 8 (expt 2 60)))
      (format t "MINBIG- ~a~%" (run 8 0 8 (- (expt 2 60))))
      ;; A multi-limb bignum must take the NIL exit and still be correct.
      (format t "MULTI ~a~%" (run 8 0 8 (+ (expt 2 130) 7)))
      ;; An empty span writes nothing and returns the value unchanged.
      (format t "EMPTY ~a~%" (run 4 2 2 42))"#
    );
    // Expected bytes computed from CL's own LDB/ASH semantics: the little-endian
    // two's-complement octets, and the lane shifted right by 8 per byte.
    agrees_at_every_tier(
        &program,
        &[
            "FULL8+ ((21 205 91 7 0 0 0 0) 0)",
            "FULL8- ((235 50 164 248 255 255 255 255) -1)",
            "BIG8+ ((57 48 0 0 0 0 0 64) 0)",
            "BIG8- ((199 207 255 255 255 255 255 191) -1)",
            "P1 ((255 99 99 99 99 99 99 99) -1)",
            "P3 ((235 50 164 99 99 99 99 99) -8)",
            "P7 ((235 50 164 248 255 255 255 99) -1)",
            "OFF ((99 99 99 21 205 91 7 0 0 0 0 99) 0)",
            "MINBIG ((0 0 0 0 0 0 0 16) 0)",
            "MINBIG- ((0 0 0 0 0 0 0 240) -1)",
            "MULTI ((7 0 0 0 0 0 0 0) 73786976294838206464)",
            "EMPTY ((99 99 99 99) 42)",
        ],
    );
}
