// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execute both versions of an analyzed integer loop. Large integers that do
//! not narrow after one shift must use the generic remainder without deopts.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn peeled_integer_loops_preserve_values_effects_and_large_integer_fallback() {
    let program = r#"
        (defvar *effects* 0)
        (defun lane-loop (initial count)
          (let ((value initial)
                (bytes (make-array (max 0 count) :element-type '(unsigned-byte 8))))
            (dotimes (i count)
              (incf *effects*)
              (setf (aref bytes i) (ldb (byte 8 0) value)
                    value (ash value -8)))
            (values bytes value)))
        (defun check-lane-loop (initial count)
          (let ((before *effects*))
            (multiple-value-bind (bytes value) (lane-loop initial count)
              (assert (= (length bytes) (max 0 count)))
              (assert (= (- *effects* before) (max 0 count)))
              (if (plusp count)
                  (progn
                    (assert (= value (ash initial (- (* 8 count)))))
                    (dotimes (i count)
                      (assert (= (aref bytes i) (ldb (byte 8 (* 8 i)) initial)))))
                  (assert (eql value initial))))))
        ;; Let actual generic execution see wide inputs and later fixnums.
        (dotimes (i 120) (check-lane-loop 18446744073709551615 8))
        (assert (= (egcl-ext:function-tier 'lane-loop) 2))
        (dolist (count '(-1 0 1 2 7 8 9 16))
          (dolist (initial '(0 1 -1 1152921504606846975 -1152921504606846976
                            18446744073709551615 18446744073709551616
                            1606938044258990275541962092341162602522202993782792835301376
                            -1606938044258990275541962092341162602522202993782792835301376))
            (check-lane-loop initial count)))
        ;; A zero-trip loop must not inspect or shift this noninteger.
        (check-lane-loop :untouched 0)
        (let ((before (egcl-ext:deopt-count)))
          (dotimes (i 40)
            (check-lane-loop 18446744073709551615 8)
            (check-lane-loop 1606938044258990275541962092341162602522202993782792835301376 8))
          (format t "STEADY ~D TIER ~D EFFECTS ~D~%"
                  (- (egcl-ext:deopt-count) before)
                  (egcl-ext:function-tier 'lane-loop) *effects*))
    "#;
    let mut outputs = Vec::new();
    for enabled in [false, true] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        cmd.args(["--no-init", "--eval", program]);
        cmd.env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            cmd.env_remove("EGCL_DISABLE_LOOP_PEELING");
        } else {
            cmd.env("EGCL_DISABLE_LOOP_PEELING", "1");
        }
        let output = cmd.output().expect("run native loop case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("STEADY 0 TIER 2"), "{stdout}\n{stderr}");
        if enabled {
            assert!(
                stderr.lines().any(|line| line
                    .contains("LANE-LOOP: type-driven loop peeling: PeelStats { loops: 1")),
                "the intended optimization did not run:\n{stderr}"
            );
        }
        outputs.push(stdout);
    }
    assert_eq!(outputs[0], outputs[1], "peeling changes observable results");
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn checked_native_case(program: &str, target: &str) {
    let mut results = Vec::new();
    for enabled in [false, true] {
        let mut cmd = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        cmd.args(["--no-init", "--eval", program])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            cmd.env_remove("EGCL_DISABLE_LOOP_PEELING");
        } else {
            cmd.env("EGCL_DISABLE_LOOP_PEELING", "1");
        }
        let out = cmd.output().expect("run native case");
        let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        if enabled {
            assert!(
                stderr.lines().any(|line| line.contains(&format!(
                    "{target}: type-driven loop peeling: PeelStats {{ loops: 1"
                ))),
                "expected loop transformation absent:\n{stderr}"
            );
        }
        results.push(stdout);
    }
    assert_eq!(results[0], results[1]);
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn peeled_loop_deopt_after_effect_resumes_exactly_once() {
    checked_native_case(
        r#"
        (defvar *effects* 0)
        (defvar *fail* nil)
        (declaim (notinline opaque-number))
        (defun opaque-number (i) (if (and *fail* (= i 2)) 1.5 1))
        (defun guarded-lane (initial count)
          (let ((value initial) (total 0))
            (dotimes (i count)
              (incf *effects*)
              (incf total (opaque-number i))
              (setf value (ash value -8)))
            (values value total)))
        (dotimes (i 120) (guarded-lane 18446744073709551615 8))
        (assert (= 2 (egcl-ext:function-tier 'guarded-lane)))
        (setf *effects* 0 *fail* t)
        (let ((before (egcl-ext:deopt-count)))
          (multiple-value-bind (value total) (guarded-lane 18446744073709551615 8)
            (assert (= value 0))
            (assert (= total 8.5))
            (assert (= *effects* 8))
            (assert (> (egcl-ext:deopt-count) before))
            (format t "DEOPT-RESULT ~A EFFECTS ~D VALUE ~D~%" total *effects* value)))
    "#,
        "GUARDED-LANE",
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn peeled_loop_early_exits_preserve_prefix_suffix_and_carried_results() {
    checked_native_case(
        r#"
        (defvar *effects* 0)
        (defun short-lane (initial count stop)
          (let ((value initial) (written 0)
                (bytes (make-array (max 0 count) :element-type '(unsigned-byte 8) :initial-element 17)))
            (dotimes (i count)
              (when (= i stop) (return))
              (incf *effects*)
              (setf (aref bytes i) (ldb (byte 8 0) value)
                    value (ash value -8))
              (incf written))
            (values bytes value written)))
        (dotimes (i 120) (short-lane 18446744073709551615 8 -1))
        (assert (= 2 (egcl-ext:function-tier 'short-lane)))
        (setf *effects* 0)
        (dolist (stop '(-1 0 1 3 8 9))
          (multiple-value-bind (bytes value written) (short-lane 18446744073709551615 8 stop)
            (assert (= written (if (and (>= stop 0) (< stop 8)) stop 8)))
            (assert (= value (ash 18446744073709551615 (- (* 8 written)))))
            (dotimes (i 8) (assert (= (aref bytes i) (if (< i written) 255 17))))))
        (multiple-value-bind (bytes value written) (short-lane :untouched 0 0)
          (assert (= (length bytes) 0)) (assert (= written 0)) (assert (eql value :untouched)))
        (format t "EARLY-EXIT-EFFECTS ~D~%" *effects*)
    "#,
        "SHORT-LANE",
    );
}
