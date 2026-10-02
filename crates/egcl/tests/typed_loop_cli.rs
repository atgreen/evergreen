// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn typed_loop_bindings_compile_before_the_first_call() {
    for body in [
        "(loop with total fixnum = 0 for i from 0 below n do (incf total i) finally (return total))",
        "(loop for i fixnum from 0 below n sum i)",
        "(loop with total of-type fixnum = 0 for i of-type fixnum from 0 below n do (incf total i) finally (return total))",
        "(loop with total of-type fixnum for i from 0 below n do (incf total i) finally (return total))",
        "(loop with zero of-type double-float repeat n finally (return (if (and (zerop zero) (typep zero 'double-float)) 10 -1)))",
    ] {
        let program = format!(
            "(progn (defun typed-loop (n) {body}) (disassemble #'typed-loop) \
             (format t \"RESULT:~S~%\" (typed-loop 5)))"
        );
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{body}: {stdout}\n{stderr}");
        assert!(stdout.contains("RESULT:10"), "{body}: {stdout}");
        assert!(stdout.contains("; TYPED-LOOP —"), "{body}: {stdout}");
    }
}
