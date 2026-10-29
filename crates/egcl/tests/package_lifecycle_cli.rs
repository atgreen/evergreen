// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

fn run(program: &str, image: Option<&std::path::Path>, stress: bool, marker: &str) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command.args(["--eval", program]);
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
    let output = command.output().expect("run package lifecycle regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stress={stress}: {}\n{stdout}\n{stderr}",
        output.status
    );
    assert_eq!(
        stdout.lines().filter(|line| line.trim() == marker).count(),
        1,
        "{stdout}\n{stderr}"
    );
}

#[test]
fn package_deletion_and_uninterning_retire_name_bindings_without_reusing_symbols() {
    let source = include_str!("fixtures/package-lifecycle.lisp");
    for interpreted in [false, true] {
        for stress in [false, true] {
            let program = if interpreted {
                format!("(eval '(progn {source}))")
            } else {
                source.to_owned()
            };
            run(&program, None, stress, "PACKAGE-LIFECYCLE-OK");
        }
    }
}

#[test]
fn detached_symbol_identity_and_home_survive_a_fresh_image_restore() {
    let dir = std::env::temp_dir().join(format!("egcl-package-lifecycle-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let image = dir.join("packages.core");
    let setup = format!(
        r#"(progn
      (defparameter *lifecycle-old-package* (make-package "LIFECYCLE-IMAGE" :use nil))
      (defparameter *lifecycle-detached* (intern "FOO" *lifecycle-old-package*))
      (setf (symbol-value *lifecycle-detached*) 123)
      (setf (symbol-function *lifecycle-detached*) (lambda () 456))
      (setf (get *lifecycle-detached* 'lifecycle-property) 789)
      (unintern *lifecycle-detached* *lifecycle-old-package*)
      (delete-package *lifecycle-old-package*)
      (compile 'lifecycle-retained-constant
               (list 'lambda nil (list 'quote *lifecycle-detached*)))
      (defparameter *lifecycle-new-package* (make-package "LIFECYCLE-IMAGE" :use nil))
      (defparameter *lifecycle-current* (intern "FOO" *lifecycle-new-package*))
      (write-line "PACKAGE-LIFECYCLE-SAVED")
      (egcl-ext:save-lisp-and-die {image:?}))"#
    );
    run(&setup, None, false, "PACKAGE-LIFECYCLE-SAVED");
    let verify = r##"(progn
      (assert (null (symbol-package *lifecycle-detached*)))
      (assert (not (eq *lifecycle-current* *lifecycle-detached*)))
      (assert (= (symbol-value *lifecycle-detached*) 123))
      (assert (= (funcall *lifecycle-detached*) 456))
      (assert (= (get *lifecycle-detached* 'lifecycle-property) 789))
      (assert (string= (symbol-name *lifecycle-detached*) "FOO"))
      (assert (string= (prin1-to-string *lifecycle-detached*) "#:FOO"))
      (assert (eq (lifecycle-retained-constant) *lifecycle-detached*))
      (assert (eq (symbol-package *lifecycle-current*) *lifecycle-new-package*))
      (assert (eq (intern "FOO" *lifecycle-new-package*) *lifecycle-current*))
      (delete-package *lifecycle-new-package*)
      (assert (null (symbol-package *lifecycle-current*)))
      (assert (null (find-package "LIFECYCLE-IMAGE")))
      (let* ((package (make-package "LIFECYCLE-IMAGE" :use nil))
             (symbol (intern "FOO" package)))
        (assert (not (eq symbol *lifecycle-detached*)))
        (assert (not (eq symbol *lifecycle-current*)))
        (assert (eq (symbol-package symbol) package)))
      (write-line "PACKAGE-LIFECYCLE-RESTORED"))"##;
    for stress in [false, true] {
        run(verify, Some(&image), stress, "PACKAGE-LIFECYCLE-RESTORED");
    }
    fs::remove_dir_all(dir).unwrap();
}
