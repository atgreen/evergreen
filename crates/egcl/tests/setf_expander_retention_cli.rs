// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn interpreted_setf_expander_temporaries_are_reclaimed() {
    let program = r#"
      (define-setf-expander retention-probe (place &environment env)
        (declare (ignore env))
        (let ((store (gensym "STORE")))
          (values nil nil (list store)
                  `(progn (setq ,place ,store) ,store)
                  place)))
      (defun old-generation-bytes ()
        (egcl-ext:gc :full t)
        (let* ((text (with-output-to-string (*standard-output*) (room)))
               (at (search "old generation:" text)))
          (parse-integer text :start (+ at (length "old generation:"))
                              :junk-allowed t)))
      (let ((first (make-symbol "STORE"))
            (second (make-symbol "STORE")))
        (assert (equal '(1 2)
                       (eval `(multiple-value-bind (,first ,second) (values 1 2)
                                (list ,first ,second))))))
      (let ((value 0))
        (dotimes (i 256) (setf (retention-probe value) i)))
      (let ((before (old-generation-bytes)) (value 0))
        (dotimes (i 1024) (setf (retention-probe value) i))
        (format t "~&RETAINED ~d~%" (- (old-generation-bytes) before)))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    let retained = stdout
        .lines()
        .find_map(|line| line.strip_prefix("RETAINED "))
        .and_then(|bytes| bytes.parse::<i64>().ok())
        .unwrap_or_else(|| panic!("missing retention report in stdout: {stdout}"));
    assert!(
        retained <= 4096,
        "interpreted SETF expansion retained {retained} old-generation bytes"
    );
}
