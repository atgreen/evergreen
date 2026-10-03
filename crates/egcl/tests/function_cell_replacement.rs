// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn warm_named_calls_honor_replaced_function_cells() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", r#"
          (defun original-target (x) (list :old x))
          (defun target-caller (x) (original-target x))
          (dotimes (i 10000) (target-caller i))
          (let ((saved (symbol-function 'original-target)) (tag :new))
            (unwind-protect
                (progn
                  (setf (symbol-function 'original-target)
                        (lambda (x) (list tag x)))
                  (assert (equal (funcall (symbol-function 'original-target) 5) '(:new 5)))
                  (assert (equal (original-target 4) '(:new 4)))
                  (assert (equal (target-caller 6) '(:new 6)))
                  (assert (equal (funcall saved 8) '(:old 8))))
              (setf (symbol-function 'original-target) saved)))
          (assert (equal (target-caller 7) '(:old 7)))
          (format t "FUNCTION-CELL-REPLACEMENT-OK~%")
        "#])
        .output().unwrap();
    assert!(output.status.success(), "{}\n{}",
        String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    assert!(String::from_utf8_lossy(&output.stdout).contains("FUNCTION-CELL-REPLACEMENT-OK"));
}
