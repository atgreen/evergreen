// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! An AREF, (SETF AREF) or (SETF SBIT) whose array the native access does not
//! cover -- an adjustable fill-pointer vector, a bit vector -- calls the
//! general accessor from T2 instead of deoptimizing (bliss-gg3i1.7.8). The
//! access used to fail its guard on every call; after two retired versions
//! the function was declined T2 for good, so SBCL's FIND-SOURCE-ROOT,
//! NOTE-CONFLICTS and ADD-LOCATION-CONFLICTS ran at T1 or T0. Each function
//! here must reach T2 once, never deoptimize, and keep every error.

use std::process::Command;

const PROGRAM: &str = r#"
(defvar *adjustable* (make-array 10 :adjustable t :fill-pointer 10 :initial-element 7))
(defvar *simple* (make-array 10 :initial-element 3))
(defvar *octets* (make-array 10 :element-type '(unsigned-byte 8) :initial-element 2))
(defvar *bits* (make-array 64 :element-type 'bit :initial-element 0))
(defun fb-ref (vector index) (aref vector index))
(defun fb-set (vector index value) (setf (aref vector index) value))
(defun fb-bit-set (vector index) (setf (sbit vector index) 1))
(defun fb-drive (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (incf sum (fb-ref *adjustable* (mod i 10)))
      (incf sum (fb-ref *simple* (mod i 10)))
      (incf sum (fb-ref *octets* (mod i 10)))
      (fb-set *adjustable* (mod i 10) 7)
      (fb-set *simple* (mod i 10) 3)
      (fb-set *octets* (mod i 10) 2)
      (fb-bit-set *bits* (mod i 64)))))
(defun fb-tiers ()
  (mapcar #'egcl-ext:function-tier '(fb-ref fb-set fb-bit-set)))
;; Promotion is compiled in the background: give it a bounded time to land.
;; FUNCTION-TIER is 0 for T0, 1 for T1 and 2 for T2.
(dotimes (round 400) (fb-drive 500) (when (every (lambda (tier) (= tier 2)) (fb-tiers)) (return)) (sleep 0.025))
(dotimes (round 20) (fb-drive 500))
(format t "TIERS ~S~%" (fb-tiers))
(format t "RESULT ~S ~S ~S~%" (fb-drive 10) (count 1 *bits*) (fill-pointer *adjustable*))
(format t "ERRORS ~S ~S ~S ~S ~S~%"
        (handler-case (fb-ref *adjustable* 20) (type-error () :type-error))
        (handler-case (fb-set *simple* 20 1) (type-error () :type-error))
        (handler-case (fb-set *octets* 0 256) (type-error () :type-error))
        (handler-case (fb-bit-set *bits* 100) (type-error () :type-error))
        (handler-case (fb-ref 5 0) (type-error () :type-error)))
;; A fill-pointer vector's AREF reads past the fill pointer, up to its capacity.
(setf (fill-pointer *adjustable*) 2)
(format t "CAPACITY ~S ~S~%" (fb-ref *adjustable* 9) (fb-set *adjustable* 8 11))
(format t "AFTER ~S ~S~%" (aref *adjustable* 8) (aref *octets* 0))
"#;

#[test]
fn unsupported_array_layouts_call_the_accessor_from_t2() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")])
        .env("EGCL_T2_LOG", "1")
        .env("EGCL_DEOPT_LOG", "1")
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_DISABLE_ARRAY_CALL_FALLBACK")
        .env_remove("EGCL_GC_STRESS")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT 120 64 10"), "{stdout}");
    assert!(
        stdout.contains("ERRORS :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR"),
        "{stdout}"
    );
    assert!(stdout.contains("CAPACITY 7 11"), "{stdout}");
    assert!(stdout.contains("AFTER 11 2"), "{stdout}");
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    assert!(stdout.contains("TIERS (2 2 2)"), "{stdout}\n{stderr}");
    let log: Vec<&str> = stdout.lines().chain(stderr.lines()).collect();
    for name in ["FB-REF", "FB-SET", "FB-BIT-SET"] {
        let installs = log
            .iter()
            .filter(|line| {
                line.contains(&format!("[T2] {name}: optimized worker result installed"))
            })
            .count();
        assert_eq!(installs, 1, "{name} installed {installs} times\n{stderr}");
        assert!(
            log.iter()
                .any(|line| line.contains(&format!("[T2] {name}: array access call fallbacks: "))),
            "{name} kept no call fallback\n{stderr}"
        );
        // Even the errors above are signalled by the retained call.
        assert!(
            !log.iter()
                .any(|line| line.contains(&format!("{name}: native guard deopt"))),
            "{name} deoptimized\n{stderr}"
        );
    }
}
