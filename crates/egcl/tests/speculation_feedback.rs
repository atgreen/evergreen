// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Numeric feedback through real mapped T2 entry, including a loop-entry guard
//! hoisted from ASH. Keep the lane/output shape of Ironclad's Keccak extraction.

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn mixed_lane_shift_recovers_to_stable_native_t2() {
    let program = r#"
        (defun extract-lane (state)
          (let ((output (make-array 8 :element-type '(unsigned-byte 8))))
            (loop with value = (aref state 0)
                  for index from 0 below 8
                  do (setf (aref output index) (ldb (byte 8 0) value)
                           value (ash value -8)))
            output))
        (defun check-lane (state byte)
          (let ((output (extract-lane state)))
            (dotimes (j 8) (assert (= (aref output j) byte)))))
        (let ((small (make-array 1 :initial-element 0))
              (large (make-array 1 :element-type '(unsigned-byte 64)
                                   :initial-element 18446744073709551615)))
          ;; Establish actual fixnum specialization, then invalidate its hoisted
          ;; lane guard with a bignum that becomes a fixnum after one shift.
          (dotimes (i 40) (check-lane small 0))
          (format t "initial=~D~%" (egcl-ext:function-tier 'extract-lane))
          (let ((before (egcl-ext:deopt-count)))
            (dotimes (i 100) (check-lane large 255))
            (assert (> (egcl-ext:deopt-count) before)))
          (let ((before (egcl-ext:deopt-count)))
            (dotimes (i 100)
              (check-lane large 255)
              (check-lane small 0))
            (format t "steady=~D tier=~D~%" (- (egcl-ext:deopt-count) before)
                    (egcl-ext:function-tier 'extract-lane))))
    "#;
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_DEOPT_BLACKLIST_THRESHOLD", "3")
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.lines().any(|line| line == "initial=2"), "{stdout}");
    assert!(
        stdout.lines().any(|line| line == "steady=0 tier=2"),
        "{stdout}"
    );
}
