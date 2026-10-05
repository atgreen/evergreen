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

fn run_with_gc_relocation(program: &str) -> Output {
    Command::new("timeout")
        .args([
            "--kill-after=5",
            "90",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .env("EGCL_FORCE_TIER", "interp")
        .env("EGCL_GC_POISON", "1")
        .env("EGCL_GC_VERIFY", "1")
        .env("EGCL_SAVE_OPTION_CURSOR_LOG", "1")
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
fn image_option_traversal_survives_relocation_during_value_evaluation() {
    for (index, operation) in ["egcl-ext:save-lisp-and-die", "egcl-ext:%save-core"]
        .iter()
        .enumerate()
    {
        let path = std::env::temp_dir().join(format!(
            "egcl-relocated-save-options-{}-{index}",
            std::process::id()
        ));
        let output = run_with_gc_relocation(&format!(
            r#"
          (eval (list '{operation} {path:?}
                      :unknown-option
                        '(progn (dotimes (i 400) (make-array 30000)) nil)
                      :executable t))
        "#
        ));
        let image = std::fs::read(&path);
        let _ = std::fs::remove_file(path);
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        let cursor_moved = stderr.lines().any(|line| {
            let Some(fields) = line.strip_prefix("[save-option-cursor] before=") else {
                return false;
            };
            let Some((before, after)) = fields.split_once(" after=") else {
                return false;
            };
            before != after
        });
        assert!(
            cursor_moved,
            "the fixture must relocate the exact unread option tail:\n{stderr}"
        );
        let image = image.expect("successful image save must create its destination");
        assert!(
            image.len() >= 16 && &image[image.len() - 16..image.len() - 8] == b"EGCLEXE\0",
            "the option following the allocating value must still select an executable image"
        );
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
