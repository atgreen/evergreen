// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// DEFMETHOD can specialize on RESTART before anything has asked for the
/// class, and the method dispatches on real restart objects (GitHub #917).
#[test]
fn defmethod_specializes_on_restart_in_a_fresh_image() {
    let program = r#"
      (defgeneric describe-restart (x))
      (defmethod describe-restart ((x restart)) (list :restart (restart-name x)))
      (defmethod describe-restart ((x t)) :other)
      (assert (equal '(:restart foo)
                     (restart-case (describe-restart (find-restart 'foo)) (foo () nil))))
      (assert (eq :other (describe-restart 1)))
      (assert (eq (find-class 'restart) (find-class 'restart)))
      (defpackage :not-cl (:use))
      (assert (handler-case (progn (eval (read-from-string "(defmethod describe-restart ((x not-cl::restart)) 1)")) nil)
                (error () t)))
      (format t "RESTART-SPECIALIZER-PASS~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run RESTART specializer checks");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("RESTART-SPECIALIZER-PASS"),
        "{stdout}\n{stderr}"
    );
}
