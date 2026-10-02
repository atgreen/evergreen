// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn once_called_loops_with_capturing_local_functions_are_compiled() {
    for local_operator in ["flet", "labels"] {
        let program = format!(
            r#"
          (progn
            (defun capturing-loop (n)
              ({local_operator} ((add-n (x) (+ x n)))
                (let ((sum 0))
                  (dotimes (i 100) (incf sum (add-n i)))
                  sum)))
            (disassemble #'capturing-loop)
            (format t "RESULT:~D~%" (capturing-loop 7)))
        "#
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{local_operator}: {stdout}\n{stderr}"
        );
        assert!(stdout.contains("RESULT:5650"), "{local_operator}: {stdout}");
        // Inspect before the first invocation: repeated calls must not be
        // required to rescue the loop through lazy compilation.
        assert!(
            stdout.contains("; CAPTURING-LOOP —"),
            "{local_operator} loop remained interpreted: {stdout}\n{stderr}"
        );
    }
}
