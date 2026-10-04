// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn public_snapshots_preserve_names_values_and_bounds_across_tiers() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let program = r#"
          (defun snapshot-leaf (value)
            (egcl-debug:list-backtrace :count 64))
          (defun snapshot-parent (value) (snapshot-leaf value))
          (defun snapshot-range-inner (count start)
            (mapcar (lambda (frame) (getf frame :function))
                    (egcl-debug:list-backtrace :count count :start start)))
          (defun snapshot-range (count start)
            (snapshot-range-inner count start))
          (defun snapshot-print (value)
            (with-output-to-string (stream)
              (egcl-debug:print-backtrace :stream stream :count 64)))
          (dotimes (i 100) (snapshot-parent nil))
          (let* ((value (list "retained argument" 42))
                 (frames (snapshot-parent value))
                 (leaf (find "COMMON-LISP-USER::SNAPSHOT-LEAF" frames
                             :key (lambda (frame) (getf frame :function)) :test #'equal))
                 (parent (find "COMMON-LISP-USER::SNAPSHOT-PARENT" frames
                               :key (lambda (frame) (getf frame :function)) :test #'equal)))
            (assert leaf) (assert parent)
            (assert (< (position leaf frames) (position parent frames)))
            (when (getf leaf :arguments-available-p)
              (assert (eq value (first (getf leaf :arguments)))))
            (when (member (egcl-ext:getenv "EGCL_FORCE_TIER") '("interp" "t0") :test #'equal)
              (assert (getf leaf :arguments-available-p)))
            (assert (member (getf leaf :origin) '(:interpreted :managed)))
            (dotimes (i 1000) (list i i i))
            (when (getf leaf :arguments-available-p)
              (assert (equal '("retained argument" 42) (first (getf leaf :arguments))))))
          (let ((all (snapshot-range 64 0)) (suffix (snapshot-range 2 1)))
            (assert (= 2 (length suffix)))
            (assert (equal (subseq all 1 3) suffix)))
          (assert (null (egcl-debug:list-backtrace :count 0)))
          (assert (null (egcl-debug:list-backtrace :start 1000000)))
          (assert (handler-case (progn (egcl-debug:list-backtrace :count -1) nil)
                    (type-error () t)))
          (assert (handler-case (progn (egcl-debug:list-backtrace :start -1) nil)
                    (type-error () t)))
          (assert (search "SNAPSHOT-PRINT" (snapshot-print 42)))
          (format t "BACKTRACE-API-OK~%")
        "#;
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env_remove("EGCL_BACKEND")
            .env_remove("EGCL_DISABLE_T2")
            .output()
            .expect("run public snapshot API");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("BACKTRACE-API-OK"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
