// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn checked(command: &mut Command) {
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCTURE-PACKAGES-OK"));
}

#[test]
fn generated_structure_functions_stay_in_the_defining_package() {
    let dir = std::env::temp_dir().join(format!("egcl-structure-packages-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("structures.lisp");
    let core = dir.join("structures.core");
    std::fs::write(
        &source,
        r#"
      (defpackage :writer (:use :cl))
      (defpackage :reader (:use :cl))
      (in-package :writer)
      (defstruct section name name-offset)
      (deftype section-alias () 'section)
      (in-package :reader)
      (defstruct section name offset)
      (in-package :cl-user)
      (defun check-structures ()
        (let ((w (writer::make-section :name "writer" :name-offset 42))
              (r (reader::make-section :name "reader" :offset 21)))
          (assert (= 42 (writer::section-name-offset w)))
          (assert (= 21 (reader::section-offset r)))
          (assert (eq (type-of w) 'writer::section))
          (assert (eq (type-of r) 'reader::section))
          (assert (writer::section-p w))
          (assert (not (writer::section-p r)))
          (assert (typep w 'writer::section-alias))
          (assert (not (typep r 'writer::section-alias)))
          (assert (reader::section-p r))
          (assert (not (reader::section-p w)))
          (let ((wc (writer::copy-section w)) (rc (reader::copy-section r)))
            (assert (not (eq w wc)))
            (assert (not (eq r rc)))
            (assert (= 42 (writer::section-name-offset wc)))
            (assert (= 21 (reader::section-offset rc)))))
        (format t "STRUCTURE-PACKAGES-OK~%"))
      (check-structures)
    "#,
    )
    .unwrap();
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--load",
        source.to_str().unwrap(),
    ]));
    let save = format!(
        "(load (compile-file {source:?})) (check-structures) (save-lisp-and-die {core:?})",
        source = source.to_str().unwrap(),
        core = core.to_str().unwrap()
    );
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args(["--no-init", "--eval", &save]));
    checked(Command::new(env!("CARGO_BIN_EXE_egcl")).args([
        "--no-init",
        "--image",
        core.to_str().unwrap(),
        "--eval",
        "(check-structures)",
    ]));
    std::fs::remove_dir_all(dir).unwrap();
}
