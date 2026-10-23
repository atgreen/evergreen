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

/// LOOP-FINISH and other MACROLET-bound local macros used inside a closure
/// expand as they were bound where the closure was WRITTEN, not where it is
/// called (bliss-9uimd). The source evaluator used to expand them against the
/// caller's lexical macros, so a closure written in an outer LOOP finished the
/// inner loop that called it (or failed to find its GO tag).
const LEXICAL_FINISH_CASES: [(&str, &str); 11] = [
    ("(let ((n 0)) (loop repeat 3 do (flet ((done () (loop-finish))) (loop repeat 5 do (incf n) (done)))) n)", "1"),
    ("(let ((n 0)) (loop repeat 3 do (let ((done (lambda () (loop-finish)))) (loop repeat 5 do (incf n) (funcall done)))) n)", "1"),
    ("(let ((n 0)) (loop repeat 3 do (labels ((done () (loop-finish))) (loop repeat 5 do (incf n) (done)))) n)", "1"),
    ("(loop for i from 0 collect i do (lexical-finish-call (lambda () (when (= i 2) (loop-finish)))) finally (return (list :done i)))", "(:DONE 2)"),
    ("(loop for i below 4 collect (let ((k (lambda () (loop-finish)))) (loop for j below 3 collect j do (when (and (= i 1) (= j 1)) (funcall k)))))", "((0 1 2))"),
    ("(loop for x in '(1 2 3 4) sum x into s do (flet ((stop () (loop-finish))) (when (= x 2) (stop))) finally (return (list :sum s)))", "(:SUM 3)"),
    ("(loop for i below 3 collect (loop for j from 0 collect j do (flet ((stop () (loop-finish))) (when (= j 1) (stop)))))", "((0 1) (0 1) (0 1))"),
    ("(macrolet ((m () 1)) (let ((f (lambda () (m)))) (macrolet ((m () 2)) (funcall f))))", "1"),
    ("(macrolet ((m () 1)) (flet ((f () (m))) (macrolet ((m () 2)) (f))))", "1"),
    ("(let ((f (lambda () (lexical-finish-global)))) (macrolet ((lexical-finish-global () ''macro)) (funcall f)))", "GLOBAL"),
    ("(let ((f (macrolet ((m () 1)) (flet ((g () (m))) #'g)))) (funcall f))", "1"),
];

const LEXICAL_FINISH_PRELUDE: &str = "(defun lexical-finish-call (f) (funcall f)) \
     (defun lexical-finish-global () 'global)";

#[test]
fn interpreted_loop_finish_is_lexical_through_closures() {
    // EVAL runs the source evaluator; the DEFUN copies are called often enough
    // to promote, so both the interpreted and the compiled tiers are covered.
    let mut program = format!("(progn {LEXICAL_FINISH_PRELUDE}");
    for (index, (body, expected)) in LEXICAL_FINISH_CASES.iter().enumerate() {
        program.push_str(&format!(
            "(format t \"EVAL-{index}:~S~%\" (eval '{body})) \
             (defun lexical-finish-{index} () {body}) \
             (dotimes (i 300) \
               (unless (equal (lexical-finish-{index}) '{expected}) \
                 (error \"Exit changed after warming case {index}\"))) \
             (format t \"DEFUN-{index}:~S~%\" (lexical-finish-{index}))"
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
    for (index, (body, expected)) in LEXICAL_FINISH_CASES.iter().enumerate() {
        for path in ["EVAL", "DEFUN"] {
            assert!(
                stdout.contains(&format!("{path}-{index}:{expected}\n")),
                "{path} {body}: {stdout}"
            );
        }
    }
}

#[test]
fn compile_file_keeps_loop_finish_lexical_through_closures() {
    let dir = std::env::temp_dir().join(format!("egcl-lexical-finish-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("lexical-finish.lisp");
    let fasl = dir.join("lexical-finish.fasl");
    let mut text = format!("{LEXICAL_FINISH_PRELUDE}\n");
    for (index, (body, _)) in LEXICAL_FINISH_CASES.iter().enumerate() {
        // A top-level form evaluated at load time, an EVAL of the same source,
        // and a compiled function.
        text.push_str(&format!(
            "(defparameter *lexical-finish-toplevel-{index}* {body})\n\
             (defparameter *lexical-finish-eval-{index}* (eval '{body}))\n\
             (defun lexical-finish-{index} () {body})\n"
        ));
    }
    std::fs::write(&source, text).unwrap();
    let mut report = format!("(progn (load {fasl:?})");
    for index in 0..LEXICAL_FINISH_CASES.len() {
        report.push_str(&format!(
            "(format t \"FASL-{index}:~S ~S ~S~%\" *lexical-finish-toplevel-{index}* \
             *lexical-finish-eval-{index}* (lexical-finish-{index}))"
        ));
    }
    report.push(')');
    let mut stdout = String::new();
    for program in [
        format!("(compile-file {source:?} :output-file {fasl:?})"),
        report,
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{stdout}\n{stderr}");
    }
    for (index, (body, expected)) in LEXICAL_FINISH_CASES.iter().enumerate() {
        assert!(
            stdout.contains(&format!("FASL-{index}:{expected} {expected} {expected}\n")),
            "{body}: {stdout}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
