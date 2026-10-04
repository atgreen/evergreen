// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::{Command, Output};
fn run(program: &str, tier: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .unwrap()
}
fn passes(output: Output, marker: &str) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains(marker));
}
#[test]
fn documentation_recording_preserves_handler_exits() {
    for definer in ["defun", "defmacro"] {
        for failure in [
            "(car 42)",
            "(error 'type-error :datum 42 :expected-type 'list)",
        ] {
            passes(
                run(
                    &format!(
                        r#"
              (defun egcl-internal::%set-documentation (name kind doc)
                (declare (ignore name kind doc)) {failure})
              (assert (eq :caught
                (catch 'escaped
                  (handler-bind ((type-error (lambda (condition)
                                               (declare (ignore condition))
                                               (throw 'escaped :caught))))
                    ({definer} doc-probe () "metadata" 1)
                    :continued))))
              (format t "DOCUMENTATION-TRANSFER-OK~%")
            "#
                    ),
                    "interp",
                ),
                "DOCUMENTATION-TRANSFER-OK",
            );
        }
    }
}
#[test]
fn image_option_evaluation_does_not_discard_handler_exits() {
    for (index, operation) in ["egcl-ext:save-lisp-and-die", "egcl-ext:%save-core"]
        .iter()
        .enumerate()
    {
        let path = std::env::temp_dir().join(format!(
            "egcl-aborted-save-{}-{index}.core",
            std::process::id()
        ));
        let output = run(
            &format!(
                r#"
          (assert (eq :caught
            (catch 'escaped
              (handler-bind ((type-error (lambda (condition)
                                           (declare (ignore condition))
                                           (throw 'escaped :caught))))
                ({operation} {path:?} :unknown-option (car 42))
                :continued))))
          (format t "SAVE-TRANSFER-OK~%")
        "#
            ),
            "interp",
        );
        let wrote_image = path.exists();
        let _ = std::fs::remove_file(path);
        passes(output, "SAVE-TRANSFER-OK");
        assert!(!wrote_image, "handler exit must prevent image creation");
    }
}
#[test]
fn instance_defaults_use_already_evaluated_initarg_keys() {
    for tier in ["interp", "t0"] {
        passes(
            run(
                r#"
          (defclass key-probe () ((value :initarg :value :reader probe-value))
            (:default-initargs :value 9))
          (let* ((evaluations 0)
                 (instance (make-instance 'key-probe
                             (progn (incf evaluations) :value) 42)))
            (assert (= 1 evaluations))
            (assert (= 42 (probe-value instance))))
          (format t "INITARG-ONCE-OK~%")
        "#,
                tier,
            ),
            "INITARG-ONCE-OK",
        );
    }
}
