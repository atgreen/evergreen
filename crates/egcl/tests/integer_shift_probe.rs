// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

#[test]
fn native_integer_shift_probe_preserves_wide_fallback_errors_and_multiple_values() {
    let program = r#"
        (defun probed-integer-shift (value) (ash value -8))
        (defun probed-huge-shift (value) (ash value -1152921504606846976))
        (dotimes (i 120)
          (probed-integer-shift 18446744073709551615)
          (probed-huge-shift -18446744073709551615))
        (assert (= 2 (egcl-ext:function-tier 'probed-integer-shift)))
        (assert (= 2 (egcl-ext:function-tier 'probed-huge-shift)))
        (dolist (value '(0 1 -1 257 -257 1152921504606846975 -1152921504606846976
                        18446744073709551615 -18446744073709551615
                        18446744073709551616 -18446744073709551616
                        1606938044258990275541962092341162602522202993782792835301376
                        -1606938044258990275541962092341162602522202993782792835301376))
          (let ((answer (probed-integer-shift value)))
            (assert (= answer (floor value 256)))
            (assert (= (probed-huge-shift value) (if (minusp value) -1 0)))
            (assert (equal (multiple-value-list (probed-integer-shift value)) (list answer)))
            (format t "SHIFT ~D ~D~%" value answer)))
        (assert (equal (multiple-value-call #'list (values 1 2) (probed-integer-shift 257)) '(1 2 1)))
        (dolist (value '(:not-an-integer 1.5 1/2))
          (let ((caught nil))
            (handler-case (probed-integer-shift value) (type-error () (setf caught t)))
            (assert caught)))
        ;; Unsupported multi-limb values take a real call, not a deopt that
        ;; invalidates the optimized function on every invocation.
        (let ((before (egcl-ext:deopt-count)))
          (dotimes (i 40)
            (probed-integer-shift 1606938044258990275541962092341162602522202993782792835301376)
            (probed-integer-shift -1606938044258990275541962092341162602522202993782792835301376))
          (assert (= before (egcl-ext:deopt-count))))
        ;; The probe's boolean remains live while its shifted result is dead.
        ;; DCE must preserve type checks, ordered effects and scalar MV reset.
        (defvar *probed-discarded-effects* 0)
        (defun probed-discarded-shift (value)
          (incf *probed-discarded-effects*)
          (ash value -8)
          42)
        (defun probed-discarded-shift-mv (value)
          (values 7 8)
          (ash value -8)
          42)
        (dotimes (i 120)
          (assert (= (probed-discarded-shift 18446744073709551615) 42))
          (assert (equal (multiple-value-list
                          (probed-discarded-shift-mv 18446744073709551615)) '(42))))
        (assert (= 2 (egcl-ext:function-tier 'probed-discarded-shift)))
        (assert (= 2 (egcl-ext:function-tier 'probed-discarded-shift-mv)))
        (setf *probed-discarded-effects* 0)
        (dolist (value '(0 -257 18446744073709551615
                        1606938044258990275541962092341162602522202993782792835301376))
          (assert (= (probed-discarded-shift value) 42))
          (assert (equal (multiple-value-list (probed-discarded-shift-mv value)) '(42))))
        (dolist (value '(:bad 1.5 1/2))
          (let ((caught nil))
            (handler-case (probed-discarded-shift value)
              (type-error () (setf caught t)))
            (assert caught))
          (let ((caught nil))
            (handler-case (probed-discarded-shift-mv value)
              (type-error () (setf caught t)))
            (assert caught)))
        (assert (= *probed-discarded-effects* 7))
        (format t "DISCARDED-SHIFT-OK EFFECTS ~D~%" *probed-discarded-effects*)
        (format t "SHIFT-PROBE-SEMANTICS-OK~%")
    "#;
    let mut outputs = Vec::new();
    for enabled in [false, true] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            command.env_remove("EGCL_DISABLE_INTEGER_SHIFT");
        } else {
            command.env("EGCL_DISABLE_INTEGER_SHIFT", "1");
        }
        let output = command.output().expect("run native integer shift case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "probe={enabled}\n{stdout}\n{stderr}"
        );
        if enabled {
            for target in ["PROBED-DISCARDED-SHIFT:", "PROBED-DISCARDED-SHIFT-MV:"] {
                assert!(
                    stderr.lines().any(|line| line.contains(target)
                        && line.contains("integer shift probes: ")
                        && !line.ends_with(": 0")),
                    "discarded-result probe missing for {target}:\n{stderr}"
                );
                assert!(
                    stderr
                        .lines()
                        .any(|line| line.contains(target) && line.contains(" T2 emitted;")),
                    "discarded-result function lacks native T2: {target}\n{stderr}"
                );
            }
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("PROBED-DISCARDED-SHIFT:")
                        && line.contains("ordinary x86-64 Lisp T2 emitted;")),
                "discarded-result function lacks ordinary native entry:\n{stderr}"
            );
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("PROBED-INTEGER-SHIFT:")
                        && line.contains("integer shift probes: ")
                        && !line.ends_with(": 0")),
                "the probe pass did not run:\n{stderr}"
            );
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("PROBED-INTEGER-SHIFT:")
                        && line.contains("ordinary x86-64 Lisp T2 emitted;")),
                "probe function lacks the real native ordinary entry:\n{stderr}"
            );
        }
        outputs.push(stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "shift probes change observable Lisp results"
    );
}
