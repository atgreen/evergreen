// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Real native execution, rather than representation metadata alone. The test
//! process is capped by the caller's standard egcl-limited cargo-test wrapper.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn differential(program: &str, target: Option<&str>, force_t2: bool) {
    let mut results = Vec::new();
    for enabled in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_T2_THREADS", "1");
        if force_t2 {
            command
                .env("EGCL_LAZY_COMPILE", "0")
                .env("EGCL_FORCE_TIER", "t2");
        } else {
            command
                .env_remove("EGCL_FORCE_TIER")
                .env("EGCL_OSR_THRESHOLD", "20")
                .env("EGCL_T0_T1_THRESHOLD", "1000000");
        }
        if enabled {
            command.env_remove("EGCL_DISABLE_RAW_FIXNUM");
        } else {
            command.env("EGCL_DISABLE_RAW_FIXNUM", "1");
        }
        let output = command.output().expect("run raw fixnum regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "raw={enabled}\n{stdout}\n{stderr}");
        if enabled {
            if let Some(name) = target {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: raw fixnum values: "))
                        && !line.ends_with(": 0")),
                    "raw pass did not produce machine values:\n{stderr}"
                );
                assert!(stderr.lines().any(|line| line.contains(&format!("{name}:"))
                    && line.contains(" T2 emitted;")
                    && line.split("raw_fixnum_values=").nth(1)
                        .and_then(|count| count.parse::<usize>().ok()).is_some_and(|count| count > 0)),
                    "raw candidate was not installed:\n{stderr}");
            }
        }
        results.push(stdout);
    }
    assert_eq!(
        results[0], results[1],
        "raw representations change Lisp results"
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_raw_integer_boundaries_preserve_arithmetic_shift_and_bignum_overflow() {
    differential(
        r#"
        (defun raw-arithmetic (initial delta)
          (declare (type fixnum delta))
          (let ((value initial) (answer 0))
            (dotimes (i 8 answer)
              (setf answer (+ value delta))
              (setf value (ash value -8)))))
        (dotimes (i 120) (raw-arithmetic 18446744073709551615 0))
        (assert (= 2 (egcl-ext:function-tier 'raw-arithmetic)))
        (dolist (delta '(0 7 -7 281474976710656 -281474976710656 -1152921504606846976))
          (let ((answer (raw-arithmetic 18446744073709551615 delta)))
            (assert (= answer (+ (ash 18446744073709551615 -56) delta)))
            (format t "RAW-ANSWER ~D ~D~%" delta answer)))
        ;; After the first shift the lane fits a fixnum; adding MAX crosses
        ;; signed61 without signed64 overflow. Recovery follows the peeled prefix.
        (let ((before (egcl-ext:deopt-count)))
          (assert (= (raw-arithmetic 18446744073709551615 1152921504606846975) 1152921504606847230))
          (assert (> (egcl-ext:deopt-count) before)))
        (format t "RAW-BOUNDARIES-OK~%")
    "#,
        Some("RAW-ARITHMETIC"),
        true,
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_raw_loop_reboxes_live_values_after_effect_and_collecting_call() {
    differential(
        r#"
        (defvar *raw-effects* 0)
        (defvar *raw-fail* nil)
        (defvar *raw-garbage* nil)
        (declaim (notinline raw-opaque-number))
        (defun raw-opaque-number (i)
          (setf *raw-garbage* (list i :allocated))
          (if (and *raw-fail* (= i 2)) 1.5 1))
        (defun raw-carried-loop (initial count token)
          (let ((value initial) (total 0))
            (dotimes (i count)
              (incf *raw-effects*)
              (incf total (raw-opaque-number i))
              (setf value (ash value -8)))
            (values value total token)))
        (dotimes (i 120) (raw-carried-loop 18446744073709551615 8 nil))
        (assert (= 2 (egcl-ext:function-tier 'raw-carried-loop)))
        (setf *raw-effects* 0 *raw-fail* t)
        (let ((before (egcl-ext:deopt-count)) (token (list :live :tagged :neighbor)))
          (multiple-value-bind (value total returned) (raw-carried-loop 18446744073709551615 8 token)
            (assert (= value 0)) (assert (= total 8.5))
            (assert (= *raw-effects* 8))
            (assert (eq returned token))
            (assert (equal returned '(:live :tagged :neighbor)))
            (assert (> (egcl-ext:deopt-count) before))
            (format t "RAW-RECOVERY ~D ~A ~D ~S~%" value total *raw-effects* returned)))
    "#,
        Some("RAW-CARRIED-LOOP"),
        true,
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn raw_integer_pass_preserves_cold_native_osr_entry_and_result() {
    differential(
        r#"
        (defun raw-osr-sum (n)
          (declare (type fixnum n))
          (let ((sum 0))
            (declare (type fixnum sum))
            (dotimes (i n sum) (incf sum i))))
        (assert (= (raw-osr-sum 100000) 4999950000))
        (assert (> (egcl-ext:function-osr-count 'raw-osr-sum) 0))
        (format t "RAW-OSR-OK~%")
    "#,
        None,
        false,
    );
}
