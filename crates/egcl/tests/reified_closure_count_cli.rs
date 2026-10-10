// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn reified_calls_count_the_selected_function_once() {
    for (profiling_disabled, expected) in [
        (
            "0",
            [
                "INITIAL 0",
                "NAMED 1",
                "OBJECT 2",
                "SYMBOL 3",
                "APPLY 4",
                "SAVED 5",
            ],
        ),
        (
            "1",
            [
                "INITIAL 0",
                "NAMED 0",
                "OBJECT 0",
                "SYMBOL 0",
                "APPLY 0",
                "SAVED 0",
            ],
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args([
                "--no-init",
                "--eval",
                r#"
(setf (symbol-function 'counted-reified) (lambda (x) (+ x 1)))
(defparameter *saved-counted* (symbol-function 'counted-reified))
(format t "INITIAL ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
(counted-reified 1)
(format t "NAMED ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
(funcall *saved-counted* 1)
(format t "OBJECT ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
(funcall 'counted-reified 1)
(format t "SYMBOL ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
(apply 'counted-reified '(1))
(format t "APPLY ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
(setf (symbol-function 'counted-reified) (lambda (x) x))
(fmakunbound 'counted-reified)
(funcall *saved-counted* 2)
(format t "SAVED ~D~%" (egcl-ext:function-invoke-count *saved-counted*))
"#,
            ])
            .env("EGCL_FORCE_TIER", "interp")
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_PROFILING_DISABLED", profiling_disabled)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "profiling_disabled={profiling_disabled}: {stdout}\n{stderr}"
        );
        for line in expected {
            assert!(
                stdout.lines().any(|actual| actual == line),
                "profiling_disabled={profiling_disabled}: missing {line:?}: {stdout}\n{stderr}"
            );
        }
    }
}
