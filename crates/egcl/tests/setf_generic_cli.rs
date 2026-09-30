// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! SETF generic names must resolve to first-class callable functions.
use std::process::Command;

fn check(lookup: &str) {
    let program = format!(
        r#"
        (defclass generic-writer-cell () ((value :initarg :value :accessor writer-value)))
        (defmethod (setf generic-write) (new (cell generic-writer-cell))
          (setf (slot-value cell 'value) new)
          (values new :written))
        (let ((writer {lookup})
              (cell (make-instance 'generic-writer-cell :value 0)))
          (assert (functionp writer))
          (assert (equal (multiple-value-list (funcall writer 41 cell)) '(41 :written)))
          (assert (= 41 (slot-value cell 'value)))
          (defmethod (setf generic-write) (new (cell generic-writer-cell))
            (setf (slot-value cell 'value) (+ new 100))
            (values new :redefined))
          (assert (equal (multiple-value-list (apply writer (list 42 cell))) '(42 :redefined)))
          (assert (= 142 (slot-value cell 'value)))
          (flet (((setf generic-write) (new object)
                   (declare (ignore new object)) :local))
            (assert (functionp #'(setf generic-write)))
            (assert (eq :local (funcall #'(setf generic-write) 7 cell)))
            (assert (= 8 (funcall (fdefinition '(setf generic-write)) 8 cell)))
            (assert (= 108 (slot-value cell 'value)))))
        (format t "GENERIC-WRITER-OK~%")
        "#
    );
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T1_THRESHOLD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("GENERIC-WRITER-OK"));
    }
}

#[test]
fn function_reifies_a_setf_generic_and_preserves_dispatch() {
    check("#'(setf generic-write)");
}

#[test]
fn fdefinition_reifies_a_setf_generic_and_preserves_dispatch() {
    check("(fdefinition '(setf generic-write))");
}
