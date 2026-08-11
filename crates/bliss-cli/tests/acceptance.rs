//! End-to-end acceptance tests for the Bliss CLI binary.
//!
//! These tests build and invoke the real `bliss` binary via std::process::Command,
//! asserting on exit codes, stdout, and stderr output. They exercise every
//! top-level user-facing capability from the spec:
//!   - --help, --version informational output
//!   - --eval expression evaluation
//!   - --load file loading
//!   - Script file execution
//!   - REPL interaction (piped stdin)
//!   - Argument conflicts and error handling
//!   - --sandbox, --bootstrap, --no-image modes
//!   - Passthrough CL args via --

use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

/// Get the path to the bliss binary built by cargo.
fn bliss_bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_bliss-cli"))
}

// ══════════════════════════════════════════════════════════════════
// --help
// ══════════════════════════════════════════════════════════════════

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    let output = bliss_bin()
        .arg("--help")
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("Usage:"),
        "help output should contain 'Usage:', got: {}",
        stdout
    );
    assert!(
        stdout.contains("--eval"),
        "help output should mention --eval"
    );
    assert!(
        stdout.contains("--load"),
        "help output should mention --load"
    );
    assert!(
        stdout.contains("--image"),
        "help output should mention --image"
    );
    assert!(
        stdout.contains("--sandbox"),
        "help output should mention --sandbox"
    );
}

// ══════════════════════════════════════════════════════════════════
// --version
// ══════════════════════════════════════════════════════════════════

#[test]
fn version_flag_prints_version_and_exits_zero() {
    let output = bliss_bin()
        .arg("--version")
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("bliss"),
        "version output should contain 'bliss', got: {}",
        stdout
    );
    // Should contain a version number (at least major.minor.patch)
    assert!(
        stdout.contains("0.1.0") || stdout.contains('.'),
        "version output should contain a version number"
    );
}

// ══════════════════════════════════════════════════════════════════
// --eval / -e: expression evaluation
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_simple_arithmetic_returns_correct_value() {
    // When --eval is fully wired, (+ 1 2) should print "3".
    // This test will fail until the evaluator is connected, which is correct
    // for red-phase TDD.
    let output = bliss_bin()
        .args(["--eval", "(+ 1 2)"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.trim().contains('3'),
        "--eval '(+ 1 2)' should output 3, got stdout: '{}', stderr: '{}'",
        stdout,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn eval_short_flag_works() {
    let output = bliss_bin()
        .args(["-e", "(quote hello)"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_uppercase().contains("HELLO"),
        "-e '(quote hello)' should output HELLO, got: '{}'",
        stdout
    );
}

#[test]
fn eval_string_expression() {
    let output = bliss_bin()
        .args(["--eval", "(format nil \"hello ~A\" 'world)"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello") && stdout.to_uppercase().contains("WORLD"),
        "format should produce 'hello WORLD', got: '{}'",
        stdout
    );
}

#[test]
fn eval_prints_multiple_values() {
    // (values 1 2 3) should print all three values
    let output = bliss_bin()
        .args(["--eval", "(values 1 2 3)"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('1') && stdout.contains('2') && stdout.contains('3'),
        "multiple values should all appear, got: '{}'",
        stdout
    );
}

#[test]
fn eval_nil_expression() {
    let output = bliss_bin()
        .args(["--eval", "nil"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_uppercase().contains("NIL"),
        "evaluating nil should output NIL, got: '{}'",
        stdout
    );
}

#[test]
fn eval_t_expression() {
    let output = bliss_bin()
        .args(["--eval", "t"])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_uppercase().contains('T'),
        "evaluating t should output T, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// --load: file loading
// ══════════════════════════════════════════════════════════════════

#[test]
fn load_file_executes_and_exits() {
    // Create a temporary .lisp file
    let dir = std::env::temp_dir().join("bliss_test_load");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_load.lisp");
    std::fs::write(&file_path, "(print 42)\n").expect("failed to write test file");

    let output = bliss_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("failed to run bliss");

    // Clean up
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "--load should execute the file and print 42, got: '{}'",
        stdout
    );
}

#[test]
fn load_nonexistent_file_fails() {
    let output = bliss_bin()
        .args(["--load", "/tmp/bliss_nonexistent_file_12345.lisp"])
        .output()
        .expect("failed to run bliss");
    // Should exit with non-zero code or print an error
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.code() != Some(0) || stderr.contains("error") || stderr.contains("cannot"),
        "loading nonexistent file should fail, exit={:?}, stderr='{}'",
        output.status.code(),
        stderr
    );
}

// ══════════════════════════════════════════════════════════════════
// Script file execution (positional arg)
// ══════════════════════════════════════════════════════════════════

#[test]
fn script_file_executes_and_exits() {
    let dir = std::env::temp_dir().join("bliss_test_script");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_script.lisp");
    std::fs::write(&file_path, "(print \"hello from script\")\n")
        .expect("failed to write test file");

    let output = bliss_bin()
        .arg(file_path.to_str().unwrap())
        .output()
        .expect("failed to run bliss");

    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hello from script"),
        "script should print its output, got: '{}'",
        stdout
    );
}

#[test]
fn script_receives_cl_args() {
    let dir = std::env::temp_dir().join("bliss_test_clargs");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_args.lisp");
    // Script that prints *command-line-args*
    std::fs::write(&file_path, "(print *command-line-args*)\n").expect("failed to write test file");

    let output = bliss_bin()
        .arg(file_path.to_str().unwrap())
        .args(["--", "foo", "bar"])
        .output()
        .expect("failed to run bliss");

    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("foo") && stdout.contains("bar"),
        "CL args should be accessible, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// REPL interaction
// ══════════════════════════════════════════════════════════════════

#[test]
fn repl_evaluates_expression_and_prints_result() {
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(+ 2 3)\n(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('5'),
        "REPL should evaluate (+ 2 3) to 5, got stdout: '{}'",
        stdout
    );
}

#[test]
fn repl_shows_prompt() {
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(quit)\n")
            .expect("failed to write to stdin");
    }

    // Use wait_with_output with a timeout thread to prevent hanging
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(());
        result
    });

    // Wait up to 10 seconds
    let output = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(_) => handle.join().unwrap().expect("failed to wait on bliss"),
        Err(_) => panic!("REPL test timed out after 10 seconds — binary may be hanging"),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The prompt could appear on either stdout or stderr
    assert!(
        stderr.contains("BLISS>")
            || stderr.contains("bliss>")
            || stdout.contains("BLISS>")
            || stdout.contains("bliss>"),
        "REPL should display a prompt, stdout: '{}', stderr: '{}'",
        stdout,
        stderr
    );
}

#[test]
fn repl_exit_command_exits_cleanly() {
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(exit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(
        output.status.code(),
        Some(0),
        "(exit) should cleanly exit with code 0"
    );
}

#[test]
fn repl_eof_exits_cleanly() {
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    // Close stdin immediately (EOF)
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(
        output.status.code(),
        Some(0),
        "EOF on stdin should exit cleanly with code 0"
    );
}

#[test]
fn repl_defun_and_call() {
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(defun square (x) (* x x))\n(square 7)\n(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("49"),
        "REPL should evaluate (square 7) to 49, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Error handling — bad flags / conflicts
// ══════════════════════════════════════════════════════════════════

#[test]
fn unknown_flag_exits_nonzero() {
    let output = bliss_bin()
        .arg("--frobnicate")
        .output()
        .expect("failed to run bliss");
    assert_ne!(
        output.status.code(),
        Some(0),
        "unknown flag should cause non-zero exit"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("unknown") || stderr.contains("frobnicate"),
        "error message should mention the bad flag, got: '{}'",
        stderr
    );
}

#[test]
fn conflicting_image_no_image_exits_nonzero() {
    let output = bliss_bin()
        .args(["--image", "core.img", "--no-image"])
        .output()
        .expect("failed to run bliss");
    assert_ne!(
        output.status.code(),
        Some(0),
        "--image + --no-image should fail"
    );
}

#[test]
fn eval_missing_value_exits_nonzero() {
    let output = bliss_bin()
        .arg("--eval")
        .output()
        .expect("failed to run bliss");
    assert_ne!(
        output.status.code(),
        Some(0),
        "--eval without value should fail"
    );
}

// ══════════════════════════════════════════════════════════════════
// Bootstrap and sandbox modes
// ══════════════════════════════════════════════════════════════════

#[test]
fn bootstrap_mode_starts_without_image() {
    // --bootstrap --no-init should start without needing an image file
    let mut child = bliss_bin()
        .args(["--bootstrap"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on bliss");
    // Bootstrap mode should at least start up (even if reduced functionality)
    assert_eq!(
        output.status.code(),
        Some(0),
        "bootstrap mode should start and exit cleanly"
    );
}

// ══════════════════════════════════════════════════════════════════
// Workers and heap-size runtime config
// ══════════════════════════════════════════════════════════════════

#[test]
fn workers_flag_accepted() {
    let mut child = bliss_bin()
        .args(["--workers", "2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(
        output.status.code(),
        Some(0),
        "--workers 2 should be accepted"
    );
}

#[test]
fn heap_size_flag_accepted() {
    let mut child = bliss_bin()
        .args(["--heap-size", "128M"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on bliss");
    assert_eq!(
        output.status.code(),
        Some(0),
        "--heap-size 128M should be accepted"
    );
}

// ══════════════════════════════════════════════════════════════════
// Condition handling (handler-bind / handler-case)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_handler_case_catches_error() {
    let output = bliss_bin()
        .args([
            "--eval",
            "(handler-case (error \"boom\") (error (c) (format nil \"caught: ~A\" c)))",
        ])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("caught"),
        "handler-case should catch the error and print 'caught: ...', got: '{}'",
        stdout
    );
}

#[test]
fn eval_handler_bind_invokes_handler() {
    let expr = r#"(let ((result nil))
  (handler-bind ((error (lambda (c) (setq result "handled"))))
    (signal (make-condition 'error)))
  result)"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("handled"),
        "handler-bind should invoke handler, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// CLOS (defclass / defmethod)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_defclass_and_make_instance() {
    let expr = r#"(progn
  (defclass point () ((x :initarg :x :accessor point-x) (y :initarg :y :accessor point-y)))
  (let ((p (make-instance 'point :x 3 :y 4)))
    (format nil "~A,~A" (point-x p) (point-y p))))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("3") && stdout.contains("4"),
        "defclass + make-instance should produce object with correct slots, got: '{}'",
        stdout
    );
}

#[test]
fn eval_defmethod_dispatches_correctly() {
    let expr = r#"(progn
  (defclass animal () ())
  (defclass dog (animal) ())
  (defmethod speak ((a animal)) "generic")
  (defmethod speak ((d dog)) "woof")
  (speak (make-instance 'dog)))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("woof"),
        "defmethod should dispatch to most specific method, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Package system (defpackage / in-package)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_defpackage_and_intern() {
    let expr = r#"(progn
  (defpackage :test-pkg (:use :cl) (:export :hello))
  (in-package :test-pkg)
  (defun hello () "hi from test-pkg")
  (in-package :cl-user)
  (test-pkg:hello))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("hi from test-pkg"),
        "defpackage + exported symbol should be accessible, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Macro definition (defmacro)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_defmacro_and_expansion() {
    let expr = r#"(progn
  (defmacro when-true (test &body body)
    `(if ,test (progn ,@body)))
  (when-true t (format nil "macro-expanded")))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("macro-expanded"),
        "defmacro should define a working macro, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Multiple values (multiple-value-bind)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_multiple_value_bind() {
    let expr = r#"(multiple-value-bind (q r) (floor 17 5)
  (format nil "~A ~A" q r))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('3') && stdout.contains('2'),
        "multiple-value-bind of (floor 17 5) should yield 3 and 2, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Error signaling (error / cerror)
// ══════════════════════════════════════════════════════════════════

#[test]
fn eval_error_produces_nonzero_exit() {
    let output = bliss_bin()
        .args(["--eval", "(error \"fatal error test\")"])
        .output()
        .expect("failed to run bliss");
    // An unhandled error should cause a non-zero exit or print the error
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.code() != Some(0)
            || stderr.contains("fatal error test")
            || stdout.contains("fatal error test"),
        "unhandled (error ...) should produce non-zero exit or error message, exit={:?}, stderr='{}', stdout='{}'",
        output.status.code(),
        stderr,
        stdout
    );
}

#[test]
fn eval_cerror_with_handler_continues() {
    let expr = r#"(handler-bind ((error (lambda (c) (invoke-restart 'continue))))
  (cerror "Continue anyway" "soft error")
  (format nil "continued"))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("continued"),
        "cerror with continue restart should resume, got: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Sandbox mode (restricts filesystem/network operations)
// ══════════════════════════════════════════════════════════════════

#[test]
fn sandbox_mode_restricts_file_access() {
    let output = bliss_bin()
        .args([
            "--sandbox",
            "--eval",
            "(with-open-file (s \"/etc/passwd\" :direction :input) (read-line s))",
        ])
        .output()
        .expect("failed to run bliss");
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    // In sandbox mode, file access should be denied
    assert!(
        output.status.code() != Some(0)
            || stderr.to_uppercase().contains("DENIED")
            || stderr.to_uppercase().contains("SANDBOX")
            || stderr.to_uppercase().contains("PERMISSION")
            || stderr.to_uppercase().contains("ERROR")
            || stdout.to_uppercase().contains("DENIED")
            || stdout.to_uppercase().contains("ERROR"),
        "sandbox mode should deny file access, exit={:?}, stderr='{}', stdout='{}'",
        output.status.code(),
        stderr,
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// --no-init: skip init file loading
// ══════════════════════════════════════════════════════════════════

#[test]
fn no_init_flag_skips_init_file() {
    // Create a temporary init file that would print a side effect if loaded
    let dir = std::env::temp_dir().join("bliss_test_no_init");
    let _ = std::fs::create_dir_all(&dir);
    let init_file = dir.join(".blissrc");
    std::fs::write(&init_file, "(print \"INIT-FILE-LOADED\")\n")
        .expect("failed to write init file");

    // Run with --no-init --eval — the init file side effect should NOT appear
    let output = bliss_bin()
        .args(["--no-init", "--eval", "(print \"main\")"])
        .env("BLISS_INIT_FILE", init_file.to_str().unwrap())
        .env("HOME", dir.to_str().unwrap())
        .output()
        .expect("failed to run bliss");

    let _ = std::fs::remove_file(&init_file);
    let _ = std::fs::remove_dir(&dir);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    // With --no-init, the init file's side effect should be suppressed
    assert!(
        !stdout.contains("INIT-FILE-LOADED") && !stderr.contains("INIT-FILE-LOADED"),
        "--no-init should suppress init file loading, stdout: '{}', stderr: '{}'",
        stdout,
        stderr
    );
}

// ══════════════════════════════════════════════════════════════════
// REPL debugger / error recovery
// ══════════════════════════════════════════════════════════════════

#[test]
fn repl_error_shows_debugger_prompt() {
    // When an unhandled error occurs in the REPL, CL implementations
    // enter a debugger break level. Verify that an error expression
    // produces a debugger prompt (e.g. "Debug>" or break level indicator)
    // and that abort returns to the top level.
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        // Trigger an error, then try to abort back to top level, then quit
        stdin
            .write_all(b"(error \"test-debugger-error\")\n:abort\n(quit)\n")
            .expect("failed to write to stdin");
    }

    // Timeout protection
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(());
        result
    });
    let output = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(_) => handle.join().unwrap().expect("failed to wait on bliss"),
        Err(_) => panic!("REPL debugger test timed out after 10 seconds"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let all_output = format!("{}{}", stdout, stderr);
    // The REPL should show some indication of the error and a debugger prompt
    assert!(
        all_output.contains("test-debugger-error")
            || all_output.to_uppercase().contains("ERROR")
            || all_output.to_uppercase().contains("DEBUG")
            || all_output.contains("[1]")
            || all_output.contains("Break"),
        "REPL should show error/debugger when an unhandled error occurs, got stdout: '{}', stderr: '{}'",
        stdout,
        stderr
    );
}

#[test]
fn repl_error_abort_returns_to_top_level() {
    // After an error in the REPL, :abort should return to the top-level
    // and the REPL should continue accepting input.
    let mut child = bliss_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start bliss");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        // Error → abort → evaluate a normal expression → quit
        stdin
            .write_all(b"(error \"recoverable\")\n:abort\n(+ 1 1)\n(quit)\n")
            .expect("failed to write to stdin");
    }

    let (tx, rx) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        let result = child.wait_with_output();
        let _ = tx.send(());
        result
    });
    let output = match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(_) => handle.join().unwrap().expect("failed to wait on bliss"),
        Err(_) => panic!("REPL abort test timed out after 10 seconds"),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // After abort, (+ 1 1) should evaluate to 2, proving the REPL recovered
    assert!(
        stdout.contains('2'),
        "after :abort, REPL should recover and evaluate (+ 1 1) to 2, got stdout: '{}'",
        stdout
    );
}

// ══════════════════════════════════════════════════════════════════
// Image save/load cycle
// ══════════════════════════════════════════════════════════════════

#[test]
fn image_save_and_load_cycle() {
    let dir = std::env::temp_dir().join("bliss_test_image");
    let _ = std::fs::create_dir_all(&dir);
    let image_path = dir.join("test.image");

    // Step 1: Define a function and save an image
    let save_expr = format!(
        "(progn (defun my-fn () 99) (save-image \"{}\"))",
        image_path.to_str().unwrap().replace('\\', "\\\\")
    );
    let output = bliss_bin()
        .args(["--eval", &save_expr])
        .output()
        .expect("failed to run bliss for save");
    assert_eq!(
        output.status.code(),
        Some(0),
        "save-image should succeed, stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );

    // Step 2: Load the image and call the function
    let output = bliss_bin()
        .args(["--image", image_path.to_str().unwrap(), "--eval", "(my-fn)"])
        .output()
        .expect("failed to run bliss for load");

    // Clean up
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("99"),
        "loading image should restore my-fn returning 99, got: '{}'",
        stdout
    );
}
