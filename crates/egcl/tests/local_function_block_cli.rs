// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// A FLET/LABELS function's body is inside a block named after it. The
/// bytecode compiler did not establish that block, so any `(return-from NAME
/// ...)` in a local function sent the whole enclosing function to the
/// tree-walker -- chipz's inflate state machine among them.
#[test]
fn return_from_a_local_function_is_compiled() {
    for local_operator in ["flet", "labels"] {
        let program = format!(
            r#"
          (progn
            (defun first-over (limit items)
              ({local_operator} ((scan (xs)
                       (dolist (x xs nil)
                         (when (> x limit) (return-from scan x)))))
                (scan items)))
            (disassemble #'first-over)
            (format t "RESULT:~S ~S~%" (first-over 3 (list 1 5 9)) (first-over 10 (list 1 2))))
        "#
        );
        let stdout = eval(&program);
        assert!(
            stdout.contains("RESULT:5 NIL"),
            "{local_operator}: {stdout}"
        );
        assert!(
            stdout.contains("; FIRST-OVER —"),
            "{local_operator} function remained interpreted: {stdout}"
        );
    }
}

/// Declarations and a docstring stay outside the implicit block, where they
/// still apply to the parameters; a `(setf NAME)` function's block is NAME.
#[test]
fn local_function_block_keeps_declarations_and_setf_names() {
    let stdout = eval(
        r#"
      (progn
        (defun clamp-all (xs limit)
          (labels ((clamp (x)
                     "Clamp X to LIMIT."
                     (declare (type fixnum x))
                     (when (> x limit) (return-from clamp limit))
                     x))
            (mapcar #'clamp xs)))
        (defun store-first (cell value)
          (flet (((setf head) (new place)
                   (when (null place) (return-from head :empty))
                   (setf (car place) new)))
            (setf (head cell) value)))
        (format t "RESULT:~S ~S ~S~%"
                (clamp-all (list 1 7 3 9) 5)
                (store-first (list 1 2) 42)
                (store-first nil 42)))
    "#,
    );
    assert!(stdout.contains("RESULT:(1 5 3 5) 42 :EMPTY"), "{stdout}");
}
