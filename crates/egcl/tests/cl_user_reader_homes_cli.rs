// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// A bare read in CL-USER homes names boot.lisp only used as variables in
/// CL-USER, as in any other package that uses COMMON-LISP (GitHub #924).
#[test]
fn cl_user_reads_home_boot_variable_names_in_cl_user() {
    let program = r#"
      (in-package :cl-user)
      (dolist (name '("X" "A" "KEY" "OBJ" "VALUE" "ITEM" "RESULT" "TMP"))
        (let ((sym (let ((*package* (find-package :cl-user))) (read-from-string name))))
          (assert (eq (symbol-package sym) (find-package :cl-user)) () "~a: ~s" name (symbol-package sym))
          (assert (eq sym (find-symbol name :cl-user)))
          (assert (eq sym (intern name :cl-user)))))
      (defvar y 1)
      (assert (eq (symbol-package 'y) (find-package :cl-user)))
      ;; Standard and extension names still resolve as before.
      (assert (eq (symbol-package 'car) (find-package :cl)))
      (assert (fboundp 'car))
      (format t "CL-USER-READER-PASS~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run CL-USER reader checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("CL-USER-READER-PASS"),
        "{stdout}\n{stderr}"
    );
}
