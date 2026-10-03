// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn fill_pointer_stores_compile_and_preserve_evaluation_order() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "t0")
        .env("EGCL_LAZY_THRESHOLD", "1")
        .args(["--no-init", "--eval", r#"
          (defun reset-vector (v)
            (let ((order nil))
              (assert (= (setf (fill-pointer (progn (push :vector order) v))
                               (progn (push :value order) 0)) 0))
              (assert (equal order '(:value :vector)))
              (assert (= (fill-pointer v) 0))
              (vector-push 42 v)
              (assert (= (aref v 0) 42))))
          (let ((v (make-array 4 :fill-pointer 3 :initial-element 7)))
            (dotimes (i 4) (reset-vector v)))
          (disassemble 'reset-vector)
          (format t "FILL-POINTER-OK~%")
        "#]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "{stdout}\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(stdout.contains("FILL-POINTER-OK"), "{stdout}");
    assert!(!stdout.contains("not a compiled EGCL function"), "{stdout}");
}
