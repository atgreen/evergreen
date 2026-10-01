// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn accessor_optimization_preserves_package_identity() {
    let program = r#"
      (defpackage :key-library (:use :cl) (:export :group))
      (in-package :key-library)
      (defclass key () ((value :initarg :value :accessor group)))
      (defpackage :parser-library (:use :cl))
      (in-package :parser-library)
      (defun group (x) (+ x 10))
      (defun parse (x) (group x))
      (defpackage :key-client (:use :cl) (:import-from :key-library :group))
      (in-package :key-client)
      (defun read-key (x) (group x))
      (defun write-key (x value) (setf (group x) value))
      (in-package :cl-user)
      (assert (= (parser-library::parse 7) 17))
      (assert (= (funcall #'parser-library::group 8) 18))
      (let ((key (make-instance 'key-library::key :value 21)))
        (assert (= (key-client::read-key key) 21))
        (key-client::write-key key 34)
        (assert (= (key-library:group key) 34)))
      (format t "ACCESSOR-PACKAGES-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ACCESSOR-PACKAGES-OK"));

    let source = std::env::temp_dir().join(format!(
        "egcl-accessor-packages-{}.lisp",
        std::process::id()
    ));
    let fasl = source.with_extension("bfasl");
    std::fs::write(&source, program).unwrap();
    let compiled = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            &format!(
                "(compile-file {:?} :output-file {:?})",
                source.to_str().unwrap(),
                fasl.to_str().unwrap()
            ),
        ])
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    let loaded = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", fasl.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(
        loaded.status.success(),
        "{}",
        String::from_utf8_lossy(&loaded.stderr)
    );
    assert!(String::from_utf8_lossy(&loaded.stdout).contains("ACCESSOR-PACKAGES-OK"));
    std::fs::remove_file(source).unwrap();
    std::fs::remove_file(fasl).unwrap();
}
