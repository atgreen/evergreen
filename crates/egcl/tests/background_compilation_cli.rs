// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Source expansion stays in the requesting execution when native compilation
//! moves to workers. Assert effects before/after COMPILE and after native calls.
#![cfg(all(
    target_os = "linux",
    any(target_arch = "x86_64", target_arch = "s390x")
))]

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn run_native_tiers(program: &str, functions: &[(&str, &str)]) {
    for (tier, installed) in [("t1", 1), ("t2", 2)] {
        let mut source = program.to_owned();
        for (function, invocation) in functions {
            source.push_str(&format!(
                "\n(assert (= (egcl-ext:function-tier '{function}) {installed}))
                 (format t \"~&TIER {function}=~d~%\" (egcl-ext:function-tier '{function}))
                 (assert (= {invocation} 42))"
            ));
        }
        source.push_str("\n(assert (= *expansions* 1)) (format t \"~&NATIVE-EFFECTS 1~%\")");
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &source]);
        for variable in [
            "EGCL_FORCE_TIER",
            "EGCL_BACKEND",
            "EGCL_PROFILING_DISABLED",
            "EGCL_T0_T1_THRESHOLD",
            "EGCL_T1_THRESHOLD",
            "EGCL_T1_T2_THRESHOLD",
            "EGCL_T1_T2_INVOKE_THRESHOLD",
            "EGCL_T1_T2_BACKEDGE_THRESHOLD",
            "EGCL_T2_THRESHOLD",
            "EGCL_T2",
            "EGCL_DISABLE_T2",
            "EGCL_T2_THREADS",
            "EGCL_COMPILE_QUEUE_SIZE",
            "EGCL_T2_NO_QUEUE",
            "EGCL_T2_DISCARD",
            "EGCL_T2_LOG",
            "EGCL_LOG",
            "EGCL_LAZY_COMPILE",
        ] {
            command.env_remove(variable);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0");
        let mut child = command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        // A recursive compilation or misplaced restart must fail this test,
        // rather than strand the test runner indefinitely.
        let deadline = Instant::now() + Duration::from_secs(60);
        let timed_out = loop {
            if child.try_wait().unwrap().is_some() {
                break false;
            }
            if Instant::now() >= deadline {
                child.kill().unwrap();
                break true;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let output = child.wait_with_output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !timed_out,
            "{tier}: compilation timed out\n{stdout}\n{stderr}"
        );
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.lines().any(|line| line == "EFFECTS 0 1 1"),
            "{tier}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.lines().any(|line| line == "NATIVE-EFFECTS 1"),
            "{tier}: {stdout}\n{stderr}"
        );
        for (function, _) in functions {
            let expected = format!("TIER {function}={installed}");
            assert!(
                stdout.lines().any(|line| line == expected),
                "missing {expected}: {stdout}\n{stderr}"
            );
        }
    }
}

const MACRO_EFFECTS: &str = r#"
    (defvar *expansions* 0)
    (defmacro counted-expansion (x)
      (incf *expansions*)
      (list '+ x 1))
    (assert (= *expansions* 0))
    (compile 'background-counted '(lambda (x) (counted-expansion x)))
    (assert (= *expansions* 1))
    (defparameter *after-compile* *expansions*)
    (dotimes (i 30) (assert (= (background-counted i) (+ i 1))))
    (assert (= *expansions* 1))
    (format t "~&EFFECTS 0 ~d ~d~%" *after-compile* *expansions*)
"#;

#[test]
fn native_compilation_does_not_repeat_macro_effects() {
    run_native_tiers(
        MACRO_EFFECTS,
        &[("background-counted", "(background-counted 41)")],
    );
}

const DYNAMIC_EXPANSION: &str = r#"
    (defvar *expansions* 0)
    (defvar *expansion-value* 7)
    (defmacro requesting-value ()
      (incf *expansions*)
      *expansion-value*)
    (assert (= *expansions* 0))
    (let ((*expansion-value* 41))
      (compile 'background-dynamic '(lambda (x) (+ x (requesting-value)))))
    (assert (= *expansions* 1))
    (assert (= *expansion-value* 7))
    (defparameter *after-compile* *expansions*)
    (dotimes (i 30) (assert (= (background-dynamic i) (+ i 41))))
    (assert (= *expansions* 1))
    (format t "~&EFFECTS 0 ~d ~d~%" *after-compile* *expansions*)
"#;

#[test]
fn macro_expansion_reads_the_requesting_dynamic_binding() {
    run_native_tiers(
        DYNAMIC_EXPANSION,
        &[("background-dynamic", "(background-dynamic 1)")],
    );
}

const RECOVERABLE_EXPANSION: &str = r#"
    (defvar *expansions* 0)
    (defvar *handled* 0)
    (defmacro recoverable-expansion ()
      (incf *expansions*)
      (restart-case (error 'program-error)
        (supply-expansion () 42)))
    (assert (= *expansions* 0))
    (handler-bind ((program-error
                     (lambda (condition)
                       (declare (ignore condition))
                       (incf *handled*)
                       (invoke-restart 'supply-expansion))))
      (compile 'background-recovered '(lambda () (recoverable-expansion))))
    (assert (= *expansions* 1))
    (assert (= *handled* 1))
    (defparameter *after-compile* *expansions*)
    (dotimes (i 30) (assert (= (background-recovered) 42)))
    (assert (= *expansions* 1))
    (assert (= *handled* 1))
    (format t "~&EFFECTS 0 ~d ~d~%" *after-compile* *expansions*)
"#;

#[test]
fn macro_conditions_keep_the_callers_handlers_and_restarts() {
    run_native_tiers(
        RECOVERABLE_EXPANSION,
        &[("background-recovered", "(background-recovered)")],
    );
}

const RECURSIVE_COMPILE: &str = r#"
    (defvar *expansions* 0)
    (defmacro compiling-expansion (x)
      (incf *expansions*)
      (compile 'background-inner '(lambda (n) (+ n 2)))
      (assert (= (background-inner 40) 42))
      (list '+ x 3))
    (assert (= *expansions* 0))
    (compile 'background-outer '(lambda (x) (compiling-expansion x)))
    (assert (= *expansions* 1))
    (defparameter *after-compile* *expansions*)
    (dotimes (i 30)
      (assert (= (background-outer i) (+ i 3)))
      (assert (= (background-inner i) (+ i 2))))
    (assert (= *expansions* 1))
    (format t "~&EFFECTS 0 ~d ~d~%" *after-compile* *expansions*)
"#;

#[test]
fn recursive_compile_during_expansion_completes_once() {
    run_native_tiers(
        RECURSIVE_COMPILE,
        &[
            ("background-outer", "(background-outer 39)"),
            ("background-inner", "(background-inner 40)"),
        ],
    );
}
