// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn memory_barriers_are_callable_in_compiled_functions() {
    let source = r#"
      (assert (eq :external (nth-value 1 (find-symbol "MEMORY-BARRIER" :egcl-ext))))
      (dolist (kind '(:read :write :full :data-dependency))
        (assert (null (egcl-ext:memory-barrier kind))))
      (assert (null (egcl-ext:memory-barrier)))
      (assert (null (egcl-ext:load-barrier)))
      (assert (null (egcl-ext:store-barrier)))
      (assert (handler-case
                  (progn (egcl-ext:memory-barrier :unknown) nil)
                (program-error () t)))
      (defun publish-value (buffer value)
        (setf (aref buffer 0) value)
        (egcl-ext:store-barrier)
        (setf (aref buffer 1) value)
        (egcl-ext:load-barrier)
        (aref buffer 0))
      (compile 'publish-value)
      (let ((buffer (vector 0 0)))
        (dotimes (i 1000)
          (assert (= i (publish-value buffer i)))
          (assert (= i (aref buffer 1)))))
      (format t "MEMORY-BARRIER-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("MEMORY-BARRIER-OK"));
}
