// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

fn check(program: &str, target: &str) {
    let mut results = Vec::new();
    for raw in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0");
        if raw {
            command.env_remove("EGCL_DISABLE_RAW_FIXNUM");
        } else {
            command.env("EGCL_DISABLE_RAW_FIXNUM", "1");
        }
        let output = command.output().expect("run ordinary raw integer case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "raw={raw}\n{stdout}\n{stderr}");
        assert!(
            stderr
                .lines()
                .any(|line| line.contains(&format!("{target}:"))
                    && line.contains("installed with ordinary x86-64 Lisp T2 ABI")),
            "the actual ordinary entry was not installed:\n{stderr}"
        );
        if raw {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains(&format!("{target}:"))
                        && line.contains("ordinary x86-64 Lisp T2 emitted;")
                        && line
                            .split("raw_fixnum_values=")
                            .nth(1)
                            .and_then(|count| count.parse::<usize>().ok())
                            .is_some_and(|count| count > 0)),
                "no installed raw machine values:\n{stderr}"
            );
        }
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "ordinary raw representation changes results"
    );
}

#[test]
fn ordinary_raw_word_loop_retains_arrays_and_signed61_overflow_recovery() {
    check(
        r#"
        (defun ordinary-raw-lane (initial delta)
          (declare (type fixnum delta))
          (let ((value initial) (answer 0) (bytes (make-array 8 :element-type '(unsigned-byte 8))))
            (dotimes (i 8)
              (setf (aref bytes i) (ldb (byte 8 0) value)
                    answer (+ value delta)
                    value (ash value -8)))
            (list answer value bytes)))
        (dotimes (i 120) (ordinary-raw-lane 18446744073709551615 0))
        (assert (= 2 (egcl-ext:function-tier 'ordinary-raw-lane)))
        (let ((answer (ordinary-raw-lane 18446744073709551615 7)))
          (assert (= (first answer) 262)) (assert (= (second answer) 0))
          (dotimes (i 8) (assert (= (aref (third answer) i) 255))))
        (let ((before (egcl-ext:deopt-count))
              (answer (ordinary-raw-lane 18446744073709551615 1152921504606846975)))
          (assert (= (first answer) 1152921504606847230))
          (assert (= (second answer) 0))
          (assert (> (egcl-ext:deopt-count) before)))
        (format t "ORDINARY-RAW-OVERFLOW-OK~%")
    "#,
        "ORDINARY-RAW-LANE",
    );
}

#[test]
fn ordinary_raw_live_lane_and_tagged_neighbor_survive_collecting_callee() {
    check(
        r#"
        (defvar *ordinary-raw-effects* 0)
        (defvar *ordinary-raw-garbage* nil)
        (defvar *ordinary-raw-fail* nil)
        (declaim (notinline ordinary-raw-collect))
        (defun ordinary-raw-collect (i)
          (incf *ordinary-raw-effects*)
          (setf *ordinary-raw-garbage* (list i :collected))
          (if (and *ordinary-raw-fail* (= i 2)) 1.5 1))
        (defun ordinary-raw-held (initial token)
          (let ((value initial) (sum 0))
            (dotimes (i 8)
              (incf sum (ordinary-raw-collect i))
              (setf value (ash value -8)))
            (list value sum token)))
        (dotimes (i 120) (ordinary-raw-held 18446744073709551615 nil))
        (assert (= 2 (egcl-ext:function-tier 'ordinary-raw-held)))
        (setf *ordinary-raw-effects* 0 *ordinary-raw-fail* t)
        (let* ((token (list :live :tagged))
               (before (egcl-ext:deopt-count))
               (answer (ordinary-raw-held 18446744073709551615 token)))
          (assert (= (first answer) 0)) (assert (= (second answer) 8.5))
          (assert (eq (third answer) token)) (assert (equal token '(:live :tagged)))
          (assert (= *ordinary-raw-effects* 8))
          (assert (> (egcl-ext:deopt-count) before)))
        (format t "ORDINARY-RAW-COLLECT-OK~%")
    "#,
        "ORDINARY-RAW-HELD",
    );
}
