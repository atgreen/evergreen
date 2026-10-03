// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn uiop_run_program_writes_to_supplied_streams() {
    let source = r#"
      (require :asdf)
      (let ((out (make-string-output-stream))
            (err (make-string-output-stream)))
        (multiple-value-bind (out-result err-result status)
            (uiop:run-program '("/bin/sh" "-c" "printf hello; printf trouble >&2; exit 7")
                              :output out :error-output err :ignore-error-status t)
          (assert (null out-result))
          (assert (null err-result))
          (assert (= status 7)))
        (assert (string= (get-output-stream-string out) "hello"))
        (assert (string= (get-output-stream-string err) "trouble")))
      (assert (string= "captured"
                (with-output-to-string (s)
                  (uiop:run-program '("/bin/sh" "-c" "printf captured") :output s))))
      (format t "UIOP-PROCESS-STREAMS-OK~%")
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
    assert!(String::from_utf8_lossy(&output.stdout).contains("UIOP-PROCESS-STREAMS-OK"));
}
