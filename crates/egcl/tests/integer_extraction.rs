// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exercise emitted extraction with fixnums and signed multi-limb integers;
//! compare the same program with fusion disabled, retaining real error checks.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn native_integer_extraction_preserves_bits_errors_effects_and_values() {
    let program = r#"
        (defvar *extract-effects* 0)
        (declaim (notinline extract-bits0 extract-bits8 extract-bits60 extract-effect))
        (defun extract-bits0 (integer)
          (values :discarded :stale)
          (ldb (byte 0 0) integer))
        (defun extract-bits8 (integer)
          (values :discarded :stale)
          (ldb (byte 8 0) integer))
        (defun extract-bits60 (integer)
          (values :discarded :stale)
          (ldb (byte 60 0) integer))
        (defun extract-effect (integer)
          (incf *extract-effects*)
          (ldb (byte 8 0) integer))
        (defun check-extracts (integer)
          (assert (= (extract-bits0 integer) 0))
          (assert (= (extract-bits8 integer) (mod integer 256)))
          (assert (= (extract-bits60 integer) (mod integer 1152921504606846976)))
          (assert (equal (multiple-value-list (extract-bits0 integer)) '(0)))
          (assert (equal (multiple-value-list (extract-bits8 integer))
                         (list (mod integer 256))))
          (assert (equal (multiple-value-list (extract-bits60 integer))
                         (list (mod integer 1152921504606846976)))))
        (defvar *extract-inputs*
          (list 0 1 -1 255 256 -257
                1152921504606846975 -1152921504606846976
                18446744073709551615 -18446744073709551615
                (+ (ash 1 200) 1234567890123456789)
                (- (+ (ash 1 200) 1234567890123456789))))
        ;; Every representation is observed before steady-state counters.
        (dotimes (round 120)
          (dolist (integer *extract-inputs*) (check-extracts integer))
          (extract-effect 18446744073709551615))
        (assert (= (egcl-ext:function-tier 'extract-bits0) 2))
        (assert (= (egcl-ext:function-tier 'extract-bits8) 2))
        (assert (= (egcl-ext:function-tier 'extract-bits60) 2))
        (assert (= (egcl-ext:function-tier 'extract-effect) 2))
        (let ((before (egcl-ext:deopt-count)))
          (dotimes (round 20)
            (dolist (integer *extract-inputs*) (check-extracts integer)))
          (assert (= (egcl-ext:deopt-count) before))
          (format t "EXTRACTION-STEADY-ZERO-DEOPTS~%"))
        ;; Bad integers must still signal at width zero. Each error is checked
        ;; through the original public calls, not converted into success skips.
        (let ((errors 0))
          (handler-case (extract-bits0 :invalid)
            (type-error () (incf errors)))
          (handler-case (extract-bits8 1.5)
            (type-error () (incf errors)))
          (handler-case (extract-bits60 'invalid)
            (type-error () (incf errors)))
          (assert (= errors 3))
          (format t "EXTRACTION-TYPE-ERRORS ~D~%" errors))
        ;; A guard resumes at BYTE, after the already-performed visible effect.
        ;; Replaying function entry would increment this twice.
        (setf *extract-effects* 0)
        (let ((before (egcl-ext:deopt-count)) (failed nil))
          (handler-case (extract-effect 1.5)
            (type-error () (setf failed t)))
          (assert failed)
          (assert (= *extract-effects* 1))
          __CHECK_NATIVE_GUARD__
          (format t "EXTRACTION-ERROR-EFFECTS ~D~%" *extract-effects*))
        (assert (equal (multiple-value-list (extract-bits8 258)) '(2)))
        (format t "EXTRACTION-DONE~%")
    "#;
    let mut outputs = Vec::new();
    for enabled in [false, true] {
        let case = program.replace(
            "__CHECK_NATIVE_GUARD__",
            if enabled {
                "(assert (> (egcl-ext:deopt-count) before))"
            } else {
                "(progn)"
            },
        );
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .args(["--no-init", "--eval", &case])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t2")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if enabled {
            command.env_remove("EGCL_DISABLE_INTEGER_EXTRACTION");
        } else {
            command.env("EGCL_DISABLE_INTEGER_EXTRACTION", "1");
        }
        let output = command.output().expect("run native extraction case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "enabled={enabled}\n{stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("EXTRACTION-STEADY-ZERO-DEOPTS")
                && stdout.contains("EXTRACTION-TYPE-ERRORS 3")
                && stdout.contains("EXTRACTION-ERROR-EFFECTS 1")
                && stdout.contains("EXTRACTION-DONE"),
            "{stdout}\n{stderr}"
        );
        for target in [
            "EXTRACT-BITS0",
            "EXTRACT-BITS8",
            "EXTRACT-BITS60",
            "EXTRACT-EFFECT",
        ] {
            if enabled {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{target}: native integer extraction: 1 site(s)"))),
                    "intended extraction absent for {target}:\n{stderr}"
                );
            } else {
                assert!(!stderr.lines().any(|line| line.contains(&format!(
                    "{target}: native integer extraction:"))),
                    "disabled extraction ran for {target}:\n{stderr}");
            }
        }
        outputs.push(stdout);
    }
    assert_eq!(
        outputs[0], outputs[1],
        "native extraction changes observable results"
    );
}
