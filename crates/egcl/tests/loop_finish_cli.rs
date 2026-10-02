// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn loop_finish_preserves_accumulation_and_lexical_scope() {
    let cases = [
        ("(let ((n 0)) (loop repeat 5 do (incf n) (loop-finish)) n)", "1"),
        ("(let ((n 0)) (loop for i from 0 below 5 do (incf n) (when (= i 2) (loop-finish))) n)", "3"),
        ("(let ((n 0)) (loop for i in '(1 2 3 4) do (when (= i 3) (loop-finish)) (incf n i)) n)", "3"),
        ("(let ((n 0)) (loop for tail on '(1 2 3 4) do (when (= (car tail) 3) (loop-finish)) (incf n (car tail))) n)", "3"),
        ("(let ((n 0)) (loop while (< n 5) do (incf n) (loop-finish)) n)", "1"),
        ("(let ((n 0)) (loop until (= n 5) do (incf n) (loop-finish)) n)", "1"),
        ("(loop for i from 0 below 5 collect (if (= i 3) (loop-finish) i))", "(0 1 2)"),
        ("(loop repeat 3 sum (loop repeat 5 sum (progn (loop-finish) 9)))", "0"),
        ("(let ((n 0)) (loop repeat 3 do (flet ((done () (loop-finish))) (loop repeat 5 do (incf n) (done)))) n)", "1"),
        ("(let ((n 0)) (loop repeat 3 do (let ((done (lambda () (loop-finish)))) (loop repeat 5 do (incf n) (funcall done)))) n)", "1"),
        ("(let ((n 0)) (loop repeat 3 do (unwind-protect (loop-finish) (incf n)) finally (incf n 10)) n)", "11"),
        ("(let ((n 0)) (loop repeat 3 do (loop (incf n) (loop-finish))) n)", "1"),
        ("(let ((n 0)) (loop repeat 3 do (incf n) (hidden-finish) finally (incf n 10)) n)", "11"),
    ];
    // Batch the cases in one process: bootstrap dominates GC-stress runtime.
    let mut program =
        String::from("(progn (defmacro hidden-finish () '(funcall (lambda () (loop-finish))))");
    for (index, (body, expected)) in cases.iter().enumerate() {
        program.push_str(&format!(
            "(defun finish-probe-{index} () {body}) \
             (disassemble #'finish-probe-{index}) \
             (dotimes (i 300) \
               (unless (equal (finish-probe-{index}) '{expected}) \
                 (error \"Exit changed after warming case {index}\"))) \
             (format t \"RESULT-{index}:~S~%\" (finish-probe-{index}))"
        ));
    }
    program.push(')');
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_T1_THRESHOLD", "10")
        .args(["--no-init", "--eval", &program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for (index, (body, expected)) in cases.iter().enumerate() {
        assert!(
            stdout.contains(&format!("RESULT-{index}:{expected}")),
            "{body}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("; FINISH-PROBE-{index} —")),
            "{body}: {stdout}"
        );
    }
}

#[test]
fn loop_finish_survives_fasl_loading_in_a_fresh_process() {
    let dir = std::env::temp_dir().join(format!("egcl-loop-finish-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("finish.lisp");
    let fasl = dir.join("finish.fasl");
    std::fs::write(
        &source,
        "(defun saved-finish ()
           (let ((n 0))
             (loop repeat 3
                   do (flet ((done () (loop-finish)))
                        (loop repeat 5 do (incf n) (done)))
                   finally (incf n 10))
             n))",
    )
    .unwrap();
    for program in [
        format!("(compile-file {source:?} :output-file {fasl:?})"),
        format!(
            "(progn (load {fasl:?}) (disassemble #'saved-finish) \
             (format t \"RESULT:~S~%\" (saved-finish)))"
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
        if program.contains("disassemble") {
            assert!(stdout.contains("RESULT:11"), "{stdout}");
            assert!(stdout.contains("; SAVED-FINISH —"), "{stdout}");
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compiled_loop_finish_runs_finally() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            "(progn
               (defun finish-loop (n)
                 (loop with total fixnum = 0 for i from 0 below n
                       do (if (= i 3) (loop-finish) (incf total i))
                       finally (return (+ 7 total))))
               (disassemble #'finish-loop)
               (format t \"RESULT:~S~%\" (finish-loop 5)))",
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT:10"), "{stdout}");
    assert!(stdout.contains("; FINISH-LOOP —"), "{stdout}");
}
