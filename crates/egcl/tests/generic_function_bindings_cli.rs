// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn ordinary_and_setf_generic_names_follow_their_actual_function_cells() {
    let cases = include_str!("fixtures/generic-function-bindings.lisp");
    for interpreted in [false, true] {
        for stress in [false, true] {
            let program = if interpreted {
                format!("(eval '(progn {cases}))")
            } else {
                cases.to_owned()
            };
            let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
            command.args(["--no-init", "--eval", &program]);
            for name in [
                "EGCL_GC_STRESS",
                "EGCL_GC_POISON",
                "EGCL_GC_STRESS_AFTER_INIT",
                "EGCL_GC_STRESS_SKIP",
                "EGCL_GC_STRESS_AT",
            ] {
                command.env_remove(name);
            }
            if stress {
                command.envs([
                    ("EGCL_GC_STRESS", "1"),
                    ("EGCL_GC_POISON", "1"),
                    ("EGCL_GC_STRESS_AFTER_INIT", "1"),
                ]);
            }
            let output = command
                .output()
                .expect("run authoritative generic binding checks");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "interpreted={interpreted}, stress={stress}: {}\n{stdout}\n{stderr}",
                output.status
            );
            assert_eq!(
                stdout
                    .lines()
                    .filter(|line| line.contains("GENERIC-BINDINGS-OK"))
                    .count(),
                1,
                "{stdout}\n{stderr}"
            );
        }
    }
}

#[test]
fn retained_generic_wrappers_dispatch_after_fresh_image_restore() {
    use std::fs;
    let dir = std::env::temp_dir().join(format!("egcl-generic-bindings-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let image = dir.join("generics.core");
    let build = format!(
        r#"(progn
          (defgeneric image-generic (x))
          (defmethod image-generic ((x integer)) (+ x 7))
          (defgeneric (setf image-place) (value object))
          (defmethod (setf image-place) (value (object cons))
            (setf (car object) value))
          (defparameter *image-generic* #'image-generic)
          (defparameter *image-setter* #'(setf image-place))
          (fmakunbound 'image-generic)
          (setf (fdefinition '(setf image-place))
                (lambda (value object) (setf (car object) (+ value 100))))
          (egcl-ext:save-lisp-and-die {image:?}))"#
    );
    let built = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &build])
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--image"]).arg(&image).args([
            "--eval",
            r#"(progn
              (assert (not (fboundp 'image-generic)))
              (assert (handler-case (progn (image-generic 5) nil)
                        (undefined-function () t)))
              (assert (= (funcall *image-generic* 5) 12))
              (let ((object (list 0)))
                (assert (= (funcall (fdefinition '(setf image-place)) 3 object) 103))
                (assert (= (funcall *image-setter* 4 object) 4))
                (assert (= (car object) 4))
                (fmakunbound '(setf image-place))
                (assert (not (fboundp '(setf image-place))))
                (assert (= (funcall *image-setter* 5 object) 5))
                (setf (fdefinition '(setf image-place)) *image-setter*)
                (assert (= (setf (image-place object) 6) 6))
                (assert (= (car object) 6)))
              (setf (fdefinition 'image-generic) *image-generic*)
              (assert (= (image-generic 6) 13))
              (format t "RESTORED-GENERIC-BINDINGS-OK~%"))"#,
        ]);
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command.envs([
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ]);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("RESTORED-GENERIC-BINDINGS-OK"),
            "stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
