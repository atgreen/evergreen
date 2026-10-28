// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::path::Path;
use std::process::Command;

const SOURCE: &str = r#"
(defpackage #:xvogh-writers (:use #:cl) (:nicknames #:xvogh-alias)
  (:export #:plain #:generic-slot))
(in-package #:xvogh-writers)
(defun plain (object) (car object))
(defun (setf plain) (value object) (rplaca object value) value)
(defgeneric generic-slot (object))
(defmethod generic-slot (object) (car object))
(defgeneric (setf generic-slot) (value object))
(defmethod (setf generic-slot) (value object) (rplaca object value) value)
(in-package #:cl-user)
(defun xvogh-check ()
  (assert (eq 'xvogh-writers:plain 'xvogh-writers::plain))
  (assert (eq 'xvogh-writers:plain 'xvogh-alias:plain))
  (assert (eq (fdefinition '(setf xvogh-writers:plain))
              (fdefinition '(setf xvogh-alias::plain))))
  (let ((object (list 1)))
    (assert (= (funcall #'(setf xvogh-writers:plain) 2 object) 2))
    (assert (= (setf (xvogh-alias:plain object) 3) 3))
    (assert (= (xvogh-writers:plain object) 3))
    (assert (= (funcall (fdefinition '(setf xvogh-alias:generic-slot)) 4 object) 4))
    (assert (= (setf (xvogh-writers:generic-slot object) 5) 5))
    (assert (= (xvogh-alias:generic-slot object) 5)))
  (assert (eq (symbol-package 'xvogh-alias:plain) (find-package :xvogh-writers)))
  (assert (not (find-if (lambda (package) (search "(SETF " (package-name package)))
                        (list-all-packages))))
  t)
"#;

const BOOT: &str = r#"
(assert (not (find-if (lambda (package) (search "(SETF " (package-name package)))
                      (list-all-packages))))
"#;

fn run(dir: &Path, args: &[&str], stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.current_dir(dir).arg("--no-init");
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(name);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command
        .args(args)
        .output()
        .expect("run SETF package regression");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn bootstrap_and_qualified_setf_functions_never_create_synthetic_packages() {
    let program =
        format!("{BOOT}\n{SOURCE}\n(assert (xvogh-check))\n(format t \"SETF-PACKAGES-OK~%\")");
    let mut reference = None;
    for stress in [false, true] {
        let stdout = run(Path::new("."), &["--eval", &program], stress);
        assert!(stdout.contains("SETF-PACKAGES-OK"));
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}

#[test]
fn qualified_setf_names_survive_bfasl_and_core_roundtrips_without_fake_packages() {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("setf-packages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("writers.lisp"), SOURCE).unwrap();
    run(&dir, &["--eval", "(compile-file \"writers.lisp\")"], false);
    assert!(dir.join("writers.fasl").is_file());
    // Fresh process: no defining package or writer objects from compilation.
    let program = format!(
        "{BOOT}\n(load \"writers.fasl\")\n(assert (xvogh-check))\n(format t \"SETF-FASL-OK~%\")"
    );
    let saved = run(
        &dir,
        &[
            "--eval",
            &program,
            "--eval",
            "(egcl-ext:save-lisp-and-die \"writers.core\")",
        ],
        false,
    );
    assert!(saved.contains("SETF-FASL-OK"));
    assert!(dir.join("writers.core").is_file());
    let restored = run(
        &dir,
        &[
            "--image",
            "writers.core",
            "--eval",
            "(assert (xvogh-check)) (format t \"SETF-CORE-OK~%\")",
        ],
        false,
    );
    assert!(restored.contains("SETF-CORE-OK"));
    std::fs::remove_dir_all(&dir).unwrap();
}
