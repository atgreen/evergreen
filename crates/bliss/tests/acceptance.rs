//! End-to-end acceptance tests for the Bliss CLI binary.
//!
//! These tests build and invoke the real `bliss-cli` binary via std::process::Command,
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
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn cargo_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn bliss_bin_path() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let _guard = cargo_lock().lock().unwrap_or_else(|e| e.into_inner());
        let status = Command::new("cargo")
            .current_dir(repo_root())
            .args(["build", "-p", "bliss-cli"])
            .status()
            .expect("build bliss-cli");
        assert!(status.success(), "cargo build -p bliss-cli failed");
        repo_root().join("target/debug/bliss-cli")
    })
    .as_path()
}

/// Get the path to the bliss-cli binary built by cargo.
fn bliss_bin() -> Command {
    Command::new(bliss_bin_path())
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
        stderr.contains("CL-USER>") || stdout.contains("CL-USER>"),
        "REPL should display the current-package prompt, stdout: '{}', stderr: '{}'",
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

/// Regression for bliss-2ke: CLOS instances must not be representable as
/// fixnums. Instance ids were once `from_fixnum(id)` (starting at 100000), so a
/// plain integer equal to a live instance id collided with it in the registry
/// (`(typep <int> <class>)` wrongly true) and `type-of`/printing treated the
/// instance as a number. Instances are now SPECIAL-tagged handles.
#[test]
fn eval_instance_not_confused_with_fixnum() {
    // type-of an instance is its class, not FIXNUM; and no integer is ever an
    // instance regardless of value (probe a wide range that spans the old id
    // base of 100000).
    let expr = r#"(progn
  (defclass point () ((x :initarg :x)))
  (let ((p (make-instance 'point :x 5)))
    (format nil "~A|~A|~A"
            (type-of p)
            (typep p 'point)
            (some (lambda (i) (typep i 'point))
                  '(99999 100000 100001 100002 100003 100004 100005 100006)))))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("POINT|T|NIL"),
        "instance must have class type, match its class, and never collide with \
         an integer; got: '{}'",
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

#[test]
fn eval_multiple_values_do_not_leak_through_single_value_ops() {
    // bliss-2pt.12: a producer (GETHASH/FLOOR) nested inside a single-value
    // operation (arithmetic, LIST, a single-valued function) must not leak its
    // extra values to an enclosing multiple-value consumer, while transparent
    // tails and genuine producers must still propagate them.
    let expr = r#"
(let ((h (make-hash-table)))
  (setf (gethash 'k h) 42)
  (flet ((mv (form) (multiple-value-bind (a b) form (list a b))))
    (format nil "~A|~A|~A|~A|~A|~A|~A"
      (multiple-value-bind (a b) (+ 0 (gethash 'k h)) (list a b))   ; leak -> (42 NIL)
      (multiple-value-bind (a b) (list (gethash 'k h)) (list a b))  ; leak -> ((42) NIL)
      (multiple-value-bind (a b) (identity (gethash 'k h)) (list a b)) ; leak -> (42 NIL)
      (multiple-value-bind (a b) (if t (floor 7 2) 0) (list a b))   ; transparent -> (3 1)
      (multiple-value-bind (a b) (progn (floor 7 2)) (list a b))    ; transparent -> (3 1)
      (multiple-value-bind (a b) (funcall #'floor 7 2) (list a b))  ; producer -> (3 1)
      (multiple-value-bind (a b) (values 1 2) (list a b)))))"#; // producer -> (1 2)
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(42 NIL)|((42) NIL)|(42 NIL)|(3 1)|(3 1)|(3 1)|(1 2)"),
        "multiple values leaked or a producer was truncated, got: '{}'",
        stdout
    );
}

#[test]
fn handler_case_catches_runtime_errors() {
    // Runtime errors raised by the evaluator (not only conditions raised through
    // SIGNAL/ERROR) are catchable CL conditions: HANDLER-CASE / IGNORE-ERRORS
    // must catch TYPE-ERROR, UNBOUND-VARIABLE, UNDEFINED-FUNCTION, and
    // DIVISION-BY-ZERO, matching by the condition class hierarchy. Control-flow
    // transfers (BLOCK/RETURN-FROM) are not conditions and must pass through.
    let expr = r#"(format nil "~A|~A|~A|~A|~A|~A|~A"
  (handler-case (car 5) (type-error (e) (type-error-datum e)))
  (handler-case undefined-var (unbound-variable (e) :unbound))
  (handler-case (nosuchfn 1) (undefined-function (e) :undef))
  (handler-case (/ 1 0) (division-by-zero (e) :divzero))
  (handler-case (car 5) (error (e) :as-error))
  (ignore-errors (car 5))
  (handler-case (block b (return-from b 7)) (error (e) :wrongly-caught)))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("5|UNBOUND|UNDEF|DIVZERO|AS-ERROR|NIL|7"),
        "runtime errors should be catchable and control transfers pass through, got: '{}'",
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

/// Regression for the handler re-signal bug (R5.94/R5.102): while a HANDLER-BIND
/// handler runs, its cluster is disestablished, so a condition it re-signals is
/// seen only by OLDER handlers — never itself. Before the fix this recursed into
/// the same handler forever and overflowed the stack.
#[test]
fn handler_bind_resignal_is_seen_by_outer_handler_not_itself() {
    let expr = r#"(progn
      (define-condition my-c (error) ())
      (handler-case
          (handler-bind ((my-c (lambda (c) (declare (ignore c)) (error 'my-c))))
            (error 'my-c))
        (my-c (e) (declare (ignore e)) (princ "outer-caught"))))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(
        output.status.code(),
        Some(0),
        "recursive re-signal must not crash; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("outer-caught"),
        "re-signalled condition must be caught by the outer handler; got: '{}'",
        stdout
    );
}

/// Regression for bliss-gd4: restart/handler functions share their establishing
/// LEXICAL frame (not a frozen snapshot), so a setf/setq inside them persists to
/// the enclosing scope. Before the fix these writes went into a thawed throwaway
/// copy and were lost.
#[test]
fn mutations_inside_restart_functions_persist() {
    // setq of an outer lexical variable inside a RESTART-CASE restart persists.
    let out1 = bliss_bin()
        .args(["--eval", "(let ((x 0)) (restart-case (invoke-restart 'bump) (bump () (setq x 99))) x)"])
        .output()
        .expect("run bliss");
    assert_eq!(out1.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out1.stdout).contains("99"),
        "setq inside a restart must persist; got: {}",
        String::from_utf8_lossy(&out1.stdout)
    );

    // CHECK-TYPE's STORE-VALUE restart corrects the place end-to-end.
    let out2 = bliss_bin()
        .args([
            "--eval",
            "(let ((x 'foo)) (handler-bind ((type-error (lambda (c) (declare (ignore c)) (store-value 42)))) (check-type x integer)) x)",
        ])
        .output()
        .expect("run bliss");
    assert_eq!(out2.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out2.stdout).contains("42"),
        "store-value must correct the place; got: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// Regression for numeric LOOP: `for var from/to/below/downto/by`, the
/// accumulation clauses (sum/count/maximize/minimize), always/never, and across
/// — previously the numeric sub-keywords weren't recognized so these were dead
/// code, and `downto` infinite-looped. (bliss-2pt LOOP completeness.)
#[test]
fn loop_numeric_iteration_and_accumulation() {
    let cases = [
        ("(loop for i from 1 to 5 collect i)", "(1 2 3 4 5)"),
        ("(loop for i from 10 downto 7 collect i)", "(10 9 8 7)"),
        ("(loop for i from 0 to 10 by 2 collect i)", "(0 2 4 6 8 10)"),
        ("(loop for i from 1 to 5 sum i)", "15"),
        ("(loop for i from 1 to 5 count (oddp i))", "3"),
        ("(loop for i from 1 to 5 maximize i)", "5"),
        ("(loop for i from 1 to 5 minimize i)", "1"),
        ("(loop for i from 1 to 5 always (< i 9))", "T"),
        ("(loop for i from 1 to 5 always (< i 3))", "NIL"),
        ("(loop for i from 1 to 5 never (> i 9))", "T"),
        ("(loop for i from 1 to 10 when (evenp i) sum i)", "30"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression for LOOP while/until/repeat driver clauses (bliss-2pt.10).
#[test]
fn loop_while_until_repeat_drivers() {
    let cases = [
        ("(loop repeat 3 collect 'x)", "(X X X)"),
        ("(loop repeat 0 collect 'x)", "NIL"),
        ("(let ((i 0)) (loop while (< i 4) do (incf i) collect i))", "(1 2 3 4)"),
        ("(let ((i 0)) (loop until (>= i 3) do (incf i) collect i))", "(1 2 3)"),
        ("(loop for i from 1 to 100 while (< i 4) collect i)", "(1 2 3)"),
        ("(loop for i from 1 repeat 3 collect i)", "(1 2 3)"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: DOTIMES/DOLIST establish an implicit `block nil`, so `(return x)`
/// in the body exits the loop with x. Previously this errored "no block named
/// NIL", breaking the ubiquitous (dolist (x l) (when … (return …))) pattern.
#[test]
fn return_exits_dotimes_and_dolist() {
    let cases = [
        ("(dotimes (i 5 :done) (when (= i 2) (return :hit)))", "HIT"),
        ("(dolist (x '(a b c) :done) (when (eq x 'b) (return x)))", "B"),
        ("(dotimes (i 5 :done) nil)", "DONE"),
        ("(block nil (dotimes (i 10) (when (> i 3) (return-from nil i))))", "4"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: REDUCE and the sequence -IF/-IF-NOT predicate family were
/// undefined. (bliss-2pt stdlib completeness.)
#[test]
fn reduce_and_if_predicate_family() {
    let cases = [
        ("(reduce (function +) '(1 2 3 4))", "10"),
        ("(reduce (function +) '(1 2 3) :initial-value 100)", "106"),
        ("(reduce (function +) '())", "0"),
        ("(find-if (function evenp) '(1 3 4 5))", "4"),
        ("(position-if (function evenp) '(1 3 4 5))", "2"),
        ("(count-if (function evenp) '(1 2 3 4))", "2"),
        ("(member-if (function evenp) '(1 3 4 5))", "(4 5)"),
        ("(find-if-not (function evenp) '(2 4 5 6))", "5"),
        ("(assoc-if (function evenp) '((1 . a) (2 . b)))", "(2 . B)"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: PSETQ, DO, and DO* iteration macros (bliss-2pt.11).
#[test]
fn do_dostar_psetq() {
    let cases = [
        ("(let ((a 1) (b 2)) (psetq a b b a) (list a b))", "(2 1)"),
        ("(do ((i 0 (1+ i)) (acc nil)) ((= i 3) (reverse acc)) (push i acc))", "(0 1 2)"),
        ("(do* ((i 0 (1+ i)) (j (* i 10) (* i 10))) ((= i 3) j))", "30"),
        ("(do ((i 0 (1+ i)) (s 0 (+ s i))) ((= i 5) s))", "10"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: character functions (char-code/code-char + comparisons, case,
/// predicates), string-upcase/downcase, and list fns (last/butlast/nthcdr/
/// mapc/mapcan/getf/nreverse) were undefined. (bliss-2pt stdlib completeness.)
#[test]
fn char_string_and_list_functions() {
    let cases = [
        ("(char-code #\\A)", "65"),
        ("(code-char 66)", "B"),
        ("(char< #\\a #\\b)", "T"),
        ("(char-upcase #\\a)", "A"),
        ("(digit-char-p #\\7)", "7"),
        ("(alpha-char-p #\\5)", "NIL"),
        ("(string-upcase \"hello\")", "HELLO"),
        ("(last '(1 2 3))", "(3)"),
        ("(butlast '(1 2 3))", "(1 2)"),
        ("(nthcdr 2 '(a b c d))", "(C D)"),
        ("(mapcan (function list) '(1 2 3))", "(1 2 3)"),
        ("(getf '(:a 1 :b 2) :b)", "2"),
        ("(nreverse (list 1 2 3))", "(3 2 1)"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: GCD/LCM and the STRING-TRIM family were undefined.
#[test]
fn gcd_lcm_and_string_trim() {
    let cases = [
        ("(gcd 12 18)", "6"),
        ("(gcd 12 18 24)", "6"),
        ("(lcm 4 6)", "12"),
        ("(string-trim \" \" \"  hi  \")", "\"hi\""),
        ("(string-left-trim \" x\" \"xx hi\")", "\"hi\""),
        ("(string-right-trim \" \" \"hi  \")", "\"hi\""),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: FORMAT ~w,dF honors d (decimal places) and w (width). Previously
/// the d parameter was ignored, so ~,2F printed full precision. (bliss-2pt.13)
#[test]
fn format_fixed_float_directive() {
    let cases = [
        ("(format nil \"~,2f\" 3.14159)", "\"3.14\""),
        ("(format nil \"~,3f\" 3.14159)", "\"3.142\""),
        ("(format nil \"~8,2f\" 3.14159)", "\"    3.14\""),
        ("(format nil \"~,0f\" 3.7)", "\"4\""),
        ("(format nil \"~f\" 2.5)", "\"2.5\""),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: CHAR, ACONS, LIST-LENGTH, NCONC, REVAPPEND, MAKE-LIST,
/// STRING-EQUAL, and SUBST were undefined. (bliss-2pt stdlib completeness.)
#[test]
fn more_list_tree_string_functions() {
    let cases = [
        ("(char \"hello\" 1)", "e"),
        ("(acons 'k 'v nil)", "((K . V))"),
        ("(list-length '(1 2 3))", "3"),
        ("(nconc (list 1 2) (list 3 4))", "(1 2 3 4)"),
        ("(revappend '(1 2 3) '(a b))", "(3 2 1 A B)"),
        ("(make-list 3 :initial-element 'x)", "(X X X)"),
        ("(string-equal \"ABC\" \"abc\")", "T"),
        ("(subst 'x 'a '(a b (a c)))", "(X B (X C))"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}
