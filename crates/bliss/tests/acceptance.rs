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

/// Regression: FIND/POSITION/COUNT/SORT/REMOVE with a real interpreter
/// :key/:test used to reach a stdlib helper that only understood sentinel keys
/// and `panic!`ed — aborting the whole process, uncatchable by HANDLER-CASE
/// (bliss-0l1). They are now defined over ELT/LENGTH/FUNCALL and accept any
/// function, and no path aborts the process.
#[test]
fn sequence_key_test_with_real_functions() {
    let cases = [
        // Named function as :key / :test (the original panic repro).
        ("(find 2 '((1 a) (2 b)) :key (function car))", "(2 B)"),
        ("(position 3 '(1 2 3) :test (function =))", "2"),
        ("(count 2 '(1 2 2 3 2) :test (function =))", "3"),
        // Lambda as :test.
        ("(find 10 '(1 5 10 20) :test (lambda (a b) (= a b)))", "10"),
        // :from-end, :start, strings, :test-not.
        ("(find 2 '((2 a) (2 b)) :key (function car) :from-end t)", "(2 B)"),
        ("(position #\\a \"banana\" :start 2)", "3"),
        ("(sort (list (list 2) (list 1)) (function <) :key (function car))", "((1) (2))"),
        ("(remove \"x\" '(\"a\" \"x\" \"b\") :test (function equal))", "(\"a\" \"b\")"),
        ("(remove 1 '((1 a) (2 b) (1 c)) :key (function car) :test-not (function =))", "((1 A) (1 C))"),
        // The unsupported-function path must be catchable, never abort.
        ("(ignore-errors (find 2 '((1 a) (2 b)) :key (function car)))", "(2 B)"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (no panic/abort)");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.trim().to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: HANDLER-BIND now sees raw evaluator errors, and malformed calls
/// signal a catchable PROGRAM-ERROR instead of an uncatchable Internal error
/// (bliss-qry). Control-flow tokens (BLOCK/RETURN-FROM, CATCH/THROW) must never
/// be misrouted into the condition machinery.
#[test]
fn handler_bind_raw_errors_and_program_error() {
    let cases = [
        // HANDLER-BIND handler fires on a raw TYPE-ERROR ((car 5)); it declines
        // (returns), so IGNORE-ERRORS still contains the error. FIRED proves the
        // handler ran.
        (
            "(let ((fired nil)) \
               (ignore-errors \
                 (handler-bind ((type-error (lambda (e) (declare (ignore e)) (setq fired t)))) \
                   (car 5))) \
               fired)",
            "T",
        ),
        // A HANDLER-BIND handler may invoke a restart that ENCLOSES the binding.
        (
            "(restart-case \
               (handler-bind ((type-error (lambda (e) (declare (ignore e)) (invoke-restart 'my-restart)))) \
                 (car 5)) \
               (my-restart () 'restarted))",
            "RESTARTED",
        ),
        // Handler on UNBOUND-VARIABLE fires too.
        (
            "(let ((n 0)) \
               (ignore-errors \
                 (handler-bind ((unbound-variable (lambda (e) (declare (ignore e)) (incf n)))) \
                   *definitely-unbound*)) \
               n)",
            "1",
        ),
        // Declining HANDLER-BIND lets an enclosing HANDLER-CASE catch the error.
        (
            "(handler-case \
               (handler-bind ((type-error (lambda (e) e))) (car 5)) \
               (type-error (e) (declare (ignore e)) 'outer))",
            "OUTER",
        ),
        // Malformed call (too many arguments) is a catchable PROGRAM-ERROR …
        (
            "(handler-case (funcall (lambda (x) x) 1 2 3) (program-error (e) (declare (ignore e)) 'caught-pe))",
            "CAUGHT-PE",
        ),
        // … and hence catchable as its superclass ERROR / by IGNORE-ERRORS.
        (
            "(handler-case (funcall (lambda (x) x) 1 2 3) (error (e) (declare (ignore e)) 'caught-err))",
            "CAUGHT-ERR",
        ),
        // Safety: BLOCK/RETURN-FROM and CATCH/THROW tokens are not misrouted even
        // when a matching HANDLER-BIND is on the stack.
        (
            "(block b (handler-bind ((error (lambda (e) (declare (ignore e)) 'nope))) (return-from b 'ok)))",
            "OK",
        ),
        (
            "(catch 'tag (handler-bind ((error (lambda (e) (declare (ignore e)) 'nope))) (throw 'tag 'thrown)))",
            "THROWN",
        ),
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

/// Regression: handlers are grouped into per-form clusters (bliss-uh4.1). While
/// any handler in a HANDLER-BIND/HANDLER-CASE runs, its whole cluster — not just
/// that one handler — and every newer cluster are disestablished (R5.94/R5.102),
/// so a re-signalled condition is seen only by strictly-older clusters. Handlers
/// within a cluster are tried in source order, so the first matching HANDLER-CASE
/// clause wins.
#[test]
fn handler_clusters_disestablish_whole_cluster_on_resignal() {
    let cases = [
        // A sibling handler in the SAME handler-bind must not catch a condition
        // re-signalled by its cluster-mate. *sib* stays NIL; the outer
        // HANDLER-CASE catches the re-signal.
        (
            "(defvar *sib* nil) \
             (handler-case \
               (handler-bind ((error (lambda (c) (declare (ignore c)) (error \"resig\"))) \
                              (error (lambda (c) (declare (ignore c)) (setq *sib* t)))) \
                 (error \"init\")) \
               (error (e) (declare (ignore e)) (list :outer *sib*)))",
            "(:OUTER NIL)",
        ),
        // Within one HANDLER-BIND, the first matching handler runs first.
        (
            "(defvar *log1* nil) \
             (handler-case \
               (handler-bind ((error (lambda (c) (declare (ignore c)) (push :a *log1*) (error \"x\"))) \
                              (error (lambda (c) (declare (ignore c)) (push :b *log1*)))) \
                 (error \"init\")) \
               (error (e) (declare (ignore e)) (reverse *log1*)))",
            "(:A)",
        ),
        // HANDLER-CASE: a specific clause listed before a general one wins.
        (
            "(handler-case (error 'type-error :datum 1 :expected-type 'string) \
               (type-error (e) (declare (ignore e)) :specific) \
               (error (e) (declare (ignore e)) :general))",
            "SPECIFIC",
        ),
        // Nested HANDLER-BINDs: a re-signal reaches the OUTER cluster, not the
        // inner one again.
        (
            "(defvar *seen* nil) \
             (handler-case \
               (handler-bind ((error (lambda (c) (declare (ignore c)) (push :outer *seen*)))) \
                 (handler-bind ((error (lambda (c) (declare (ignore c)) (push :inner *seen*) (error \"re\")))) \
                   (error \"go\"))) \
               (error (e) (declare (ignore e)) (reverse *seen*)))",
            "(:INNER :OUTER)",
        ),
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

/// R2.20 / bliss-nmq: runaway interpreter recursion raises a catchable
/// STORAGE-CONDITION via a host-stack depth guard, instead of overflowing the
/// native stack into a process-killing SIGSEGV.
#[test]
fn deep_recursion_raises_catchable_storage_condition_not_sigsegv() {
    // Caught as STORAGE-CONDITION (its own type)…
    let caught = bliss_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
               (storage-condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run bliss");
    assert_eq!(caught.status.code(), Some(0), "overflow should be catchable, exit 0");
    assert!(
        String::from_utf8_lossy(&caught.stdout).to_uppercase().contains("CAUGHT"),
        "storage-condition clause should fire: {}",
        String::from_utf8_lossy(&caught.stdout)
    );

    // …and as the CONDITION superclass.
    let via_super = bliss_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f () (f))) (f)) \
               (condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run bliss");
    assert!(
        String::from_utf8_lossy(&via_super.stdout).to_uppercase().contains("CAUGHT"),
        "condition clause should fire on overflow"
    );

    // Uncaught overflow must still exit gracefully — a normal exit code, never a
    // signal (status.code() == None would mean the process was killed, e.g. by
    // SIGSEGV/SIGABRT).
    let uncaught = bliss_bin()
        .args(["--eval", "(labels ((f () (f))) (f))"])
        .output()
        .expect("run bliss");
    assert!(
        uncaught.status.code().is_some(),
        "uncaught stack overflow must not kill the process with a signal"
    );

    // A recursion depth well within budget still runs normally.
    let ok = bliss_bin()
        .args([
            "--eval",
            "(labels ((down (n) (if (= n 0) :done (down (- n 1))))) (down 1500))",
        ])
        .output()
        .expect("run bliss");
    assert_eq!(ok.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&ok.stdout).to_uppercase().contains("DONE"));
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

// ══════════════════════════════════════════════════════════════════
// Gray streams — CLOS stream hierarchy + generic-function dispatch
// (bliss-jtc.7b). A user-defined FUNDAMENTAL-STREAM subclass must
// dispatch through the Gray generics when the standard stream
// functions are called on it.
// ══════════════════════════════════════════════════════════════════

#[test]
fn gray_output_stream_routes_standard_functions_through_generics() {
    // counting-stream implements only stream-write-char; write-char,
    // write-string (default loops write-char), and terpri (default writes
    // #\Newline) must all reach it. A + bcd + newline = 5 chars.
    let prog = "(progn \
       (defclass counting-stream (fundamental-character-output-stream) \
         ((n :initform 0 :accessor cs-n))) \
       (defmethod stream-write-char ((s counting-stream) ch) \
         (setf (cs-n s) (+ 1 (cs-n s))) ch) \
       (let ((cs (make-instance 'counting-stream))) \
         (write-char #\\A cs) \
         (write-string \"bcd\" cs) \
         (terpri cs) \
         (format t \"COUNT=~a\" (cs-n cs))))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("COUNT=5"),
        "expected COUNT=5 (write-char/write-string/terpri routed through stream-write-char), \
         got stdout: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn gray_input_stream_routes_read_functions_through_generics() {
    // list-input implements only stream-read-char; read-line (default loops
    // stream-read-char) and read-char must reach it.
    let prog = "(progn \
       (defclass list-input (fundamental-character-input-stream) \
         ((cs :initarg :cs :accessor li-cs))) \
       (defmethod stream-read-char ((s list-input)) \
         (if (li-cs s) \
             (let ((c (car (li-cs s)))) (setf (li-cs s) (cdr (li-cs s))) c) \
             :eof)) \
       (let ((s (make-instance 'list-input :cs (list #\\a #\\b #\\Newline #\\c #\\d)))) \
         (format t \"L1=~a L2=~a EOF=~a\" (read-line s) (read-line s) (read-char s nil :done))))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("L1=ab") && stdout.contains("L2=cd") && stdout.contains("EOF=DONE"),
        "expected read-line/read-char routed through stream-read-char, got stdout: '{stdout}', stderr: '{err}'"
    );
}

// ══════════════════════════════════════════════════════════════════
// ASDF long-tail fixes (bliss-lb6.7): -if sequence bounds, LOOP `by`,
// catchable unknown-keyword errors.
// ══════════════════════════════════════════════════════════════════

#[test]
fn sequence_if_functions_accept_start_end_from_end() {
    let prog = "(format t \"~a ~a ~a ~a\" \
       (position-if (function evenp) (list 1 3 4 5 6)) \
       (position-if (function evenp) (list 1 3 4 5 6) :from-end t) \
       (find-if (function evenp) (list 1 3 4 5) :start 1 :end 3) \
       (count-if (function evenp) (list 1 2 3 4 5 6) :end 4))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    // first even @2; last even @4; find-if even in [1,3)=(3 4)->4; evens in first 4=(1 2 3 4)->2
    assert!(
        stdout.contains("2 4 4 2"),
        "expected '2 4 4 2', got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn loop_supports_by_step_for_numeric_and_lists() {
    let prog = "(format t \"~a|~a|~a\" \
       (loop for i from 0 to 10 by 3 collect i) \
       (loop for x in (list 1 2 3 4 5 6) by (function cddr) collect x) \
       (loop for x on (list 1 2 3 4) by (function cddr) collect (car x)))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(0 3 6 9)|(1 3 5)|(1 3)"),
        "expected loop-by results, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unknown_keyword_argument_is_a_catchable_program_error() {
    let prog = "(handler-case (find-if (function evenp) (list 1 2) :bogus 3) \
       (program-error (e) (declare (ignore e)) (format t \"CAUGHT\")))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CAUGHT"),
        "unknown keyword should signal a catchable PROGRAM-ERROR, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reexported_inherited_symbol_keeps_identity_and_home_package() {
    // bliss-lb6.8: a package that :uses another and re-exports one of its
    // symbols must share the SAME symbol (same home package), not fork a new
    // one — otherwise a downstream package reaching it via two :use paths sees
    // a conflict (as UIOP's define-package does).
    let prog = "(progn \
       (defpackage :lb6a (:use) (:export #:sym)) \
       (defpackage :lb6b (:use :lb6a) (:export #:sym)) \
       (defpackage :lb6c (:use :lb6a :lb6b)) \
       (let ((a (find-symbol \"SYM\" :lb6a)) \
             (b (find-symbol \"SYM\" :lb6b)) \
             (c (find-symbol \"SYM\" :lb6c))) \
         (format t \"~a ~a ~a\" \
           (package-name (symbol-package b)) (eq a b) (eq a c))))";
    let output = bliss_bin().args(["--eval", prog]).output().expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LB6A T T"),
        "re-exported inherited symbol should stay EQ with home LB6A, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}
