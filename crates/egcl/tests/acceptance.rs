// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! End-to-end acceptance tests for the EGCL CLI binary.
//!
//! These tests build and invoke the real `egcl` binary via std::process::Command,
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
use std::sync::OnceLock;
use std::time::Duration;

fn egcl_bin_path() -> &'static Path {
    // Cargo builds the bin as a prerequisite of this integration test and sets
    // CARGO_BIN_EXE_egcl to its path — target-aware, so this works whether
    // the default target is glibc or the static musl target (bliss-bca.5).
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_egcl")))
        .as_path()
}

/// Get the path to the egcl binary built by cargo.
fn egcl_bin() -> Command {
    let mut cmd = Command::new(egcl_bin_path());
    // Keep tests hermetic against the developer's real ~/.egclrc: point the init
    // file at a path that does not exist, so REPL-mode runs never load it (a
    // present init file would otherwise inject output/latency and fail the REPL
    // prompt tests). Tests that exercise init loading set EGCL_INIT_FILE
    // themselves, overriding this.
    cmd.env(
        "EGCL_INIT_FILE",
        std::env::temp_dir().join("egcl-tests-nonexistent-init.lisp"),
    );
    cmd
}

// ══════════════════════════════════════════════════════════════════
// --help
// ══════════════════════════════════════════════════════════════════

#[test]
fn multiple_eval_forms_all_run_and_share_one_env() {
    // bliss-7zl: every --eval form must run, in order, sharing one env — so
    // state (a defvar here) created by an earlier form is visible to later ones.
    // Previously only the last --eval ran.
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defvar *seven-z-l* 41)",
            "--eval",
            "(princ (+ *seven-z-l* 1))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "second --eval should see the first's defvar (expect 42), got: {stdout}"
    );
}

#[test]
fn help_flag_prints_usage_and_exits_zero() {
    let output = egcl_bin()
        .arg("--help")
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .arg("--version")
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("egcl"),
        "version output should contain 'egcl', got: {}",
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
    let output = egcl_bin()
        .args(["--eval", "(+ 1 2)"])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["-e", "(quote hello)"])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", "(format nil \"hello ~A\" 'world)"])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", "(values 1 2 3)"])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", "nil"])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", "t"])
        .output()
        .expect("failed to run egcl");
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
    let dir = std::env::temp_dir().join("egcl_test_load");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_load.lisp");
    std::fs::write(&file_path, "(print 42)\n").expect("failed to write test file");

    let output = egcl_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("failed to run egcl");

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
    let output = egcl_bin()
        .args(["--load", "/tmp/egcl_nonexistent_file_12345.lisp"])
        .output()
        .expect("failed to run egcl");
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
    let dir = std::env::temp_dir().join("egcl_test_script");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_script.lisp");
    std::fs::write(&file_path, "(print \"hello from script\")\n")
        .expect("failed to write test file");

    let output = egcl_bin()
        .arg(file_path.to_str().unwrap())
        .output()
        .expect("failed to run egcl");

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
    let dir = std::env::temp_dir().join("egcl_test_clargs");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("test_args.lisp");
    // Script that prints *command-line-args*
    std::fs::write(&file_path, "(print *command-line-args*)\n").expect("failed to write test file");

    let output = egcl_bin()
        .arg(file_path.to_str().unwrap())
        .args(["--", "foo", "bar"])
        .output()
        .expect("failed to run egcl");

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
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(+ 2 3)\n(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on egcl");
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
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

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
        Ok(_) => handle.join().unwrap().expect("failed to wait on egcl"),
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
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(exit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "(exit) should cleanly exit with code 0"
    );
}

#[test]
fn repl_eof_exits_cleanly() {
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    // Close stdin immediately (EOF)
    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "EOF on stdin should exit cleanly with code 0"
    );
}

#[test]
fn repl_defun_and_call() {
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(defun square (x) (* x x))\n(square 7)\n(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on egcl");
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
    let output = egcl_bin()
        .arg("--frobnicate")
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--image", "core.img", "--no-image"])
        .output()
        .expect("failed to run egcl");
    assert_ne!(
        output.status.code(),
        Some(0),
        "--image + --no-image should fail"
    );
}

#[test]
fn eval_missing_value_exits_nonzero() {
    let output = egcl_bin()
        .arg("--eval")
        .output()
        .expect("failed to run egcl");
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
    let mut child = egcl_bin()
        .args(["--bootstrap"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    {
        let stdin = child.stdin.as_mut().expect("failed to open stdin");
        stdin
            .write_all(b"(quit)\n")
            .expect("failed to write to stdin");
    }

    let output = child.wait_with_output().expect("failed to wait on egcl");
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
    let mut child = egcl_bin()
        .args(["--workers", "2"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "--workers 2 should be accepted"
    );
}

#[test]
fn heap_size_flag_accepted() {
    let mut child = egcl_bin()
        .args(["--heap-size", "128M"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

    drop(child.stdin.take());

    let output = child.wait_with_output().expect("failed to wait on egcl");
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
    let output = egcl_bin()
        .args([
            "--eval",
            "(handler-case (error \"boom\") (error (c) (format nil \"caught: ~A\" c)))",
        ])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("handled"),
        "handler-bind should invoke handler, got: '{}'",
        stdout
    );
}

#[test]
fn aref_ignores_fill_pointer() {
    // CLHS: AREF ignores fill pointers — it may access any element up to the
    // total size, and (SETF AREF) likewise. ARRAY-DIMENSION/ARRAY-DIMENSIONS
    // report the total size, while ELT/LENGTH follow the fill pointer. bliss-30be:
    // previously AREF delegated to ELT and errored at any index >= fill pointer,
    // which broke ansi-test universe.lsp's fill-pointer bit vectors.
    let expr = "(let ((v (make-array 5 :fill-pointer 3 :initial-element 0)))
                  (setf (aref v 4) 9)
                  (princ (list (aref v 4) (length v)
                               (array-dimension v 0) (array-dimensions v))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(9 3 5 (5))"),
        "AREF must reach past the fill pointer and dimensions report total size, got: '{}'",
        stdout
    );
}

#[test]
fn shadow_honors_let_bound_package() {
    // A user `(let ((*package* pkg)) (shadow ...))` must shadow in PKG, not in
    // the lexically-enclosing package: package operations read the dynamic value
    // of *PACKAGE*, not a stale env field (bliss-30be). ansi-test's
    // cl-test-package.lsp does all its SHADOW/IMPORT/EXPORT inside such a LET.
    let expr = "(progn
                  (make-package :zztest :use (list :cl))
                  (let ((*package* (find-package :zztest))) (shadow 'foo))
                  (princ (package-shadowing-symbols (find-package :zztest))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_uppercase().contains("FOO"),
        "SHADOW under a LET of *PACKAGE* must record FOO in ZZTEST's shadowing list, got: '{}'",
        stdout
    );
}

#[test]
fn compile_of_generic_function_is_noop() {
    // (COMPILE name) on an already-callable generic function returns NAME rather
    // than an uncatchable "not fbound" internal error — generic functions carry
    // no ordinary function-cell value but are fbound (bliss-30be: ansi-test
    // universe.lsp does `(compile 'a-defgeneric)`).
    let expr = "(progn (defgeneric gf30be (x)) (princ (compile 'gf30be)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "COMPILE of a GF must not error"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.to_uppercase().contains("GF30BE"),
        "COMPILE of a generic function should return its NAME, got: '{}'",
        stdout
    );
}

#[test]
fn setf_unsupported_place_is_catchable() {
    // An unsupported/undefined SETF place signals a catchable PROGRAM-ERROR (not
    // an uncatchable internal abort), so IGNORE-ERRORS/HANDLER-CASE can handle it
    // (bliss-30be: ansi-test universe.lsp wraps `(setf (logical-pathname-
    // translations …) …)` in IGNORE-ERRORS).
    let expr = "(princ (ignore-errors (setf (no-such-setf-place-30be 1) 2)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "an unsupported SETF place inside IGNORE-ERRORS must be caught, not abort"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL"),
        "IGNORE-ERRORS should catch the unsupported place and yield NIL, got: '{}'",
        stdout
    );
}

#[test]
fn find_method_locates_by_qualifiers_and_specializers() {
    // FIND-METHOD returns the method with the given qualifiers and specializers
    // (CLHS 7.6.2; bliss-7y1s: ansi-test universe.lsp builds *methods* with it).
    // `#'gf` on a generic function reifies a FUNCTIONP wrapper closure, which
    // FIND-METHOD maps back to the generic it names. errorp NIL returns NIL
    // when no method matches.
    let expr = "(progn
                  (defgeneric g30be (x y z))
                  (defmethod g30be ((x fixnum) (y fixnum) (z fixnum)) (+ x y z))
                  (defmethod g30be ((x symbol) (y t) (z t)) :sym)
                  (let ((m (find-method #'g30be nil
                             (mapcar #'find-class '(fixnum fixnum fixnum))))
                        (m2 (find-method #'g30be nil
                              (list (find-class 'symbol) (find-class 't) (find-class 't))))
                        (none (find-method #'g30be nil
                                (list (find-class 'character) (find-class 't) (find-class 't)) nil)))
                    (princ (list (and m t) (and m2 t) (not (eql m m2)) none))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "FIND-METHOD must not error");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(T T T NIL)"),
        "FIND-METHOD should locate both methods (distinct) and return NIL for no match, got: '{}'",
        stdout
    );
}

#[test]
fn find_class_of_builtin_number_and_sequence_classes() {
    // FIND-CLASS resolves the standard built-in classes INTEGER/NUMBER/REAL/
    // STRING/… with a coherent CLASS-PRECEDENCE-LIST, additive over the existing
    // FIXNUM/etc. (class-of and subtypep are unchanged) — bliss-qxfg, needed by
    // ansi-test universe.lsp's `(mapcar #'find-class '(integer …))`.
    let expr = "(princ (list (class-name (find-class 'integer))
                              (mapcar #'class-name
                                      (class-precedence-list (find-class 'integer)))
                              (class-name (find-class 'string))
                              (class-of 5)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(INTEGER (INTEGER RATIONAL REAL NUMBER T) STRING"),
        "find-class must resolve the built-in number/sequence classes with a proper CPL, got: '{}'",
        stdout
    );
}

#[test]
fn remove_duplicates_is_linear_and_honors_keywords() {
    // REMOVE-DUPLICATES dedups a large list quickly (O(n) EQL hash fast path —
    // the O(n^2) scan hung ansi-test universe.lsp; bliss-qxfg) and honors
    // :test/:key/:from-end, keeping the LAST occurrence by default (CLHS 17.3).
    let expr = "(princ (list
                  (length (remove-duplicates (loop for i from 1 to 3000 collect (mod i 100))))
                  (remove-duplicates '(a b a))
                  (remove-duplicates '(a b a) :from-end t)
                  (remove-duplicates (list \"a\" \"b\" \"a\") :test #'equal)
                  (remove-duplicates '((1 . x) (2 . y) (1 . z)) :key #'car)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "remove-duplicates must not time out"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(100 (B A) (A B) (\"b\" \"a\") ((2 . Y) (1 . Z)))"),
        "remove-duplicates fast path + keyword semantics, got: '{}'",
        stdout
    );
}

#[test]
fn macroexpansion_is_package_neutral() {
    // Expanding a macro defined in another package must not change the caller's
    // *PACKAGE* (bliss-cpm9): a macro expander runs in a fresh expansion env, and
    // its package must not leak into the global cell — else a following relative
    // LOAD (which resolves bare symbols against *PACKAGE*) breaks. Define a macro
    // and compiler macro in CL-USER, switch to a fresh package that inherits
    // them, expand both, and confirm each expander sees its definition package
    // while the caller remains in the fresh package.
    let expr = "(progn
                  (defmacro pkgtest-m30 () (list '+ 1 2))
                  (defun pkgtest-cm30 (x) x)
                  (define-compiler-macro pkgtest-cm30 (x)
                    (declare (ignore x))
                    (list 'quote (package-name *package*)))
                  (export '(pkgtest-m30 pkgtest-cm30))
                  (make-package :pkgtest30 :use '(:common-lisp :common-lisp-user))
                  (in-package :pkgtest30)
                  (let ((definition-package (pkgtest-cm30 nil)))
                    (macroexpand-1 '(pkgtest-m30))
                    (cl:format t \"~A/~A\" definition-package
                               (cl:package-name cl:*package*))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("COMMON-LISP-USER/PKGTEST30"),
        "macro expansion must use its definition package and restore PKGTEST30, got: '{}'",
        stdout
    );
}

#[test]
fn odd_keyword_args_signal_catchable_error() {
    // An odd number of keyword arguments is a catchable PROGRAM-ERROR, not an
    // uncatchable internal abort — ansi-test's *.ERROR.* tests pass malformed
    // &key args inside IGNORE-ERRORS (bliss-cpm9).
    let expr = "(progn
                  (defun kwfn (&key a b) (list a b))
                  (princ (ignore-errors (kwfn :a 1 :b))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "odd &key args inside IGNORE-ERRORS must be caught, not abort"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL"),
        "IGNORE-ERRORS should catch the odd-keyword PROGRAM-ERROR and yield NIL, got: '{}'",
        stdout
    );
}

#[test]
fn typep_unsigned_byte_accepts_positive_bignum() {
    // (typep <positive bignum> 'unsigned-byte) must be T — UNSIGNED-BYTE is
    // (integer 0 *), not fixnum-only. The old fixnum-only check returned NIL for
    // 2^64, and ansi-test's check-type-error then fed that bignum to MAKE-LIST,
    // building a 2^64-element list (hang) instead of skipping it (bliss-x7aa).
    let expr = "(princ (list (typep (expt 2 64) 'unsigned-byte)
                             (typep (expt 2 100) 'unsigned-byte)
                             (typep (- (expt 2 64)) 'unsigned-byte)
                             (typep -1 'unsigned-byte)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(T T NIL NIL)"),
        "unsigned-byte must accept positive bignums and reject negatives, got: '{}'",
        stdout
    );
}

#[test]
fn append_last_argument_is_the_tail_as_is() {
    // CLHS: APPEND copies all arguments but the last; the last becomes the tail
    // AS-IS (an atom yields a dotted list). The old code flattened the last arg,
    // dropping its atom tail — `(append '(1 2) 'x)` gave (1 2) not (1 2 . X)
    // (ansi-test cons/append.lsp APPEND.4/APPEND.5; bliss-x7aa).
    let expr = "(princ (list (append '(1 2) 'x)
                             (append nil 'x)
                             (append '(1) '(2) 3)
                             (append nil nil 'a)
                             (append '(1 2) '(3 4))
                             (append 'z)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("((1 2 . X) X (1 2 . 3) A (1 2 3 4) Z)"),
        "APPEND's last arg must be the tail as-is, got: '{}'",
        stdout
    );
}

#[test]
fn mapl_maplist_with_no_lists_signal_not_hang() {
    // MAPL/MAPLIST require >=1 list; with zero lists the "all exhausted" test
    // `(some #'null '())` is NIL, so the loop never terminated. Signal a
    // PROGRAM-ERROR instead of hanging (ansi-test mapl.error.3; bliss-x7aa).
    let expr = "(princ (list (typep (nth-value 1 (ignore-errors (mapl #'append))) 'program-error)
                             (typep (nth-value 1 (ignore-errors (maplist #'identity))) 'program-error)
                             (mapl #'identity '(a b c))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "must signal, not hang");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(T T (A B C))"),
        "MAPL/MAPLIST no-list must signal PROGRAM-ERROR and still work with a list, got: '{}'",
        stdout
    );
}

#[test]
fn copy_seq_and_subseq_of_string_are_fresh_and_mutable() {
    // SUBSEQ (and COPY-SEQ = (subseq s 0)) must return a FRESH independent string,
    // not the interned/shared one — else copies are EQ, mutating a copy aliases the
    // original, and default-EQL MEMBER/UNION over COPY-SEQ'd strings mis-dedup
    // (bliss-9kxg). Check distinct identity, mutation independence, and the EQL set
    // ops that depend on it.
    let expr = "(princ (list
                  (eq (copy-seq \"xyz\") (copy-seq \"xyz\"))
                  (let* ((s (copy-seq \"abc\")) (c (copy-seq s)))
                    (setf (char c 0) #\\Z) (list s c))
                  (member (copy-seq \"cc\") (list \"aa\" \"cc\"))
                  (length (union (list (copy-seq \"x\")) (list (copy-seq \"x\"))))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(NIL (\"abc\" \"Zbc\") NIL 2)"),
        "copy-seq/subseq strings must be fresh, mutable, and distinct, got: '{}'",
        stdout
    );
}

#[test]
fn get_properties_and_nsubst_keywords() {
    // GET-PROPERTIES returns (indicator value tail) for the first PLIST indicator
    // EQ to one in the list, else (nil nil nil). NSUBST must forward
    // :key/:test/:test-not to SUBST (it dropped them). ansi-test CONS; bliss-30be.
    let expr = "(princ (list
                  (multiple-value-list (get-properties '(a b c d) '(c)))
                  (multiple-value-list (get-properties '(a b) '(x)))
                  (nsubst '(1 2) '(foo . bar)
                          (list (cons 'foo 'baz) (cons 'foo 'bar)) :test #'equal)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("((C D (C D)) (NIL NIL NIL) ((FOO . BAZ) (1 2)))"),
        "get-properties/nsubst keyword handling, got: '{}'",
        stdout
    );
}

#[test]
fn fixed_arity_builtins_signal_program_error_on_wrong_count() {
    // Calling a fixed-arity builtin with the wrong number of arguments is a
    // PROGRAM-ERROR (CLHS 3.5.1) — egcl accepted any count silently, so
    // ansi-test's (cons)/(consp 'a 'b)/(caddr)/... .ERROR tests saw no error
    // (bliss-30be). Valid calls still work.
    let expr = "(princ (list
                  (typep (nth-value 1 (ignore-errors (cons))) 'program-error)
                  (typep (nth-value 1 (ignore-errors (cons 'a 'b 'c))) 'program-error)
                  (typep (nth-value 1 (ignore-errors (consp 'a 'b))) 'program-error)
                  (typep (nth-value 1 (ignore-errors (caddr))) 'program-error)
                  (cons 1 2)
                  (caddr '(1 2 3 4))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(T T T T (1 . 2) 3)"),
        "fixed-arity builtins must signal PROGRAM-ERROR on wrong count, got: '{}'",
        stdout
    );
}

#[test]
fn member_assoc_nonlist_signal_type_error() {
    // MEMBER's list and ASSOC's alist must be proper lists; a non-list (or an
    // improper tail reached without a match) is a TYPE-ERROR (CLHS; ansi-test
    // member.error/assoc.error, check-type-error over non-lists). Valid and
    // dotted-before-match calls still work.
    let expr = "(princ (list
                  (typep (nth-value 1 (ignore-errors (member 'a 1.3))) 'type-error)
                  (typep (nth-value 1 (ignore-errors (member 'z '(a b . c)))) 'type-error)
                  (typep (nth-value 1 (ignore-errors (assoc 'a 1.3))) 'type-error)
                  (member 'b '(a b c))
                  (member 'a '(a b . c))
                  (assoc 'b '((a . 1) nil (b . 2)))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(T T T (B C) (A B . C) (B . 2))"),
        "MEMBER/ASSOC non-list must signal TYPE-ERROR, valid calls unaffected, got: '{}'",
        stdout
    );
}

#[test]
fn cons_chapter_list_functions_conform() {
    // Batch of CONS-chapter conformance fixes (bliss-30be): COPY-ALIST fresh
    // pairs; LDIFF preserves a dotted tail; LAST on a dotted list; ASSOC-IF
    // TYPE-ERRORs on a non-cons alist element; the mapping functions TYPE-ERROR
    // on a non-list arg; #'cons at wrong arity (funcall path) PROGRAM-ERRORs.
    let expr = "(princ (list
                  (let* ((x (list (cons 'a 'b))) (y (copy-alist x)))
                    (list (equal x y) (eq (car x) (car y))))
                  (ldiff '(a b c . d) 'z)
                  (last '(a b . c))
                  (typep (nth-value 1 (ignore-errors (assoc-if #'null '((a . b) :bad (c . d))))) 'type-error)
                  (typep (nth-value 1 (ignore-errors (mapcar #'identity 3))) 'type-error)
                  (typep (nth-value 1 (ignore-errors (maplist #'identity 3))) 'type-error)
                  (typep (nth-value 1 (ignore-errors (funcall #'cons 'a))) 'program-error)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("((T NIL) (A B C . D) (B . C) T T T T)"),
        "CONS-chapter list-function conformance, got: '{}'",
        stdout
    );
}

#[test]
fn plist_places_and_nconc_conform() {
    // bliss-30be plist/place + set-op batch (parallel agent D): PUSH/PUSHNEW/REMF
    // evaluate the place subforms once via GET-SETF-EXPANSION and read the place
    // after the other subforms (order); PUSH on a (GETF p k) place writes the new
    // plist head back to p; PUSHNEW returns the list unchanged (EQ) when present;
    // NCONC splices destructively (reuses the last cons) and treats a non-list
    // final arg as the tail; a wrong-arity #'cons via a :key funcall PROGRAM-ERRORs.
    let expr = "(princ (list
                  (let ((x (list 1 2))) (push 0 x) x)
                  (let ((x (list 1 2))) (pushnew 1 x) (eq x x))
                  (let ((x (list 1 2))) (pushnew 3 x) x)
                  (let ((p (list :a 1 :b 2))) (remf p :a) p)
                  (let ((p (list :a 1))) (push 9 (getf p :a)) p)
                  (let ((a (list 1 2)) (b (list 3 4))) (eq (cddr (nconc a b)) b))
                  (nconc (list 'a 'b) 'z)
                  (typep (nth-value 1 (ignore-errors (adjoin 1 '(2) :key #'cons))) 'program-error)))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("((0 1 2) T (3 1 2) (:B 2) (:A (9 . 1)) T (A B . Z) T)"),
        "plist/place + nconc conformance, got: '{}'",
        stdout
    );
}

#[test]
fn get_setf_expansion_store_form_handles_standard_places() {
    // GET-SETF-EXPANSION's fourth value must be an executable storing form,
    // including for standard places whose writers are implemented directly by
    // SETF rather than exposed as functions named (SETF accessor) (bliss-42iv).
    let expr = "(progn
                  (defmacro store-through-expansion (place value &environment env)
                    (multiple-value-bind (temps vals stores store access)
                        (get-setf-expansion place env)
                      (declare (ignore access))
                      `(let* (,@(mapcar #'list temps vals)
                               (,(car stores) ,value))
                         ,store)))
                  (defun custom-place (cell) (car cell))
                  (defun (setf custom-place) (new cell)
                    (setf (car cell) new))
                  (let* ((bits (make-array 4 :element-type 'bit :initial-element 0))
                         (text (copy-seq \"abc\"))
                         (items (list 1 2 3 4))
                         (nested (list (list 'a 'b) (list 'c 'd)))
                         (custom-cell (list 0))
                         (sym (gensym))
                         (fp (make-array 4 :fill-pointer 2 :initial-element 0))
                         (matrix (make-array '(2 2) :initial-element 0))
                         (unknown (gensym))
                         (unknown-store
                           (nth-value 3 (get-setf-expansion (list unknown))))
                         (foreign-package (make-package (symbol-name (gensym))))
                         (foreign-bit (intern \"BIT\" foreign-package))
                         (foreign-store
                           (nth-value 3 (get-setf-expansion (list foreign-bit))))
                         (uninterned-bit (make-symbol \"BIT\"))
                         (uninterned-store
                           (nth-value 3 (get-setf-expansion (list uninterned-bit))))
                         (nonsymbol-accessor (list 'lambda (list 'x) 'x))
                         (nonsymbol-store
                           (nth-value 3
                                      (get-setf-expansion
                                        (list nonsymbol-accessor)))))
                    (store-through-expansion (bit bits 1) 1)
                    (store-through-expansion (char text 2) #\\z)
                    (store-through-expansion (subseq text 0 2) \"XY\")
                    (store-through-expansion (nth 2 items) 9)
                    (store-through-expansion (fourth items) 5)
                    (store-through-expansion (cadar nested) 6)
                    (store-through-expansion (custom-place custom-cell) 11)
                    (store-through-expansion (get sym 'k) 7)
                    (store-through-expansion (fill-pointer fp) 3)
                    (store-through-expansion (row-major-aref matrix 3) 8)
                    (store-through-expansion (symbol-plist sym) '(:p 4 k 7))
                    (princ (list (bit bits 1) text items (cadar nested)
                                 (custom-place custom-cell)
                                 (get sym 'k) (fill-pointer fp)
                                 (row-major-aref matrix 3) (symbol-plist sym)
                                 (labels ((contains-writer (tree name)
                                            (or (equal tree
                                                       (list 'function
                                                             (list 'setf name)))
                                                (and (consp tree)
                                                     (or (contains-writer (car tree) name)
                                                         (contains-writer (cdr tree) name))))))
                                   (list (contains-writer unknown-store unknown)
                                         (contains-writer foreign-store foreign-bit)
                                         (contains-writer uninterned-store uninterned-bit)
                                         (contains-writer nonsymbol-store
                                                          nonsymbol-accessor)))))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "GET-SETF-EXPANSION store form failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(1 \"XYz\" (1 2 9 5) 6 11 7 3 8 (:P 4 K 7) (T T T T))"),
        "GET-SETF-EXPANSION store forms produced wrong values: '{stdout}'"
    );
}

#[test]
fn nth_wrong_arg_count_is_catchable() {
    // (nth) with too few args signals a catchable PROGRAM-ERROR, not an
    // uncatchable internal abort (ansi-test nth.error.*; bliss-x7aa).
    let output = egcl_bin()
        .args(["--eval", "(princ (ignore-errors (nth 0)))"])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "must be caught, not abort");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL"),
        "IGNORE-ERRORS should catch the NTH arg-count error, got: '{}'",
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("3") && stdout.contains("4"),
        "defclass + make-instance should produce object with correct slots, got: '{}'",
        stdout
    );
}

/// Regression for bliss-aid: a defclass `:reader`/`:accessor`/`:writer` must be
/// a real generic method, callable as a *function value* (`#'reader`, funcall,
/// apply, mapcar), not only in operator position. Previously readers were only
/// resolved by an accessor fast-path in operator position, so `(funcall #'reader
/// x)` signalled "no applicable method" — which broke UIOP's ENSURE-FUNCTION
/// calling ASDF slot readers during `asdf:load-system`.
#[test]
fn defclass_accessors_work_as_function_values() {
    // Includes the ASDF-shaped case: an explicit defgeneric + a `null` method
    // coexisting with a defclass :reader of the same (package-qualified) name.
    let expr = r#"(progn
  (defgeneric g (x))
  (defmethod g ((x null)) 'null-m)
  (defclass c () ((s :initarg :s :initform 9 :reader g :writer set-g :accessor acc)))
  (let ((o (make-instance 'c :s 1)))
    (setf (acc o) 8)
    (funcall #'set-g 7 o)
    (format nil "~A ~A ~A ~A ~A"
      (funcall #'g o)          ; reader as function value -> 7
      (apply #'acc (list o))   ; accessor via apply     -> 7
      (mapcar #'g (list o))    ; reader via mapcar       -> (7)
      (g o)                    ; operator position       -> 7
      (funcall #'g nil))))"#; // explicit null method still applies -> NULL-M
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("7 7 (7) 7 NULL-M"),
        "defclass reader/accessor must dispatch as a function value and coexist \
         with an explicit null method; got: '{}', stderr: '{}'",
        stdout,
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Regression for bliss-x4p: `:allocation :class` slots are shared across
/// subclasses (stored in the owning class), a subclass may redeclare a class
/// slot with an `:initform`, and `:default-initargs` bind slots the caller
/// didn't supply — the shape ASDF's operation classes rely on (e.g.
/// SELFWARD-OPERATION's class slot set by LOAD-OP's `:initform '(prepare-op …)`).
#[test]
fn class_allocated_slots_and_default_initargs() {
    let expr = r#"(progn
  ;; class slot shared across subclasses
  (defclass owner () ((cs :allocation :class :reader cs-of :initarg :cs)))
  (defclass sub (owner) ())
  (make-instance 'owner :cs 'shared)
  ;; subclass redeclaring a class slot with an initform
  (defclass op () ())
  (defclass swo (op) ((swo :reader swo :allocation :class)))
  (defclass load-op (swo) ((swo :initform '(prep comp) :allocation :class)))
  ;; :default-initargs (instance slot) + explicit override
  (defclass base () ((s :initarg :s)))
  (defclass di (base) () (:default-initargs :s 42))
  (format nil "~A ~A ~A ~A ~A"
    (cs-of (make-instance 'owner))          ; SHARED
    (cs-of (make-instance 'sub))            ; SHARED (inherited class slot)
    (swo (make-instance 'load-op))          ; (PREP COMP)
    (slot-value (make-instance 'di) 's)     ; 42
    (slot-value (make-instance 'di :s 9) 's)))"#; // 9 (explicit overrides default)
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SHARED SHARED (PREP COMP) 42 9"),
        "class-allocated slots + default-initargs; got: '{}', stderr: '{}'",
        stdout,
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Regression for bliss-htff follow-up: an instance-allocated slot in a subclass
/// SHADOWS a superclass's `:allocation :class` slot, so each instance gets its
/// own storage rather than the superclass's shared cell.
///
/// `class_slot_owner` used to collapse "slot not found in the super graph" and
/// "slot found, instance-allocated" into the same `None`, which then triggered a
/// whole-image fallback scan that happily matched the SUPERCLASS's
/// class-allocated declaration. Writing through one instance was therefore
/// visible through every other one. The same conflation made every ordinary slot
/// access scan every class and slot in the image (measured 8.9x slower slot
/// reads after defining 300 unrelated classes), so the fix is both a
/// correctness and a performance one. SBCL 2.6.8 is the reference: it answers
/// INST here.
#[test]
fn instance_slot_shadows_a_superclass_class_slot() {
    let expr = r#"(progn
  (defclass shadow-base () ((v :initform 'cls :accessor v :allocation :class)))
  ;; Redeclared WITHOUT :allocation, i.e. instance-allocated: it shadows.
  (defclass shadow-sub (shadow-base) ((v :initform 'inst :accessor v)))
  (let ((s1 (make-instance 'shadow-sub))
        (s2 (make-instance 'shadow-sub)))
    (setf (v s1) 'mine)
    ;; s2 must be untouched by the write through s1.
    (format nil "~A|~A" (v s2) (v s1))))"#;
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("INST|MINE"),
        "an instance slot must shadow a superclass class slot, so a write \
         through one instance is not visible through another; got: '{}', \
         stderr: '{}'",
        stdout,
        String::from_utf8_lossy(&output.stderr),
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("macro-expanded"),
        "defmacro should define a working macro, got: '{}'",
        stdout
    );
}

/// `&whole` binds the ENTIRE macro call form, including the macro name (CLHS
/// 3.4.4) — `(w 7)` binds `(W 7)`, not `(7)`. A `&whole` on a nested
/// destructuring sublist still binds only that sublist (no operator).
#[test]
fn eval_macro_whole_includes_operator() {
    let expr = r#"(progn
  (defmacro w (&whole form x) (declare (ignore x)) (list 'quote form))
  (defmacro nested ((&whole sub a b)) (declare (ignore a b)) (list 'quote sub))
  (format nil "~S ~S" (w 7) (nested (8 9))))"#;
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(W 7) (8 9)"),
        "&whole should bind the whole call form (with operator) at top level and \
         the sublist when nested, got: '{}'",
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(42 NIL)|((42) NIL)|(42 NIL)|(3 1)|(3 1)|(3 1)|(1 2)"),
        "multiple values leaked or a producer was truncated, got: '{}'",
        stdout
    );
}

#[test]
fn handler_case_handler_clause_returns_all_values() {
    // bliss-xy7t: a HANDLER-CASE handler clause returns the values of its last
    // form, exactly like PROGN — so IGNORE-ERRORS' `(values nil c)` keeps the
    // condition as its secondary value. The old code cleared multiple values on
    // the handler path (tree-walker ran the clause in a child env; the bytecode
    // lowerer mirrored that with an explicit ClearMv), dropping every secondary.
    let expr = r#"(format nil "~A|~A|~A|~A"
  (multiple-value-list (handler-case (error "x") (error (c) (values 1 2 3))))       ; -> (1 2 3)
  (multiple-value-list (handler-case (error "x") (error (c) 42)))                    ; -> (42)
  (multiple-value-list (handler-case (values 7 8 9) (error (c) 0)))                  ; -> (7 8 9)
  (typep (nth-value 1 (ignore-errors (error "boom"))) 'condition))"#; // -> T
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(1 2 3)|(42)|(7 8 9)|T"),
        "handler clause dropped secondary values or ignore-errors lost the \
         condition, got: '{}'",
        stdout
    );
}

#[test]
fn handler_case_clause_variable_survives_gc_in_body() {
    // bliss-5jwg: the HANDLER-CASE clause variable (the bound condition) and any
    // clause-body local must survive a GC triggered by an allocation in the
    // handler body. The tree-walker ran the clause in a child env that was not
    // registered as a GC root, so a collection during the body left the condition
    // (and body locals) dangling — under moving GC the value relocated but the
    // frame slot did not, so `(typep c 'condition)` later saw a wrong object.
    // EGCL_GC_STRESS collapses the latent, allocation-timing-dependent bug into
    // a deterministic one (it manifests in the unoptimized/debug build; release
    // codegen happens to keep the value otherwise reachable). The condition `c`
    // is checked after an intervening allocation in the handler body.
    let expr = r#"(progn
  (dotimes (i 500)
    (let ((ok (handler-case (error "x")
                (error (c) (list i i i) (typep c 'condition)))))
      (assert ok)))
  (princ :ALL-OK))"#;
    let output = egcl_bin()
        .args(["--eval", expr])
        .env("EGCL_GC_STRESS", "100")
        .output()
        .expect("failed to run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(0),
        "handler-case clause state corrupted under GC stress; stdout='{stdout}' stderr='{stderr}'"
    );
    assert!(
        stdout.contains("ALL-OK"),
        "expected ALL-OK, got stdout='{stdout}' stderr='{stderr}'"
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args(["--eval", "(error \"fatal error test\")"])
        .output()
        .expect("failed to run egcl");
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
fn eval_warn_renders_asdf_format_control() {
    let expr = r#"(warn "~@<Invalid :version specifier ~S~@[ for component ~S~]~@[ in ~S~]~@[ from file ~A~]~@[, using NIL instead~]~3i~_~@:>"
                        'version "completions" nil "/tmp/completions.asd" t)"#;
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(
            "WARNING: Invalid :version specifier VERSION for component \"completions\" from file /tmp/completions.asd, using NIL instead"
        ),
        "WARN should render its format control through FORMAT, got: '{stderr}'"
    );
    assert!(
        !stderr.contains("~@<") && !stderr.contains("~@[") && !stderr.contains("~3i"),
        "WARN leaked FORMAT directives: '{stderr}'"
    );
}

#[test]
fn eval_cerror_with_handler_continues() {
    let expr = r#"(handler-bind ((error (lambda (c) (invoke-restart 'continue))))
  (cerror "Continue anyway" "soft error")
  (format nil "continued"))"#;
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let output = egcl_bin()
        .args([
            "--sandbox",
            "--eval",
            "(with-open-file (s \"/etc/passwd\" :direction :input) (read-line s))",
        ])
        .output()
        .expect("failed to run egcl");
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

#[test]
fn sandbox_cpu_timeout_is_catchable_timeout_condition() {
    let expr =
        "(handler-case (loop) (egcl-ext:timeout-condition (e) (declare (ignore e)) :timeout))";
    let mut child = egcl_bin()
        .env("EGCL_SANDBOX_CPU_MS", "25")
        .args(["--sandbox", "--eval", expr])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn egcl sandbox timeout check");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().expect("poll egcl timeout child") {
            let output = child
                .wait_with_output()
                .expect("collect egcl timeout child");
            assert_eq!(
                status.code(),
                Some(0),
                "stderr: {} stdout: {}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .to_uppercase()
                    .contains("TIMEOUT"),
                "timeout condition should be caught and printed, stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            break;
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let output = child.wait_with_output().expect("collect killed child");
            panic!(
                "sandbox runaway loop was not interrupted within 2s; stdout={} stderr={}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

// ══════════════════════════════════════════════════════════════════
// --no-init: skip init file loading
// ══════════════════════════════════════════════════════════════════

#[test]
fn no_init_flag_skips_init_file() {
    // Create a temporary init file that would print a side effect if loaded
    let dir = std::env::temp_dir().join("egcl_test_no_init");
    let _ = std::fs::create_dir_all(&dir);
    let init_file = dir.join(".egclrc");
    std::fs::write(&init_file, "(print \"INIT-FILE-LOADED\")\n")
        .expect("failed to write init file");

    // Run with --no-init --eval — the init file side effect should NOT appear
    let output = egcl_bin()
        .args(["--no-init", "--eval", "(print \"main\")"])
        .env("EGCL_INIT_FILE", init_file.to_str().unwrap())
        .env("HOME", dir.to_str().unwrap())
        .output()
        .expect("failed to run egcl");

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
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

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
        Ok(_) => handle.join().unwrap().expect("failed to wait on egcl"),
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
    let mut child = egcl_bin()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to start egcl");

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
        Ok(_) => handle.join().unwrap().expect("failed to wait on egcl"),
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
    let dir = std::env::temp_dir().join("egcl_test_image");
    let _ = std::fs::create_dir_all(&dir);
    let image_path = dir.join("test.image");

    // Step 1: Define a function and save an image
    let save_expr = format!(
        "(progn (defun my-fn () 99) (save-image \"{}\"))",
        image_path.to_str().unwrap().replace('\\', "\\\\")
    );
    let output = egcl_bin()
        .args(["--eval", &save_expr])
        .output()
        .expect("failed to run egcl for save");
    assert_eq!(
        output.status.code(),
        Some(0),
        "save-image should succeed, stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );

    // Step 2: Load the image and call the function
    let output = egcl_bin()
        .args(["--image", image_path.to_str().unwrap(), "--eval", "(my-fn)"])
        .output()
        .expect("failed to run egcl for load");

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

/// bliss-cje1: a SAVE-LISP-AND-DIE image must preserve global macros and
/// `(defun (setf place) …)` writers, not just plain functions. Before the fix
/// the image dropped both — an ASDF-preloaded install could hold ASDF but failed
/// to load libraries whose code used UIOP macros (NEST) or ASDF's setf-functions.
#[test]
fn image_preserves_macros_and_setf_functions() {
    let dir = std::env::temp_dir().join("egcl_test_image_macros");
    let _ = std::fs::create_dir_all(&dir);
    let image_path = dir.join("mac.image");
    let p = image_path.to_str().unwrap().replace('\\', "\\\\");

    // Save an image defining a macro, a setf-function, and a serializable global.
    let save_expr = format!(
        "(progn \
           (defmacro twice (x) (list '* x 2)) \
           (defvar *cje1-place* (list 1 2 3)) \
           (defun (setf cje1-second) (v l) (setf (cadr l) v) v) \
           (save-lisp-and-die \"{p}\"))"
    );
    let output = egcl_bin()
        .args(["--eval", &save_expr])
        .output()
        .expect("run egcl for save-lisp-and-die");
    assert_eq!(
        output.status.code(),
        Some(0),
        "save-lisp-and-die should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Reload and use the macro and the setf-function.
    let output = egcl_bin()
        .args([
            "--image",
            image_path.to_str().unwrap(),
            "--eval",
            "(progn (format t \"twice=~a \" (twice 21)) \
                    (setf (cje1-second *cje1-place*) 99) \
                    (format t \"place=~a\" *cje1-place*))",
        ])
        .output()
        .expect("run egcl with saved image");
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(
        output.status.code(),
        Some(0),
        "reload should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("twice=42"),
        "macro must survive the image (twice=42), got: {stdout}"
    );
    assert!(
        stdout.contains("place=(1 99 3)"),
        "setf-function must survive the image (place=(1 99 3)), got: {stdout}"
    );
}

/// bliss-66io: after a core image load, defining new CLOS classes/methods must
/// NOT corrupt the dispatch of methods restored from the image. Class and method
/// ids are minted from a process-local counter that resets on load; if it is not
/// advanced past every restored metaobject id, a class/method defined after the
/// load re-mints an id that aliases a restored one and silently overwrites its
/// CLOS registration — e.g. a restored method's qualifier flips to `:primary`,
/// so the effective method combination is wrong. Symptom in the wild: an
/// ASDF-preloaded image failed `(asdf:load-system :cl-ppcre)` with "Required
/// method PERFORM not implemented for cl-source-file" once enough of the
/// library's own methods had been defined to collide with ASDF's restored
/// perform methods.
#[test]
fn image_load_then_defclass_preserves_restored_method_dispatch() {
    let dir = std::env::temp_dir().join("egcl_test_image_66io");
    let _ = std::fs::create_dir_all(&dir);
    let image_path = dir.join("clos.image");
    let p = image_path.to_str().unwrap().replace('\\', "\\\\");

    // Save an image whose surviving CLOS metaobject ids span a wide range, with
    // the RENDER methods' ids near the TOP of that range. Padding must be
    // *methods on a generic* (kept alive in the generic's method list) rather than
    // bare classes (dropped by the save-time GC), so the restored id range really
    // extends past where the rest of startup re-advances the id counter on load.
    // RENDER carries :before/:after/:primary methods whose qualifiers are what a
    // post-load id collision corrupts — and unlike a lone :around, their effect is
    // observable: a :before wrongly demoted to :primary becomes the sole primary
    // that runs, so neither the real primary nor the :after fire.
    let save_expr = format!(
        "(progn \
           (defgeneric pad (x)) \
           (dotimes (i 1500) \
             (eval (list 'defmethod 'pad (list (list 'x (list 'eql i))) nil))) \
           (defvar *log* nil) \
           (defclass widget () ()) \
           (defgeneric render (x)) \
           (defmethod render :before ((x widget)) (push :before *log*)) \
           (defmethod render :after ((x widget)) (push :after *log*)) \
           (defmethod render ((x widget)) (push :primary *log*) :done) \
           (save-lisp-and-die \"{p}\"))"
    );
    let output = egcl_bin()
        .args(["--eval", &save_expr])
        .output()
        .expect("run egcl for save-lisp-and-die");
    assert_eq!(
        output.status.code(),
        Some(0),
        "save-lisp-and-die should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Reload, then mint many NEW methods (dense id consumption) covering the
    // restored id range, then dispatch the restored generic. All three restored
    // methods must still run in standard order → result :DONE, log
    // (:before :primary :after). Without the counter fix, a post-load method
    // re-mints a restored method's id and set-method-specializers overwrites its
    // qualifier to :primary, so RENDER's :before runs as the lone primary and the
    // real primary + :after never fire (result (:before), log (:before)).
    let load_expr = "(progn \
        (dotimes (i 3000) \
          (eval (list 'defmethod 'render (list (list 'x (list 'eql i))) nil))) \
        (setq *log* nil) \
        (let ((r (render (make-instance 'widget)))) \
          (format t \"RESULT=~a LOG=~a~%\" r (reverse *log*))))";
    let output = egcl_bin()
        .args(["--image", image_path.to_str().unwrap(), "--eval", load_expr])
        .output()
        .expect("run egcl with saved image");
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(
        output.status.code(),
        Some(0),
        "reload+dispatch should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RESULT=DONE") && stdout.contains("LOG=(BEFORE PRIMARY AFTER)"),
        "restored :before/:primary/:after methods must all dispatch in order after \
         post-load method churn (expected RESULT=DONE LOG=(BEFORE PRIMARY AFTER)), got: {stdout}"
    );
}

/// bliss-0pn8: gensyms created after a core image load must not re-mint the
/// index of a gensym restored from the image. Uninterned-symbol indices come
/// from a process-local counter that resets to its base on load; if it is not
/// advanced past the restored gensyms, a post-load GENSYM aliases a restored one
/// (same index ⇒ EQ), silently sharing its cells. Symptom in the wild: compiling
/// alexandria's numbers.lisp after a core load produced a gensym that collided
/// with a restored gensym and reached UIOP's package-designator typecheck,
/// erroring with "#:G… is not of type PACKAGE-DESIGNATOR". Mirrors the
/// class/method-id counter fix (bliss-66io).
#[test]
fn image_load_gensyms_do_not_alias_restored_gensyms() {
    let dir = std::env::temp_dir().join("egcl_test_image_0pn8");
    let _ = std::fs::create_dir_all(&dir);
    let image_path = dir.join("gs.image");
    let p = image_path.to_str().unwrap().replace('\\', "\\\\");

    // Save an image retaining 50 gensyms (reachable ⇒ they survive the save GC).
    let save_expr = format!(
        "(progn \
           (defparameter *saved* (let (acc) (dotimes (i 50 acc) (push (gensym) acc)))) \
           (save-lisp-and-die \"{p}\"))"
    );
    let output = egcl_bin()
        .args(["--eval", &save_expr])
        .output()
        .expect("run egcl for save-lisp-and-die");
    assert_eq!(
        output.status.code(),
        Some(0),
        "save-lisp-and-die should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // Reload, mint many fresh gensyms, and confirm none is EQ to a restored one.
    // Without the counter fix the fresh gensyms restart at the base index and
    // collide with the restored set.
    let load_expr = "(let ((new (let (acc) (dotimes (i 400 acc) (push (gensym) acc)))) (bad 0)) \
        (dolist (g new) (when (member g *saved*) (incf bad))) \
        (format t \"ALIASED=~a~%\" bad))";
    let output = egcl_bin()
        .args(["--image", image_path.to_str().unwrap(), "--eval", load_expr])
        .output()
        .expect("run egcl with saved image");
    let _ = std::fs::remove_file(&image_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(
        output.status.code(),
        Some(0),
        "reload should exit 0, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("ALIASED=0"),
        "post-load gensyms must not alias restored gensyms (expected ALIASED=0), got: {stdout}"
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
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
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
    let out1 = egcl_bin()
        .args([
            "--eval",
            "(let ((x 0)) (restart-case (invoke-restart 'bump) (bump () (setq x 99))) x)",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(out1.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&out1.stdout).contains("99"),
        "setq inside a restart must persist; got: {}",
        String::from_utf8_lossy(&out1.stdout)
    );

    // CHECK-TYPE's STORE-VALUE restart corrects the place end-to-end.
    let out2 = egcl_bin()
        .args([
            "--eval",
            "(let ((x 'foo)) (handler-bind ((type-error (lambda (c) (declare (ignore c)) (store-value 42)))) (check-type x integer)) x)",
        ])
        .output()
        .expect("run egcl");
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
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression for LOOP while/until/repeat driver clauses (bliss-2pt.10).
/// An arithmetic LOOP variable IS the iteration counter: a body that assigns to
/// it moves the iteration with it (bliss-rnd7). babel's UTF-8 decoder is
/// `for i fixnum from start below end` with an `(incf i)` for every continuation
/// byte it consumes, so with a private counter it re-read each continuation byte
/// as a starter byte and `babel:octets-to-string` signalled
/// INVALID-UTF8-STARTER-BYTE on octets `babel:string-to-octets` had just made.
///
/// Two separate drivers had the bug and both are covered here, because
/// `run_expression_cases` runs every case compiled AND tree-walked: the
/// lowerer's general LOOP compiler (cli/bytecode.rs `lower_loop_general`) and
/// the tree-walker's `ForState::From`. The `of-type` rows matter — `of-type`
/// is what decides which of the two handles a clause.
///
/// Only the ARITHMETIC driver is mutation-sensitive. `across`/`in`/`on`/`repeat`
/// step private state, so assigning to their variable must NOT move them; those
/// rows pin that down. The collected values here all match SBCL.
#[test]
fn an_arithmetic_loop_variable_is_the_iteration_counter() {
    let cases = [
        // Skipping an index from the body: the reduced form of the babel bug.
        (
            "(loop for i from 0 below 6 collect i do (incf i))",
            "(0 2 4)",
        ),
        ("(loop for i from 0 to 5 collect i do (incf i))", "(0 2 4)"),
        (
            "(loop for i fixnum from 0 below 6 collect i do (incf i))",
            "(0 2 4)",
        ),
        // `by` composes with the mutation: 0, then (1+2)=3, then 4+2=6 exits.
        (
            "(loop for i from 0 below 6 by 2 collect i do (incf i))",
            "(0 3)",
        ),
        // Descending steps from the mutated value too.
        (
            "(loop for i downfrom 6 above 0 collect i do (decf i))",
            "(6 4 2)",
        ),
        // A second driver keeps its own independent stepping.
        (
            "(loop for d from 10 for i from 0 below 4 collect (list d i) do (incf i))",
            "((10 0) (11 2))",
        ),
        // The babel decoder's shape: a starter byte consumes its continuation
        // byte and the loop must not revisit it.
        (
            "(let ((v #(104 195 169 108)))               (loop for i fixnum from 0 below 4                     collect (let ((u1 (aref v i)))                               (if (> u1 127) (list u1 (aref v (incf i))) u1))))",
            "(104 (195 169) 108)",
        ),
        // The non-arithmetic drivers must NOT follow their variable.
        (
            "(loop for x across #(1 2 3) collect x do (setq x 99))",
            "(1 2 3)",
        ),
        (
            "(loop for x in '(1 2 3) collect x do (setq x 99))",
            "(1 2 3)",
        ),
        (
            "(loop for x on '(1 2) collect (car x) do (setq x nil))",
            "(1 2)",
        ),
        // A LOOP that does not touch its variable is unchanged, and FINALLY sees
        // the value that failed the termination test (CLHS 6.1.2.1.1;
        // bliss-bpjw6). The COLLECTED values are the same either way.
        ("(loop for i from 1 to 5 collect i)", "(1 2 3 4 5)"),
        (
            "(loop for x from 1 to 5 do (progn) finally (return x))",
            "6",
        ),
        (
            "(loop for x from 1 below 5 do (progn) finally (return x))",
            "5",
        ),
        (
            "(loop for x from 10 downto 0 do (progn) finally (return x))",
            "-1",
        ),
        (
            "(loop for x from 10 above 0 do (progn) finally (return x))",
            "0",
        ),
        // A zero-iteration loop still leaves the start value.
        (
            "(loop for x from 5 to 3 do (progn) finally (return x))",
            "5",
        ),
        // `for nil` is a legal driver that discards its value (LOOP.1.50-53).
        // NIL and T answer is_symbol() but are SPECIAL immediates rather than
        // TAG_SYMBOL values, so reading or binding them through the symbol index
        // PANICS — which is what stepping-from-the-variable first did here, and
        // it took the whole ansi-test `iteration` chapter down with it.
        (
            "(let ((i 0)) (loop for nil from 10 to 15 collect (incf i)))",
            "(1 2 3 4 5 6)",
        ),
        (
            "(let ((i 0)) (loop for nil from 10 below 15 collect (incf i)))",
            "(1 2 3 4 5)",
        ),
        ("(loop for nil from 10 to 0 collect 'a)", "NIL"),
        (
            "(let ((i 0)) (loop for nil from 0 to 10 by 2 collect (incf i)))",
            "(1 2 3 4 5 6)",
        ),
        // `for t` is a program error SBCL rejects at compile time; EGCL does not
        // signal it yet (bliss-pj0n), but it must not crash the process.
        ("(loop for t from 1 to 3 collect 1)", "(1 1 1)"),
    ];
    run_expression_cases(&cases);
}

/// CLHS 6.1.2.1.1 lets an arithmetic FOR's start/limit/step subclauses be
/// written in ANY order (bliss-cnf2). EGCL accepted `from` only in the lead
/// position and otherwise signalled a hard "LOOP: unsupported clause `FROM`",
/// so `(loop for x to 10 from 1 collect x)` did not run at all.
///
/// The evaluation-ORDER rows are the point of the test, not a bonus: each
/// subform must be evaluated exactly once, in the order written, which a
/// canonicalised from/to/by triple cannot express. All four permutations are
/// pinned (ansi-test LOOP.1.29-32) by recording which `(incf i)` ran first.
#[test]
fn an_arithmetic_loops_subclauses_may_be_written_in_any_order() {
    let cases = [
        // `from` after the limit, and after both the limit and the step.
        (
            "(loop for x to 10 from 1 collect x)",
            "(1 2 3 4 5 6 7 8 9 10)",
        ),
        ("(loop for x to 10 by 2 from 1 collect x)", "(1 3 5 7 9)"),
        ("(loop for x by 2 to 10 from 1 collect x)", "(1 3 5 7 9)"),
        ("(loop for x below 5 from 2 collect x)", "(2 3 4)"),
        // A descending limit keyword with the start written last.
        (
            "(loop for x above 6 from 14 by 2 collect x)",
            "(14 12 10 8)",
        ),
        ("(loop for x above 0 from 3 collect x)", "(3 2 1)"),
        // The limit form is evaluated BEFORE the start form here, so `n` is
        // still 0 when `(+ n 5)` runs and 1 by the time the start is taken.
        (
            "(let ((n 0)) (loop for x to (+ n 5) from (incf n) collect x))",
            "(1 2 3 4 5)",
        ),
        // All four permutations of from/to/by: the trailing list is
        // (result a b c i) where a/b/c record which position each subform ran in.
        (
            "(let (a b c (i 0)) (list (loop for x from (progn (setq a (incf i)) 0)                below (progn (setq b (incf i)) 9) by (progn (setq c (incf i)) 2) collect x)                a b c i))",
            "((0 2 4 6 8) 1 2 3 3)",
        ),
        (
            "(let (a b c (i 0)) (list (loop for x from (progn (setq a (incf i)) 0)                by (progn (setq c (incf i)) 2) below (progn (setq b (incf i)) 9) collect x)                a b c i))",
            "((0 2 4 6 8) 1 3 2 3)",
        ),
        (
            "(let (a b c (i 0)) (list (loop for x below (progn (setq b (incf i)) 9)                by (progn (setq c (incf i)) 2) from (progn (setq a (incf i)) 0) collect x)                a b c i))",
            "((0 2 4 6 8) 3 1 2 3)",
        ),
        (
            "(let (a b c (i 0)) (list (loop for x by (progn (setq c (incf i)) 2)                below (progn (setq b (incf i)) 9) from (progn (setq a (incf i)) 0) collect x)                a b c i))",
            "((0 2 4 6 8) 3 2 1 3)",
        ),
        // The canonical orders and the omitted-start default keep working.
        ("(loop for i from 1 to 5 collect i)", "(1 2 3 4 5)"),
        ("(loop for i below 4 collect i)", "(0 1 2 3)"),
        ("(loop for i from 10 downto 7 collect i)", "(10 9 8 7)"),
        (
            "(loop for x downfrom 16 above 7 by 3 collect x)",
            "(16 13 10)",
        ),
        // A bignum start, limit and step still step exactly.
        (
            "(loop for x to (expt 2 63) from (expt 2 62) by (expt 2 61) collect x)",
            "(4611686018427387904 6917529027641081856 9223372036854775808)",
        ),
    ];
    run_expression_cases(&cases);
}

/// ALLOCATE-INSTANCE and SLOT-MAKUNBOUND (CLHS 7.7.1), wired to the stdlib
/// implementations that already had the right semantics (bliss-2icv).
///
/// The defining property of ALLOCATE-INSTANCE is what it does NOT do: every slot
/// comes back unbound, with no initforms, no initargs and no
/// INITIALIZE-INSTANCE. The `:initform 1` row is the one that matters — a naive
/// wiring to MAKE-INSTANCE would pass everything else and fail that.
///
/// SLOT-MAKUNBOUND was already implemented in operator position; what was broken
/// is that neither name was in the builtin list, so FBOUNDP answered NIL and
/// FUNCALL/APPLY could not reach them. Those rows are the regression.
#[test]
fn allocate_instance_and_slot_makunbound_are_available() {
    let cases = [
        // Every slot unbound, initform NOT run.
        (
            "(progn (defclass ai () ((a :initform 1) (b)))                (let ((i (allocate-instance (find-class 'ai))))                  (list (slot-boundp i 'a) (slot-boundp i 'b))))",
            "(NIL NIL)",
        ),
        // MAKE-INSTANCE is unaffected: it still runs initforms.
        (
            "(progn (defclass ai2 () ((a :initform 7)))                (slot-value (make-instance 'ai2) 'a))",
            "7",
        ),
        // SLOT-MAKUNBOUND unbinds and returns the INSTANCE, not the value.
        (
            "(progn (defclass ai3 () ((a :initform 1)))                (let ((i (make-instance 'ai3)))                  (list (eq i (slot-makunbound i 'a)) (slot-boundp i 'a))))",
            "(T NIL)",
        ),
        // Both must be FBOUND and reachable as function designators — this is
        // what was actually missing.
        (
            "(list (not (null (fboundp 'allocate-instance)))                    (not (null (fboundp 'slot-makunbound))))",
            "(T T)",
        ),
        (
            "(progn (defclass ai4 () ((a :initform 1)))                (slot-boundp (funcall #'allocate-instance (find-class 'ai4)) 'a))",
            "NIL",
        ),
        (
            "(progn (defclass ai5 () ((a :initform 1)))                (slot-boundp (apply #'allocate-instance (list (find-class 'ai5))) 'a))",
            "NIL",
        ),
    ];
    run_expression_cases(&cases);
}

/// CLOS slot-definition metaobjects: CLASS-SLOTS, CLASS-DIRECT-SLOTS and the
/// SLOT-DEFINITION-* accessors (bliss-h1mx), the surface closer-mop needs.
///
/// The effective-vs-direct distinction is the point, not decoration: AMOP defines
/// SLOT-DEFINITION-READERS/-WRITERS only on DIRECT slot definitions (SBCL signals
/// no-applicable-method on an effective one), so aliasing the two would be wrong
/// even though both would "work" for simple classes.
///
/// `:accessor` is checked specifically because DEFCLASS records it in `readers`
/// and in a separate `accessor` field but NEVER in `writers` — so the writer
/// ((setf name)) is synthesized, and a naive exposure reports no writer at all.
#[test]
fn class_slots_and_slot_definition_accessors_are_available() {
    let cases = [
        // Effective slots include inherited ones, in class-precedence order.
        (
            "(progn (defclass mb () ((a :initform 1)(sh :allocation :class)))                (defclass md (mb) ((b)))                (mapcar #'slot-definition-name (class-slots (find-class 'md))))",
            "(B A SH)",
        ),
        // Direct slots exclude inherited ones.
        (
            "(progn (defclass mb2 () ((a)(sh))) (defclass md2 (mb2) ((b)))                (mapcar #'slot-definition-name (class-direct-slots (find-class 'md2))))",
            "(B)",
        ),
        // Every initarg a slot declares, not just the first.
        (
            "(progn (defclass mi () ((a :initarg :a :initarg :aye)))                (slot-definition-initargs (car (class-direct-slots (find-class 'mi)))))",
            "(:A :AYE)",
        ),
        // :accessor implies a reader AND the (setf name) writer.
        (
            "(progn (defclass mac () ((a :accessor mac-a)))                (let ((s (car (class-direct-slots (find-class 'mac)))))                  (list (slot-definition-readers s) (slot-definition-writers s))))",
            "((MAC-A) ((SETF MAC-A)))",
        ),
        // :reader gives no writer; :writer gives no reader.
        (
            "(progn (defclass mrw () ((r :reader mrw-r) (w :writer set-mrw-w)))                (mapcar (lambda (s) (list (slot-definition-readers s)                                          (slot-definition-writers s)))                        (class-direct-slots (find-class 'mrw))))",
            "(((MRW-R) NIL) (NIL (SET-MRW-W)))",
        ),
        // Allocation and initform are reported.
        (
            "(progn (defclass mal () ((i :initform 42) (c :allocation :class)))                (mapcar (lambda (s) (list (slot-definition-allocation s)                                          (slot-definition-initform s)))                        (class-direct-slots (find-class 'mal))))",
            "((:INSTANCE 42) (:CLASS NIL))",
        ),
        // A class designator is accepted, as for MAKE-INSTANCE/ALLOCATE-INSTANCE,
        // because FIND-CLASS returns the name here (bliss-rj5o).
        (
            "(progn (defclass mdes () ((a)))                (equal (mapcar #'slot-definition-name (class-slots 'mdes))                       (mapcar #'slot-definition-name (class-slots (find-class 'mdes)))))",
            "T",
        ),
        // Every accessor is FBOUND and reachable as a function designator.
        (
            "(every (lambda (f) (not (null (fboundp f))))                '(class-slots class-direct-slots slot-definition-name                  slot-definition-initargs slot-definition-initform                  slot-definition-allocation slot-definition-readers                  slot-definition-writers slot-definition-type))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// FILE-ERROR, PROGRAM-ERROR and CONTROL-ERROR must report the message they were
/// built with, not their bare type name (bliss-kk0i).
///
/// None of these classes has a slot for a message, so the text was discarded and
/// every one printed as "FILE-ERROR" / "PROGRAM-ERROR" / "CONTROL-ERROR". Mixing
/// SIMPLE-CONDITION in supplies FORMAT-CONTROL, exactly as SIMPLE-PACKAGE-ERROR
/// already did for package failures.
///
/// The typep and handler rows are the regression that matters: the fix changes
/// which CLASS is instantiated, so code written against FILE-ERROR must still
/// catch it.
#[test]
fn file_program_and_control_errors_report_their_message() {
    let cases = [
        // A message, not the bare type name.
        (
            "(let ((s (princ-to-string (handler-case (open \"/nonexistent/zz\") (error (e) e))))) (and (search \"zz\" s) t))",
            "T",
        ),
        (
            "(princ-to-string (handler-case (car 1 2) (error (e) e)))",
            "\"CAR called with 2 argument(s); requires 1\"",
        ),
        // A CONTROL-ERROR names the offending tag rather than reporting nothing.
        (
            "(let ((s (princ-to-string (handler-case (throw 'nope 1) (error (e) e))))) (and (search \"NOPE\" s) t))",
            "T",
        ),
        // Still the right classes, so existing handlers keep working.
        (
            "(typep (handler-case (open \"/nonexistent/zz\") (error (e) e)) 'file-error)",
            "T",
        ),
        (
            "(typep (handler-case (car 1 2) (error (e) e)) 'program-error)",
            "T",
        ),
        (
            "(typep (handler-case (throw 'nope 1) (error (e) e)) 'control-error)",
            "T",
        ),
        // And still caught by a handler for the specific class, not just ERROR.
        (
            "(handler-case (open \"/nonexistent/zz\") (file-error () :caught))",
            ":CAUGHT",
        ),
        (
            "(handler-case (car 1 2) (program-error () :caught))",
            ":CAUGHT",
        ),
        (
            "(handler-case (throw 'nope 1) (control-error () :caught))",
            ":CAUGHT",
        ),
    ];
    run_expression_cases(&cases);
}

#[test]
fn loop_while_until_repeat_drivers() {
    let cases = [
        ("(loop repeat 3 collect 'x)", "(X X X)"),
        ("(loop repeat 0 collect 'x)", "NIL"),
        (
            "(let ((i 0)) (loop while (< i 4) do (incf i) collect i))",
            "(1 2 3 4)",
        ),
        (
            "(let ((i 0)) (loop until (>= i 3) do (incf i) collect i))",
            "(1 2 3)",
        ),
        (
            "(loop for i from 1 to 100 while (< i 4) collect i)",
            "(1 2 3)",
        ),
        ("(loop for i from 1 repeat 3 collect i)", "(1 2 3)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-9q4): several gaps found while making cl-ppcre load.
///  - LOOP `for VAR = INIT then STEP`: INIT/STEP must run in source order with
///    the other clauses' steppings, so one referencing an earlier `for` variable
///    sees that variable's value for the current iteration (was: INIT hoisted to
///    the LET* bindings / STEP emitted to the bottom `steps`, both reading a stale
///    earlier variable).
///  - MAKE-ARRAY :element-type 'character with :fill-pointer/:adjustable is a
///    STRING (STRINGP true, CHAR/VECTOR-PUSH-EXTEND/COERCE work).
///  - SUBST accepts &key test.
///  - SETF of (THE type place), and of (nth/third/fourth … list).
#[test]
fn cl_ppcre_enabling_regressions() {
    let cases = [
        // LOOP for = then, cross-referencing an earlier clause.
        (
            "(loop for q = 7 for s = q then (1+ s) repeat 3 collect s)",
            "(7 8 9)",
        ),
        (
            "(loop for a = 1 for b = a for s = b then 0 repeat 1 collect (list a b s))",
            "((1 1 1))",
        ),
        (
            "(loop for s = 100 then (1+ s) repeat 3 collect s)",
            "(100 101 102)",
        ),
        // Adjustable/fill-pointer character array is a string.
        (
            "(let ((s (make-array 0 :element-type 'character :fill-pointer t :adjustable t)))\
               (vector-push-extend #\\a s) (vector-push-extend #\\b s)\
               (list (stringp s) (length s) (coerce s 'simple-string)))",
            "(T 2 \"ab\")",
        ),
        (
            "(stringp (make-array 3 :element-type 'character :fill-pointer 0 :adjustable t))",
            "T",
        ),
        // SUBST with :test.
        ("(subst 'z 'b '(a b (c b)) :test #'eql)", "(A Z (C Z))"),
        // SETF of (THE type place) and ordinal / nth places.
        (
            "(let ((l (list 1 2 3 4))) (setf (the fixnum (third l)) 99) l)",
            "(1 2 99 4)",
        ),
        // Updating macros use GET-SETF-EXPANSION rather than the evaluator's
        // direct SETF path.  THE's type specifier is syntax and must not become
        // a temporary value form (which would try to evaluate FIXNUM).
        ("(let ((n 2)) (list (incf (the fixnum n)) n))", "(3 3)"),
        ("(let ((n 2)) (list (setf (the fixnum n) 7) n))", "(7 7)"),
        ("(load-time-value (+ 1 2) t)", "3"),
        (
            "(let ((l (list 1 2 3 4 5))) (setf (fourth l) 40) (setf (nth 4 l) 50) l)",
            "(1 2 3 40 50)",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout} (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Regression (bliss-w5t): SUBSEQ / REVERSE / COPY-SEQ / CONCATENATE accept
/// COMPLEX_ARRAY (fill-pointer / adjustable) vectors as input — previously they
/// bailed with "not of type sequence" because collect_elements only handled
/// simple vectors. A general fill-pointer vector yields a vector result; a
/// character-typed fill-pointer vector (a fill-pointer STRING) yields a string.
#[test]
fn complex_vector_sequence_ops() {
    let cases = [
        // General (non-character) adjustable fill-pointer vector.
        (
            "(let ((v (make-array 0 :adjustable t :fill-pointer 0)))\
               (dolist (x '(10 20 30)) (vector-push-extend x v))\
               (list (subseq v 1) (reverse v) (copy-seq v) (concatenate 'list v '(99))))",
            "(#(20 30) #(30 20 10) #(10 20 30) (10 20 30 99))",
        ),
        // Character-typed fill-pointer vector: results are STRINGS.
        (
            "(let ((s (make-array 0 :element-type 'character :adjustable t :fill-pointer 0)))\
               (dolist (c '(#\\a #\\b #\\c)) (vector-push-extend c s))\
               (list (subseq s 1) (reverse s) (copy-seq s) (concatenate 'string s \"XY\")))",
            "(\"bc\" \"cba\" \"abc\" \"abcXY\")",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-oht4): PRINT/PRINC/FORMAT-T/WRITE-STRING honour a DYNAMIC
/// rebinding of *standard-output*, so (with-output-to-string (*standard-output*)
/// …) captures their output. The stream specials were seeded as lexical frame
/// bindings that shadowed the dynamic-bind cell, so a rebinding was ignored and
/// output leaked to the real stdout.
#[test]
fn dynamic_standard_output_rebinding_is_honoured() {
    let cases = [
        // Each op's output is captured by the *standard-output* rebinding.
        (
            r#"(with-output-to-string (*standard-output*) (princ "hi"))"#,
            "hi",
        ),
        (
            r#"(with-output-to-string (*standard-output*) (write-string "ws"))"#,
            "ws",
        ),
        (
            r#"(with-output-to-string (*standard-output*) (format t "f~d" 9))"#,
            "f9",
        ),
        (
            r#"(let ((s (make-string-output-stream))) (let ((*standard-output* s)) (princ 42)) (get-output-stream-string s))"#,
            "42",
        ),
        (
            r#"(with-output-to-string (*standard-output*) (dotimes (i 3) (format t "~d," i)))"#,
            "0,1,2,",
        ),
    ];
    for (expr, expected) in cases {
        // princ the captured string + terpri, so the first line is exactly it.
        let prog = format!("(progn (princ {expr}) (terpri))");
        let out = egcl_bin()
            .args(["--eval", &prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().next().unwrap_or("");
        assert_eq!(got, expected, "{expr} => {got:?} (full: {stdout:?})");
    }
}

/// Regression (bliss-6w2y): FIND-SYMBOL has no side effects and reports the right
/// accessibility. It used to intern a fresh symbol for any name in CL-USER and
/// report :EXTERNAL, so a never-seen name wrongly "found" a symbol; and INTERN
/// always returned :INTERNAL as its second value. Now a non-existent name yields
/// (NIL NIL), a newly-interned symbol yields NIL, and existing symbols report
/// their real status.
#[test]
fn find_symbol_intern_accessibility() {
    let cases = [
        // FIND-SYMBOL of a never-interned name: (NIL NIL), no symbol created.
        (
            "(multiple-value-list (find-symbol \"NEVER-SEEN-QQ\" :cl-user))",
            "(NIL NIL)",
        ),
        // INTERN a brand-new symbol: second value NIL (freshly created).
        ("(nth-value 1 (intern \"BRAND-NEW-QQ\" :cl-user))", "NIL"),
        // The STRING argument is an opaque symbol name.  Package-marker
        // characters inside it are not reader syntax (CLHS INTERN).
        (
            "(let ((s (intern \"BIDICLASS:L\" :cl-user))) (list (symbol-name s) (eq s (find-symbol \"BIDICLASS:L\" :cl-user))))",
            "(\"BIDICLASS:L\" T)",
        ),
        // CL builtins are :EXTERNAL, consistently via FIND-SYMBOL and INTERN.
        ("(nth-value 1 (find-symbol \"CAR\" :cl))", ":EXTERNAL"),
        ("(nth-value 1 (intern \"CONS\" :common-lisp))", ":EXTERNAL"),
        // A CL symbol inherited into CL-USER is :INHERITED.
        ("(nth-value 1 (find-symbol \"CAR\" :cl-user))", ":INHERITED"),
        // An EXISTING keyword is external (`:foo` reader-interns it first).
        (
            "(progn :some-kw (nth-value 1 (find-symbol \"SOME-KW\" :keyword)))",
            ":EXTERNAL",
        ),
        // FIND-SYMBOL of an ABSENT keyword returns NIL and must NOT intern it —
        // CLHS forbids the side effect. Previously egcl went through the reader
        // (interning) and reported :EXTERNAL for any name (bliss-2evi).
        (
            "(nth-value 1 (find-symbol \"NEVER-KW-QQ\" :keyword))",
            "NIL",
        ),
        // No side effect: a second lookup of the same absent name is still NIL
        // (it was not fabricated by the first).
        (
            "(progn (find-symbol \"ALSO-NEVER-KW\" :keyword) (nth-value 1 (find-symbol \"ALSO-NEVER-KW\" :keyword)))",
            "NIL",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-nmhw, follow-up to bliss-xe2p): a builtin fast-path in the
/// interpreter must not ignore keyword arguments that its boot.lisp defun
/// honours — otherwise a DIRECT-FORM call `(fn … :kw v)` diverges from the same
/// call made via FUNCALL/APPLY (which always routes to the defun). This guards
/// the audited keyword-honouring builtins against future fast-path/defun drift.
#[test]
fn keyword_builtins_agree_direct_and_via_funcall() {
    // Each conjunct compares the direct form against the FUNCALL form; the whole
    // program prints T iff every builtin honours its keywords identically both
    // ways. WRITE-TO-STRING (:base) is the original bliss-xe2p offender.
    let expr = r#"(and
      (equal (count 1 (list 1 2 1 3 1) :start 2) (funcall #'count 1 (list 1 2 1 3 1) :start 2))
      (equal (count-if #'oddp (list 1 2 3 4 5) :start 2) (funcall #'count-if #'oddp (list 1 2 3 4 5) :start 2))
      (equal (position 1 (list 1 2 1) :from-end t) (funcall #'position 1 (list 1 2 1) :from-end t))
      (equal (remove 1 (list 1 2 1 1) :count 1) (funcall #'remove 1 (list 1 2 1 1) :count 1))
      (equal (substitute 0 1 (list 1 1 1) :count 1) (funcall #'substitute 0 1 (list 1 1 1) :count 1))
      (equal (fill (list 1 2 3 4) 0 :start 1 :end 3) (funcall #'fill (list 1 2 3 4) 0 :start 1 :end 3))
      (equal (reduce #'cons (list 1 2 3) :from-end t) (funcall #'reduce #'cons (list 1 2 3) :from-end t))
      (equal (string-upcase "abcdef" :start 2 :end 4) (funcall #'string-upcase "abcdef" :start 2 :end 4))
      (equal (make-list 3 :initial-element 'x) (funcall #'make-list 3 :initial-element 'x))
      (equal (parse-integer "42abc" :junk-allowed t) (funcall #'parse-integer "42abc" :junk-allowed t))
      (equal (write-to-string 255 :base 16) (funcall #'write-to-string 255 :base 16)))"#;
    let out = egcl_bin()
        .args(["--eval", &format!("(print {expr})")])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "should exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let got = stdout
        .lines()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("");
    assert_eq!(
        got, "T",
        "a keyword builtin diverges between direct and funcall call forms (full: {stdout:?})"
    );
}

/// Regression (bliss-znib): FORMAT ~A/~S honour the minpad and padchar params
/// (`~mincol,colinc,minpad,padchar`), which were ignored — padding always used a
/// space. Width is measured in characters.
#[test]
fn format_a_s_padchar() {
    let cases = [
        (r#"(format nil "~10,,,'*a" "hi")"#, "hi********"),
        (r#"(format nil "~10,,,'*@a" "hi")"#, "********hi"),
        (r#"(format nil "~5,,2,'-a" "ab")"#, "ab---"),
        (r#"(format nil "~v,,,'*a" 6 "hi")"#, "hi****"),
        // Defaults (space) still work; plain ~a unchanged.
        (r#"(format nil "~10a" "hi")"#, "hi        "),
        (r#"(format nil "~10@a" "hi")"#, "        hi"),
        (r#"(format nil "~a" "plain")"#, "plain"),
    ];
    for (expr, expected) in cases {
        // princ the padded string then a newline, so the first line is exactly it
        // (trailing padding preserved).
        let prog = format!("(progn (princ {expr}) (terpri))");
        let out = egcl_bin()
            .args(["--eval", &prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().next().unwrap_or("");
        assert_eq!(got, expected, "{expr} => {got:?} (full: {stdout:?})");
    }
}

/// Regression (bliss-h7ay): a numeric LOOP `for` clause may omit the start,
/// beginning with a limit/step keyword — `for i below 5` ≡ `for i from 0 below 5`
/// (CLHS 6.1.2.1). Previously only `from`/`upfrom`/`downfrom` entered the numeric
/// branch, so a bare `below`/`to`/`upto` errored.
#[test]
fn loop_numeric_for_without_explicit_from() {
    let cases = [
        ("(loop for i below 5 collect i)", "(0 1 2 3 4)"),
        ("(loop for i to 3 collect i)", "(0 1 2 3)"),
        ("(loop for i upto 3 collect i)", "(0 1 2 3)"),
        ("(loop for i below 6 by 2 collect i)", "(0 2 4)"),
        ("(loop for i to 10 by 3 collect i)", "(0 3 6 9)"),
        ("(loop for i below 0 collect i)", "NIL"),
        ("(loop for i to 4 sum i)", "10"),
        // Explicit-from forms still work.
        ("(loop for i from 2 to 5 collect i)", "(2 3 4 5)"),
        ("(loop for i downfrom 5 above 0 collect i)", "(5 4 3 2 1)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-v304): (COERCE symbol 'FUNCTION) returns the FUNCTION named
/// by the symbol (fdefinition), so the result is FUNCTIONP and callable — it used
/// to return the symbol unchanged. `#'name` is unaffected (shared helper).
#[test]
fn coerce_symbol_to_function() {
    // Builtins.
    let out = egcl_bin()
        .args([
            "--eval",
            "(defun v304-uf (x) (* x x))",
            "--eval",
            "(print (list (functionp (coerce 'car 'function)) \
                    (funcall (coerce 'car 'function) '(9 8)) \
                    (functionp (coerce '+ 'function)) \
                    (functionp (coerce 'v304-uf 'function)) \
                    (funcall (coerce 'v304-uf 'function) 5)))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Two --eval forms each echo; the print result is the LAST non-empty line.
    let got = stdout
        .lines()
        .map(|l| l.trim())
        .rev()
        .find(|l| !l.is_empty())
        .unwrap_or("");
    assert_eq!(got, "(T 9 T T 25)", "coerce-to-function: {stdout:?}");

    // An already-callable value passes through; #'name still FUNCTIONP; an
    // unbound name errors.
    for (expr, expected) in [
        ("(functionp (coerce (lambda (x) x) 'function))", "T"),
        ("(functionp #'car)", "T"),
        (
            "(handler-case (coerce 'no-such-fn-xyz 'function) (error () :err))",
            ":ERR",
        ),
    ] {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => {got}");
    }
}

/// Regression (bliss-gijz): REMOVE-IF / REMOVE-IF-NOT / DELETE-IF operate on any
/// sequence and return a sequence of the SAME type. They used DOLIST, so a vector
/// or string argument errored "not of type list".
#[test]
fn remove_if_over_sequences() {
    let cases = [
        ("(remove-if #'oddp (vector 1 2 3 4))", "#(2 4)"),
        ("(remove-if-not #'evenp (vector 1 2 3 4))", "#(2 4)"),
        ("(remove-if #'oddp (list 1 2 3 4))", "(2 4)"),
        ("(delete-if #'oddp (vector 1 2 3 4))", "#(2 4)"),
        (
            "(remove-if (lambda (c) (char= c #\\l)) \"hello\")",
            "\"heo\"",
        ),
        ("(remove-if #'plusp #())", "#()"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-lac): MAKE-HASH-TABLE :test accepts a function DESIGNATOR
/// (the FUNCTION `#'equal`, not just the symbol `'equal`). bliss-uuh made
/// `#'<builtin>` a wrapper closure rather than the bare symbol, which the :test
/// parser (matching by symbol name) then ignored, silently defaulting to EQL — so
/// an EQUAL table with list/string keys wrongly missed. Resolved via a
/// function-designator name lookup.
#[test]
fn make_hash_table_test_accepts_function_designator() {
    let cases = [
        // #'equal with list AND string keys.
        (
            "(let ((h (make-hash-table :test #'equal))) (setf (gethash (list 1 2) h) :a) (gethash (list 1 2) h))",
            ":A",
        ),
        (
            "(let ((h (make-hash-table :test #'equal))) (setf (gethash \"s\" h) :b) (gethash \"s\" h))",
            ":B",
        ),
        // #'equalp (case-insensitive).
        (
            "(let ((h (make-hash-table :test #'equalp))) (setf (gethash \"AbC\" h) :c) (gethash \"abc\" h))",
            ":C",
        ),
        // The symbol form still works.
        (
            "(let ((h (make-hash-table :test 'equal))) (setf (gethash (list 9) h) :d) (gethash (list 9) h))",
            ":D",
        ),
        // Default EQL: a fresh list key does NOT match (identity), fixnum does.
        (
            "(let ((h (make-hash-table))) (setf (gethash (list 1) h) :e) (gethash (list 1) h))",
            "NIL",
        ),
        (
            "(let ((h (make-hash-table))) (setf (gethash 5 h) :f) (gethash 5 h))",
            ":F",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-apr): rational/heap-numeric stdlib gaps — ABS works on
/// ratios/bignums (not just fixnum/single-float); NUMERATOR/DENOMINATOR/FLOAT are
/// defined; and ratio LITERALS are reduced to lowest terms with a positive
/// denominator (6/4 => 3/2, 4/2 => 2).
#[test]
fn rational_numeric_ops_and_ratio_literal_reduction() {
    let cases = [
        // Ratio literals canonicalize (reader).
        ("6/4", "3/2"),
        ("4/2", "2"),
        ("-6/4", "-3/2"),
        ("6/-4", "-3/2"),
        ("10/5", "2"),
        ("7/7", "1"),
        // ABS over the numeric tower.
        ("(abs -3/4)", "3/4"),
        ("(abs (- (expt 2 80)))", "1208925819614629174706176"),
        ("(abs -5)", "5"),
        ("(abs -2.5)", "2.5"),
        // NUMERATOR/DENOMINATOR/FLOAT.
        ("(numerator 6/4)", "3"),
        ("(denominator 6/4)", "2"),
        ("(numerator 5)", "5"),
        ("(denominator 5)", "1"),
        ("(float 1/4)", "0.25"),
        ("(float 3)", "3.0"),
        // usable via #'.
        ("(funcall #'abs -7/8)", "7/8"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (egcl-c0m): TYPEP recognises the real heap numeric types (bignum,
/// ratio, double-float, complex) — previously its NUMBER/REAL/RATIONAL/INTEGER/
/// FLOAT checks only saw fixnum/single-float, so e.g. (typep (expt 2 100)
/// 'number) wrongly returned NIL. Plus complex introspection: REALPART/IMAGPART/
/// COMPLEXP and #C(r i) printing, consistent across tree-walker and compiler.
#[test]
fn typep_heap_numerics_and_complex_introspection() {
    let cases = [
        ("(typep (expt 2 100) 'number)", "T"),
        ("(typep (expt 2 100) 'integer)", "T"),
        ("(typep (expt 2 100) 'bignum)", "T"),
        ("(typep 1/2 'rational)", "T"),
        ("(typep 1/2 'ratio)", "T"),
        ("(typep 1/2 'number)", "T"),
        ("(typep 1/2 'integer)", "NIL"),
        ("(typep #c(1 2) 'complex)", "T"),
        ("(typep #c(1 2) 'number)", "T"),
        ("(typep 3 'complex)", "NIL"),
        // Complex introspection.
        ("(complexp #c(1 2))", "T"),
        ("(complexp 5)", "NIL"),
        ("(realpart #c(3 4))", "3"),
        ("(imagpart #c(3 4))", "4"),
        ("(realpart 7)", "7"),
        ("(imagpart 7)", "0"),
        ("(type-of #c(1 2))", "COMPLEX"),
        ("(prin1-to-string #c(1 2))", "\"#C(1 2)\""),
        // Character subtypes: STANDARD-CHAR (newline + printable ASCII),
        // BASE-CHAR (all chars here), EXTENDED-CHAR (none). (bliss-vdtz)
        ("(typep #\\a 'standard-char)", "T"),
        ("(typep #\\Newline 'standard-char)", "T"),
        ("(typep #\\Tab 'standard-char)", "NIL"),
        ("(typep #\\a 'base-char)", "T"),
        ("(typep #\\a 'extended-char)", "NIL"),
        ("(typep 5 'standard-char)", "NIL"),
        // Standard float constants: PI (single-precision approximation) and the
        // single-float magnitude extremes. (bliss-ncor)
        ("(floatp pi)", "T"),
        ("(and (> pi 3.1415) (< pi 3.1416))", "T"),
        ("(< (abs (- pi (* 4 (atan 1)))) 0.001)", "T"),
        ("(> most-positive-single-float 1.0e38)", "T"),
        ("(< most-negative-single-float -1.0e38)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// EXPT of a negative real to a non-integer power, or of a complex base, returns
/// a complex (real `powf` returned NaN, and a complex base errored). Integer
/// powers of a complex base are exact via repeated multiplication.
#[test]
fn expt_complex_results() {
    let cases = [
        // Integer powers of complex — exact (canonicalise when real).
        ("(expt #c(0 1) 2)", "-1"),
        ("(expt #c(1 1) 2)", "#C(0 2)"),
        ("(expt #c(0 1) 3)", "#C(0 -1)"),
        ("(expt #c(2 0) -1)", "1/2"),
        // Negative real to a non-integer power → complex.
        ("(expt -8 1/3)", "#C(1.0 1.7320508)"),
        // (expt -1 0.5) ≈ i (tiny float epsilon in the real part).
        ("(if (< (abs (realpart (expt -1 0.5))) 1d-3) t nil)", "T"),
        (
            "(if (< (abs (- (imagpart (expt -1 0.5)) 1.0)) 1d-3) t nil)",
            "T",
        ),
        // i*i via expt round-trips to ~-1.
        ("(round (realpart (* (expt -1 0.5) (expt -1 0.5))))", "-1"),
        // Real/integer cases unchanged.
        ("(expt 2 3)", "8"),
        ("(expt -2 3)", "-8"),
        ("(expt 4 0.5)", "2.0"),
        ("(expt 2 -3)", "1/8"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// *PRINT-LENGTH* (truncate a list with `...` after N elements) and *PRINT-LEVEL*
/// (print `#` past a nesting depth) — both were ignored, so lists always printed
/// in full.
#[test]
fn print_length_and_level() {
    let cases = [
        (
            "(let ((*print-length* 3)) (prin1-to-string '(1 2 3 4 5)))",
            "\"(1 2 3 ...)\"",
        ),
        (
            "(let ((*print-length* 2)) (prin1-to-string '(a b c)))",
            "\"(A B ...)\"",
        ),
        // Fewer than the limit prints in full (no ...).
        (
            "(let ((*print-length* 3)) (prin1-to-string '(1 2)))",
            "\"(1 2)\"",
        ),
        (
            "(let ((*print-level* 2)) (prin1-to-string '(1 (2 (3 (4))))))",
            "\"(1 (2 #))\"",
        ),
        (
            "(let ((*print-level* 1)) (prin1-to-string '(1 (2) 3)))",
            "\"(1 # 3)\"",
        ),
        (
            "(let ((*print-level* 0)) (prin1-to-string '(1 2)))",
            "\"#\"",
        ),
        // Combined, and honored via FORMAT ~A.
        (
            "(let ((*print-length* 3) (*print-level* 2)) (prin1-to-string '(1 2 3 4 (5 (6 7)))))",
            "\"(1 2 3 ...)\"",
        ),
        (
            "(let ((*print-length* 2)) (format nil \"~a\" '(a b c d)))",
            "\"(A B ...)\"",
        ),
        // Default (unbounded) is unchanged.
        ("(prin1-to-string '(1 2 3 4 5))", "\"(1 2 3 4 5)\""),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// prin1/~S escape `"` and `\` inside a string so the printed form reads back
/// (CLHS 22.1.3.4). The stdlib printer (prin1-to-string / write-to-string /
/// FORMAT ~S) previously emitted the raw body, so the output did not round-trip
/// and disagreed with the cli prin1 builtin. (bliss-i608)
#[test]
fn prin1_escapes_string_body_for_roundtrip() {
    let cases = [
        // read of the printed form equals the original string.
        (
            r#"(equal "a\"b" (read-from-string (prin1-to-string "a\"b")))"#,
            "T",
        ),
        (
            r#"(equal "back\\slash" (read-from-string (prin1-to-string "back\\slash")))"#,
            "T",
        ),
        // stdlib (prin1-to-string) and cli (prin1 to a stream) paths agree.
        (
            r#"(equal (prin1-to-string "x\"y\\z") (with-output-to-string (o) (prin1 "x\"y\\z" o)))"#,
            "T",
        ),
        // write-to-string escapes too; a plain string is unchanged.
        (
            r#"(equal "plain" (read-from-string (write-to-string "plain")))"#,
            "T",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// COERCE must not silently return a wrong value for a non-coercible target:
/// an integer is not a character designator (65 was becoming #\6), and there is
/// no coercion to INTEGER/RATIONAL/REAL/NUMBER — a mismatched object type-errors
/// rather than passing through unchanged (bliss-vv1b).
#[test]
fn coerce_rejects_non_designators_and_bad_numeric_targets() {
    let cases = [
        // Wrong-result cases now signal a catchable type-error.
        (
            "(handler-case (coerce 65 'character) (type-error () :err))",
            ":ERR",
        ),
        (
            "(handler-case (coerce (code-char 65) 'integer) (type-error () :err))",
            ":ERR",
        ),
        (
            "(handler-case (coerce 1.5 'integer) (type-error () :err))",
            ":ERR",
        ),
        (
            "(handler-case (coerce #\\a 'integer) (type-error () :err))",
            ":ERR",
        ),
        // Multi-character strings are not character designators.
        (
            "(handler-case (coerce \"ab\" 'character) (type-error () :err))",
            ":ERR",
        ),
        // Valid coercions and identity cases are unchanged.
        ("(coerce \"A\" 'character)", "#\\A"),
        ("(coerce #\\A 'character)", "#\\A"),
        // 'a reads as the symbol A (reader upcases), so its name char is A.
        ("(coerce 'a 'character)", "#\\A"),
        ("(coerce 3 'integer)", "3"),
        ("(coerce 1/2 'rational)", "1/2"),
        ("(coerce 3 'real)", "3"),
        ("(coerce 7 'number)", "7"),
        ("(coerce 3 'float)", "3.0"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// *PRINT-CIRCLE* t: shared and circular structure prints with #N=/#N# labels
/// and terminates, instead of looping forever (bliss-dlil). Covers both the
/// stdlib printer (prin1-to-string) and — via the returned string — the fact
/// that a circular list no longer hangs the printer.
#[test]
fn print_circle_labels_shared_and_circular() {
    let cases = [
        // Circular list: the cdr of the last cons points back to the head.
        (
            "(let ((x (list 1 2 3))) (setf (cdddr x) x) (let ((*print-circle* t)) (prin1-to-string x)))",
            "\"#1=(1 2 3 . #1#)\"",
        ),
        // Self-referential car.
        (
            "(let ((x (list 1))) (setf (car x) x) (let ((*print-circle* t)) (prin1-to-string x)))",
            "\"#1=(#1#)\"",
        ),
        // Shared (non-circular) substructure: the same list appears twice.
        (
            "(let* ((y (list 1 2)) (z (list y y))) (let ((*print-circle* t)) (prin1-to-string z)))",
            "\"(#1=(1 2) #1#)\"",
        ),
        // Two distinct shared subtrees get distinct labels, in first-print order.
        (
            "(let ((a (list 1)) (b (list 2))) (let ((*print-circle* t)) (prin1-to-string (list a b a b))))",
            "\"(#1=(1) #2=(2) #1# #2#)\"",
        ),
        // A shared node used three times labels once, back-references twice.
        (
            "(let ((y (list 7))) (let ((*print-circle* t)) (prin1-to-string (list y y y))))",
            "\"(#1=(7) #1# #1#)\"",
        ),
        // No shared structure under *print-circle* t: no labels appear.
        (
            "(let ((*print-circle* t)) (prin1-to-string (list 1 (list 2 3) 4)))",
            "\"(1 (2 3) 4)\"",
        ),
        // Dotted pair is unaffected.
        (
            "(let ((*print-circle* t)) (prin1-to-string (cons 1 2)))",
            "\"(1 . 2)\"",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// *PRINT-CASE* (was ignored) controls how symbol names are cased on output:
/// :UPCASE (default), :DOWNCASE, :CAPITALIZE — in prin1/princ/format/write.
#[test]
fn print_case_variable() {
    let cases = [
        (
            "(let ((*print-case* :downcase)) (prin1-to-string 'foo-bar))",
            "\"foo-bar\"",
        ),
        (
            "(let ((*print-case* :capitalize)) (prin1-to-string 'foo-bar))",
            "\"Foo-Bar\"",
        ),
        (
            "(let ((*print-case* :upcase)) (prin1-to-string 'foo-bar))",
            "\"FOO-BAR\"",
        ),
        ("(prin1-to-string 'foo-bar)", "\"FOO-BAR\""),
        // Keyword keeps its colon; the name is cased.
        (
            "(let ((*print-case* :downcase)) (prin1-to-string :my-key))",
            "\":my-key\"",
        ),
        // Applies recursively and through format/write.
        (
            "(let ((*print-case* :downcase)) (prin1-to-string '(a b (c d))))",
            "\"(a b (c d))\"",
        ),
        (
            "(let ((*print-case* :downcase)) (format nil \"~a\" 'hello))",
            "\"hello\"",
        ),
        (
            "(let ((*print-case* :downcase)) (write-to-string 'abc))",
            "\"abc\"",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// MAP-INTO (was undefined) destructively fills the result sequence with the
/// results of applying the function to successive elements, up to the shortest
/// input length (or the whole result when there are no input sequences).
#[test]
fn map_into_sequence() {
    let cases = [
        ("(map-into (make-list 3) #'1+ '(10 20 30))", "(11 21 31)"),
        (
            "(map-into (make-array 3) #'+ '(1 2 3) '(10 20 30))",
            "#(11 22 33)",
        ),
        // Shortest length wins; the trailing element keeps its value.
        (
            "(map-into (vector 0 0 0 0) #'* '(1 2 3) '(4 5 6 7))",
            "#(4 10 18 0)",
        ),
        // No input sequences: fill the whole result by calling the function.
        (
            "(let ((c 0)) (map-into (make-list 3) (lambda () (incf c))))",
            "(1 2 3)",
        ),
        ("(fboundp 'map-into)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Heap numerics (bignum/ratio/complex/double-float) hash and compare by VALUE,
/// so they work as hash-table keys under EQL/EQUAL/EQUALP — previously SXHASH and
/// the key comparison used the object's address, so two distinct-but-equal keys
/// never matched. Also HASH-TABLE-TEST (was undefined).
#[test]
fn hash_tables_with_heap_numeric_keys() {
    let cases = [
        // SXHASH is value-consistent for heap numerics.
        ("(= (sxhash #c(1 2)) (sxhash #c(1 2)))", "T"),
        ("(= (sxhash (expt 2 70)) (sxhash (expt 2 70)))", "T"),
        ("(= (sxhash (/ 1 3)) (sxhash (/ 1 3)))", "T"),
        // Keys found under each test.
        (
            "(let ((h (make-hash-table :test 'equal))) (setf (gethash #c(1 2) h) :cx) (gethash #c(1 2) h))",
            ":CX",
        ),
        (
            "(let ((h (make-hash-table :test 'eql))) (setf (gethash #c(1 2) h) :x) (gethash #c(1 2) h))",
            ":X",
        ),
        (
            "(let ((h (make-hash-table :test 'equal))) (setf (gethash (/ 1 3) h) :r) (gethash (/ 1 3) h))",
            ":R",
        ),
        (
            "(let ((h (make-hash-table :test 'eql))) (setf (gethash (expt 2 70) h) :b) (gethash (expt 2 70) h))",
            ":B",
        ),
        // Regressions: fixnum/string/list keys unaffected.
        (
            "(let ((h (make-hash-table))) (setf (gethash 1.5 h) :f) (gethash 1.5 h))",
            ":F",
        ),
        (
            "(let ((h (make-hash-table :test 'equal))) (setf (gethash (list 1 2) h) :l) (gethash (list 1 2) h))",
            ":L",
        ),
        // HASH-TABLE-TEST.
        ("(hash-table-test (make-hash-table :test 'equal))", "EQUAL"),
        ("(hash-table-test (make-hash-table))", "EQL"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// A non-local exit with no active target — THROW to a tag with no CATCH, GO to
/// a dead tag, RETURN-FROM to a dead block — signals a catchable CONTROL-ERROR
/// (CLHS 5.2), not an uncatchable internal error. Covers both the tree-walker
/// and the bytecode-compiled path.
#[test]
fn uncaught_nonlocal_exit_is_catchable_control_error() {
    let cases = [
        (
            "(handler-case (throw 'nope 1) (control-error () :ce))",
            ":CE",
        ),
        ("(handler-case (throw 'nope 1) (error () :err))", ":ERR"),
        (
            "(handler-case (return-from nowhere 5) (control-error () :ce))",
            ":CE",
        ),
        ("(handler-case (go nowhere) (control-error () :ce))", ":CE"),
        ("(ignore-errors (throw 'x 1))", "NIL"),
        // A THROW inside a called (compiled) function takes the bytecode path.
        (
            "(progn (defun thr () (throw 'gone 1)) (handler-case (thr) (control-error () :ce)))",
            ":CE",
        ),
        // Normal non-local exits are unaffected.
        ("(catch 't1 (throw 't1 99))", "99"),
        ("(block b (return-from b 7))", "7"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-8zrb: FORMAT ~E/~G format a single-float from the f32 itself (shortest
/// round-trip) rather than its widened f64, which exposed binary32 imprecision
/// ((format nil "~e" 0.001) had been 1.0000000474974513E-3). ~E keeps a decimal
/// point in the mantissa.
#[test]
fn format_exponential_shortest_float() {
    let cases = [
        ("(format nil \"~e\" 0.001)", "\"1.0e-3\""),
        ("(format nil \"~e\" 1234.5)", "\"1.2345e+3\""),
        ("(format nil \"~e\" 1.0)", "\"1.0e+0\""),
        ("(format nil \"~e\" 0.1)", "\"1.0e-1\""),
        ("(format nil \"~e\" -0.5)", "\"-5.0e-1\""),
        ("(format nil \"~e\" 12)", "\"1.2e+1\""),
        ("(format nil \"~g\" 0.001)", "\"0.001\""),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// The float introspection/manipulation family (were undefined): DECODE-FLOAT,
/// INTEGER-DECODE-FLOAT, SCALE-FLOAT, FLOAT-SIGN, FLOAT-RADIX/DIGITS/PRECISION,
/// and FFLOOR/FCEILING/FTRUNCATE/FROUND (float-quotient rounding).
#[test]
fn float_introspection_and_frounding() {
    let cases = [
        (
            "(multiple-value-list (integer-decode-float 1.0))",
            "(8388608 -23 1)",
        ),
        (
            "(multiple-value-list (integer-decode-float 0.5))",
            "(8388608 -24 1)",
        ),
        ("(multiple-value-list (decode-float 1.0))", "(0.5 1 1.0)"),
        ("(multiple-value-list (decode-float 8.0))", "(0.5 4 1.0)"),
        ("(scale-float 1.5 2)", "6.0"),
        ("(scale-float 1.0 -1)", "0.5"),
        ("(float-sign -3.0)", "-1.0"),
        ("(float-sign -1.0 5.0)", "-5.0"),
        (
            "(list (float-radix 1.0) (float-digits 1.0) (float-precision 0.0))",
            "(2 24 0)",
        ),
        // F-rounding: float quotient + exact remainder.
        ("(multiple-value-list (ffloor 17 5))", "(3.0 2)"),
        ("(values (fround 3.5))", "4.0"),
        ("(values (ftruncate 3.7))", "3.0"),
        // fboundp is consistent.
        ("(list (fboundp 'scale-float) (fboundp 'ffloor))", "(T T)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-3bk3: GET-DECODED-TIME, UPGRADED-ARRAY-ELEMENT-TYPE,
/// UPGRADED-COMPLEX-PART-TYPE (were undefined), and PATHNAME-MATCH-P coercing
/// string designators (was a type-error on strings).
#[test]
fn misc_time_type_and_pathname_designators() {
    let cases = [
        ("(length (multiple-value-list (get-decoded-time)))", "9"),
        ("(upgraded-array-element-type 'character)", "CHARACTER"),
        ("(upgraded-array-element-type 'bit)", "BIT"),
        ("(upgraded-array-element-type t)", "T"),
        ("(upgraded-array-element-type '(unsigned-byte 8))", "T"),
        ("(upgraded-complex-part-type 'single-float)", "SINGLE-FLOAT"),
        ("(upgraded-complex-part-type 'integer)", "RATIONAL"),
        // PATHNAME-MATCH-P coerces string designators.
        (
            "(if (pathname-match-p \"/a/b.lisp\" \"/a/*.lisp\") t nil)",
            "T",
        ),
        ("(pathname-match-p \"/a/b.txt\" \"/a/*.lisp\")", "NIL"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// SUBTYPEP with a compound (parameterized/bounded) SUBTYPE: it narrows its head
/// type, so it is a subtype of whatever the head is — (integer 0 10) ⊆ integer,
/// (vector t 3) ⊆ array, (mod 5) ⊆ integer. Previously sym_name on the cons gave
/// "", so any two compounds compared equal (a false positive) and compound-vs-
/// atom gave NIL. Also fills SIMPLE-ARRAY/SIMPLE-VECTOR into the type lattice.
#[test]
fn subtypep_compound_subtypes() {
    let cases = [
        ("(subtypep '(integer 0 10) 'integer)", "(T T)"),
        ("(subtypep '(integer 0 10) 'number)", "(T T)"),
        ("(subtypep '(vector t 3) 'vector)", "(T T)"),
        ("(subtypep '(vector t 3) 'array)", "(T T)"),
        ("(subtypep '(string 5) 'string)", "(T T)"),
        ("(subtypep '(mod 5) 'integer)", "(T T)"),
        ("(subtypep '(unsigned-byte 8) 'integer)", "(T T)"),
        ("(subtypep '(simple-array t) 'array)", "(T T)"),
        ("(subtypep 'simple-vector 'vector)", "(T T)"),
        // A wider type is NOT a subtype of a narrower bounded one — and with the
        // numeric-range interval algebra this is now a DEFINITE answer (NIL T),
        // matching ansi-test subtypep.integer.4.
        ("(subtypep 'integer '(integer 0 10))", "(NIL T)"),
        // Different compound heads are unrelated (no false positive).
        ("(subtypep '(integer 0 10) '(vector t))", "(NIL NIL)"),
        // NIL is a subtype of everything, including bounded types.
        ("(subtypep nil '(integer 0 5))", "(T T)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print (multiple-value-list {expr}))")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// PEEK-CHAR (was undefined): returns the next character without consuming it;
/// peek-type NIL = next char, T = skip whitespace, a character = skip until it.
#[test]
fn peek_char_all_peek_types() {
    let cases = [
        // NIL: peek doesn't consume — the next READ-CHAR returns the same char.
        (
            "(let ((s (make-string-input-stream \"abc\"))) (list (peek-char nil s) (read-char s) (read-char s)))",
            "(#\\a #\\a #\\b)",
        ),
        // T: skip leading whitespace, leave the found char unconsumed.
        (
            "(let ((s (make-string-input-stream \"   xy\"))) (list (peek-char t s) (read-char s)))",
            "(#\\x #\\x)",
        ),
        // a character: skip until it, leave it unconsumed.
        (
            "(let ((s (make-string-input-stream \"abcXd\"))) (list (peek-char #\\X s) (read-char s)))",
            "(#\\X #\\X)",
        ),
        // EOF handling with eof-error-p NIL and an eof-value.
        (
            "(peek-char nil (make-string-input-stream \"\") nil :eof)",
            ":EOF",
        ),
        ("(fboundp 'peek-char)", "T"),
        (
            "(with-input-from-string (s \"  42\") (list (peek-char t s) (read s)))",
            "(#\\4 42)",
        ),
        // WITH-SIMPLE-RESTART (also was undefined): normal value passes through;
        // invoking the restart aborts the body and yields (values nil t).
        ("(with-simple-restart (foo \"d\") 42)", "42"),
        (
            "(multiple-value-list (with-simple-restart (foo \"desc\") (invoke-restart 'foo)))",
            "(NIL T)",
        ),
        (
            "(handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart 'use)))) (with-simple-restart (use \"skip\") (error \"x\")) :recovered)",
            ":RECOVERED",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// LOOP `named NAME` establishes a block named NAME (CLHS 6.1.1.4) so
/// (return-from NAME …) exits the loop — previously "unbound variable: NAMED".
#[test]
fn loop_named_establishes_a_block() {
    let cases = [
        (
            "(loop named outer for i from 1 to 3 do (return-from outer i))",
            "1",
        ),
        (
            "(loop named outer for i from 1 to 5 when (> i 3) do (return-from outer i))",
            "4",
        ),
        (
            "(loop named foo for i from 1 to 10 do (when (= i 5) (return-from foo (* i 100))))",
            "500",
        ),
        // A named loop still collects/sums normally.
        ("(loop named foo for i from 1 to 3 collect i)", "(1 2 3)"),
        ("(loop named n for i in '(1 2 3) sum i)", "6"),
        // return-from an ENCLOSING block passes through the named loop.
        (
            "(block b (loop named inner for i from 1 to 3 do (return-from b (* i 10))) 999)",
            "10",
        ),
        // Unnamed loops and the RETURN clause are unaffected.
        (
            "(loop for i from 1 to 5 when (= i 3) return (* i 11))",
            "33",
        ),
        ("(loop for i from 1 to 3 collect i)", "(1 2 3)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-rh0t: the reader reads the `#nA(...)` multidimensional-array literal
/// (the printer already emits it), and EQUALP compares multidim arrays
/// element-wise — so an array round-trips through print → read → EQUALP.
#[test]
fn read_nd_array_literal_and_equalp_roundtrip() {
    let cases = [
        (
            "(array-rank (read-from-string \"#2A((1 2 3) (4 5 6))\"))",
            "2",
        ),
        (
            "(aref (read-from-string \"#2A((1 2 3) (4 5 6))\") 1 2)",
            "6",
        ),
        (
            "(array-dimensions (read-from-string \"#2A((1 2 3) (4 5 6))\"))",
            "(2 3)",
        ),
        (
            "(aref (read-from-string \"#3A(((1 2) (3 4)) ((5 6) (7 8)))\") 1 0 1)",
            "6",
        ),
        ("(vectorp (read-from-string \"#1A(1 2 3)\"))", "T"),
        // EQUALP on multidim arrays.
        ("(equalp #2A((1 2)) #2A((1 2)))", "T"),
        ("(equalp #2A((1 2)) #2A((1 3)))", "NIL"),
        ("(equalp #2A((1 2)) #2A((1 2 3)))", "NIL"),
        ("(equalp #2A((1.0 2)) #2A((1 2)))", "T"),
        // The full print → read → EQUALP round-trip.
        (
            "(let ((a (make-array '(2 3) :initial-contents '((1 2 3) (4 5 6))))) (equalp a (read-from-string (prin1-to-string a))))",
            "T",
        ),
        // Non-rectangular literals are rejected.
        (
            "(handler-case (read-from-string \"#2A((1 2) (3))\") (error () :err))",
            ":ERR",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// The stdlib printer (PRIN1-TO-STRING / WRITE-TO-STRING / FORMAT ~S) names the
/// non-graphic characters under *print-escape* (#\Newline, not `#\`+literal),
/// matching cli PRIN1 — this was a tier/path divergence (stdlib emitted the raw
/// char).
#[test]
fn character_prin1_names_nongraphic_chars() {
    let cases = [
        ("(prin1-to-string #\\Newline)", "\"#\\\\Newline\""),
        ("(prin1-to-string #\\Space)", "\"#\\\\Space\""),
        ("(prin1-to-string #\\Tab)", "\"#\\\\Tab\""),
        ("(prin1-to-string #\\Return)", "\"#\\\\Return\""),
        ("(prin1-to-string #\\Vt)", "\"#\\\\Vt\""),
        ("(prin1-to-string #\\Next-Line)", "\"#\\\\Next-Line\""),
        (
            "(prin1-to-string #\\No-break_space)",
            "\"#\\\\NO-BREAK_SPACE\"",
        ),
        (
            "(prin1-to-string #\\Ideographic_space)",
            "\"#\\\\IDEOGRAPHIC_SPACE\"",
        ),
        ("(prin1-to-string #\\a)", "\"#\\\\a\""),
        ("(format nil \"~s\" #\\Space)", "\"#\\\\Space\""),
        // princ/~A still emits the literal character.
        ("(format nil \"~a\" #\\a)", "\"a\""),
        // cli PRIN1 and the stdlib printer must agree.
        (
            "(equal (prin1-to-string #\\Newline) (with-output-to-string (s) (prin1 #\\Newline s)))",
            "T",
        ),
        (
            "(equal (prin1-to-string #\\No-break_space) (with-output-to-string (s) (prin1 #\\No-break_space s)))",
            "T",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// TYPEP compound-type combinators: (NOT type) — was unhandled and always
/// returned NIL, breaking (and integer (not (eql 0))) etc. — and (string N) /
/// (simple-string N) length-qualified string types.
#[test]
fn typep_not_and_string_length_compounds() {
    let cases = [
        // (NOT type): the complement combinator.
        ("(typep 3.0 '(not integer))", "T"),
        ("(typep 3 '(not integer))", "NIL"),
        ("(typep 'x '(not integer))", "T"),
        ("(typep 5 '(not (not integer)))", "T"),
        // (NOT ...) composed under AND (a common declared type).
        ("(typep 5 '(and integer (not (eql 0))))", "T"),
        ("(typep 0 '(and integer (not (eql 0))))", "NIL"),
        ("(typep 3.0 '(and number (not integer)))", "T"),
        // length-qualified string types.
        ("(typep \"abc\" '(string 3))", "T"),
        ("(typep \"abc\" '(string 2))", "NIL"),
        ("(typep \"ab\" '(simple-string 2))", "T"),
        ("(typep \"abc\" '(string *))", "T"),
        ("(typep 5 '(string 3))", "NIL"),
        // bliss-ajb3: BASE-STRING / SIMPLE-BASE-STRING match ONLY 8-bit base
        // strings (all BASE-CHARs), not the 32-bit character strings STRING also
        // covers. An all-ASCII literal is a base string; one with a wide char is
        // not, though it is still a STRING.
        ("(typep \"abc\" 'base-string)", "T"),
        ("(typep \"abc\" 'simple-base-string)", "T"),
        ("(typep \"hλ\" 'base-string)", "NIL"),
        ("(typep \"hλ\" 'simple-base-string)", "NIL"),
        ("(typep \"hλ\" 'string)", "T"),
        ("(typep \"abc\" '(base-string 3))", "T"),
        ("(typep \"hλ\" '(base-string 2))", "NIL"),
        // A mutable (constructed) string is a 32-bit CHARACTER string, so it is a
        // STRING but not a BASE-STRING.
        ("(typep (make-string 3) 'string)", "T"),
        ("(typep (make-string 3) 'base-string)", "NIL"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-tjrb: RPLACA/RPLACD (CLHS 14.2) destructively set a cons's car/cdr and
/// return THE CONS (not the value, unlike setf), erroring on a non-cons.
#[test]
fn rplaca_rplacd_mutate_and_return_the_cons() {
    let prog = "\
        (let ((c (cons 1 2))) \
          (unless (eq (rplaca c 9) c) (error \"rplaca-ret\")) \
          (unless (and (eql (car c) 9) (eql (cdr c) 2)) (error \"rplaca-mut\")) \
          (unless (eq (rplacd c 8) c) (error \"rplacd-ret\")) \
          (unless (eql (cdr c) 8) (error \"rplacd-mut\"))) \
        (let ((l (list 1 2 3))) \
          (rplaca (cdr l) :x) \
          (unless (equal l (quote (1 :x 3))) (error \"list-mut\"))) \
        (unless (eq (handler-case (rplaca 5 1) (type-error () :caught)) :caught) \
          (error \"non-cons\")) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-2r5: the core list functions (copy-list, copy-tree, nthcdr, and
/// therefore last/butlast) were non-tail recursive in boot.lisp, so a long flat
/// list — e.g. the ~22000-entry flexi-streams code-page tables — recursed one
/// EgclStack frame per element and overflowed the default 512 KiB stack
/// (needing EGCL_STACK_SIZE=64MB). They are now iterative and run on the
/// default stack.
#[test]
fn long_list_ops_do_not_overflow_the_stack() {
    // Build a 40k-element list at runtime and exercise the rewritten functions;
    // recursive versions overflow well below this length.
    let prog = "\
        (let ((l (loop for i from 0 below 40000 collect i))) \
          (let ((c (copy-list l))) \
            (unless (and (= (length c) 40000) (= (car (last c)) 39999)) (error \"copy-list\"))) \
          (unless (= (car (nthcdr 39999 l)) 39999) (error \"nthcdr\")) \
          (unless (= (car (last l)) 39999) (error \"last\")) \
          (let ((tr (copy-tree l))) \
            (unless (= (car (last tr)) 39999) (error \"copy-tree\"))) \
          (unless (tree-equal l (copy-list l)) (error \"tree-equal\")) \
          (when (tree-equal l (cons 99 (cdr (copy-list l)))) (error \"tree-equal-ne\")) \
          (unless (equal (copy-list (quote (1 2 . 3))) (quote (1 2 . 3))) (error \"dotted\")) \
          (princ :ok))";
    let mut cmd = egcl_bin();
    // Explicitly the DEFAULT stack — no EGCL_STACK_SIZE override.
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "long-list ops overflowed the default stack (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-tjru: a large SIMPLE-VECTOR (body >= the large-object threshold, ~65534
/// elements) has a 16-byte object header, so its value pointer, element
/// accessors, and the GC's field-tracing must all honour the large-object
/// payload offset. Before the fix aref/setf saw it as "not a vector", and the
/// GC scanned its element slots 8 bytes off (reading the size-extension word as
/// the element count). This exercises usability AND that heap references held
/// only by a large vector survive collection.
#[test]
fn large_simple_vector_is_usable_and_gc_safe() {
    // Fire a minor GC roughly every 8000 allocations so several collections
    // land DURING the fill: each must trace the large vector to keep the conses
    // it already holds alive, exactly the path the fix repairs. make-sequence
    // avoids make-array's (apply #'vector (make-list n)) so the build is cheap.
    let prog = "\
        (let* ((n 65540) (v (make-sequence (quote vector) n))) \
          (unless (and (vectorp v) (= (length v) n)) (error \"recog\")) \
          (dotimes (i n) (setf (aref v i) (cons i (- 0 i)))) \
          (dotimes (i n) \
            (let ((c (aref v i))) \
              (unless (and (consp c) (= (car c) i) (= (cdr c) (- 0 i))) \
                (error \"corrupt\")))) \
          (princ :ok))";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "8000")
        .env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "large vector under GC must not abort (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-9tcy: a freshly-made CLOS instance passed INLINE as a ~A/~S format arg
/// must stay rooted across FORMAT's internal allocations. Under a moving GC the
/// unrooted inline instance was collected mid-format and printed as "#" instead
/// of "#<ZZZ …>". Loop many format calls under GC stress so a collection
/// reliably lands during a format window if the rooting ever regresses.
#[test]
fn format_of_inline_fresh_instance_is_gc_safe() {
    let prog = "\
        (defclass zzz () ((a :initform 5))) \
        (dotimes (i 50) \
          (let ((s (format nil \"~A|~S\" (make-instance (quote zzz)) (make-instance (quote zzz))))) \
            (unless (and (search \"#<ZZZ\" s) (search \"|\" s)) \
              (error \"instance collected mid-format at ~D: ~A\" i s)))) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "100")
        .env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "inline instance format under GC must not abort/collect (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-biol: a FLET function's body must stay GC-scanned across calls. When a
/// FLET function runs, eval_named_call_ex swaps env.funs to the parent snapshot
/// (a FLET body sees neither its siblings nor itself); the swapped-out funs hold
/// the FLET FunDef body cons trees, so a minor GC during the body freed them and
/// a LATER call read a poisoned body -> "null guard SIGSEGV". COUNT (boot.lisp)
/// is exactly this shape — (flet ((matchp ...)) (loop ... count (matchp ...))) —
/// and crashed under GC stress before the fix. Exercise COUNT and a bare FLET
/// under stress; both must survive and stay correct.
#[test]
fn flet_body_survives_gc_across_calls() {
    let prog = "\
        (dotimes (i 300) \
          (unless (= 3 (count #\\a \"banana\")) (error \"count-str\")) \
          (unless (= 2 (count 1 (list 1 2 1))) (error \"count-list\")) \
          (let ((tf (function eql)) (item 5)) \
            (flet ((m (e) (funcall tf item e))) \
              (unless (and (m 5) (not (m 6))) (error \"flet\"))))) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "100")
        .env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "FLET body under GC must not crash (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-3ltr: eval_flet builds each local function's FunDef (with a freshly
/// block-wrapped body cons tree) into child_env.funs, but only rooted child_env
/// AFTER the whole defs loop. Building a LATER function's body (arena_cons) can
/// fire a minor GC that frees an EARLIER, already-inserted body — a subsequent
/// call then evaluated a poisoned body / walked a corrupted frame chain and
/// crashed. SUBST/SUBLIS are (labels ((match ...) (rec ...)) ...); they crashed
/// under GC stress. Exercise SUBST and a bare 2-function LABELS under stress.
#[test]
fn multi_function_labels_bodies_survive_gc() {
    let prog = "\
        (dotimes (i 300) \
          (unless (equal (subst (quote x) (quote b) (quote (a b (c b)))) (quote (a x (c x)))) \
            (error \"subst\")) \
          (let ((new 9) (old 2)) \
            (labels ((hit (e) (eql old e)) \
                     (walk (l) (cond ((null l) nil) \
                                     ((hit (car l)) (cons new (walk (cdr l)))) \
                                     (t (cons (car l) (walk (cdr l))))))) \
              (unless (equal (walk (list 1 2 3 2)) (list 1 9 3 9)) (error \"labels\"))))) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "100")
        .env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "multi-function LABELS/SUBST under GC must not crash (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-961p: FIND-SYMBOL is case-SENSITIVE (CLHS 11.1.1.2.1 / the dictionary
/// entry) — it matches the name string verbatim, doing no readtable case
/// folding (the READER upcases; FIND-SYMBOL does not). (find-symbol "car") must
/// be NIL even though CAR exists; (find-symbol "CAR") finds it. Covers both the
/// 1-arg (*package*) and 2-arg (explicit package) paths.
#[test]
fn find_symbol_is_case_sensitive() {
    let prog = "(prin1 (list
        (nth-value 1 (find-symbol \"car\"))
        (nth-value 1 (find-symbol \"CAR\"))
        (nth-value 1 (find-symbol \"car\" :cl))
        (nth-value 1 (find-symbol \"CAR\" :cl))
        (nth-value 1 (find-symbol \"car\" :keyword))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // lowercase -> NIL (not found); uppercase CAR -> INHERITED (via *package*) /
    // EXTERNAL (in :cl); lowercase keyword -> NIL.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("(NIL :INHERITED NIL :EXTERNAL NIL)"),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-rsur: CHAR-NAME/NAME-CHAR must round-trip. Char 65533 (U+FFFD) was
/// wrongly named "Rubout" (which is char 127), so (name-char (char-name #\U+FFFD))
/// gave 127, not 65533. U+FFFD has no CL standard name -> char-name returns nil.
#[test]
fn char_name_round_trips() {
    let prog = "(let ((bad nil))
        (dolist (c '(32 10 9 13 12 8 127 0 7 27 65533 65 200 1000))
          (let* ((ch (code-char c)) (nm (char-name ch)))
            (when (and nm (not (eql (name-char nm) ch))) (push (list c nm) bad))))
        (prin1 (list (char-name (code-char 65533)) (char-name (code-char 127)) bad
                     (char-name #\\Vt) (char-name #\\Next-Line)
                     (char-name #\\No-break_space)
                     (char-name #\\Ideographic_space)
                     (char-code (name-char \"Vt\"))
                     (char-code (name-char \"Next-Line\"))
                     (char-code (name-char \"No-break_space\"))
                     (char-code (name-char \"Ideographic_space\")))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // U+FFFD unnamed, 127 is Rubout, and the implementation names used by
    // cl-str round-trip with SBCL-compatible spellings and code points.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains(
            "(NIL \"Rubout\" NIL \"Vt\" \"Next-Line\" \"NO-BREAK_SPACE\" \"IDEOGRAPHIC_SPACE\" 11 133 160 12288)"
        ),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-gxe9: `(macro-function 'NAME)` must return an expander that expands as
/// NAME regardless of the operator of the form it is applied to, so it can be
/// installed elsewhere via (setf (macro-function OTHER) …) (CLHS 3.1.2.1.2.2;
/// ansi macro-function.15). The old synthesized expander re-expanded by the
/// argument form's own operator, so installed on a gensym it never expanded and
/// the evaluator looped on the unchanged form -> SIGSEGV. Also, (macroexpand
/// form nil) must accept NIL as the null lexical environment (bliss-pmox-adjacent).
#[test]
fn macro_function_is_reinstallable_on_another_symbol() {
    let prog = "(let ((s (gensym)))
        (setf (macro-function s) (macro-function 'pop))
        (prin1 (eval `(let ((x '(1 2 3))) (list (,s x) x)))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("(1 (2 3))"),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );
    // (macroexpand form nil): NIL is the null lexical environment, not an error.
    let out2 = egcl_bin()
        .args(["--eval", "(prin1 (macroexpand '(pop y) nil))"])
        .output()
        .expect("run egcl");
    assert_eq!(
        out2.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out2.stdout).contains("PROG1"),
        "macroexpand with nil env should expand POP: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// bliss-pmox: top-level and nested EVAL-WHEN process in EXECUTE mode when a
/// form is evaluated or a SOURCE file is loaded (CLHS 3.2.3.1): the body runs
/// iff :EXECUTE is present. Firing on :LOAD-TOPLEVEL here was wrong — it ran
/// load-only situations that have no :execute (ansi eval-when.1). :LOAD-TOPLEVEL
/// takes effect only when a COMPILE-FILE'd fasl is loaded, which COMPILE-FILE
/// resolves separately. (The compile/load-compiled columns are exercised by the
/// ansi eval-when.1 test and by asdf+babel compiling under the same fix.)
#[test]
fn eval_when_execute_mode_on_eval_and_source() {
    let prog = "(let ((r nil)) \
        (eval-when (:execute) (push :e r)) \
        (eval-when (:load-toplevel) (push :lt r)) \
        (eval-when (:compile-toplevel) (push :ct r)) \
        (eval-when (:load-toplevel :execute) (push :ltx r)) \
        (eval-when (:compile-toplevel :load-toplevel) (push :ctlt r)) \
        (let () (eval-when (:load-toplevel) (push :nested-lt r))) \
        (let () (eval-when (:execute) (push :nested-e r))) \
        (prin1 (reverse r)))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Only situations containing :EXECUTE fire; :load-toplevel-/:compile-toplevel-
    // only ones (top-level and nested) do not.
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("(:E :LTX :NESTED-E)"),
        "got: {}",
        String::from_utf8_lossy(&out.stdout)
    );

    // CLHS 3.2.3.1 retains COMPILE, LOAD, and EVAL as deprecated aliases for
    // :COMPILE-TOPLEVEL, :LOAD-TOPLEVEL, and :EXECUTE (bliss-wne9.1).
    let short = egcl_bin()
        .args([
            "--eval",
            "(progn (eval-when (compile load eval) (defun eval-when-short () 42))
                    (princ (eval-when-short)))",
        ])
        .output()
        .expect("run egcl with short EVAL-WHEN situations");
    assert_eq!(
        short.status.code(),
        Some(0),
        "short EVAL-WHEN situations failed: {}",
        String::from_utf8_lossy(&short.stderr)
    );
    assert!(
        String::from_utf8_lossy(&short.stdout).contains("42"),
        "short EVAL-WHEN situations did not execute: {}",
        String::from_utf8_lossy(&short.stdout)
    );

    for (invalid, printed_name) in [
        (":not-a-situation", "NOT-A-SITUATION"),
        ("#:eval", "EVAL"),
        (":eval", "EVAL"),
        ("nil", "NIL"),
        ("t", "T"),
    ] {
        let program = format!("(eval-when (:execute {invalid}) 1)");
        let invalid_result = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("run egcl with invalid EVAL-WHEN situation");
        assert_ne!(
            invalid_result.status.code(),
            Some(0),
            "{invalid} is not an ANSI EVAL-WHEN situation and must signal"
        );
        let stderr = String::from_utf8_lossy(&invalid_result.stderr);
        assert!(
            stderr.contains("EVAL-WHEN") && stderr.contains(printed_name),
            "invalid-situation error must identify EVAL-WHEN and {invalid}: {stderr}"
        );
    }
}

/// FBOUNDP and FMAKUNBOUND consulted only NAME-keyed tables, and an uninterned
/// symbol is not in the name registry. A function installed with
/// `(setf (symbol-function (gensym)) …)` was therefore CALLABLE but reported
/// not fbound, and could not be unbound at all (ansi FMAKUNBOUND.1/2/4).
/// Matches SBCL.
#[test]
fn fboundp_and_fmakunbound_see_uninterned_symbols() {
    let cases = [
        // The gap: callable, but FBOUNDP said no.
        (
            "(let ((g (gensym))) (setf (symbol-function g) #'car) \
               (list (and (fboundp g) t) (funcall g '(1 2))))",
            "(T 1)",
        ),
        (
            "(let ((g (make-symbol \"U1\"))) (setf (symbol-function g) #'car) \
            (and (fboundp g) t))",
            "T",
        ),
        // ...including a closure rather than a builtin.
        (
            "(let ((g (gensym))) (setf (symbol-function g) (lambda (x) (car x))) \
               (list (and (fboundp g) t) (funcall g '(7 8))))",
            "(T 7)",
        ),
        // FMAKUNBOUND must actually unbind it, and return the symbol.
        (
            "(let ((g (gensym))) (setf (symbol-function g) #'car) \
               (list (eq (fmakunbound g) g) (fboundp g)))",
            "(T NIL)",
        ),
        // The whole ansi FMAKUNBOUND.1 sequence.
        (
            "(let ((g (gensym))) \
               (list (fboundp g) \
                     (progn (setf (symbol-function g) #'car) (and (fboundp g) t)) \
                     (eq (fmakunbound g) g) \
                     (fboundp g)))",
            "(NIL T T NIL)",
        ),
        // Interned symbols and DEFUN are unaffected.
        (
            "(progn (setf (symbol-function 'fb-i) #'car) (and (fboundp 'fb-i) t))",
            "T",
        ),
        (
            "(progn (defun fb-d () 1) (list (and (fboundp 'fb-d) t) (fb-d)))",
            "(T 1)",
        ),
        // A name that was never bound is still not fbound.
        ("(fboundp (gensym))", "NIL"),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "fboundp case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong FBOUNDP/FMAKUNBOUND result for: {expr}"
        );
    }
}

/// COMPILED-FUNCTION-P and FUNCTION-LAMBDA-EXPRESSION were entirely
/// unimplemented — 12 ansi failures between them — even though TYPEP already
/// decided the COMPILED-FUNCTION type. Every expectation matches SBCL.
#[test]
fn compiled_function_p_and_function_lambda_expression() {
    let cases = [
        // COMPILED-FUNCTION-P agrees with TYPEP, which is what
        // COMPILED-FUNCTION-P.1 checks across the whole test universe.
        ("(compiled-function-p #'car)", "T"),
        ("(compiled-function-p '(lambda (x y) (cons y x)))", "NIL"),
        (
            "(and (compiled-function-p (compile nil '(lambda (y x) (cons x y)))) t)",
            "T",
        ),
        // Its argument is evaluated exactly once.
        (
            "(let ((i 0)) (list (compiled-function-p (progn (incf i) '(lambda () nil))) i))",
            "(NIL 1)",
        ),
        // Exactly one argument; anything else is a PROGRAM-ERROR.
        (
            "(handler-case (compiled-function-p) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (compiled-function-p nil nil) (program-error () :pe))",
            ":PE",
        ),
        // The type is in the lattice, so SUBTYPEP is certain rather than unsure.
        (
            "(multiple-value-list (subtypep 'compiled-function 'function))",
            "(T T)",
        ),
        // FUNCTION-LAMBDA-EXPRESSION returns exactly THREE values...
        (
            "(length (multiple-value-list (function-lambda-expression #'cons)))",
            "3",
        ),
        // ...and reports a closure as one.
        (
            "(let ((x nil)) (flet ((%f () x)) \
               (let ((r (multiple-value-list (function-lambda-expression #'%f)))) \
                 (list (length r) (and (second r) t)))))",
            "(3 T)",
        ),
        (
            "(let ((i 0)) (function-lambda-expression (progn (incf i) #'cons)) i)",
            "1",
        ),
        (
            "(handler-case (function-lambda-expression) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (function-lambda-expression #'cons nil) (program-error () :pe))",
            ":PE",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "function-predicate case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong result for: {expr}"
        );
    }
}

/// A macro parameter the expander body declares SPECIAL must be bound
/// DYNAMICALLY, not lexically (CLHS 3.3.4): the expander may call a closure that
/// reads it, and a lexical binding leaves that closure seeing the outer value
/// (ansi MACROLET.44/45). Matches SBCL.
#[test]
fn special_declared_macro_parameters_bind_dynamically() {
    let cases = [
        // A global macro whose parameter is declared special: the closure in *FG*
        // must observe the value the expansion bound, not the outer NIL.
        (
            "(progn (defvar *xg* nil) (defvar *fg* nil) \
               (defmacro %gm (*xg*) (declare (special *fg* *xg*)) (funcall *fg*)) \
               (let ((*xg* nil)) \
                 (declare (special *xg*)) \
                 (let ((*fg* (lambda () *xg*))) \
                   (declare (special *fg*)) \
                   (eval '(%gm t)))))",
            "T",
        ),
        // The MACROLET form ansi actually uses.
        (
            "(progn (defvar *x1* nil) (defvar *f1* nil) \
               (let ((*x1* nil)) \
                 (declare (special *x1*)) \
                 (let ((*f1* (lambda () *x1*))) \
                   (declare (special *f1*)) \
                   (eval `(macrolet ((%m (*x1*) \
                                       (declare (special *f1* *x1*)) \
                                       (funcall *f1*))) \
                            (%m t))))))",
            "T",
        ),
        // ...and its destructuring variant.
        (
            "(progn (defvar *x2* nil) (defvar *f2* nil) \
               (let ((*x2* nil)) \
                 (declare (special *x2*)) \
                 (let ((*f2* (lambda () *x2*))) \
                   (declare (special *f2*)) \
                   (eval `(macrolet ((%m ((*x2*)) \
                                       (declare (special *f2* *x2*)) \
                                       (funcall *f2*))) \
                            (%m (t)))))))",
            "T",
        ),
        // A parameter that needs no DECLARE because DEFVAR already made the name
        // GLOBALLY special: binding it is still a dynamic binding, so the
        // expander's (symbol-value '*x3*) sees the argument (CLHS 3.1.2.1.1;
        // bliss-pw4d). This row read ":OUTER" until the lambda-list binders
        // learned that rule; SBCL prints :INNER for exactly this program, so the
        // old expectation contradicted the comment it was written under.
        (
            "(progn (defvar *x3* :outer) \
               (defmacro %gm3 (*x3*) (declare (ignorable *x3*)) \
                 (list 'quote (symbol-value '*x3*))) \
               (%gm3 :inner))",
            ":INNER",
        ),
        // An ordinary macro is unaffected.
        ("(progn (defmacro %gm4 (a b) `(+ ,a ,b)) (%gm4 2 3))", "5"),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "special macro parameter case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "special-declared macro parameter was not bound dynamically: {expr}"
        );
    }
}

/// CLHS 3.4.4: the &ENVIRONMENT parameter is bound BEFORE every other
/// parameter, wherever it appears in the lambda list, so an &OPTIONAL default
/// or an &AUX initform may use it. Binding it when the scan reached it left the
/// variable unbound in those forms (bliss-vjfz / ansi MACROLET.38). Same class
/// as the &AUX ordering fix in 526745d. Every expectation matches SBCL.
#[test]
fn macro_environment_parameter_is_bound_first() {
    let cases = [
        // The failing shape: &environment declared AFTER the &optional that uses it.
        (
            "(macrolet ((foo () 9)) \
               (macrolet ((%g (&optional (x (macroexpand '(foo) e)) &environment e) x)) \
                 (%g)))",
            "9",
        ),
        // ...and in an &aux initform.
        (
            "(macrolet ((foo () 10)) \
               (macrolet ((%g (&aux (x (macroexpand '(foo) e)) &environment e) x)) \
                 (%g)))",
            "10",
        ),
        // The ordinary trailing position keeps working.
        (
            "(macrolet ((foo () 7)) \
               (macrolet ((%g (&environment e) (macroexpand '(foo) e))) (%g)))",
            "7",
        ),
        // A lambda list with no &environment is unaffected.
        ("(macrolet ((%g (a) a)) (%g 42))", "42"),
        // A macro lambda list with ordinary parameters still binds them.
        (
            "(macrolet ((%g (a b &environment e) (declare (ignore e)) (+ a b))) (%g 2 3))",
            "5",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "&environment case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "&environment was not bound first for: {expr}"
        );
    }
}

/// A macro bound by MACROLET is a valid SETF PLACE. The direct SETF path had no
/// branch for a lexically bound macro place, so it fell through to "unsupported
/// place" — while the SAME place worked through an updating macro, because
/// GET-SETF-EXPANSION does expand it (bliss-vjfz). It only showed up in a
/// COMPILED context; at toplevel it happened to work.
#[test]
fn macrolet_defined_places_work_with_setf() {
    let cases = [
        // Inside a DEFUN — the context that failed.
        (
            "(progn (defun mlp1 () (macrolet ((%m (x) `(car ,x))) \
                                     (let ((y (list 1 2))) (setf (%m y) 6) y))) \
               (mlp1))",
            "(6 2)",
        ),
        // Through EVAL — likewise.
        (
            "(eval '(macrolet ((%m (x) `(car ,x))) \
                      (let ((y (list 1 2))) (setf (%m y) 6) y)))",
            "(6 2)",
        ),
        // An inner MACROLET shadows an outer one (ansi MACROLET.3's shape).
        (
            "(progn (defun mlp2 () \
                      (macrolet ((%m (w) `(cadr ,w))) \
                        (macrolet ((%m (w) `(car ,w))) \
                          (let ((x (list 1 2))) (setf (%m x) 7) x)))) \
               (mlp2))",
            "(7 2)",
        ),
        // The place's subforms are evaluated exactly ONCE.
        (
            "(progn (defvar *mlp-n* 0) \
               (defun mlp3 () (macrolet ((%m (x) `(car ,x))) \
                                (let ((y (list 1 2))) \
                                  (flet ((g () (incf *mlp-n*) y)) (setf (%m (g)) 9)) \
                                  (list y *mlp-n*)))) \
               (mlp3))",
            "((9 2) 1)",
        ),
        // Updating macros through a MACROLET place keep working.
        (
            "(progn (defun mlp4 () (macrolet ((%m (x) `(car ,x))) \
                                     (let ((y (list 1 2))) (incf (%m y) 5) y))) \
               (mlp4))",
            "(6 2)",
        ),
        // A GLOBAL macro place is deliberately left on its existing path.
        (
            "(progn (defmacro gmp (x) `(car ,x)) \
               (defun mlp5 () (let ((y (list 1 2))) (setf (gmp y) 6) y)) \
               (mlp5))",
            "(6 2)",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "macrolet place case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong MACROLET place behaviour for: {expr}"
        );
    }
}

/// CLHS 5.1.1.1: a place's subforms are evaluated left to right, then the
/// new-value form LAST. For a place handled by a USER setf expander the new
/// value was evaluated FIRST (bliss-gdom), which the ansi DEFSETF.4C test
/// detects by counting evaluations. Builtin places were already correct.
#[test]
fn setf_evaluates_user_place_subforms_before_the_value() {
    // i counts every evaluation; j and k record when each subform ran. Correct
    // order leaves j=1, k=2 and i=3 — value last.
    let setup = "(progn \
        (defun dse (n s) (nth n s)) \
        (define-setf-expander dse (n s &environment e) \
          (declare (ignore e)) \
          (let ((tn (gensym)) (ts (gensym)) (v (gensym))) \
            (values (list tn ts) (list n s) (list v) \
                    `(progn (setf (nth ,tn ,ts) ,v) ,v) `(nth ,tn ,ts)))) \
        (defun sfa (n s) (nth n s)) \
        (defun set-sfa (n s v) (setf (nth n s) v) v) \
        (defsetf sfa set-sfa) \
        (defun lfa (n s) (nth n s)) \
        (defsetf lfa (n s) (v) `(progn (setf (nth ,n ,s) ,v) ,v)))";
    let order = |place: &str| {
        format!(
            "(let ((x (list 1 2 3)) (i 0) (j nil) (k nil)) \
               (setf ({place} (progn (setf j (incf i)) 1) (progn (setf k (incf i)) x)) \
                     (progn (incf i) 'a)) \
               (list i j k))"
        )
    };
    let cases = [
        (order("dse"), "(3 1 2)"),
        (order("sfa"), "(3 1 2)"),
        (order("lfa"), "(3 1 2)"),
        // A builtin place was already correct and must stay so.
        (order("nth"), "(3 1 2)"),
        // Deferring the value must not change WHAT is stored, or the result.
        (
            "(let ((x (list 1 2 3))) (list (setf (lfa 1 x) 'z) x))".to_string(),
            "(Z (1 Z 3))",
        ),
        // ...including through an updating macro, which must still read and
        // write the place exactly once.
        (
            "(let ((x (list 1 5 3))) (incf (lfa 1 x) 10) x)".to_string(),
            "(1 15 3)",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "setf order case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong SETF evaluation order for: {expr}"
        );
    }
}

/// Long-form DEFSETF `(defsetf access-fn (arg…) (store…) body…)` was accepted
/// and DISCARDED, so `(setf (access-fn …) …)` fell through to the
/// `(setf access-fn)` writer path and signalled a PROGRAM-ERROR with no such
/// writer (bliss-pbp8). Unlike DEFINE-SETF-EXPANDER the body yields only the
/// STORE FORM, so the temporaries, value forms, store variables and access form
/// are synthesised around it (CLHS 5.5.5). Matches SBCL.
#[test]
fn defsetf_long_form_defines_a_place() {
    let setup = "(progn \
        (defun cellv (x) (second x)) \
        (defsetf cellv (x) (v) `(progn (setf (second ,x) ,v) ,v)) \
        (defun nth2 (i x) (nth i x)) \
        (defsetf nth2 (i x) (v) `(progn (setf (nth ,i ,x) ,v) ,v)) \
        (defun shv (x) (first x)) \
        (defun set-shv (x v) (setf (first x) v) v) \
        (defsetf shv set-shv) \
        (defvar *n* 0))";
    let cases = [
        // The long form now defines a usable place...
        ("(let ((c (list 1 2))) (setf (cellv c) 7) c)", "(1 7)"),
        // ...and SETF returns the stored value.
        ("(let ((c (list 1 2))) (setf (cellv c) 42))", "42"),
        // Multiple place arguments are bound in order.
        ("(let ((c (list 1 2 3))) (setf (nth2 1 c) 9) c)", "(1 9 3)"),
        // An updating macro reads and writes through it, evaluating the place's
        // subforms EXACTLY ONCE — the whole point of the temporaries.
        (
            "(progn (setf *n* 0) \
               (let ((c (list 1 5))) \
                 (flet ((get-c () (incf *n*) c)) (incf (cellv (get-c)) 10)) \
                 (list c *n*)))",
            "((1 15) 1)",
        ),
        ("(let ((c (list 1 nil))) (push 5 (cellv c)) c)", "(1 (5))"),
        // The SHORT form keeps working.
        ("(let ((c (list 1 2))) (setf (shv c) 5) c)", "(5 2)"),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "defsetf case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong long-form DEFSETF behaviour for: {expr}"
        );
    }
}

/// FLET/LABELS may bind a `(setf name)` WRITER, and it must shadow any global
/// writer of the same name. Clauses were keyed with `sym_name`, which cannot
/// produce the canonical "(SETF PLACE)" key the store path looks up, so a local
/// writer was invisible; and the store path probed the GLOBAL mangled writer
/// symbol before the lexical binding (bliss-gl15). Matches SBCL.
#[test]
fn local_setf_writers_work_and_shadow_globals() {
    let globals = "(progn (defun gcell (x) (first x)) \
                          (defun (setf gcell) (v x) (setf (first x) (list :global v)) v))";
    let cases = [
        // A writer bound by FLET is found at all.
        (
            "(let ((c (list 1 2))) \
               (flet ((ch (x) (first x)) ((setf ch) (v x) (setf (first x) v) v)) \
                 (setf (ch c) 99)) \
               c)",
            "(99 2)",
        ),
        // ...by LABELS too.
        (
            "(let ((c (list 1 2))) \
               (labels ((ch2 (x) (first x)) ((setf ch2) (v x) (setf (first x) v) v)) \
                 (setf (ch2 c) 7)) \
               c)",
            "(7 2)",
        ),
        // An updating macro reads through the local reader and writes through
        // the local writer.
        (
            "(let ((c (list 1 2))) \
               (flet ((ch3 (x) (first x)) ((setf ch3) (v x) (setf (first x) v) v)) \
                 (incf (ch3 c) 10)) \
               c)",
            "(11 2)",
        ),
        // A local writer SHADOWS a global one of the same name...
        (
            "(let ((c (list 1 2))) \
               (flet ((gcell (x) (first x)) \
                      ((setf gcell) (v x) (setf (first x) (list :local v)) v)) \
                 (setf (gcell c) 5)) \
               c)",
            "((:LOCAL 5) 2)",
        ),
        // ...and the global is intact again outside the FLET.
        (
            "(let ((c (list 1 2))) (setf (gcell c) 6) c)",
            "((:GLOBAL 6) 2)",
        ),
        // A reader-only FLET must NOT be treated as defining a writer.
        (
            "(handler-case (let ((c (list 1 2))) \
                             (flet ((ronly (x) (first x))) (setf (ronly c) 1) c)) \
               (error () :errors))",
            ":ERRORS",
        ),
        // #'(setf localname) is a callable value.
        (
            "(let ((c (list 1 2))) \
               (flet (((setf wq) (v x) (setf (first x) v) v)) \
                 (funcall #'(setf wq) 77 c)) \
               c)",
            "(77 2)",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {globals} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "local setf writer case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong local setf writer behaviour for: {expr}"
        );
    }
}

/// CLHS 9.1.4.2: restart names are SYMBOLS, compared with EQ. Both sides were
/// reduced to their bare names before comparing, throwing the package away, so
/// `(find-restart 'cl:continue)` matched a restart named
/// `other-package::continue` — a genuinely different symbol (bliss-iom5).
#[test]
fn restart_names_compare_by_symbol_identity() {
    // RI uses nothing, so ri::continue is its OWN symbol, not CL's.
    let setup = "(defpackage :ri (:use))";
    let cases = [
        // The premise: these really are different symbols.
        ("(not (eq 'ri::continue 'cl:continue))", "T"),
        // A restart named ri::continue must NOT answer to cl:continue.
        (
            "(restart-case (and (find-restart 'cl:continue) t) (ri::continue () :x))",
            "NIL",
        ),
        // ...but must answer to its own name, and report that name.
        (
            "(restart-case (and (find-restart 'ri::continue) t) (ri::continue () :x))",
            "T",
        ),
        (
            "(restart-case (eq (restart-name (find-restart 'ri::continue)) 'ri::continue) \
               (ri::continue () :x))",
            "T",
        ),
        // Invoking by the CL name must not fire the differently-named restart.
        (
            "(restart-case (handler-case (invoke-restart 'cl:continue) (error () :not-found)) \
               (ri::continue () :wrongly-fired))",
            ":NOT-FOUND",
        ),
        // A symbol INHERITED from CL is the SAME symbol, so it still matches.
        // Resolved with INTERN at run time: writing ri2::continue literally here
        // would be read before the DEFPACKAGE in the same form had run, creating
        // a fresh symbol instead of finding the inherited one.
        (
            "(progn (defpackage :ri2 (:use :cl)) \
               (restart-case (and (find-restart (intern \"CONTINUE\" :ri2)) t) \
                 (continue () :x)))",
            "T",
        ),
        // The standard restarts keep working.
        (
            "(handler-bind ((error (lambda (c) (declare (ignore c)) (continue)))) \
               (cerror \"go\" \"boom\") :continued)",
            ":CONTINUED",
        ),
        ("(restart-case (abort) (abort () :aborted))", ":ABORTED"),
        (
            "(handler-bind ((warning (lambda (c) (muffle-warning c)))) (warn \"w\") :muffled)",
            ":MUFFLED",
        ),
        (
            "(restart-case (invoke-restart 'my-r) (my-r () :fired))",
            ":FIRED",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "restart identity case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "restart name matched by bare name instead of identity: {expr}"
        );
    }
}

/// Builtin condition classes register LAZILY — nothing materialises
/// PACKAGE-ERROR until something signals one — so SUBTYPEP saw no class and
/// answered "cannot determine" for relationships the standard defines, while
/// SIMPLE-ERROR (registered during bootstrap) answered correctly (bliss-4bv3).
/// Reading the builtin table directly makes the answer independent of whether
/// anything has signalled. Every expectation matches SBCL.
#[test]
fn subtypep_knows_the_builtin_condition_lattice() {
    let cases = [
        // Positive relations, none of which had been signalled in this image.
        ("(subtypep 'package-error 'error)", "(T T)"),
        ("(subtypep 'simple-type-error 'type-error)", "(T T)"),
        ("(subtypep 'stream-error 'error)", "(T T)"),
        ("(subtypep 'end-of-file 'stream-error)", "(T T)"),
        ("(subtypep 'reader-error 'parse-error)", "(T T)"),
        ("(subtypep 'package-error 'condition)", "(T T)"),
        ("(subtypep 'package-error 't)", "(T T)"),
        // Negative relations must be CERTAIN — (NIL T), not (NIL NIL). A fix
        // that only ever said "yes" would pass the positives and break these.
        ("(subtypep 'error 'type-error)", "(NIL T)"),
        ("(subtypep 'package-error 'stream-error)", "(NIL T)"),
        // The two-parent class added for bliss-wne9.3 resolves through both.
        ("(subtypep 'simple-package-error 'package-error)", "(T T)"),
        (
            "(subtypep 'simple-package-error 'simple-condition)",
            "(T T)",
        ),
        // A user-defined condition still resolves through the CLOS path...
        (
            "(progn (define-condition sub-my-err (package-error) ()) \
               (subtypep 'sub-my-err 'error))",
            "(T T)",
        ),
        // ...and handler dispatch is unaffected.
        (
            "(progn (define-condition sub-my-err2 (package-error) ()) \
               (handler-case (error 'sub-my-err2) (package-error () :caught)))",
            ":CAUGHT",
        ),
        // A non-condition type is untouched.
        ("(subtypep 'fixnum 'integer)", "(T T)"),
    ];
    for (expr, expected) in cases {
        let wrapped = if expected.starts_with('(') {
            format!("(multiple-value-list {expr})")
        } else {
            expr.to_string()
        };
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S\" {wrapped})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "subtypep case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong subtypep answer for: {expr}"
        );
    }
}

/// A package marker must be followed by a symbol name (CLHS 2.3.4). egcl read
/// `PKG::` as a symbol with the empty name instead of signalling (bliss-gw2a).
/// The subtlety is that `PKG::||` IS a valid empty-named symbol, and so is a
/// bare `||`, so only an UNESCAPED empty remainder may be rejected.
#[test]
fn a_trailing_package_marker_is_a_reader_error() {
    let setup = "(progn (make-package \"GW\" :use (list \"CL\")) \
                        (export (intern \"EXT\" \"GW\") \"GW\"))";
    let cases = [
        // Malformed: a marker with nothing after it.
        ("(read-from-string \"GW::\")", ":SIGNALLED"),
        ("(read-from-string \"GW:\")", ":SIGNALLED"),
        // Valid: bars make an empty name legitimate.
        ("(symbol-name (read-from-string \"GW::||\"))", "\"\""),
        ("(symbol-name (read-from-string \"||\"))", "\"\""),
        // Valid: ordinary qualified symbols, internal and external.
        ("(symbol-name (read-from-string \"GW::X\"))", "\"X\""),
        ("(symbol-name (read-from-string \"GW:EXT\"))", "\"EXT\""),
    ];
    for (expr, expected) in cases {
        let program = format!(
            "(progn {setup} \
               (cl:format t \"~S\" (handler-case {expr} (error () :signalled))))"
        );
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "trailing-marker case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "wrong reading for: {expr}"
        );
    }
}

/// A HANDLER-BIND handler spec is a FORM that must be EVALUATED to produce the
/// handler function (CLHS 9.1.4.1). The bytecode path stored it UNEVALUATED
/// when the enclosing function had no boxed locals, which only looked correct
/// because applying a raw `(LAMBDA …)` form happens to succeed — every other
/// spelling was applied as a literal list and died with "Cons is not of type
/// FUNCTION" the moment the handler transferred control (bliss-ddpl).
#[test]
fn handler_bind_evaluates_its_handler_spec() {
    let setup = "(defun ddpl-h (c) (declare (ignore c)) (throw 'done :thrown))";
    // Every spelling of "the function DDPL-H", plus an inline lambda.
    for spec in [
        "#'ddpl-h",
        "'ddpl-h",
        "(symbol-function 'ddpl-h)",
        "(lambda (c) (ddpl-h c))",
    ] {
        let program = format!(
            "(progn {setup} \
               (cl:format t \"~S\" (catch 'done (handler-bind ((error {spec})) (error \"x\")))))"
        );
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "handler spec {spec} failed:\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            ":THROWN",
            "handler spec {spec} did not run the handler"
        );
    }

    // The shape ansi-test actually uses (has-non-abort-restart): a NAMED handler
    // that throws out of the handler. This gates MAKE-PACKAGE.ERROR.1-4,
    // DELETE-PACKAGE.6 and IMPORT.ERROR.4/5.
    let program = "(progn \
        (defun ddpl-probe (c) \
          (throw 'handled (if (some (lambda (r) (not (eq (restart-name r) 'abort))) \
                                    (compute-restarts c)) \
                              'success 'fail))) \
        (make-package \"DDPL-P\" :use '(\"CL\")) \
        (cl:format t \"~S\" \
          (catch 'handled \
            (handler-bind ((error #'ddpl-probe)) (make-package \"DDPL-P\")))))";
    let output = egcl_bin()
        .args(["--eval", program])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "ansi handler shape failed:\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim(),
        "SUCCESS",
        "the ansi handle-non-abort-restart shape did not work"
    );
}

/// Every `EgclError::PackageError` carries a description of what failed, but
/// the condition builder discarded it (`PackageError(_)`), so ASDF loads died
/// with a bare, unactionable "Package error." — no package, no operation, no
/// symbol (bliss-wne9.3). The message now rides on a SIMPLE-PACKAGE-ERROR,
/// which is also what SBCL signals.
#[test]
fn package_errors_report_what_failed() {
    let cases = [
        // The report names the symbol AND the package.
        (
            "(handler-case (export 'foo \"NO-SUCH-PKG-ZZ\") \
               (package-error (c) (princ-to-string c)))",
            "\"symbol FOO is not accessible in package NO-SUCH-PKG-ZZ\"",
        ),
        // It is no longer the bare string.
        (
            "(handler-case (export 'foo \"NO-SUCH-PKG-ZZ\") \
               (package-error (c) (string= (princ-to-string c) \"Package error.\")))",
            "NIL",
        ),
        // A SIMPLE-PACKAGE-ERROR is still a PACKAGE-ERROR...
        (
            "(handler-case (export 'foo \"NO-SUCH-PKG-ZZ\") (package-error () :pkg))",
            ":PKG",
        ),
        // ...and still an ERROR.
        (
            "(handler-case (export 'foo \"NO-SUCH-PKG-ZZ\") (error () :err))",
            ":ERR",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "package-error report case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "package error did not report what failed: {expr}"
        );
    }
}

/// CLHS makes several package operations CORRECTABLE. egcl signalled the right
/// condition type but established no restarts, so `(compute-restarts c)` never
/// grew inside a handler — which is exactly what ansi MAKE-PACKAGE.ERROR.1-4,
/// DELETE-PACKAGE.6 and IMPORT.ERROR.4/5 check, via a helper that looks for any
/// restart not named ABORT (bliss-069a). Every expectation matches SBCL.
#[test]
fn package_errors_are_correctable() {
    // The ansi helper's shape: succeed iff some restart is not ABORT.
    let helper = "(defmacro non-abort-restart-p (&body body) \
        `(catch 'handled \
           (handler-bind ((error (lambda (c) \
                                   (throw 'handled \
                                     (if (some (lambda (r) \
                                                 (not (eq (restart-name r) 'abort))) \
                                               (compute-restarts c)) \
                                         'success 'fail))))) \
             ,@body)))";
    let cases = [
        // A name clash offers a non-ABORT restart...
        (
            "(progn (make-package \"PE-A\" :use '(\"CL\")) \
               (non-abort-restart-p (make-package \"PE-A\")))",
            "SUCCESS",
        ),
        // ...and continuing yields the EXISTING package.
        (
            "(progn (make-package \"PE-B\" :use '(\"CL\")) \
               (handler-bind ((error (lambda (c) (declare (ignore c)) (continue)))) \
                 (package-name (make-package \"PE-B\"))))",
            "\"PE-B\"",
        ),
        // DELETE-PACKAGE on a non-package is correctable...
        (
            "(non-abort-restart-p (delete-package \"PE-NO-SUCH-QQ\"))",
            "SUCCESS",
        ),
        // ...and CLHS says correcting attempts no deletion and returns NIL.
        (
            "(handler-bind ((error (lambda (c) (declare (ignore c)) (continue)))) \
               (delete-package \"PE-NO-SUCH-QQ\"))",
            "NIL",
        ),
        // The condition is still a PACKAGE-ERROR...
        (
            "(handler-case (progn (make-package \"PE-C\" :use '(\"CL\")) \
                                  (make-package \"PE-C\")) \
               (package-error () :package-error))",
            ":PACKAGE-ERROR",
        ),
        // ...and it now names the package rather than reporting bare
        // "Package error." (the complaint in bliss-wne9.3).
        (
            "(progn (make-package \"PE-D\" :use '(\"CL\")) \
               (handler-case (make-package \"PE-D\") \
                 (package-error (c) (and (search \"PE-D\" (princ-to-string c)) t))))",
            "T",
        ),
        // Unhandled, it must still be an error rather than silently continuing.
        (
            "(handler-case (delete-package \"PE-NO-SUCH-RR\") (error () :signalled))",
            ":SIGNALLED",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {helper} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "package-error case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "wrong correctable-package-error behaviour for: {expr}"
        );
    }
}

/// A symbol registry key is `PACKAGE::NAME`, and NAME may itself contain
/// colons. Ten sites split such a key at the LAST marker, which lands inside
/// the name: SYMBOL-PACKAGE reported "COMMON-LISP-USER::PR2" — not even a well
/// formed package name — for a symbol named "PR2::ZED" (bliss-lgml). Every
/// expectation matches SBCL.
#[test]
fn registry_keys_split_at_the_first_package_marker() {
    let setup = "(make-package \"PV\" :use (list \"CL\"))";
    let cases = [
        // A symbol whose NAME contains a marker still belongs to CL-USER.
        (
            "(package-name (symbol-package (read-from-string \"|PV::ZED|\")))",
            "\"COMMON-LISP-USER\"",
        ),
        (
            "(symbol-name (read-from-string \"|PV::ZED|\"))",
            "\"PV::ZED\"",
        ),
        // ...and it prints as ONE bar-quoted token, so it reads back unchanged.
        (
            "(prin1-to-string (read-from-string \"|PV::ZED|\"))",
            "\"|PV::ZED|\"",
        ),
        (
            "(let ((s (read-from-string \"|PV::ZED|\"))) \
               (eq s (read-from-string (prin1-to-string s))))",
            "T",
        ),
        // A leading colon inside an escaped name is likewise name text.
        (
            "(package-name (symbol-package (read-from-string \"\\\\:notkw\")))",
            "\"COMMON-LISP-USER\"",
        ),
        // An UNINTERNED symbol has no registry key, so its name is never split.
        ("(symbol-name (make-symbol \"A:B\"))", "\"A:B\""),
        (
            "(symbol-name (copy-symbol (make-symbol \"A:B\")))",
            "\"A:B\"",
        ),
        // Ordinary symbols are unaffected.
        (
            "(package-name (symbol-package (intern \"ZED\" \"PV\")))",
            "\"PV\"",
        ),
        ("(symbol-name (intern \"A:B\" \"PV\"))", "\"A:B\""),
        ("(package-name (symbol-package 'car))", "\"COMMON-LISP\""),
        ("(package-name (symbol-package :kw))", "\"KEYWORD\""),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "registry key case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "registry key split at the wrong marker for: {expr}"
        );
    }
}

/// A package marker is located among the UNESCAPED characters only (CLHS
/// 2.3.4). The reader scanned the whole token first and only remembered whether
/// *some* escape occurred, so an escaped token never got package treatment:
/// "PY::|has space|" read as a CL-USER symbol NAMED "PY::has space"
/// (bliss-i83w). With the printer half (bliss-10cf) this closes the print/read
/// round trip. Every expectation matches SBCL.
#[test]
fn package_marker_is_found_among_unescaped_characters() {
    let setup = "(progn (make-package \"PW\" :use (list \"CL\")) \
                        (export (intern \"EX\" \"PW\") \"PW\"))";
    let cases = [
        // The bug: a bar-quoted NAME after a package marker.
        (
            "(let ((s (read-from-string \"PW::|has space|\"))) \
               (list (symbol-name s) (package-name (symbol-package s))))",
            "(\"has space\" \"PW\")",
        ),
        // A colon INSIDE the bars is name text, not a second marker.
        ("(symbol-name (read-from-string \"PW::|A:B|\"))", "\"A:B\""),
        // A backslash-escaped colon is likewise ordinary name text.
        ("(symbol-name (read-from-string \"PW::a\\:b\"))", "\"A:B\""),
        // Bars around the PACKAGE token still name the package.
        (
            "(package-name (symbol-package (read-from-string \"|PW|::ZED\")))",
            "\"PW\"",
        ),
        // Unescaped markers keep working: internal, external, keyword, bare.
        (
            "(package-name (symbol-package (read-from-string \"PW::ZED\")))",
            "\"PW\"",
        ),
        (
            "(package-name (symbol-package (read-from-string \"PW:EX\")))",
            "\"PW\"",
        ),
        (
            "(symbol-name (read-from-string \":|foo bar|\"))",
            "\"foo bar\"",
        ),
        ("(read-from-string \":|A|\")", ":A"),
        ("(symbol-name (read-from-string \"|foo|\"))", "\"foo\""),
        ("(symbol-name (read-from-string \"||\"))", "\"\""),
        // A wholly bar-quoted spelling has NO marker — one symbol whose name
        // contains colons.
        (
            "(symbol-name (read-from-string \"|PW::ZED|\"))",
            "\"PW::ZED\"",
        ),
        // Numbers must still read as numbers, not symbols.
        ("(+ (read-from-string \"123\") 1)", "124"),
        // The point of the exercise: print then read yields the SAME symbol,
        // including for names that need escaping.
        (
            "(every (lambda (s) (eq s (read-from-string (prin1-to-string s)))) \
               (list (intern \"ZED\" \"PW\") (intern \"EX\" \"PW\") \
                     (intern \"has space\" \"PW\") (intern \"A:B\" \"PW\")))",
            "T",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "reader case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "wrong read result for: {expr}"
        );
    }
}

/// Bars quote a TOKEN, not a whole qualified spelling: the package marker is
/// printer syntax and belongs outside them. egcl printed `|WQ::ZED|` for every
/// qualified symbol — the colon in the spelling made the whole thing "need"
/// escaping — and `~A` wrongly kept the package prefix (bliss-10cf). Both
/// printers (cli print_val and stdlib egclval_to_print_inner) are covered
/// here; they have drifted before. Every expectation matches SBCL.
#[test]
fn qualified_symbols_print_with_bars_around_the_name_only() {
    let setup = "(progn (make-package \"PZ\" :use (list \"CL\")) \
                        (export (intern \"EXT\" \"PZ\") \"PZ\"))";
    let cases = [
        // An internal symbol takes no bars at all.
        ("(prin1-to-string (intern \"ZED\" \"PZ\"))", "\"PZ::ZED\""),
        ("(format nil \"~S\" (intern \"ZED\" \"PZ\"))", "\"PZ::ZED\""),
        // An external symbol prints with a single marker.
        ("(prin1-to-string (intern \"EXT\" \"PZ\"))", "\"PZ:EXT\""),
        // A name that needs escaping: bars around the NAME, not the spelling.
        (
            "(prin1-to-string (intern \"has space\" \"PZ\"))",
            "\"PZ::|has space|\"",
        ),
        // A colon INSIDE the name must stay inside the bars.
        ("(prin1-to-string (intern \"A:B\" \"PZ\"))", "\"PZ::|A:B|\""),
        // princ / ~A print only the name — the prefix is escaped syntax.
        ("(format nil \"~A\" (intern \"ZED\" \"PZ\"))", "\"ZED\""),
        (
            "(with-output-to-string (s) (princ (intern \"ZED\" \"PZ\") s))",
            "\"ZED\"",
        ),
        // ~A and princ must agree — the two printers drifted here before.
        (
            "(string= (format nil \"~A\" (intern \"ZED\" \"PZ\")) \
               (with-output-to-string (s) (princ (intern \"ZED\" \"PZ\") s)))",
            "T",
        ),
        // An uninterned name is a bare token and is quoted whole, keeping its
        // #: prefix outside the bars.
        ("(prin1-to-string (make-symbol \"A:B\"))", "\"#:|A:B|\""),
        ("(prin1-to-string (make-symbol \"GG\"))", "\"#:GG\""),
        // Keywords and CL symbols are unaffected.
        ("(prin1-to-string :kw)", "\":KW\""),
        ("(prin1-to-string 'car)", "\"CAR\""),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "symbol printing case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "wrong printed representation for: {expr}"
        );
    }
}

/// A package designator may be a SYMBOL, and only its name counts — the
/// package it happens to be interned in is irrelevant (CLHS 11.1.1.1). These
/// sites read the symbol with `val_as_str`, which yields the QUALIFIED
/// spelling, so `(make-package 'foo)` evaluated inside package HOST created a
/// package literally named "HOST::FOO" (bliss-43ad). Every expectation matches
/// SBCL.
#[test]
fn package_designator_symbols_use_only_their_name() {
    // Run each case from inside a non-CL package, which is what exposes the
    // qualified spelling: from CL-USER the prefix happens to be elided.
    let cases = [
        // MAKE-PACKAGE: name, :nicknames and :use are all designators.
        ("(package-name (make-package 'dz-a))", "\"DZ-A\""),
        (
            "(package-nicknames (make-package 'dz-b :nicknames '(dz-nick)))",
            "(\"DZ-NICK\")",
        ),
        (
            "(mapcar #'package-name (package-use-list (make-package 'dz-c :use '(cl))))",
            "(\"COMMON-LISP\")",
        ),
        // FIND-PACKAGE must agree with what MAKE-PACKAGE created — before the
        // fix both were consistently wrong, which hid the bug.
        (
            "(progn (make-package 'dz-d) (package-name (find-package 'dz-d)))",
            "\"DZ-D\"",
        ),
        // A nickname given as a symbol resolves as a designator too.
        (
            "(progn (make-package 'dz-e :nicknames '(dz-en)) (package-name (find-package 'dz-en)))",
            "\"DZ-E\"",
        ),
        // INTERN / FIND-SYMBOL take a package designator as their 2nd argument.
        (
            "(progn (make-package 'dz-f) (package-name (symbol-package (intern \"ZED\" 'dz-f))))",
            "\"DZ-F\"",
        ),
        (
            "(progn (make-package 'dz-g) (intern \"ZED\" 'dz-g) \
               (nth-value 1 (find-symbol \"ZED\" 'dz-g)))",
            ":INTERNAL",
        ),
        // USE-PACKAGE / UNUSE-PACKAGE / DELETE-PACKAGE.
        (
            "(progn (make-package 'dz-h) (make-package 'dz-i) (use-package 'dz-h 'dz-i) \
               (mapcar #'package-name (package-use-list 'dz-i)))",
            "(\"DZ-H\")",
        ),
        (
            "(progn (make-package 'dz-j) (make-package 'dz-k) (use-package 'dz-j 'dz-k) \
               (unuse-package 'dz-j 'dz-k) (package-use-list 'dz-k))",
            "NIL",
        ),
        (
            "(progn (make-package 'dz-l) (delete-package 'dz-l) (find-package 'dz-l))",
            "NIL",
        ),
        // (satisfies find-package) reads the designator the same way.
        (
            "(progn (make-package 'dz-m) (typep 'dz-m '(satisfies find-package)))",
            "T",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!(
            "(progn (defpackage :dz-host (:use :common-lisp)) (in-package :dz-host) \
               (cl:format t \"~S\" {expr}))"
        );
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "package designator case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "package designator symbol leaked its qualified spelling: {expr}"
        );
    }
}

/// OPEN, APROPOS and APROPOS-LIST were unbound (bliss-wne9.5). ASDF and UIOP
/// call them, so their absence surfaced downstream as library bugs. Every
/// expectation here matches SBCL.
#[test]
fn open_and_apropos_entry_points() {
    let setup = "(progn \
        (with-open-file (s \"/tmp/egcl-open-acc.txt\" :direction :output \
                           :if-exists :supersede) \
          (write-string \"roundtrip\" s)) \
        (defpackage :egcl-ap-acc (:use :cl)) \
        (defvar egcl-ap-acc::alpha-widget 1) \
        (defun egcl-ap-acc::make-alpha-widget () :x))";
    let cases = [
        // The bead's acceptance criterion: OPEN a file, READ-LINE it, CLOSE it.
        (
            "(let ((s (open \"/tmp/egcl-open-acc.txt\"))) (prog1 (read-line s) (close s)))",
            "\"roundtrip\"",
        ),
        // :if-does-not-exist nil yields NIL rather than signalling.
        (
            "(open \"/tmp/egcl-open-acc-missing-qq\" :if-does-not-exist nil)",
            "NIL",
        ),
        // ...and the default does signal.
        (
            "(handler-case (open \"/tmp/egcl-open-acc-missing-qq\") (error () :signalled))",
            ":SIGNALLED",
        ),
        // A stream from OPEN is a real, open input stream, and CLOSE shuts it.
        (
            "(let ((s (open \"/tmp/egcl-open-acc.txt\"))) \
               (list (streamp s) (input-stream-p s) (open-stream-p s) \
                     (progn (close s) (open-stream-p s))))",
            "(T T T NIL)",
        ),
        // APROPOS-LIST finds present and inherited symbols of a named package.
        (
            "(sort (mapcar #'symbol-name (apropos-list \"WIDGET\" :egcl-ap-acc)) #'string<)",
            "(\"ALPHA-WIDGET\" \"MAKE-ALPHA-WIDGET\")",
        ),
        // Matching is case-insensitive.
        (
            "(sort (mapcar #'symbol-name (apropos-list \"widget\" :egcl-ap-acc)) #'string<)",
            "(\"ALPHA-WIDGET\" \"MAKE-ALPHA-WIDGET\")",
        ),
        // No match is the empty list, not an error.
        ("(apropos-list \"NO-SUCH-THING-QQQ\" :egcl-ap-acc)", "NIL"),
        // CLHS: APROPOS returns NO values.
        (
            "(multiple-value-list (apropos \"NO-SUCH-THING-QQQ\" :egcl-ap-acc))",
            "NIL",
        ),
        // NIL and T satisfy IS-SYMBOL but carry their own bit patterns rather
        // than a symbol tag; scanning a package must not choke on them.
        ("(not (null (member 'nil (apropos-list \"NIL\" :cl))))", "T"),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args([
                "--eval",
                &format!("(progn {setup} (cl:format t \"~S\" {expr}))"),
            ])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "OPEN/APROPOS case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "wrong result for: {expr}"
        );
    }
}

/// CLHS 3.4.1: &AUX initforms are evaluated only after every other parameter
/// is bound, and left to right so each sees the preceding &AUX bindings. The
/// binder used to evaluate them during its lambda-list scan, before the
/// deferred &KEY pass, so `(&key (y 1) &aux (z y))` signalled "unbound
/// variable: Y" (bliss-65c4). Every expectation below matches SBCL exactly.
#[test]
fn macro_aux_params_bind_after_key_defaults() {
    let cases = [
        // &aux reads a &key default, and an explicitly supplied &key value.
        (
            "(progn (defmacro m1 (&key (y 1) &aux (z y)) (list 'quote (list y z))) (m1))",
            "(1 1)",
        ),
        (
            "(progn (defmacro m1b (&key (y 1) &aux (z y)) (list 'quote (list y z))) (m1b :y 42))",
            "(42 42)",
        ),
        // &aux reads an &optional default computed from a required parameter.
        (
            "(progn (defmacro m2 (a &optional (b (* a 2)) &aux (c (+ a b))) \
               (list 'quote (list a b c))) (m2 3))",
            "(3 6 9)",
        ),
        // &aux reads the &rest list.
        (
            "(progn (defmacro m3 (&rest r &aux (n (length r))) (list 'quote (list r n))) \
               (m3 1 2 3))",
            "((1 2 3) 3)",
        ),
        // Sequential: each &aux initform sees the earlier ones (LET* order),
        // and a bare &aux variable is NIL.
        (
            "(progn (defmacro m4 (&key (y 2) &aux (a (* y 10)) (b (+ a 1)) c) \
               (list 'quote (list y a b c))) (m4 :y 5))",
            "(5 50 51 NIL)",
        ),
        // &aux after a supplied-p variable, &rest and &key together.
        (
            "(progn (defmacro m5 (a &optional (b 0 bp) &rest r &key k &aux (tot (list a b bp r k))) \
               (list 'quote tot)) (m5 1 2 :k 9))",
            "(1 2 T (:K 9) 9)",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "&aux macro case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        // --eval echoes the form's own value (NIL from FORMAT) on a later
        // line; the first line is what FORMAT wrote.
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            expected,
            "wrong &aux binding for: {expr}"
        );
    }
}

#[test]
fn macro_lambda_list_defaults_are_gc_safe_and_package_neutral_on_error() {
    let expr = "(progn
                  (defpackage :macro-root-test (:use :common-lisp))
                  (in-package :macro-root-test)
                  (defmacro rooted-ll
                      ((&optional (x (list 4 5)))
                       &key (y (list 6 7)))
                    (list 'quote (list x y (append x y))))
                  (defmacro failing-ll
                      (&optional (x (progn (in-package :common-lisp-user)
                                           (error \"expected\"))))
                    x)
                  (let ((value (eval '(rooted-ll ()))))
                    (ignore-errors (macroexpand-1 '(failing-ll)))
                    (cl:format t \"~S/~A\" value
                               (package-name *package*))))";
    let output = egcl_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "macro lambda-list expansion failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("((4 5) (6 7) (4 5 6 7))/MACRO-ROOT-TEST"),
        "macro lambda-list values or package restore were wrong: {}",
        String::from_utf8_lossy(&output.stdout)
    );
}

/// bliss-hupm: a "medium" allocation whose total footprint falls in
/// (tlab_size, region_size/2] — bigger than a TLAB (256 KiB) but not over the
/// old `> region_size/2` (512 KiB) large-object routing threshold — used to
/// panic "GC heap unavailable": alloc_fast couldn't fit it, refill only carved
/// another same-size TLAB, and it was never routed to alloc_large. (make-string
/// 100000) is a ~400 KiB character string right in that band; it blocked the
/// whole ansi-test harness (gclload1.lsp). Exercise a range of medium sizes for
/// strings AND vectors, and confirm one survives GC intact.
#[test]
fn medium_sized_objects_allocate_and_survive_gc() {
    // The panic reproduced on a plain allocation (no GC stress needed): sizes in
    // the gap band (footprint 256 KiB..512 KiB, ~65534..131072 chars/elements)
    // returned None from alloc_typed. Larger objects use the already-tested
    // large-object path. A modest cons churn after each alloc exercises a real
    // minor GC over the medium object without the O(n)-per-GC cost of forced
    // stress on many large objects.
    let prog = "\
        (dolist (n '(65540 100000 131000)) \
          (let ((s (make-string n :initial-element #\\q)) \
                (v (make-array n :initial-element 7))) \
            (unless (and (= (length s) n) (char= (char s (1- n)) #\\q) \
                         (= (length v) n) (= (aref v (1- n)) 7)) \
              (error \"medium alloc wrong at ~D\" n)))) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "medium-sized allocations must not panic/crash (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-31x8: a large (>~512KB) string LITERAL read by the reader gets a
/// 16-byte object header, so the reader's value pointer (gc_value) and the
/// simple-string accessors must honour the large-object payload offset — the
/// same family as bliss-tjru for vectors. Before the fix the literal's value
/// pointed at the size-extension word and LENGTH/CHAR saw a non-sequence.
#[test]
fn large_string_literal_is_usable_and_gc_safe() {
    // Build a 600001-char literal ("aaa…Z") and read it at runtime; verify
    // length, indexed reads at both ends, mutation, and printing round-trip.
    let prog = "\
        (let* ((n 600000) \
               (src (make-string (+ n 4) :initial-element #\\a))) \
          (setf (char src 0) #\\\") \
          (setf (char src (+ n 1)) #\\Z) \
          (setf (char src (+ n 2)) #\\\") \
          (setf (char src (+ n 3)) #\\Space) \
          (let ((s (read-from-string src))) \
            (unless (stringp s) (error \"recog\")) \
            (unless (= (length s) (+ n 1)) (error \"len\")) \
            (unless (char= (char s 0) #\\a) (error \"head\")) \
            (unless (char= (char s n) #\\Z) (error \"tail\")) \
            (princ :ok)))";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "8000")
        .env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "large string literal under GC must not abort (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-bjue: defstruct's generated constructor/accessor/predicate/copier were
/// built from movable cons intermediates left unrooted across allocating
/// sym()/quote()/vec_to_list() calls, so under a minor GC (EGCL_GC_STRESS) the
/// forms were corrupted — (make-NAME …) aborted with "undefined function: &REST".
/// This exercises the whole defstruct surface under GC stress; it must not abort.
#[test]
fn defstruct_is_gc_safe_under_stress() {
    let prog = "\
        (defstruct box a b) \
        (defstruct (animal) name) \
        (defstruct (dog (:include animal)) breed) \
        (dotimes (k 30) \
          (let ((x (make-box :a k :b (list k)))) \
            (when (or (/= (box-a x) k) (not (box-p x)) \
                      (not (equal (box-b (copy-box x)) (list k)))) \
              (error \"box\"))) \
          (let ((d (make-dog :name k :breed (list k)))) \
            (when (or (/= (animal-name d) k) (not (dog-p d))) (error \"dog\")))) \
        (defstruct (v3 (:constructor mk-v3 (x y z)) (:conc-name v-)) x y z) \
        (dotimes (k 30) \
          (let ((v (mk-v3 k (* 2 k) (* 3 k)))) \
            (when (/= (v-z v) (* 3 k)) (error \"boa\")))) \
        (princ :ok)";
    let mut cmd = egcl_bin();
    cmd.env("EGCL_GC_STRESS", "1").env("EGCL_GC_POISON", "1");
    cmd.args(["--eval", prog]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "defstruct under GC stress must not abort (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("OK"),
        "expected OK: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// bliss-44qp-adjacent: COPY-STRUCTURE (was fboundp => T but "undefined
/// function") shallow-copies any structure/instance — a distinct object with the
/// same class and every bound slot copied, including inherited (:include) slots.
#[test]
fn copy_structure_shallow_copies_instances() {
    let prog = "\
        (defstruct point x y) \
        (defstruct (animal) name) \
        (defstruct (dog (:include animal)) breed) \
        (let* ((p (make-point :x 1 :y 2)) (q (copy-structure p))) \
          (setf (point-x q) 99) \
          (let ((d (make-dog :name \"rex\" :breed \"lab\"))) \
            (let ((e (copy-structure d))) \
              (format t \"~a|~a|~a|~a|~a|~a\" \
                (point-x p) (point-x q) (not (eq p q)) \
                (animal-name e) (dog-breed e) (typep e 'dog)))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "should exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // original x unchanged (1), copy's x mutated (99), distinct objects (T),
    // inherited + own slots copied, and the copy is still a DOG.
    assert!(
        stdout.contains("1|99|T|rex|lab|T"),
        "copy-structure: {stdout}"
    );
}

/// bliss-05hy: FLOOR/CEILING/TRUNCATE/ROUND/MOD/REM and EVENP/ODDP were computed
/// via f64 (num_val + `as i64`), which overflows and loses precision on bignums
/// (and large fixnums) — (floor (expt 2 70) 2) returned -1. They now use exact
/// rational division; this also transitively fixes bignum LOGAND (via boot's
/// bit-recursion through FLOOR).
#[test]
fn integer_division_is_exact_on_bignums() {
    let cases = [
        // The reported bug: bignum FLOOR/TRUNCATE.
        ("(floor (expt 2 70) 2)", "590295810358705651712"),
        ("(truncate (expt 2 70) 2)", "590295810358705651712"),
        ("(floor (expt 2 70) 3)", "393530540239137101141"),
        ("(nth-value 1 (floor (expt 2 70) 3))", "1"),
        // Transitively fixed: bignum bitwise ops (boot recurses via FLOOR).
        ("(logand (expt 2 70) (expt 2 70))", "1180591620717411303424"),
        ("(evenp (expt 2 70))", "T"),
        ("(oddp (1+ (expt 2 70)))", "T"),
        // Rounding-mode correctness (fixnum regressions).
        ("(floor -7 2)", "-4"),
        ("(ceiling -7 2)", "-3"),
        ("(truncate -7 2)", "-3"),
        ("(mod -7 3)", "2"),
        ("(rem -7 3)", "-1"),
        // ROUND ties to even.
        ("(round 5 2)", "2"),
        ("(round 7 2)", "4"),
        ("(round -5 2)", "-2"),
        // Ratios and floats still work.
        ("(floor 7/2)", "3"),
        ("(floor 3.7)", "3"),
        ("(round 2.5)", "2"),
        // MIN/MAX return the exact extreme argument (were f64→i64, giving -1).
        ("(min (expt 2 70) (expt 2 69))", "590295810358705651712"),
        ("(max (expt 2 70) 5)", "1180591620717411303424"),
        ("(min 1/2 1/3)", "1/3"),
        ("(max 1.5 2.5 0.5)", "2.5"),
        ("(min 3 1 2)", "1"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-44qp: the transcendental functions (EXP, LOG 1-/2-arg, SIN/COS/TAN,
/// ASIN/ACOS/ATAN 1-/2-arg, hyperbolics) — previously all "undefined function".
#[test]
fn transcendental_functions() {
    let cases = [
        ("(exp 0)", "1.0"),
        ("(log 8 2)", "3.0"),
        ("(log 100 10)", "2.0"),
        ("(log 1)", "0.0"),
        ("(sin 0)", "0.0"),
        ("(cos 0)", "1.0"),
        ("(tan 0)", "0.0"),
        ("(atan 1 1)", "0.7853982"),
        ("(asin 1)", "1.5707964"),
        ("(sinh 0)", "0.0"),
        ("(cosh 0)", "1.0"),
        ("(tanh 0)", "0.0"),
        // Round-trips and identities.
        ("(exp (log 5))", "5.0"),
        ("(* 4 (atan 1))", "3.1415927"),
        // LOG of a negative real is complex (matches SQRT).
        ("(log -1)", "#C(0.0 3.1415927)"),
        // First-class use.
        ("(funcall #'sin 0)", "0.0"),
        ("(mapcar #'exp '(0 1))", "(1.0 2.7182817)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-kzhq: FORMAT ~A/~S print whole single-floats with a decimal point
/// (2.0, not 2) so they read back as floats — matching cli PRINC/PRIN1. Was a
/// tier/path divergence (stdlib format dropped the .0).
#[test]
fn format_prints_whole_floats_with_decimal_point() {
    let cases = [
        ("(format nil \"~a\" 2.0)", "\"2.0\""),
        ("(format nil \"~s\" 2.0)", "\"2.0\""),
        ("(format nil \"~a\" 0.0)", "\"0.0\""),
        ("(format nil \"~a\" -3.0)", "\"-3.0\""),
        ("(format nil \"~a\" 100.0)", "\"100.0\""),
        ("(format nil \"~a\" 2.5)", "\"2.5\""),
        // Fractional/exponent floats and the ~F directive are unaffected.
        ("(format nil \"~a\" 1234.5)", "\"1234.5\""),
        ("(format nil \"~,2f\" 3.14159)", "\"3.14\""),
        // Floats nested in aggregates, and complex float parts, print right.
        ("(format nil \"~a\" (list 1.0 2.5))", "\"(1.0 2.5)\""),
        ("(format nil \"~a\" (sqrt -4))", "\"#C(0.0 2.0)\""),
        // Matches princ (same value, both paths).
        ("(equal (format nil \"~a\" 7.0) (princ-to-string 7.0))", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Complex numbers compare correctly under all the equality predicates:
/// EQL/EQUAL/EQUALP componentwise (CLHS: same-type numbers with eql parts), and
/// =//= numerically (they are defined on complex, which is unordered). This was
/// broken because the shared numeric compare (numeric_cmp) errors on complex.
#[test]
fn complex_number_equality_predicates() {
    let cases = [
        ("(eql #c(0 2) #c(0 2))", "T"),
        ("(eql #c(1 2) #c(1 3))", "NIL"),
        ("(equal #c(0 2) #c(0 2))", "T"),
        ("(equalp #c(0 2) #c(0 2))", "T"),
        ("(= #c(0 2) #c(0 2))", "T"),
        ("(= #c(1 2) #c(1 3))", "NIL"),
        ("(/= #c(1 2) #c(1 3))", "T"),
        // = across real/complex when the imaginary part is zero.
        ("(= 2 #c(2 0.0))", "T"),
        // eql is type-strict on the parts (0 vs 0.0 differ).
        ("(eql #c(0 2) #c(0.0 2.0))", "NIL"),
        // member/assoc (built on eql) find complex keys.
        ("(if (member #c(1 2) (list #c(3 4) #c(1 2))) t nil)", "T"),
        // Ordering predicates and cross-type = unchanged.
        ("(= 1 1.0)", "T"),
        ("(eql 1 1.0)", "NIL"),
        ("(< 1 2 3)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Functions/closures print as #<FUNCTION>, not as the internal
/// (EGCL::CLOSURE . id) data list — in cli PRINT/PRIN1 and in FORMAT ~A/~S,
/// including nested in a list. Data conses (incl. NIL/T cars) are unaffected.
#[test]
fn functions_print_as_function_objects() {
    // String-returning exprs print with surrounding quotes under (print …).
    let cases = [
        ("(princ-to-string (lambda (x) x))", "\"#<FUNCTION>\""),
        ("(princ-to-string #'car)", "\"#<FUNCTION>\""),
        ("(prin1-to-string (complement #'evenp))", "\"#<FUNCTION>\""),
        ("(format nil \"~a\" #'car)", "\"#<FUNCTION>\""),
        ("(format nil \"~s\" (constantly 7))", "\"#<FUNCTION>\""),
        (
            "(princ-to-string (list #'car #'cdr))",
            "\"(#<FUNCTION> #<FUNCTION>)\"",
        ),
        // Regression: printing data lists with NIL/T in the car must not crash
        // the closure-cons detector (as_symbol_index panics on NIL/T).
        (
            "(format nil \"~a\" '((nil 1) (t 2)))",
            "\"((NIL 1) (T 2))\"",
        ),
        ("(princ-to-string (cons nil 5))", "\"(NIL . 5)\""),
        // The functions still work.
        ("(funcall (complement #'evenp) 3)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-k0jg follow-up: SQRT of a negative real returns the pure-imaginary
/// complex root (not NaN), and SQRT of a complex returns the principal complex
/// root; non-negative reals still return a float.
#[test]
fn sqrt_of_negative_and_complex() {
    let cases = [
        ("(sqrt -4)", "#C(0.0 2.0)"),
        ("(sqrt -1)", "#C(0.0 1.0)"),
        ("(sqrt -2.0)", "#C(0.0 1.4142135)"),
        ("(sqrt #C(0 2))", "#C(1.0 1.0)"),
        ("(sqrt #C(-1 0))", "#C(0.0 1.0)"),
        ("(* (sqrt -1) (sqrt -1))", "#C(-1.0 0.0)"),
        // Non-negative reals unchanged.
        ("(sqrt 4)", "2.0"),
        ("(sqrt 0)", "0.0"),
        ("(complexp (sqrt -9))", "T"),
        ("(realpart (sqrt -9))", "0.0"),
        ("(imagpart (sqrt -9))", "3.0"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-k0jg: RATIONAL (exact float → rational) and RATIONALIZE (simplest
/// rational that reads back to the same float). Both leave integers/ratios
/// unchanged and round-trip through FLOAT.
#[test]
fn rational_and_rationalize() {
    let cases = [
        ("(rational 0.5)", "1/2"),
        ("(rational 0.25)", "1/4"),
        ("(rational 1.0)", "1"),
        ("(rational -0.5)", "-1/2"),
        ("(rational 3)", "3"),
        ("(rational 1/3)", "1/3"),
        ("(integerp (rational 4.0))", "T"),
        ("(rationalize 0.5)", "1/2"),
        ("(rationalize 0.1)", "1/10"),
        ("(rationalize -0.1)", "-1/10"),
        ("(rationalize 2.5)", "5/2"),
        ("(rationalize 0.0)", "0"),
        // The defining property: FLOAT of the result reproduces the input float.
        ("(= (float (rational 0.1)) 0.1)", "T"),
        ("(= (float (rationalize 0.1)) 0.1)", "T"),
        ("(= (float (rationalize 0.333333)) 0.333333)", "T"),
        // Usable as a first-class function.
        ("(mapcar #'rational '(0.5 0.25 1.0))", "(1/2 1/4 1)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-rh0t: real multidimensional (rank ≥ 2) arrays — make-array on a
/// dimension list builds a row-major array with the right rank/dimensions/total
/// size, AREF/(setf AREF) address it per-axis, ROW-MAJOR-AREF addresses the flat
/// storage, :initial-contents fills nested, and it prints/types as an array (not
/// a vector).
#[test]
fn multidimensional_arrays() {
    let cases = [
        ("(array-rank (make-array '(2 3)))", "2"),
        ("(array-dimensions (make-array '(2 3)))", "(2 3)"),
        ("(array-total-size (make-array '(2 3)))", "6"),
        ("(array-dimension (make-array '(2 3 4)) 2)", "4"),
        ("(aref (make-array '(2 3) :initial-element 7) 1 2)", "7"),
        (
            "(let ((a (make-array '(2 3) :initial-element 0))) (setf (aref a 1 2) 99) (aref a 1 2))",
            "99",
        ),
        (
            "(aref (make-array '(2 3) :initial-contents '((1 2 3) (4 5 6))) 1 0)",
            "4",
        ),
        (
            "(row-major-aref (make-array '(2 3) :initial-contents '((1 2 3) (4 5 6))) 4)",
            "5",
        ),
        (
            "(let ((a (make-array '(2 2 2) :initial-element 0))) (setf (aref a 1 0 1) 7) (aref a 1 0 1))",
            "7",
        ),
        // Predicates / type.
        ("(arrayp (make-array '(2 3)))", "T"),
        ("(vectorp (make-array '(2 3)))", "NIL"),
        ("(typep (make-array '(2 3)) 'array)", "T"),
        ("(typep (make-array '(2 3)) 'vector)", "NIL"),
        ("(type-of (make-array '(2 3)))", "(SIMPLE-ARRAY T (2 3))"),
        // Printing (#nA row-major nesting).
        (
            "(prin1-to-string (make-array '(2 3) :initial-contents '((1 2 3) (4 5 6))))",
            "\"#2A((1 2 3) (4 5 6))\"",
        ),
        (
            "(prin1-to-string (make-array '(2 2 2) :initial-contents '(((1 2) (3 4)) ((5 6) (7 8)))))",
            "\"#3A(((1 2) (3 4)) ((5 6) (7 8)))\"",
        ),
        // Rank-1 list dimension stays an ordinary vector.
        ("(array-rank (make-array 5))", "1"),
        ("(vectorp (make-array '(5)))", "T"),
        // ARRAY-TOTAL-SIZE is the backing capacity, not the fill pointer;
        // ADJUST-ARRAY grows a plain adjustable array (length tracks the new
        // size) while preserving a real fill pointer. (bliss-6wng)
        ("(array-total-size (make-array 5 :fill-pointer 3))", "5"),
        ("(length (make-array 5 :fill-pointer 3))", "3"),
        (
            "(let ((a (make-array 3 :adjustable t :initial-element 0))) \
               (adjust-array a 5 :initial-element 1) (list (length a) (aref a 4) (aref a 0)))",
            "(5 1 0)",
        ),
        (
            "(let ((a (make-array 5 :fill-pointer 3 :adjustable t))) \
               (adjust-array a 8) (list (fill-pointer a) (array-total-size a)))",
            "(3 8)",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// bliss-qng: the COMPLEX constructor (type contagion + canonicalisation) and
/// complex arithmetic (+ - * /) over the numeric tower, including real⇄complex
/// mixing, unary negate/reciprocal, and canonicalisation of a rational complex
/// with zero imaginary part back to a real.
#[test]
fn complex_constructor_and_arithmetic() {
    let cases = [
        // Constructor + canonicalisation (CLHS 12.1.3.3).
        ("(complex 1 2)", "#C(1 2)"),
        ("(complex 3 0)", "3"),               // rational, zero imag ⇒ real
        ("(complex 3)", "3"),                 // default imag 0
        ("(complex 1.0 0.0)", "#C(1.0 0.0)"), // float never canonicalises
        ("(complex 1 2.0)", "#C(1.0 2.0)"),   // float contagion on both parts
        ("(complexp (complex 3 0))", "NIL"),
        // Addition / subtraction.
        ("(+ #c(1 2) #c(3 4))", "#C(4 6)"),
        ("(- #c(5 3) #c(1 1))", "#C(4 2)"),
        ("(+ 1 #c(0 1))", "#C(1 1)"),               // real + complex
        ("(- #c(2 2))", "#C(-2 -2)"),               // unary negate
        ("(complexp (+ #c(1 1) #c(1 -1)))", "NIL"), // 2+0i ⇒ real
        // Multiplication (incl. i*i ⇒ -1, N-ary).
        ("(* #c(0 1) #c(0 1))", "-1"),
        ("(* 2 #c(1 1))", "#C(2 2)"),
        ("(* #c(1 2) 3 #c(1 0))", "#C(3 6)"),
        // Division (exact rational parts) + unary reciprocal.
        ("(/ #c(1 2) #c(3 4))", "#C(11/25 2/25)"),
        ("(/ #c(1 2))", "#C(1/5 -2/5)"),
        // ABS magnitude, CONJUGATE, PHASE, CIS (bliss-dhrx).
        ("(abs #c(3 4))", "5.0"),
        ("(conjugate #c(3 4))", "#C(3 -4)"),
        ("(conjugate 5)", "5"),
        ("(< (abs (- (phase #c(0 1)) 1.5707964)) 0.001)", "T"),
        ("(< (abs (phase 5)) 0.001)", "T"),
        ("(realpart (cis 0))", "1.0"),
        ("(< (abs (imagpart (cis 0))) 0.001)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-av5): DEFVAR/DEFPARAMETER proclaim the variable SPECIAL
/// (ANSI 3.8), so a later LET binds it dynamically on BOTH backends. Previously
/// only the tree-walker treated a globally-bound var as dynamic while the
/// compiler bound it lexically — the same function returned different values
/// cold (tree-walked) vs hot (compiled), a tier inconsistency.
#[test]
fn defparameter_proclaims_special_tier_consistent() {
    // A defparameter'd var LET-bound and read by a callee → dynamic (99), and the
    // result is the SAME cold and hot (compiled), with low tier thresholds.
    let mut cmd = egcl_bin();
    cmd.env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T1_T2_INVOKE_THRESHOLD", "3");
    cmd.args([
        "--eval",
        "(defun av-rd () av-pv)",
        "--eval",
        "(defun av-h () (let ((av-pv 99)) (av-rd)))",
        "--eval",
        "(defparameter av-pv 5)",
        "--eval",
        "(format t \"cold=~a~%\" (av-h))",
        "--eval",
        "(dotimes (i 20) (av-h))",
        "--eval",
        "(format t \"hot=~a~%\" (av-h))",
    ]);
    let out = cmd.output().expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "should exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cold=99"), "cold should be 99: {stdout}");
    assert!(
        stdout.contains("hot=99"),
        "hot must match cold (tier-consistent): {stdout}"
    );
    // defvar likewise proclaims special; and defparameter returns the NAME (ANSI).
    let out2 = egcl_bin()
        .args([
            "--eval",
            "(defun av-rd2 () av-dv)",
            "--eval",
            "(defvar av-dv 5)",
            "--eval",
            "(print (let ((av-dv 42)) (av-rd2)))",
        ])
        .output()
        .expect("run egcl");
    assert!(
        String::from_utf8_lossy(&out2.stdout).contains("42"),
        "defvar var is special: {}",
        String::from_utf8_lossy(&out2.stdout)
    );
}

/// Regression (bliss-x5y.23): the bytecode compiler binds a LOCAL
/// `(declare (special v))` LET variable dynamically (BindSpecial) and reads it
/// dynamically in the body, instead of bailing to the tree-walker. Verified on
/// the default (bytecode) backend; results match interpretation.
#[test]
fn compiler_local_declare_special_binds_dynamically() {
    let cases: [(&[&str], &str, &str); 4] = [
        // A LET-bound declared-special var is visible (dynamically) to a callee.
        (
            &[
                "(defun ds-helper () ds-v)",
                "(defun ds-caller () (let ((ds-v 42)) (declare (special ds-v)) (ds-helper)))",
                "(defparameter ds-v 1)",
            ],
            "(ds-caller)",
            "42",
        ),
        // The body reads/updates the dynamic binding directly.
        (
            &[
                "(defun ds-f (x) (let ((ds-acc 0)) (declare (special ds-acc)) \
                 (setq ds-acc (+ ds-acc x)) ds-acc))",
            ],
            "(ds-f 5)",
            "5",
        ),
        // A declared-special inner LET shadows an outer LEXICAL binding: the
        // callee sees the dynamic (inner) value, not the outer lexical.
        (
            &[
                "(defun ds-rd () ds-x)",
                "(defun ds-g () (let ((ds-x 1)) (let ((ds-x 99)) (declare (special ds-x)) (ds-rd))))",
                "(defparameter ds-x 7)",
            ],
            "(ds-g)",
            "99",
        ),
        // A special declaration is pervasive: an inner LET rebinding of the same
        // name is also dynamic, and the body reads that inner dynamic value.
        (
            &[
                "(defun ds-nested () (let ((ds-z 1)) (declare (special ds-z)) \
                 (let ((ds-z 2)) (+ ds-z ds-z))))",
            ],
            "(ds-nested)",
            "4",
        ),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = egcl_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{final_expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{final_expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-7na): `(declaim (special x))` / `(proclaim '(special x))`
/// register a NON-earmuffed name as special via the proclamation registry, so a
/// LET on it establishes a DYNAMIC binding (visible to a called function) on both
/// backends. Previously declaim/proclaim were no-ops and specialness was an
/// earmuff-only heuristic, so the LET bound lexically.
#[test]
fn declaim_proclaim_special_binds_dynamically() {
    // Each case is (setup top-level forms, final expression, expected). The forms
    // are passed as separate top-level `--eval`s (like real file/REPL top-level
    // forms) rather than one wrapped expression, so declaim/defparameter/let bind
    // with top-level semantics.
    let cases: [(&[&str], &str, &str); 3] = [
        // declaim special → dynamic LET binding seen by the called function.
        (
            &[
                "(declaim (special na-zz))",
                "(defun na-rd () na-zz)",
                "(defparameter na-zz 1)",
            ],
            "(let ((na-zz 99)) (na-rd))",
            "99",
        ),
        // proclaim (the run-time counterpart) → same.
        (
            &[
                "(proclaim '(special na-qq))",
                "(defun na-rq () na-qq)",
                "(defparameter na-qq 1)",
            ],
            "(let ((na-qq 77)) (na-rq))",
            "77",
        ),
        // declaim tolerates and ignores non-special declarations.
        (
            &["(declaim (optimize (speed 3)) (type fixnum na-foo))"],
            ":ok",
            ":OK",
        ),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = egcl_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{final_expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Each `--eval` echoes its result, so the final `(print …)` value is the
        // LAST non-empty line, not the first.
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{final_expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-3jkz): CLHS DISASSEMBLE takes an *extended function
/// designator*, so a FUNCTION OBJECT is as valid as a symbol. Only the symbol
/// form was handled, so `(disassemble #'f)` answered "not a compiled EGCL
/// function (a builtin or interpreted closure)" about a function that plainly
/// was one -- and that is the spelling you reach for first when inspecting a
/// value you already hold. Found while using DISASSEMBLE to investigate why
/// float loops never tier up (bliss-fenm).
#[test]
fn disassemble_accepts_a_function_object() {
    let out = egcl_bin()
        .args([
            "--eval",
            "(defun dis3jkz (n) (let ((a 0)) (dotimes (i n) (setq a (+ a 1))) a))",
        ])
        .args(["--eval", "(dis3jkz 50000)"])
        .args(["--eval", "(disassemble 'dis3jkz)"])
        .args(["--eval", "(disassemble #'dis3jkz)"])
        // A builtin and NIL have no listing and must still decline, not crash.
        .args(["--eval", "(disassemble #'car)"])
        .args(["--eval", "(disassemble nil)"])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = String::from_utf8_lossy(&out.stdout);
    // One listing header per successful DISASSEMBLE: the symbol form and the
    // function-object form must BOTH produce one.
    let listings = s.matches("; DIS3JKZ").count();
    assert_eq!(
        listings, 2,
        "expected a listing from both spellings, got {listings}:\n{s}"
    );
    // The two declines are expected; a third would mean the function object was
    // rejected again.
    let declines = s.matches("is not a compiled EGCL function").count();
    assert_eq!(
        declines, 2,
        "expected exactly the builtin and NIL to decline, got {declines}:\n{s}"
    );
}

/// Regression (bliss-gdom): CLHS 5.1.1.1 evaluates a place's subforms left to
/// right and the new-value form LAST. CLHS 5.1.2.9 separately says the new value
/// is the FIRST ARGUMENT to a `(setf f)` writer. Those are different orders, and
/// `lower_setf`'s user-writer branch conflated them -- it lowered the value first
/// because it is passed first, so the value ran before the place's subforms:
///
///     (setf (gf (progn (setf j (incf i)) 1) (progn (setf k (incf i)) x))
///           (progn (incf i) 'a))
///     egcl => j=2 k=3     sbcl => j=1 k=2
///
/// Each subform now goes into a temporary slot first, then the value, then the
/// temps are reloaded -- which puts the value beneath the arguments on the stack
/// without evaluating it early.
///
/// Compiled path only. The tree-walker has the same bug by a different route and
/// is bliss-9n9q; these cases are therefore driven as real top-level forms rather
/// than through `run_expression_cases`, whose second half would evaluate them in
/// the interpreter and still fail.
#[test]
fn setf_evaluates_a_function_places_subforms_before_the_new_value() {
    let cases: [(&[&str], &str, &str); 5] = [
        // A (defun (setf f) …) writer -- the branch that was wrong.
        (
            &[
                "(defun gdf (i x) (nth i x))",
                "(defun (setf gdf) (v i x) (setf (nth i x) v) v)",
            ],
            "(let ((x (list 1 2 3)) (i 0) (j nil) (k nil))\
               (setf (gdf (progn (setf j (incf i)) 1) (progn (setf k (incf i)) x))\
                     (progn (incf i) 'a))\
               (list x i j k))",
            "((1 A 3) 3 1 2)",
        ),
        // The writer's own return is still what SETF yields (bliss-9298 /
        // bliss-n0dc) -- reordering the evaluation must not disturb that.
        (
            &[
                "(defun gdr (x) (car x))",
                "(defun (setf gdr) (v x) (setf (car x) v) :writer-ret)",
            ],
            "(setf (gdr (list 1)) 9)",
            ":WRITER-RET",
        ),
        // Guards: places that were already correct must stay correct. A builtin
        // place, a long-form DEFSETF place, and a DEFSTRUCT accessor (whose store
        // goes through the class-slot branch, not a writer) all take different
        // paths through the same SETF.
        (
            &[],
            "(let ((x (list 1 2 3)) (i 0) (j nil) (k nil))\
               (setf (nth (progn (setf j (incf i)) 1) (progn (setf k (incf i)) x))\
                     (progn (incf i) 'a))\
               (list x i j k))",
            "((1 A 3) 3 1 2)",
        ),
        (
            &[
                "(defun gdd (i x) (nth i x))",
                "(defsetf gdd (i x) (v) `(progn (setf (nth ,i ,x) ,v) ,v))",
            ],
            "(let ((x (list 1 2 3)) (i 0) (j nil) (k nil))\
               (setf (gdd (progn (setf j (incf i)) 1) (progn (setf k (incf i)) x))\
                     (progn (incf i) 'a))\
               (list x i j k))",
            "((1 A 3) 3 1 2)",
        ),
        (
            &["(defstruct gds (pend nil))"],
            "(let ((l (list (make-gds) (make-gds))))\
               (dolist (e l) (setf (gds-pend e) t))\
               (mapcar #'gds-pend l))",
            "(T T)",
        ),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = egcl_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{final_expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{final_expr} => expected {expected}, got: {got}"
        );
    }
}

/// Regression (bliss-eq72): the proclaimed-special registry was keyed by BARE
/// name, so `(declaim (special x))` / `(defvar pkg::x 1)` made EVERY package's
/// X special. An unrelated `(let ((x 42)) ...)` then bound DYNAMICALLY.
///
/// That is not a near-miss. A dynamic binding is unwound when the LET exits, so
/// a closure over what the programmer wrote as a lexical variable loses it:
/// `(funcall (let ((zvar 7)) (lambda () zvar)))` signalled "unbound variable:
/// ZVAR". A `defvar` in a library package could silently break closures in
/// unrelated user code.
///
/// The earmuff test stays bare-name -- it is about SPELLING, and `*x*` reads as
/// special in every package -- so only the proclaimed set is keyed by identity.
/// The paired case below pins that: proclaiming still makes the name dynamic in
/// its OWN package (bliss-7na), which is the whole reason the registry exists.
#[test]
fn proclaiming_special_does_not_leak_across_packages() {
    let cases: [(&[&str], &str, &str); 4] = [
        // A defvar in another package must NOT make CL-USER's same-named symbol
        // special: the LET below is lexical, so the symbol stays unbound.
        (
            &["(defpackage :eq72p (:use :cl))", "(defvar eq72p::zvar 1)"],
            "(let ((zvar 42)) (declare (ignorable zvar)) (boundp 'zvar))",
            "NIL",
        ),
        // The damage that made this worth fixing: a closure over the lexical.
        (
            &["(defpackage :eq72q (:use :cl))", "(defvar eq72q::wvar 1)"],
            "(funcall (let ((wvar 7)) (lambda () wvar)))",
            "7",
        ),
        // Proclaiming is still dynamic in its own package (bliss-7na).
        (
            &[
                "(declaim (special eq72c))",
                "(defun eq72rd () eq72c)",
                "(defparameter eq72c 1)",
            ],
            "(let ((eq72c 5)) (eq72rd))",
            "5",
        ),
        // ...and the earmuff convention is untouched.
        (
            &["(defvar *eq72e* 1)", "(defun eq72re () *eq72e*)"],
            "(let ((*eq72e* 9)) (eq72re))",
            "9",
        ),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = egcl_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{final_expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .rev()
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{final_expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-zg9): bit-vectors (`#*…`) support the predicates
/// BIT-VECTOR-P / SIMPLE-BIT-VECTOR-P, the sequence protocol (LENGTH/ELT/AREF/BIT
/// and COERCE via collect_elements), TYPE-OF `(SIMPLE-BIT-VECTOR n)`, and print in
/// `#*bits` syntax. Previously `#*01` read but these all failed / were undefined.
#[test]
fn bit_vector_support() {
    let cases = [
        ("(bit-vector-p #*101)", "T"),
        ("(simple-bit-vector-p #*101)", "T"),
        ("(bit-vector-p #(1 0 1))", "NIL"),
        ("(bit-vector-p 'foo)", "NIL"),
        ("(vectorp #*101)", "T"),
        ("(length #*10110)", "5"),
        ("(aref #*101 0)", "1"),
        ("(aref #*101 1)", "0"),
        ("(elt #*1100 2)", "0"),
        ("(bit #*1100 0)", "1"),
        ("(type-of #*101)", "(SIMPLE-BIT-VECTOR 3)"),
        ("(coerce #*101 'list)", "(1 0 1)"),
        ("(prin1-to-string #*10110)", "\"#*10110\""),
        // Callable predicate through the builtin function object (bliss-uuh).
        ("(funcall #'bit-vector-p #*1)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-uuh): `#'<builtin>` is a FUNCTIONP function object (a wrapper
/// closure), callable via funcall/apply/higher-order functions, while the bare
/// symbol `'car` stays non-FUNCTIONP. Builtins have no heap function cell, so
/// `#'car` used to return the symbol CAR (not FUNCTIONP).
#[test]
fn sharp_quote_builtin_is_functionp() {
    let cases = [
        ("(functionp #'car)", "T"),
        ("(functionp #'+)", "T"),
        ("(typep #'cons 'function)", "T"),
        // The bare symbol is NOT a function.
        ("(functionp 'car)", "NIL"),
        // Callable directly and as a higher-order argument.
        ("(funcall #'car '(1 2))", "1"),
        ("(apply #'+ '(1 2 3))", "6"),
        ("(mapcar #'car '((1 2) (3 4)))", "(1 3)"),
        ("(sort (list 3 1 2) #'<)", "(1 2 3)"),
        // A builtin call in operator position is unaffected.
        ("(car '(9 8))", "9"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-ok5): LOOP ALWAYS/NEVER return T by vacuous truth when the
/// range is empty (the test is never evaluated, CLHS 6.1.6). The default was
/// seeded lazily inside the per-iteration body, so an empty range wrongly
/// returned NIL. THEREIS still defaults to NIL.
#[test]
fn loop_always_never_vacuous_truth() {
    let cases = [
        // Empty range: ALWAYS/NEVER are vacuously true.
        ("(loop for i from 0 below 0 always nil)", "T"),
        ("(loop for i from 0 below 0 always (> i 5))", "T"),
        ("(loop for i from 0 below 0 never t)", "T"),
        // THEREIS over an empty range is NIL (nothing found).
        ("(loop for i from 0 below 0 thereis t)", "NIL"),
        // Non-empty ranges keep their normal short-circuit semantics.
        ("(loop for i from 0 below 3 always (< i 5))", "T"),
        ("(loop for i from 0 below 3 always (< i 2))", "NIL"),
        ("(loop for i from 0 below 3 never (> i 5))", "T"),
        ("(loop for i from 0 below 3 never (> i 1))", "NIL"),
        ("(loop for i in '(1 2 3) thereis (> i 2))", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(
            got, expected,
            "{expr} => expected {expected}, got: {got} (full: {stdout:?})"
        );
    }
}

/// Regression (bliss-cm0): CONCATENATE with a VECTOR result-type returns a
/// VECTOR, not a LIST. The result type was matched against a hardcoded intern
/// index that the VECTOR symbol did not reliably occupy, so `(concatenate
/// 'vector …)` silently produced a list; matched by symbol NAME now.
#[test]
fn concatenate_vector_result_type() {
    let cases = [
        ("(vectorp (concatenate 'vector #(1 2) #(3 4)))", "T"),
        ("(vectorp (concatenate 'vector '(1 2) '(3 4)))", "T"),
        ("(vectorp (concatenate '(vector t) '(1 2) '(3 4)))", "T"),
        ("(concatenate 'vector #(1 2) '(3))", "#(1 2 3)"),
        // String and list result types are unaffected.
        ("(concatenate 'string \"ab\" \"cd\")", "abcd"),
        ("(concatenate 'list #(1 2) '(3))", "(1 2 3)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-5ir, bliss-omw): further gaps found making cl-ppcre's
/// matcher run correctly.
///  - `#'foo` / a closure is FUNCTIONP, TYPEP FUNCTION, and TYPE-OF FUNCTION, and
///    a `(function)` method specializer dispatches on it.
///  - PSETF assigns in parallel (a later place sees the earlier place's OLD
///    value) — cl-ppcre's parser splices sequence elements with
///    `(psetf last-cdr cons (cdr last-cdr) cons)`.
///  - Variadic char comparisons `(char<= lo c hi)`.
///  - ADJUST-ARRAY grows an adjustable char vector in place.
#[test]
fn cl_ppcre_matcher_regressions() {
    let cases = [
        // Function-ness of #' (for a user function) and closures. (Builtins like
        // #'car have no reified cell yet — a separate, out-of-scope gap.)
        ("(progn (defun ftest (x) x) (functionp #'ftest))", "T"),
        (
            "(progn (defun ftest2 (x) x) (type-of #'ftest2))",
            "FUNCTION",
        ),
        ("(typep (lambda (x) x) 'function)", "T"),
        ("(type-of (lambda (x) x))", "FUNCTION"),
        (
            "(let ((f (lambda (x) x))) (list (atom f) (consp f) (listp f) (typep f 'atom) (typep f 'cons) (typep f 'list)))",
            "(T NIL NIL T NIL NIL)",
        ),
        // A (function) method specializer must dispatch on a closure.
        (
            "(progn (defgeneric gg (x)) (defmethod gg ((f function)) :got-fn) \
               (defmethod gg ((x integer)) :got-int) \
               (list (gg (lambda () 1)) (gg 5)))",
            "(:GOT-FN :GOT-INT)",
        ),
        // PSETF parallel semantics: swap, and the cl-ppcre splice pattern.
        ("(let ((x 1) (y 2)) (psetf x y y x) (list x y))", "(2 1)"),
        (
            "(let* ((a (list 1)) (lc a) (c (list 99))) \
               (psetf lc c (cdr lc) c) a)",
            "(1 99)",
        ),
        // Variadic char comparisons (range test).
        ("(char<= #\\0 #\\5 #\\9)", "T"),
        ("(char<= #\\0 #\\a #\\9)", "NIL"),
        ("(char= #\\a #\\a #\\a)", "T"),
        // ADJUST-ARRAY grows an adjustable string in place.
        (
            "(let ((s (make-array 1 :element-type 'character :fill-pointer t :adjustable t))) \
               (setf (char s 0) #\\a) (adjust-array s 3 :fill-pointer 3) \
               (setf (char s 1) #\\b) (setf (char s 2) #\\c) (coerce s 'simple-string))",
            "\"abc\"",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout} (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Regression (bliss-b1o): `defpackage :shadow` (and the `shadow` function) must
/// intern a NEW symbol home-owned by the shadowing package, distinct from the
/// same-named inherited (e.g. CL:) symbol — not a plain intern that returns the
/// inherited symbol. cl-ppcre `(:shadow :defconstant)` then defines a macro whose
/// body expands to `cl:defconstant`; if the two names stayed EQ the macro
/// re-invoked itself → infinite expansion → SIGSEGV while loading util.lisp.
/// Verifies distinctness, home package, PACKAGE-SHADOWING-SYMBOLS, and that the
/// self-referential macro pattern terminates.
#[test]
fn defpackage_shadow_interns_distinct_symbol() {
    let dir = std::env::temp_dir().join("egcl_test_shadow");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("shadow.lisp");
    // Separate top-level forms: IN-PACKAGE must take effect (at read time of the
    // *next* form) before the shadowed name is read, so this cannot be one progn.
    std::fs::write(
        &file_path,
        "(defpackage :shtest (:use :cl) (:shadow :defconstant))\n\
         (in-package :shtest)\n\
         (format t \"~&DISTINCT=~s~%\" (eq 'cl:defconstant 'defconstant))\n\
         (format t \"~&HOME=~s~%\" (package-name (symbol-package 'defconstant)))\n\
         (format t \"~&SHADOWING=~s~%\" (mapcar #'symbol-name (package-shadowing-symbols :shtest)))\n\
         (defmacro defconstant (n v &optional d) (declare (ignore d)) (list 'cl:defconstant n v))\n\
         (defconstant +ok+ 42)\n\
         (format t \"~&VALUE=~s~%\" +ok+)\n",
    )
    .expect("write shadow test file");

    let out = egcl_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(
        out.status.code(),
        Some(0),
        "shadow load should exit 0 (not SIGSEGV)"
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_uppercase();
    assert!(
        stdout.contains("DISTINCT=NIL"),
        "cl:defconstant must differ from shadowed defconstant, got: {stdout}"
    );
    assert!(
        stdout.contains("HOME=\"SHTEST\""),
        "shadowed symbol home package must be SHTEST, got: {stdout}"
    );
    assert!(
        stdout.contains("SHADOWING=(\"DEFCONSTANT\")"),
        "package-shadowing-symbols must list DEFCONSTANT, got: {stdout}"
    );
    assert!(
        stdout.contains("VALUE=42"),
        "self-referential defconstant macro must terminate, got: {stdout}"
    );
}

/// Regression (bliss-255): the tree-walker's FUNCALL and TAGBODY arms held a
/// `EgclVal` across an allocation that can fire a relocating minor GC, without
/// rooting it — the classic invariant-#1 violation (AGENTS.md "GC safety"):
///   - FUNCALL: the callee `fn_val` sat unrooted across `eval_args` (which
///     allocates), so a moved young closure went stale → "Cannot apply: Cons(..)".
///   - TAGBODY: the `items` statement `Vec<EgclVal>` was unrooted across the
///     per-statement `eval_form`, so a moved statement form read back as a zeroed
///     cons (car => Fixnum(0)) → "undefined function:".
///
/// Both surfaced deep in cl-ppcre's tagbody-heavy, `(funcall next-fn ..)` CPS
/// scanner closures. `EGCL_GC_STRESS` fires a minor GC on (almost) every
/// allocation, turning the load-dependent corruption into a deterministic one;
/// the callee/statements must be freshly allocated (young) at the vulnerable
/// point, and TAGBODY needs a moderate stride so its fresh form is not promoted
/// out of the nursery before the collection. Each program computes a known total
/// that only comes out right if nothing went stale.
#[test]
fn gc_roots_funcall_callee_and_tagbody_statements() {
    // FUNCALL: `gg` is a fresh young closure; the `(list ii ii)` argument
    // allocates (firing the GC) while `gg` is the in-flight callee. eval forces
    // the tree-walker path. Sum of (i+i+i) for i in 0..59 = 3*1770 = 5310.
    //
    // `gg`/`ii` are SPECIAL, not caller lexicals: EVAL evaluates in the null
    // lexical environment (bliss-e57h), so a lexical `g` is correctly invisible
    // to the EVAL'd form — SBCL signals UNBOUND-VARIABLE on the earlier
    // formulation of this program, which had been written against EGCL's old
    // leaking EVAL. The GC-root coverage is unchanged: still a young closure as
    // the in-flight callee with an allocating argument.
    let funcall_prog = "(defvar gg nil) (defvar ii 0) \
       (let ((s 0)) \
         (dotimes (i 60) \
           (setq ii i) \
           (setq gg (let ((k i)) (lambda (p) (+ k (car p) (cadr p))))) \
           (setf s (+ s (eval '(funcall gg (list ii ii)))))) \
         (format t \"~s\" s))";
    // TAGBODY: a freshly-consed tagbody form each iteration; a statement allocates
    // mid-body. zz = (cons 0 (list 1 2 3 4 5)) => length 6, over 200 iters = 1200.
    let tagbody_prog = "(defvar zz nil) \
       (let ((s 0)) \
         (dotimes (i 200) \
           (let ((form (list 'tagbody \
                             (list 'setq 'zz (list 'list 1 2 3 4 5)) \
                             (list 'go 'done) 'done \
                             (list 'setq 'zz (list 'cons 0 'zz))))) \
             (eval form) \
             (setf s (+ s (length zz))))) \
         (format t \"~s\" s))";
    for (prog, stride, expected) in [(funcall_prog, "1", "5310"), (tagbody_prog, "17", "1200")] {
        let out = egcl_bin()
            .env("EGCL_GC_STRESS", stride)
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "GC-stress program should exit 0, not crash (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "expected {expected} under EGCL_GC_STRESS={stride}, got: {stdout} (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Regression (bliss-7bi): the numeric comparators `< > <= >= = /=` are variadic
/// in CL — `(op x1 … xn)` holds iff every adjacent pair satisfies `op` (and `/=`
/// iff all pairs are distinct); one argument is always true. They previously
/// compared only the first two arguments, so `(= 2 2 1)` and `(< 1 2 1)` wrongly
/// returned T and `(< 5)` errored. cl-ppcre's REPETITION dispatch does
/// `(= minimum maximum 1)`; with the bug, `b{2}` (min=max=2) matched
/// `(= 2 2 1)` => T and signalled "Got REPETITION with MAXIMUM 1 and MINIMUM 1",
/// so all bounded repetitions `{n}`/`{n,m}`/`{n,}` were unusable.
#[test]
fn variadic_numeric_comparisons() {
    let cases = [
        // = : all-equal
        ("(= 2 2 2)", "T"),
        ("(= 2 2 1)", "NIL"),
        ("(= 2 2.0 2)", "T"),
        // < / > : strictly monotonic across every adjacent pair
        ("(< 1 2 3)", "T"),
        ("(< 1 2 2)", "NIL"),
        ("(< 1 2 1)", "NIL"),
        ("(> 3 2 1)", "T"),
        ("(> 3 2 3)", "NIL"),
        // <= / >= : weakly monotonic
        ("(<= 1 2 2 3)", "T"),
        ("(>= 3 3 2 2)", "T"),
        ("(<= 1 3 2)", "NIL"),
        // /= : pairwise distinct (not just adjacent)
        ("(/= 1 2 3)", "T"),
        ("(/= 1 2 1)", "NIL"),
        // single argument is always true (after a numeric type check)
        ("(< 5)", "T"),
        ("(= 5)", "T"),
        ("(/= 5)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Match the exact printed token so "NIL" does not satisfy a "T" expectation.
        let got = stdout.trim().to_uppercase();
        assert!(
            got.split_whitespace().any(|t| t == expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-sdd): a DEFUN or DEFMETHOD written inside a user binding
/// form closes over that lexical environment (CLHS 3.1.2.1.3), e.g. cl-ppcre's
/// `(let ((reg-scanner …)) (defmethod build-replacement-template …))`. egcl
/// previously ran the body with no captured environment, so the free lexical was
/// unbound. The capture must fire ONLY for genuine nesting, not for a top-level
/// (or nested-LOAD top-level) definition — otherwise every library function would
/// wrongly run against the load frame.
#[test]
fn defun_and_defmethod_close_over_enclosing_lexicals() {
    let dir = std::env::temp_dir().join("egcl_test_sdd");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("sdd.lisp");
    std::fs::write(
        &file_path,
        // Separate top-level forms so each is evaluated at the load top level.
        "(let ((x 42)) (defun f () x))\n\
         (format t \"~&F=~s~%\" (f))\n\
         (let ((c 0)) (defun inc () (incf c)) (defun cur () c))\n\
         (inc) (inc)\n\
         (format t \"~&CUR=~s~%\" (cur))\n\
         (let ((y 10)) (defmethod gm ((n integer)) (+ n y)))\n\
         (format t \"~&GM=~s~%\" (gm 5))\n\
         (defun top () 7)\n\
         (format t \"~&TOP=~s~%\" (top))\n",
    )
    .expect("write sdd test file");
    let out = egcl_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(
        out.status.code(),
        Some(0),
        "sdd load should exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).to_uppercase();
    assert!(
        stdout.contains("F=42"),
        "defun closes over x, got: {stdout}"
    );
    assert!(
        stdout.contains("CUR=2"),
        "two defuns share captured mutable c, got: {stdout}"
    );
    assert!(
        stdout.contains("GM=15"),
        "defmethod closes over y, got: {stdout}"
    );
    assert!(
        stdout.contains("TOP=7"),
        "a top-level defun still works (no spurious capture), got: {stdout}"
    );
}

/// Regression (bliss-sdd follow-ons found via cl-ppcre regex-replace): WRITE-STRING
/// / WRITE-LINE must honor the `:start`/`:end` bounding keywords (cl-ppcre stitches
/// replacement output with them; ignoring them emitted the whole string each time),
/// and ARRAY-DIMENSION (singular, axis 0) returns a vector's length.
#[test]
fn write_string_bounds_and_array_dimension() {
    let cases = [
        (
            "(with-output-to-string (s) (write-string \"hello world\" s :start 2 :end 5))",
            "\"llo\"",
        ),
        (
            "(with-output-to-string (s) (write-string \"abcdef\" s :start 3))",
            "\"def\"",
        ),
        (
            "(with-output-to-string (s) (write-string \"abcdef\" s))",
            "\"abcdef\"",
        ),
        ("(array-dimension (vector 1 2 3 4) 0)", "4"),
        ("(array-dimension (make-array 5 :initial-element 0) 0)", "5"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-x5y.20): compiling method bodies and caching the effective
/// method by argument class must not change CLOS semantics. Exercises repeated
/// calls (cache warm), cross-class dispatch under one generic (distinct cache
/// keys), eql specializers (must bypass the class-keyed cache), subclass
/// precedence + call-next-method, before/after auxiliary methods, and cache
/// invalidation on class definition.
#[test]
fn method_dispatch_cache_preserves_clos_semantics() {
    let cases = [
        // Repeated calls (warm cache) return the same result.
        (
            "(progn (defgeneric g (x)) (defmethod g ((n integer)) (* n 2)) \
               (format t \"~s\" (list (g 1) (g 2) (g 3))))",
            "(2 4 6)",
        ),
        // Two classes under one generic: each class must keep its own decision.
        (
            "(progn (defgeneric h (x)) (defmethod h ((n integer)) :int) \
               (defmethod h ((s string)) :str) \
               (format t \"~s\" (list (h 5) (h \"a\") (h 6) (h \"b\"))))",
            "(:INT :STR :INT :STR)",
        ),
        // Eql specializer: dispatch depends on VALUE, so the class cache must be
        // bypassed — (e 5) and (e 6) differ though both are integers.
        (
            "(progn (defgeneric e (x)) (defmethod e ((x (eql 5))) :five) \
               (defmethod e ((n integer)) :int) \
               (format t \"~s\" (list (e 5) (e 6) (e 5) (e 7))))",
            "(:FIVE :INT :FIVE :INT)",
        ),
        // Subclass precedence + call-next-method (never compiled/cached-away).
        (
            "(progn (defclass a () ()) (defclass b (a) ()) (defgeneric w (x)) \
               (defmethod w ((o a)) (list :a)) \
               (defmethod w ((o b)) (cons :b (call-next-method))) \
               (format t \"~s\" (w (make-instance 'b))))",
            "(:B :A)",
        ),
        // Before/after auxiliary methods run around the primary.
        (
            "(progn (defvar *l* nil) (defgeneric m (x)) \
               (defmethod m ((n integer)) (push :prim *l*) n) \
               (defmethod m :before ((n integer)) (push :before *l*)) \
               (defmethod m :after ((n integer)) (push :after *l*)) \
               (m 1) (m 2) (format t \"~s\" (reverse *l*)))",
            "(:BEFORE :PRIM :AFTER :BEFORE :PRIM :AFTER)",
        ),
        // Defining a new subclass after warming the cache must invalidate it:
        // a `c` (subclass of b) now dispatches to w's b-method, not a's.
        (
            "(progn (defclass a () ()) (defclass b (a) ()) (defgeneric w (x)) \
               (defmethod w ((o a)) :a) (defmethod w ((o b)) :b) \
               (w (make-instance 'a)) \
               (defclass c (b) ()) \
               (format t \"~s\" (w (make-instance 'c))))",
            ":B",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// R5.67 / bliss-uqle: derived CPL reuse must survive real Lisp allocation and
/// invalidate descendants when a superclass is redefined after dispatch warms.
#[test]
fn class_precedence_reuse_preserves_redefinition_under_gc_stress() {
    let program = r#"
      (defclass cpl-a () ())
      (defclass cpl-b () ())
      (defclass cpl-parent (cpl-a) ())
      (defclass cpl-leaf (cpl-parent) ())
      (defgeneric cpl-route (x))
      (defmethod cpl-route ((x cpl-a)) :a)
      (defmethod cpl-route ((x cpl-b)) :b)
      (defun cpl-query (x)
        (let ((order (class-precedence-list (class-of x))))
          (list (cpl-route x)
                (not (null (member (find-class 'cpl-a) order)))
                (not (null (member (find-class 'cpl-b) order))))))
      (let ((instance (make-instance 'cpl-leaf)))
        (dotimes (i 40) (cpl-query instance))
        (let ((before (cpl-query instance)))
          (defclass cpl-parent (cpl-b) ())
          (dotimes (i 40) (cpl-query instance))
          (format t "~&CPL-RESULT ~S~%" (list before (cpl-query instance)))))
    "#;
    for stride in ["0", "1", "7", "31"] {
        let output = egcl_bin()
            // Keep this focused on CLOS; bootstrap itself hits the pre-existing
            // evacuated-nursery verification failure tracked in bliss-vzph.
            .args(["--no-bootstrap", "--no-init", "--eval", program])
            .env("EGCL_GC_STRESS", stride)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .expect("run CPL regression");
        assert!(
            output.status.success(),
            "stride {stride}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("CPL-RESULT ((:A T NIL) (:B NIL T))"),
            "stride {stride}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// Regression (bliss-day): DEFMETHOD with the same qualifier and specializers
/// REPLACES the existing method (CLHS 7.6.2). egcl appended it, leaving the
/// stale method applicable so a redefinition never took effect.
#[test]
fn defmethod_redefinition_replaces() {
    let cases = [
        (
            "(progn (defgeneric r (x)) (defmethod r ((n integer)) :v1) \
            (defmethod r ((n integer)) :v2) (format t \"~s\" (r 1)))",
            ":V2",
        ),
        // Replace only after the cache is warm (invalidation on redefinition).
        (
            "(progn (defgeneric r (x)) (defmethod r ((n integer)) :v1) (r 1) \
            (defmethod r ((n integer)) :v2) (format t \"~s\" (r 1)))",
            ":V2",
        ),
        // A different specializer is a distinct method, not a replacement.
        (
            "(progn (defgeneric d (x)) (defmethod d ((n integer)) :int) \
            (defmethod d ((s string)) :str) (format t \"~s\" (list (d 1) (d \"x\"))))",
            "(:INT :STR)",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-x5y.20): COND is not a macro in egcl — it is lowered
/// directly by the tree-walker and the bytecode compiler — but the macroexpander
/// did not descend into its clauses, so a symbol-macro referenced inside a COND
/// clause (e.g. a WITH-SLOTS slot used in a `(cond ((zerop maximum) …))` test)
/// was left unexpanded and read as an unbound variable once compiled. WHEN/UNLESS
/// /AND/OR/IF were fine (plain-expression subforms); only COND's `(test . body)`
/// clause shape needed handling. This blocked cl-ppcre's create-matcher-aux.
#[test]
fn cond_expands_symbol_macros_in_clauses() {
    let cases = [
        // Slot read in a COND test, in a compiled defun and a compiled method.
        (
            "(progn (defclass r () ((a :initform 7))) \
            (defun f (o) (with-slots (a) o (cond ((eql a 7) :yes) (t :no)))) \
            (format t \"~s\" (f (make-instance 'r))))",
            ":YES",
        ),
        (
            "(progn (defclass r () ((mn :initform 2) (mx :initform 2))) \
            (defgeneric cm (x)) \
            (defmethod cm ((o r)) (with-slots (mn mx) o \
              (cond ((= mn mx 1) :one) ((eql mn mx) :eq) (t :other)))) \
            (format t \"~s\" (cm (make-instance 'r))))",
            ":EQ",
        ),
        // Multiple slots, one used only in a clause body.
        (
            "(progn (defclass r () ((a :initform 3) (b :initform 9))) \
            (defun f (o) (with-slots (a b) o (cond ((eql a 3) b) (t a)))) \
            (format t \"~s\" (f (make-instance 'r))))",
            "9",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-x5y.21): a closure created inside SYMBOL-MACROLET/WITH-SLOTS
/// and invoked AFTER the form exits must still see the symbol-macro expansion.
/// The symbol-macro table is per-Env, not part of the lexical frame a closure
/// captures, so the lazy path lost it for escaping closures (unbound variable).
/// eval_symbol_macrolet now expands the body eagerly (baking the expansion in),
/// respecting inner LET shadowing and QUOTE.
#[test]
fn escaping_closure_sees_symbol_macrolet() {
    let cases = [
        // Escaping closure over a bare symbol-macro.
        (
            "(format t \"~s\" (funcall (funcall (lambda () (symbol-macrolet ((m 42)) (lambda () m))))))",
            "42",
        ),
        // Escaping closure over a WITH-SLOTS slot (the cl-ppcre shape).
        (
            "(progn (defclass k () ((s :initform 5))) \
            (defun mk (o) (with-slots (s) o (lambda () s))) \
            (format t \"~s\" (funcall (mk (make-instance 'k)))))",
            "5",
        ),
        // Inner LET still shadows the symbol-macro.
        (
            "(format t \"~s\" (symbol-macrolet ((m 42)) (let ((m 7)) m)))",
            "7",
        ),
        // QUOTE still suppresses expansion.
        ("(format t \"~s\" (symbol-macrolet ((m 42)) 'm))", "M"),
        // Common with-slots read/write unaffected.
        (
            "(progn (defclass k () ((s :initform 0))) \
            (defun f (o) (with-slots (s) o (setf s 9) s)) \
            (format t \"~s\" (f (make-instance 'k))))",
            "9",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-x5y): a LET/LET* binding declared `(declare (special v))`
/// for a non-earmuffed name must bind v DYNAMICALLY so a nested function's
/// dynamic reference sees it (cl-ppcre's `convert` binds FLAGS specially and
/// calls CONVERT-AUX which reads it). The bytecode compiler bound it lexically —
/// a silent miscompile (unbound in the callee). It now bails such a LET to the
/// tree-walker, which handles it. Also covers `(setf (slot-value obj slot) val)`
/// compiling for ordinary (non-declared-special) functions.
#[test]
fn let_declared_special_and_slot_value_setf() {
    let cases = [
        // Special LET binding seen by a nested function's dynamic reference.
        (
            "(progn (defun helper () (declare (special v)) v) \
            (defun caller () (let ((v 42)) (declare (special v)) (helper))) \
            (format t \"~s\" (caller)))",
            "42",
        ),
        // LET* form (init of a later binding calls the reader), like cl-ppcre convert.
        (
            "(progn (defun helper () (declare (special v)) v) \
            (defun caller () (let* ((v 7) (w (helper))) (declare (special v)) w)) \
            (format t \"~s\" (caller)))",
            "7",
        ),
        // (setf (slot-value ...)) in a compiled defun and method.
        (
            "(progn (defclass k () ((s :initform 0))) \
            (defun f (o v) (setf (slot-value o 's) v) (slot-value o 's)) \
            (format t \"~s\" (f (make-instance 'k) 42)))",
            "42",
        ),
        (
            "(progn (defclass k () ((s :initform 0))) (defgeneric g (o v)) \
            (defmethod g ((o k) v) (setf (slot-value o 's) (* v 2)) (slot-value o 's)) \
            (format t \"~s\" (g (make-instance 'k) 21)))",
            "42",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-x5y): lazy compilation (EGCL_LAZY_COMPILE, now the default
/// — bliss-x5y.24) defers a DEFUN's bytecode compile from definition time to the
/// first call that crosses the invoke threshold — cold functions never compile
/// (removing the load-time penalty that made the bytecode backend slower than the
/// tree-walker on asdf.lisp). Results must be identical to eager compilation, and
/// a hot, repeatedly-called function must still compile and give the right answer.
/// This test pins the mode/threshold explicitly so it stays meaningful regardless
/// of the default.
#[test]
fn lazy_compile_preserves_results() {
    let cases = [
        // A hot recursive function crosses the threshold and compiles mid-run.
        (
            "(progn (defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) \
            (format t \"~s\" (fib 20)))",
            "6765",
        ),
        // Cold function (called once) stays interpreted but still correct.
        (
            "(progn (defun once (x) (* x x)) (format t \"~s\" (once 9)))",
            "81",
        ),
        // Redefinition after warmup takes effect (lazy state cleared).
        (
            "(progn (defun g (x) (* x 2)) (dotimes (i 20) (g i)) \
            (defun g (x) (* x 3)) (format t \"~s\" (g 10)))",
            "30",
        ),
    ];
    for (prog, expected) in cases {
        let out = egcl_bin()
            .env("EGCL_LAZY_COMPILE", "1")
            .env("EGCL_LAZY_THRESHOLD", "4")
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{prog} should exit 0 (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "expected {expected}, got: {stdout}"
        );
    }
}

#[test]
fn lazy_compile_does_not_leave_capturing_closures_tree_walked() {
    let program = "(progn \
        (defun outer (x) (funcall (lambda (y) (+ x y)) 1)) \
        (dotimes (i 20) (outer i)) \
        (disassemble 'outer))";
    let out = egcl_bin()
        .env("EGCL_LAZY_COMPILE", "1")
        .env("EGCL_LAZY_THRESHOLD", "4")
        .env("EGCL_T0_T1_THRESHOLD", "100000")
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    assert!(
        out.status.success(),
        "lazy compile run failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("MakeClosure"),
        "capturing closure should be compiled as nested bytecode: {stdout}"
    );
    assert!(
        !stdout.contains("MakeClosureEnv"),
        "capturing closure fell back to eval_form and will stay tree-walked: {stdout}"
    );
}

/// Regression: DOTIMES/DOLIST establish an implicit `block nil`, so `(return x)`
/// in the body exits the loop with x. Previously this errored "no block named
/// NIL", breaking the ubiquitous (dolist (x l) (when … (return …))) pattern.
#[test]
fn return_exits_dotimes_and_dolist() {
    let cases = [
        ("(dotimes (i 5 :done) (when (= i 2) (return :hit)))", "HIT"),
        (
            "(dolist (x '(a b c) :done) (when (eq x 'b) (return x)))",
            "B",
        ),
        ("(dotimes (i 5 :done) nil)", "DONE"),
        (
            "(block nil (dotimes (i 10) (when (> i 3) (return-from nil i))))",
            "4",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
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
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
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
        (
            "(find 2 '((2 a) (2 b)) :key (function car) :from-end t)",
            "(2 B)",
        ),
        ("(position #\\a \"banana\" :start 2)", "3"),
        (
            "(sort (list (list 2) (list 1)) (function <) :key (function car))",
            "((1) (2))",
        ),
        (
            "(remove \"x\" '(\"a\" \"x\" \"b\") :test (function equal))",
            "(\"a\" \"b\")",
        ),
        (
            "(remove 1 '((1 a) (2 b) (1 c)) :key (function car) :test-not (function =))",
            "((1 A) (1 C))",
        ),
        // The unsupported-function path must be catchable, never abort.
        (
            "(ignore-errors (find 2 '((1 a) (2 b)) :key (function car)))",
            "(2 B)",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(
            out.status.code(),
            Some(0),
            "{expr} should exit 0 (no panic/abort)"
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
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
        // bliss-9kc: … and, crucially, a restart established INSIDE the handler-bind
        // body, for a RAW evaluator error. The raw TYPE-ERROR is now signalled
        // in-context (before the RESTART-CASE is disestablished), so the handler can
        // reach the K restart established between it and the signal — which the old
        // post-unwind handler path could not.
        (
            "(handler-bind ((type-error (lambda (e) (declare (ignore e)) (invoke-restart 'k)))) \
               (restart-case (car 5) (k () :ok)))",
            ":OK",
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
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
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
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
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
    let caught = egcl_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
               (storage-condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(
        caught.status.code(),
        Some(0),
        "overflow should be catchable, exit 0"
    );
    assert!(
        String::from_utf8_lossy(&caught.stdout)
            .to_uppercase()
            .contains("CAUGHT"),
        "storage-condition clause should fire: {}",
        String::from_utf8_lossy(&caught.stdout)
    );

    // …and as the CONDITION superclass.
    let via_super = egcl_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f () (f))) (f)) \
               (condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run egcl");
    assert!(
        String::from_utf8_lossy(&via_super.stdout)
            .to_uppercase()
            .contains("CAUGHT"),
        "condition clause should fire on overflow"
    );

    // Uncaught overflow must still exit gracefully — a normal exit code, never a
    // signal (status.code() == None would mean the process was killed, e.g. by
    // SIGSEGV/SIGABRT).
    let uncaught = egcl_bin()
        .args(["--eval", "(labels ((f () (f))) (f))"])
        .output()
        .expect("run egcl");
    assert!(
        uncaught.status.code().is_some(),
        "uncaught stack overflow must not kill the process with a signal"
    );

    // A recursion depth well within budget still runs normally.
    let ok = egcl_bin()
        .args([
            "--eval",
            "(labels ((down (n) (if (= n 0) :done (down (- n 1))))) (down 1500))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(ok.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&ok.stdout)
            .to_uppercase()
            .contains("DONE")
    );
}

/// bliss-j5fo: a HANDLER-BIND handler must fire for a STORAGE-CONDITION (stack
/// overflow). Since bliss-9kc ordinary raw errors signal in-context, so the
/// bytecode HandlerBind frame's post-unwind handler path is exercised *only* by
/// storage conditions (Oom / StackOverflow), which are excluded from the
/// allocating in-context signal. This locks that path in so it is not mistaken
/// for dead code — the handler transfers out via THROW to prove it ran.
#[test]
fn handler_bind_handler_fires_for_storage_condition() {
    let caught = egcl_bin()
        .args([
            "--eval",
            "(catch 'out \
               (handler-bind ((storage-condition \
                                (lambda (c) (declare (ignore c)) (throw 'out :ovf)))) \
                 (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0))))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(
        caught.status.code(),
        Some(0),
        "handler-bind on storage-condition should transfer out cleanly (stderr: {})",
        String::from_utf8_lossy(&caught.stderr)
    );
    assert!(
        String::from_utf8_lossy(&caught.stdout)
            .to_uppercase()
            .contains("OVF"),
        "handler-bind storage-condition handler should have fired: {}",
        String::from_utf8_lossy(&caught.stdout)
    );
}

/// Regression: PSETQ, DO, and DO* iteration macros (bliss-2pt.11).
#[test]
fn do_dostar_psetq() {
    let cases = [
        ("(let ((a 1) (b 2)) (psetq a b b a) (list a b))", "(2 1)"),
        (
            "(do ((i 0 (1+ i)) (acc nil)) ((= i 3) (reverse acc)) (push i acc))",
            "(0 1 2)",
        ),
        (
            "(do* ((i 0 (1+ i)) (j (* i 10) (* i 10))) ((= i 3) j))",
            "30",
        ),
        ("(do ((i 0 (1+ i)) (s 0 (+ s i))) ((= i 5) s))", "10"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Fixed-width simple strings (SBCL model, spec §1.6.3 / bliss-pd0): O(1)
/// character indexing and mutation, and full Unicode — `LENGTH` counts
/// characters, `CHAR` returns code points, `(SETF CHAR)` stores any code point,
/// and content is preserved through read/print and reverse/concatenate.
#[test]
fn fixed_width_unicode_strings() {
    let cases = [
        ("(length \"héllo\")", "5"),
        ("(char-code (char \"λμν\" 1))", "956"),
        ("(char \"héllo\" 1)", "é"),
        // (SETF CHAR) stores a wide code point into a (default character) string.
        (
            "(let ((s (make-string 3 :initial-element #\\a))) (setf (char s 1) #\\λ) s)",
            "aλa",
        ),
        ("(reverse \"aλb\")", "bλa"),
        ("(concatenate 'string \"ab\" \"λ\")", "abλ"),
        // Content equality is by character, independent of storage.
        ("(equal \"héllo\" (copy-seq \"héllo\"))", "T"),
        ("(string= \"abλ\" (concatenate 'string \"ab\" \"λ\"))", "T"),
        // Read/print round-trip of a Unicode literal.
        ("(read-from-string (prin1-to-string \"héλλo\"))", "héλλo"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => want {expected}, got: {stdout}"
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
        // NTH/NTHCDR reject a negative index with a catchable TYPE-ERROR
        // instead of silently returning element 0 / the whole list (bliss-novy).
        (
            "(handler-case (nth -1 '(1 2 3)) (type-error () :ok))",
            ":OK",
        ),
        (
            "(handler-case (nthcdr -1 '(1 2 3)) (type-error () :ok))",
            ":OK",
        ),
        ("(nth 2 '(a b c d))", "C"),
        // MAKE-LIST / MAKE-ARRAY reject a negative size with a catchable
        // TYPE-ERROR instead of a silently-empty result (bliss-czum).
        ("(handler-case (make-list -1) (type-error () :ok))", ":OK"),
        ("(handler-case (make-array -1) (type-error () :ok))", ":OK"),
        ("(make-list 0)", "NIL"),
        // ASSOC honors :test and :key (previously both were dropped, so a
        // :test '= match on a float and a :key transform silently failed).
        ("(assoc 2.0 '((1 . a) (2 . b)) :test '=)", "(2 . B)"),
        ("(assoc 3 '((1 . a) (2 . b) (3 . c)) :key '1+)", "(2 . B)"),
        ("(assoc 2 '((1 . a) (2 . b)) :test-not '=)", "(1 . A)"),
        ("(mapcan (function list) '(1 2 3))", "(1 2 3)"),
        ("(getf '(:a 1 :b 2) :b)", "2"),
        ("(nreverse (list 1 2 3))", "(3 2 1)"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
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
        // GCD/LCM require integers: a ratio/float argument is a catchable
        // TYPE-ERROR, not a silently-computed result (bliss-fvni).
        ("(handler-case (gcd 1/2 1/3) (type-error () :ok))", ":OK"),
        ("(handler-case (lcm 3 1.0) (type-error () :ok))", ":OK"),
        // MIN/MAX with no arguments is a catchable PROGRAM-ERROR (bliss-pxim).
        ("(handler-case (min) (program-error () :ok))", ":OK"),
        ("(handler-case (max) (program-error () :ok))", ":OK"),
        ("(string-trim \" \" \"  hi  \")", "\"hi\""),
        ("(string-left-trim \" x\" \"xx hi\")", "\"hi\""),
        ("(string-right-trim \" \" \"hi  \")", "\"hi\""),
        // PARSE-INTEGER on non-integer junk signals a catchable PARSE-ERROR,
        // not a plain SIMPLE-ERROR (bliss-dxyd). Valid parses are unaffected.
        (
            "(handler-case (parse-integer \"12x\") (parse-error () :ok))",
            ":OK",
        ),
        ("(parse-integer \"  42  \")", "42"),
        ("(parse-integer \"12x\" :junk-allowed t)", "12"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
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
        ("(format nil \"~,0f\" 3.7)", "\"4.\""),
        ("(format nil \"~f\" 2.5)", "\"2.5\""),
        // `v` prefix params consume args in directive order BEFORE the value, so
        // ~v,vF binds width/digits to 6/2 and the value is 3.14159, not the other
        // way round (previously errored "float is not integer"; bliss-8rs1).
        ("(format nil \"~v,vf\" 6 2 3.14159)", "\"  3.14\""),
        ("(format nil \"~vf\" 8 3.14159)", "\" 3.14159\""),
        // Same fix for ~E and ~$.
        ("(format nil \"~v,v$\" 2 8 3.5)", "\"00000003.50\""),
        ("(format nil \"~v$\" 4 3.14159)", "\"3.1416\""),
        ("(format nil \"~v,ve\" 10 3 31.4159)", "3.142e+1"),
        // ~E/~G honour their width param, right-justified with space pad; ~G
        // consumes its `v` prefix param rather than mis-binding the value
        // (bliss-o30e).
        ("(format nil \"~10,3e\" 31.4159)", "\"  3.142e+1\""),
        ("(format nil \"~10g\" 31.4159)", "\"   31.4159\""),
        ("(format nil \"~vg\" 10 31.4159)", "\"   31.4159\""),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

/// Regression: FORMAT ~D honors its 3rd/4th params (comma character and comma
/// interval); previously both were ignored and it always grouped by 3 with ','.
/// (bliss-ycv8)
#[test]
fn format_decimal_comma_params() {
    let cases = [
        ("(format nil \"~:d\" 1234567)", "\"1,234,567\""),
        ("(format nil \"~,,' ,4:d\" 12345678)", "\"1234 5678\""),
        ("(format nil \"~,,'.:d\" 1234567)", "\"1.234.567\""),
        ("(format nil \"~,,,2:d\" 1234)", "\"12,34\""),
        ("(format nil \"~:d\" -1234567)", "\"-1,234,567\""),
        // ~[ honors the ~:; default/else clause when the index is out of range
        // (previously produced empty output). (bliss-mrmv)
        ("(format nil \"~[zero~;one~:;many~]\" 5)", "\"many\""),
        ("(format nil \"~[zero~;one~:;many~]\" 0)", "\"zero\""),
        ("(format nil \"~[zero~;one~:;many~]\" 1)", "\"one\""),
        // No ~:; default => an out-of-range index selects nothing.
        ("(format nil \"~[a~;b~;c~]\" 9)", "\"\""),
        // ~N{ caps the iteration count (previously looped over all args).
        // (bliss-flth)
        ("(format nil \"~2{~a ~}\" '(1 2 3 4))", "\"1 2 \""),
        ("(format nil \"~0{~a ~}\" '(1 2 3))", "\"\""),
        ("(format nil \"~2@{~a ~}\" 1 2 3 4)", "\"1 2 \""),
        ("(format nil \"~{~a ~}\" '(1 2 3 4))", "\"1 2 3 4 \""),
        // ~#[ dispatches on the remaining-arg count (was an uncatchable error),
        // and an explicit ~n[ prefix param selects without consuming an arg.
        // (bliss-qzxu)
        ("(format nil \"~#[none~;one~:;many~]\")", "\"none\""),
        ("(format nil \"~#[none~;one~:;many~]\" 'a)", "\"one\""),
        (
            "(format nil \"~#[none~;one~:;many~]\" 'a 'b 'c)",
            "\"many\"",
        ),
        ("(format nil \"~2[a~;b~;c~]\")", "\"c\""),
        // ~D/~X/~B render bignums (previously type-errored on non-fixnums,
        // though ~A/PRINT rendered them). (bliss-0mh7)
        (
            "(format nil \"~d\" (expt 2 70))",
            "\"1180591620717411303424\"",
        ),
        (
            "(format nil \"~:d\" (expt 2 70))",
            "\"1,180,591,620,717,411,303,424\"",
        ),
        ("(format nil \"~x\" (expt 2 70))", "\"400000000000000000\""),
        (
            "(format nil \"~d\" (- (expt 2 70)))",
            "\"-1180591620717411303424\"",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
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
        let out = egcl_bin()
            .args(["--eval", expr])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout
                .trim()
                .to_uppercase()
                .contains(&expected.to_uppercase()),
            "{expr} => expected {expected}, got: {stdout}"
        );
    }
}

// ══════════════════════════════════════════════════════════════════
// Gray streams — CLOS stream hierarchy + generic-function dispatch
// (egcl-jtc.7b). A user-defined FUNDAMENTAL-STREAM subclass must
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
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
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LB6A T T"),
        "re-exported inherited symbol should stay EQ with home LB6A, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn bare_read_in_package_shares_value_cell_with_find_symbol() {
    // bliss-lb6.12: a symbol read BARE inside its home package (e.g. a DEFVAR)
    // and the same symbol reached via FIND-SYMBOL must be ONE symbol with ONE
    // value cell — not two name-keyed twins with split cells. This blocked the
    // ASDF load footer, where (asdf-version) does
    //   (symbol-value (find-symbol "*ASDF-VERSION*" :asdf))
    // against a value bound by a bare DEFVAR in ASDF/UPGRADE. The forms are
    // separate top-level reads so IN-PACKAGE takes effect before the bare read.
    let prog = "(defpackage :lb612 (:use :cl) (:export #:*v*)) \
                (in-package :lb612) \
                (defvar *v* 42) \
                (in-package :cl-user) \
                (format t \"~a\" (symbol-value (find-symbol \"*V*\" :lb612)))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "bare DEFVAR and FIND-SYMBOL should share the value cell, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn key_param_named_like_exported_symbol_still_matches_bare_keyword() {
    // bliss-lb6.12 regression guard: a &key parameter whose name is an exported
    // (hence package-qualified) symbol of its own package must still match a
    // bare :keyword at the call site — the keyword indicator is the variable's
    // bare name regardless of its home package.
    let prog = "(defpackage :lb612k (:use :cl) (:export #:f #:wilden)) \
                (in-package :lb612k) \
                (defun f (&key wilden) wilden) \
                (in-package :cl-user) \
                (format t \"~a\" (lb612k:f :wilden 7))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains('7'),
        "exported-symbol &key param should accept a bare keyword, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn hash_p_literal_is_a_real_pathname() {
    // #P"…" must produce the system's registry-backed PATHNAME (recognised by
    // PATHNAMEP / NAMESTRING / LOAD), not the reader's own object that the rest
    // of the system treats as a non-pathname.
    let prog = "(let ((p #P\"/tmp/x\")) (format t \"~a ~a\" (pathnamep p) (namestring p)))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("T /tmp/x"),
        "#P should be a pathname with a namestring, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn let_dynamically_binds_special_variables() {
    // bliss-lb6.14: a LET on a special (earmuffed) variable must establish a
    // DYNAMIC binding visible to called functions, not a lexical one — otherwise
    // `(let ((*x* v)) (helper))` where HELPER reads *x* sees the global value,
    // which broke ASDF's session cache (a let-bound special read by helpers).
    // Uses a global defun (compiled on the bytecode backend) plus a closure, to
    // cover both execution paths; the value is captured into a var so the whole
    // expression is a compilable top-level form.
    let prog = "(defvar *dv* :outer) \
                (defun reader () *dv*) \
                (defvar *r1* (let ((*dv* :inner)) (reader))) \
                (defvar *r2* (let ((f (lambda () *dv*))) (let ((*dv* :dyn)) (funcall f)))) \
                (format t \"~a ~a ~a\" *r1* *r2* *dv*)";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("INNER DYN OUTER"),
        "let must dynamically bind specials (seen by callees) and restore on exit, \
         got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reduce_honors_explicit_nil_initial_value() {
    // bliss-lb6.14: (reduce f '() :initial-value nil) must return NIL, not call f
    // with zero args — an explicit :initial-value nil differs from an omitted one.
    let prog = "(format t \"~a|~a\" \
                (reduce (function +) '() :initial-value nil) \
                (reduce (lambda (a b) (list a b)) '(1 2 3) :initial-value nil))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL|"),
        "reduce with :initial-value nil on empty seq should return NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// (setf (subseq place …) new) works on lists, not just vectors/strings —
/// set_elt rejects lists, so the list case walks the spine in place with
/// REPLACE semantics (bounded by the shorter of place-slice / new). (bliss-mnee)
#[test]
fn setf_subseq_on_a_list() {
    let cases = [
        (
            "(let ((l (list 1 2 3 4))) (setf (subseq l 1 3) '(20 30)) l)",
            "(1 20 30 4)",
        ),
        (
            "(let ((l (list 1 2 3 4))) (setf (subseq l 1) '(20 30 40)) l)",
            "(1 20 30 40)",
        ),
        // New shorter than the slice: only its elements are stored.
        (
            "(let ((l (list 1 2 3 4))) (setf (subseq l 1 3) '(9)) l)",
            "(1 9 3 4)",
        ),
        // New longer than the destination: bounded by the list length.
        (
            "(let ((l (list 1 2 3))) (setf (subseq l 0) '(7 8 9 10)) l)",
            "(7 8 9)",
        ),
        // Vectors still work.
        (
            "(let ((v (vector 1 2 3 4))) (setf (subseq v 1 3) #(20 30)) v)",
            "#(1 20 30 4)",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// STRING= / STRING-EQUAL honor the :start1/:end1/:start2/:end2 bounding
/// keywords, comparing only the delimited substrings (they were ignored before,
/// so a bounded compare of equal substrings returned NIL). (bliss-cnzs)
#[test]
fn string_equal_honors_bounds_keywords() {
    let cases = [
        ("(string= \"xabcy\" \"abc\" :start1 1 :end1 4)", "T"),
        ("(string= \"abc\" \"xabcy\" :start2 1 :end2 4)", "T"),
        ("(string= \"abcd\" \"abce\" :end1 3 :end2 3)", "T"),
        ("(string= \"abc\" \"abd\")", "NIL"),
        ("(string-equal \"XABCY\" \"abc\" :start1 1 :end1 4)", "T"),
        ("(string-equal \"ABC\" \"abc\")", "T"),
        // STRING< / STRING> honor the bounds too, and report the mismatch index
        // in string1's original coordinates (start1 + offset). (bliss-c62r)
        ("(string< \"Zabc\" \"abd\" :start1 1)", "3"),
        ("(string< \"abc\" \"abd\")", "2"),
        ("(string> \"abd\" \"abc\")", "2"),
        ("(string< \"hello\" \"help\" :end1 3 :end2 3)", "NIL"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// WRITE-TO-STRING honors :base / :radix / :escape on a direct-form call, not
/// just via FUNCALL/APPLY — the interpreter's builtin fast-path used to ignore
/// the keywords and print in base 10. (bliss-xe2p)
#[test]
fn write_to_string_honors_print_control_keywords() {
    let cases = [
        ("(write-to-string 255 :base 16)", "\"FF\""),
        ("(write-to-string 10 :radix t :base 2)", "\"#b1010\""),
        ("(write-to-string 255 :base 16 :radix t)", "\"#xFF\""),
        ("(write-to-string 255)", "\"255\""),
        // Reached through EVAL (the path that previously ignored the keywords).
        ("(eval '(write-to-string 255 :base 16))", "\"FF\""),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// #S(name :slot val …) reads back to a real DEFSTRUCT instance, so a struct
/// round-trips: (equalp s (read (prin1 s))) and its accessors work. Unknown
/// struct names fall back to the reader's legacy object (no crash). (bliss-ipn7)
#[test]
fn defstruct_reads_from_hash_s_syntax() {
    let prog = "(defstruct pt a b) \
                (defstruct (pt3 (:include pt)) c) \
                (print (let ((s (read-from-string \"#S(PT :A 1 :B 2)\"))) \
                         (list (pt-a s) (pt-b s) (typep s 'pt) (typep s 'structure-object)))) \
                (print (let ((s (read-from-string \"#S(PT3 :A 1 :B 2 :C 3)\"))) \
                         (list (pt-a s) (pt3-c s)))) \
                (print (equalp (make-pt :a 1 :b 2) \
                               (read-from-string (prin1-to-string (make-pt :a 1 :b 2)))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("(1 2 T T)"),
        "read #S accessors/typep: {stdout}"
    );
    assert!(
        stdout.contains("(1 3)"),
        "read #S inherited slots: {stdout}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "T"),
        "print/read round-trip equalp: {stdout}"
    );
}

/// DEFSTRUCT instances print in readable #S(NAME :slot val …) syntax (both the
/// prin1 builtin and prin1-to-string agree), with inherited :include slots
/// included in precedence order. (bliss-i1i9)
#[test]
fn defstruct_prints_in_hash_s_syntax() {
    let prog = "(defstruct pt a b) \
                (defstruct (pt3 (:include pt)) c) \
                (print (prin1-to-string (make-pt :a 1 :b 2))) \
                (print (prin1-to-string (make-pt3 :a 1 :b 2 :c 3))) \
                (print (prin1-to-string (make-pt :a \"hi\" :b (list 1 2)))) \
                (print (equal (prin1-to-string (make-pt :a 1 :b 2)) \
                              (with-output-to-string (s) (prin1 (make-pt :a 1 :b 2) s))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("\"#S(PT :A 1 :B 2)\""),
        "simple struct: {stdout}"
    );
    assert!(
        stdout.contains("\"#S(PT3 :A 1 :B 2 :C 3)\""),
        "included struct: {stdout}"
    );
    assert!(
        stdout.contains("\"#S(PT :A \\\"hi\\\" :B (1 2))\""),
        "escaped slots: {stdout}"
    );
    // prin1 builtin and prin1-to-string agree (the two printers don't diverge).
    assert!(
        stdout.lines().any(|l| l.trim() == "T"),
        "cli and stdlib printers should agree: {stdout}"
    );
}

/// TYPEP: a DEFSTRUCT instance is a STRUCTURE-OBJECT and NOT a STANDARD-OBJECT
/// (disjoint), even though egcl builds structs as DEFCLASSes; a plain DEFCLASS
/// instance is the reverse. (bliss-ta0a)
#[test]
fn typep_structure_object_vs_standard_object() {
    let prog = "(defstruct pt a b) \
                (defstruct (pt3 (:include pt)) c) \
                (defclass cl () ()) \
                (print (list \
                  (typep (make-pt) 'structure-object) \
                  (typep (make-pt) 'standard-object) \
                  (typep (make-pt3) 'structure-object) \
                  (typep (make-pt3) 'standard-object) \
                  (typep (make-pt3) 'pt) \
                  (typep (make-instance 'cl) 'standard-object) \
                  (typep (make-instance 'cl) 'structure-object)))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // struct-so, struct-stdo, pt3-so, pt3-stdo, pt3-is-pt, clos-stdo, clos-so
    assert!(
        stdout.contains("(T NIL T NIL T T NIL)"),
        "typep structure-object/standard-object wrong, got: {stdout}"
    );
}

/// EQUALP descends DEFSTRUCT instances slot-by-slot (same type + EQUALP slots),
/// including :include inheritance and case-insensitive string slots, but does
/// NOT descend general CLOS standard-objects (those stay identity-compared).
/// (bliss-rup1)
#[test]
fn equalp_descends_structures_not_clos() {
    let prog = "(defstruct pt a b) \
                (defstruct (pt3 (:include pt)) c) \
                (defclass cl () ((a :initarg :a))) \
                (print (list \
                  (equalp (make-pt :a 1 :b 2) (make-pt :a 1 :b 2)) \
                  (equalp (make-pt :a 1 :b 2) (make-pt :a 1 :b 3)) \
                  (equalp (make-pt :a \"X\" :b 2) (make-pt :a \"x\" :b 2)) \
                  (equalp (make-pt3 :a 1 :b 2 :c 3) (make-pt3 :a 1 :b 2 :c 3)) \
                  (equalp (make-pt3 :a 1 :b 2 :c 3) (make-pt3 :a 1 :b 2 :c 9)) \
                  (equalp (make-pt :a 1 :b 2) (make-pt3 :a 1 :b 2 :c 3)) \
                  (equalp (make-instance 'cl :a 1) (make-instance 'cl :a 1))))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // same, diff-slot, ci-string, included-same, included-diff, cross-type, clos
    assert!(
        stdout.contains("(T NIL T T NIL NIL NIL)"),
        "equalp struct semantics wrong, got: {stdout}"
    );
}

/// PPRINT is bound and behaves as the degenerate (non-pretty) case: a leading
/// fresh newline, the escaped representation, no trailing space, no values.
/// (bliss-nopz — full XP pretty-printing remains a separate epic.)
#[test]
fn pprint_minimal_behavior() {
    let cases = [
        // Leading fresh newline, then the printed list (to a string stream so
        // it doesn't interleave with the test's own output).
        (
            "(equal (with-output-to-string (o) (pprint '(1 2 3) o)) (format nil \"~%(1 2 3)\"))",
            "T",
        ),
        // Escapes strings (prin1-style) and returns no values.
        (
            "(equal (with-output-to-string (o) (pprint \"hi\" o)) (format nil \"~%~s\" \"hi\"))",
            "T",
        ),
        (
            "(multiple-value-list (pprint 5 (make-string-output-stream)))",
            "NIL",
        ),
        ("(fboundp 'pprint)", "T"),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// (setf (getf place indicator) val) updates the plist: overwrite an existing
/// indicator's value in place, or prepend a new indicator/value pair and store
/// the new head back into the place. Was an uncatchable "unsupported place".
/// (bliss-ihqw)
#[test]
fn setf_getf_place() {
    let cases = [
        // New indicator: prepended (ANSI), place updated.
        (
            "(let ((s (list :a 1))) (setf (getf s :b) 2) s)",
            "(:B 2 :A 1)",
        ),
        // Existing indicator: value replaced in place.
        (
            "(let ((s (list :a 1 :b 2))) (setf (getf s :a) 99) s)",
            "(:A 99 :B 2)",
        ),
        // Empty plist.
        ("(let ((s nil)) (setf (getf s :x) 10) s)", "(:X 10)"),
        // Inside a DEFUN (compiled path).
        (
            "(progn (defun gtst () (let ((s (list :a 1))) (setf (getf s :c) 3) s)) (gtst))",
            "(:C 3 :A 1)",
        ),
    ];
    for (expr, expected) in cases {
        let out = egcl_bin()
            .args(["--eval", &format!("(print {expr})")])
            .output()
            .expect("run egcl");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout
            .lines()
            .map(|l| l.trim())
            .find(|l| !l.is_empty())
            .unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got}");
    }
}

/// (setf (cXr place) v) works for composite CAR/CDR accessors even when the
/// form is compiled (inside a defun) or run through EVAL — previously only the
/// top-level interpreter path handled them and the rest hit "unsupported place".
/// (bliss-fpyw)
#[test]
fn setf_composite_cxr_accessors() {
    // Exercised through a DEFUN so the compiled/lowered SETF path is used.
    let prog = "(defun run () \
                  (let ((a (list (list 1 2))) \
                        (b (list 10 20 30 40 50))) \
                    (setf (caar a) 9) \
                    (setf (caddr b) 99) \
                    (setf (cdddr b) (list 8)) \
                    (list a b))) \
                (print (run))";
    let out = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    assert_eq!(
        out.status.code(),
        Some(0),
        "should exit 0 (stderr: {})",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("((9 2)) (10 20 99 8)"),
        "composite cXr setf should mutate in place, got: {stdout}"
    );
}

#[test]
fn setf_of_a_setf_generic_function() {
    // bliss-lb6.14: (setf (place …) v) dispatches to a (defmethod (setf place) …)
    // writer generic — ASDF uses (setf (action-status …) …).
    let prog = "(defgeneric (setf gsp) (v x)) \
                (defmethod (setf gsp) (v (x cons)) (setf (car x) v)) \
                (let ((c (list 0))) (setf (gsp c) 8) (format t \"~a\" c))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(8)"),
        "setf of a (setf f) generic should mutate the place, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setf_generic_writer_compiles() {
    let prog = "(defgeneric (setf gspc) (v x)) \
                (defmethod (setf gspc) (v (x cons)) (setf (car x) v)) \
                (defun run-gspc (c v) (setf (gspc c) v)) \
                (let ((c (list 0))) \
                  (dotimes (i 20) (run-gspc c i)) \
                  (format t \"RESULT=~a~%\" c) \
                  (disassemble 'run-gspc))";
    let output = egcl_bin()
        .env("EGCL_LAZY_COMPILE", "1")
        .env("EGCL_LAZY_THRESHOLD", "4")
        .env("EGCL_T0_T1_THRESHOLD", "100000")
        .args(["--no-init", "--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("RESULT=(19)"),
        "compiled generic writer must preserve SETF semantics; stdout: {stdout}, stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("; RUN-GSPC —"),
        "generic SETF made the bytecode compiler bail: {stdout}"
    );
}

#[test]
fn setf_of_a_setf_function_place() {
    // bliss-lb6.14: (setf (place …) v) must call a (defun (setf place) …) writer
    // (CLHS 5.1.2.9) — ASDF uses (defun (setf operate-level) …).
    let prog = "(defun (setf hd) (v x) (setf (car x) v) v) \
                (let ((c (list 1 2))) (setf (hd c) 9) (format t \"~a\" c))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(9 2)"),
        "setf of a (setf f) writer should mutate the place, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn generic_accepts_union_of_method_keywords() {
    // bliss-lb6.14: a keyword accepted by ANY applicable method is valid for the
    // generic call, even if the method that runs doesn't list it (CLHS 7.6.5) —
    // ASDF's OPERATE :around declares :verbose, the primary method doesn't.
    let prog = "(defgeneric gk (x)) \
                (defmethod gk :around (x &key verbose) (declare (ignore verbose)) (call-next-method)) \
                (defmethod gk (x &key mode) (declare (ignore mode)) :ran) \
                (format t \"~a\" (gk 1 :verbose t :mode :fast))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("RAN"),
        "a method must not reject a keyword another applicable method declares, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn reader_on_a_non_instance_yields_nil() {
    // bliss-lb6.14: an accessor/reader applied to a non-instance (e.g. NIL) yields
    // NIL rather than an uncatchable error — ASDF relies on (system-source-file nil).
    let prog = "(defclass k () ((s :accessor ks :initform 7))) \
                (format t \"~a ~a\" (ks (make-instance 'k)) (ks nil))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("7 NIL"),
        "reader on instance=7, on nil=NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn stringp_of_a_pathname_is_false() {
    // bliss-lb6.15: a pathname must not satisfy STRINGP, even though its
    // namestring is registered as a string. When it did, UIOP's
    // ENSURE-DIRECTORY-PATHNAME — `(cond ((stringp p) (recurse (pathname p))) …)`
    // — recursed forever (the string branch never cleared), stack-overflowing
    // the whole ASDF load.
    let prog = "(let ((p (pathname \"/tmp/d/\"))) (format t \"~a ~a\" (stringp p) (pathnamep p)))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL T"),
        "a pathname must be pathnamep=T, stringp=NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn load_accepts_a_pathname_designator() {
    // LOAD must accept a #P pathname, not only a string (common in ~/.egclrc).
    let dir = std::env::temp_dir().join("egcl_test_load_pathname");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("p.lisp");
    std::fs::write(&file_path, "(format t \"LOADED-VIA-PATHNAME\")\n").expect("write");
    let prog = format!("(load #P\"{}\")", file_path.to_str().unwrap());
    let output = egcl_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("LOADED-VIA-PATHNAME"),
        "LOAD should accept a #P pathname, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn load_pathname_is_dynamic_across_called_functions_and_nested_loads() {
    // bliss-lb6.23: LOAD used to insert *LOAD-PATHNAME* into only the current
    // evaluator frame. A global function called by a loaded file therefore saw
    // NIL, and ASDF lost the directory of a .asd file while defining its system.
    let dir = std::env::temp_dir().join(format!(
        "egcl_test_dynamic_load_pathname_{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create load-pathname test directory");
    let inner = dir.join("inner.lisp");
    let outer = dir.join("outer.lisp");
    let failing = dir.join("failing.lisp");
    std::fs::write(
        &inner,
        "(format t \"INNER=~a,~a;\"\n\
         (namestring (observed-load-pathname))\n\
         (namestring (observed-load-truename)))\n",
    )
    .expect("write inner load file");
    std::fs::write(
        &outer,
        format!(
            "(defun observed-load-pathname () *load-pathname*)\n\
             (defun observed-load-truename () *load-truename*)\n\
             (format t \"OUTER-BEFORE=~a,~a;\"\n\
                     (namestring (observed-load-pathname))\n\
                     (namestring (observed-load-truename)))\n\
             (load #P\"{}\")\n\
             (format t \"OUTER-AFTER=~a,~a;\"\n\
                     (namestring (observed-load-pathname))\n\
                     (namestring (observed-load-truename)))\n",
            inner.display()
        ),
    )
    .expect("write outer load file");
    std::fs::write(&failing, "(undefined-load-pathname-probe)\n").expect("write failing load file");

    let outer_abs = outer.canonicalize().expect("canonical outer path");
    let inner_abs = inner.canonicalize().expect("canonical inner path");
    let prog = format!(
        "(progn (load #P\"{}\") (format t \"TOP=~a\" *load-pathname*))",
        outer.display()
    );
    let output = egcl_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "load failed: {stdout}\n{stderr}");
    assert!(
        stdout.contains(&format!(
            "OUTER-BEFORE={},{};",
            outer_abs.display(),
            outer_abs.display()
        )),
        "called function did not see outer *LOAD-PATHNAME*: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "INNER={},{};",
            inner_abs.display(),
            inner_abs.display()
        )),
        "nested load did not install its *LOAD-PATHNAME*: {stdout}"
    );
    assert!(
        stdout.contains(&format!(
            "OUTER-AFTER={},{};",
            outer_abs.display(),
            outer_abs.display()
        )),
        "nested load did not restore outer *LOAD-PATHNAME*: {stdout}"
    );
    assert!(
        stdout.contains("TOP=NIL"),
        "LOAD leaked *LOAD-PATHNAME* to its caller: {stdout}"
    );

    // The RAII guards must restore a pre-existing caller binding when reading or
    // evaluating the loaded file fails, too.
    let error_prog = format!(
        "(progn\n\
           (setq *load-pathname* #P\"/caller/path.lisp\")\n\
           (setq *load-truename* #P\"/caller/true.lisp\")\n\
           (handler-case (load #P\"{}\") (error () nil))\n\
           (format t \"RESTORED=~a,~a\"\n\
                   (namestring *load-pathname*)\n\
                   (namestring *load-truename*)))",
        failing.display()
    );
    let error_output = egcl_bin()
        .args(["--eval", &error_prog])
        .output()
        .expect("run failing nested load");

    let _ = std::fs::remove_file(&inner);
    let _ = std::fs::remove_file(&outer);
    let _ = std::fs::remove_file(&failing);
    let _ = std::fs::remove_dir(&dir);

    let error_stdout = String::from_utf8_lossy(&error_output.stdout);
    let error_stderr = String::from_utf8_lossy(&error_output.stderr);
    assert!(
        error_output.status.success(),
        "handler-case did not catch failing LOAD: {error_stdout}\n{error_stderr}"
    );
    assert!(
        error_stdout.contains("RESTORED=/caller/path.lisp,/caller/true.lisp"),
        "failing LOAD did not restore caller bindings: {error_stdout}"
    );
}

#[test]
fn probe_file_reports_existing_and_missing() {
    // PROBE-FILE returns a truename for an existing file, NIL for a missing one,
    // and accepts both string and pathname designators.
    let prog = "(format t \"~a ~a\" \
                (and (probe-file #P\"/etc/hostname\") t) \
                (probe-file \"/no/such/file/xyzzy\"))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("T NIL"),
        "probe-file should report existing=T missing=NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn random_returns_a_value_in_range() {
    // (random n) yields an integer in [0,n); (random f) a float in [0,f).
    let prog = "(let ((i (random 5)) (f (random 1.0))) \
                (format t \"~a ~a\" (and (integerp i) (<= 0 i) (< i 5)) \
                                    (and (floatp f) (<= 0 f) (< f 1.0))))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("T T"),
        "random should stay in range, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pathname_directory_returns_a_list_not_a_namestring() {
    // bliss-lb6: PATHNAME-DIRECTORY must return (:absolute|:relative comp…) so
    // UIOP/ANSI directory-list arithmetic works, not a namestring string.
    let prog = "(let ((d (pathname-directory (make-pathname :directory (list :absolute \"a\" \"b\"))))) \
       (format t \"~a ~a ~a\" (eq (car d) :absolute) (second d) (third d)))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("T a b"),
        "expected '(:absolute \"a\" \"b\")', got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn pathname_accessors_coerce_a_namestring_designator() {
    // bliss-aid: PATHNAME-NAME/-TYPE/-HOST/-DEVICE/-VERSION must accept a
    // namestring designator (coerce it to a pathname), like PATHNAME-DIRECTORY.
    // They previously returned NIL for a string, which broke UIOP's
    // pathname-directory-pathname / subpathname during ASDF's source-registry
    // scan. Also check make-pathname :defaults coerces a string designator so the
    // directory is inherited rather than silently dropped.
    let prog = "(format t \"~a|~a|~a\" \
       (pathname-name \"/a/b/foo.txt\") \
       (pathname-type \"/a/b/foo.txt\") \
       (namestring (make-pathname :name nil :type nil :version nil \
                                  :defaults \"/a/b/foo.txt\")))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("foo|txt|/a/b/"),
        "expected 'foo|txt|/a/b/', got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn unsupplied_optional_and_key_params_shadow_enclosing_bindings() {
    // bliss-lb6: an unsupplied &optional/&key parameter must default to its
    // default form (NIL here), not inherit a same-named variable from an
    // enclosing scope. This bug drove UIOP's split-string off the end of a
    // string (find/position called without :end picked up a caller's `end`).
    let prog = "(progn \
       (defun pk (seq &key end) (list end)) \
       (defun po (&optional end) end) \
       (format t \"~a ~a ~a\" \
         (car (let ((end 99)) (pk \"ab\"))) \
         (let ((end 88)) (po)) \
         (let ((end 7)) (find #\\x \"ab\"))))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL NIL NIL"),
        "unsupplied &optional/&key must be NIL, not the caller's binding; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn defun_establishes_implicit_block_for_return_from() {
    // bliss-lb6: a defun body is wrapped in a block named after the function
    // (ANSI 3.1.2.1), so (return-from NAME ...) works from anywhere in the body,
    // including nested forms. UIOP's resolve-absolute-location relies on this.
    let prog = "(progn \
       (defun f (x) (declare (ignore x)) (return-from f 42) 99) \
       (defun g (n) (dolist (i (list 1 2 3)) (when (= i n) (return-from g i))) :none) \
       (format t \"~a ~a ~a\" (f 1) (g 2) (g 9)))";
    let output = egcl_bin()
        .args(["--eval", prog])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42 2 NONE"),
        "return-from <function> should work from top-level and nested forms; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A Lisp function named by a symbol runs on a real spawned native thread and
/// its value is returned by JOIN-THREAD (bliss-q9i1, epic bliss-jiwf). This is
/// the foundational threading slice: the interpreter installs a thread-entry
/// runner (egcl_rt::set_thread_entry_runner) so egcl-rt can run interpreted
/// Lisp — not just bare native fn-pointers — on a worker thread. Passing a
/// SYMBOL (an immediate) avoids sharing a nursery-allocated closure across
/// threads (bliss-nubv). The spawner blocks in JOIN-THREAD, so only one
/// interpreter thread runs at a time — the tree-walker does not yet poll GC
/// safepoints, so true parallel allocation still deadlocks (bliss-bw3t).
#[test]
fn make_thread_runs_named_lisp_function_and_join_returns_its_value() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defun worker () (+ 40 2))",
            "--eval",
            "(princ (egcl-thread:join-thread (egcl-thread:make-thread (quote worker))))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "worker thread should compute (+ 40 2) and JOIN-THREAD return 42; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A worker thread that allocates heavily (2M conses → many natural minor GCs)
/// while the spawner blocks in JOIN-THREAD completes correctly. Guards the
/// single-active-mutator GC path for spawned interpreter threads (bliss-q9i1);
/// concurrent multi-mutator allocation is a separate, still-open case
/// (bliss-bw3t).
#[test]
fn spawned_thread_survives_natural_minor_gc_while_spawner_joins() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defun heavy () (let ((acc nil)) (dotimes (i 2000000) (setq acc (cons i acc))) (length acc)))",
            "--eval",
            "(princ (egcl-thread:join-thread (egcl-thread:make-thread (quote heavy))))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("2000000"),
        "heavy allocating worker should return list length 2000000; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Two interpreter threads allocating concurrently both complete with correct
/// results, as long as the workload stays under the minor-GC nursery threshold
/// (bliss-q9i1). Above that threshold two mutators currently thrash in the
/// stop-the-world minor GC (each safepoint retires the parked thread's TLAB, so
/// the threads ping-pong collections) plus contend on the symbol registry lock —
/// tracked as bliss-bw3t. This guards the working regime and documents the
/// boundary: 200k conses/thread is comfortably under the default nursery.
#[test]
fn two_interpreter_threads_allocate_concurrently_under_gc_threshold() {
    let output = egcl_bin()
        .args([
            "--eval",
            "(defun heavy (k) (let ((acc nil)) (dotimes (i k) (setq acc (cons i acc))) (length acc)))",
            "--eval",
            "(defun worker () (heavy 200000))",
            "--eval",
            "(let ((w (egcl-thread:make-thread (quote worker)))) \
               (let ((mine (heavy 200000))) \
                 (format t \"CONC ~a ~a~%\" mine (egcl-thread:join-thread w))))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("CONC 200000 200000"),
        "both threads should compute 200000 concurrently; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// MAKE-THREAD accepts a symbol, a function object (#'global-defun), AND a
/// LAMBDA — the closure is reified into a self-contained function object so a
/// body that references only globals/its own parameters runs on the worker (the
/// common `(make-thread (lambda () (work)))` form that bordeaux-threads passes).
/// A lambda that reads a *captured lexical* still finds it unbound on the worker
/// (that frame is not shared across threads yet — bliss-nubv), which surfaces as
/// a runtime unbound-variable rather than a MAKE-THREAD rejection (bliss-q9i1).
#[test]
fn make_thread_reifies_lambda_and_runs_non_capturing_body() {
    // #'global works
    let sharp = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defun w () 99)",
            "--eval",
            "(princ (egcl-thread:join-thread (egcl-thread:make-thread #'w)))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(sharp.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&sharp.stdout).contains("99"));

    // a lambda that references only a global runs on the worker
    let lam = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defun work () (* 7 6))",
            "--eval",
            "(princ (egcl-thread:join-thread (egcl-thread:make-thread (lambda () (work)))))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(
        lam.status.code(),
        Some(0),
        "non-capturing lambda should run on a worker"
    );
    assert!(
        String::from_utf8_lossy(&lam.stdout).contains("42"),
        "lambda calling a global should return 42; got: {}",
        String::from_utf8_lossy(&lam.stdout)
    );
}

/// A spawned worker that allocates heavily enough to trigger its OWN minor GC
/// (5M conses) runs to completion while the spawner blocks in JOIN-THREAD
/// (bliss-bw3t). Previously this deadlocked: the joiner waited in a futex while
/// still marked Running, so the worker's stop-the-world GC counted it as a
/// participant and waited for it to reach a safepoint that never came. JOIN-THREAD
/// now marks the caller Blocked (stack published) so the collector skips it.
#[test]
fn worker_can_gc_while_spawner_blocks_in_join() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defun heavy (n) (let ((acc nil)) (dotimes (i n) (setq acc (cons i acc))) (length acc)))",
            "--eval",
            "(defun worker () (heavy 5000000))",
            "--eval",
            "(princ (egcl-thread:join-thread (egcl-thread:make-thread (quote worker))))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "exit code should be 0 (no deadlock)"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("5000000"),
        "worker's own GC while the spawner joins must complete and return 5000000; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// A COMPILED macro whose lambda list uses `&whole` binds the whole variable to
/// the ENTIRE call form including the operator (CLHS 3.4.4). The bytecode
/// macro-invocation path previously bound `&whole` to the argument list WITHOUT
/// the operator, so `(rest whole)` silently dropped the first argument — which
/// broke global-vars' DEFINE-GLOBAL-VAR* and blocked loading bordeaux-threads
/// (bliss-66ny). Interpreted macros were unaffected; this exercises the compiled
/// path via COMPILE-FILE + LOAD.
#[test]
fn compiled_macro_whole_includes_operator_and_all_args() {
    let dir = std::env::temp_dir().join(format!("egcl_test_whole_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create whole test dir");
    let src = dir.join("wm.lisp");
    // The macro echoes (rest whole); a correct binding yields (A B), not (B).
    std::fs::write(
        &src,
        "(defmacro wm (&whole whole a b) (declare (ignore a b)) \
           (list 'quote (rest whole)))\n",
    )
    .expect("write whole macro source");
    let fasl = dir.join("wm.fasl");
    let prog = format!(
        "(progn (compile-file #P\"{}\" :output-file #P\"{}\") (load #P\"{}\") \
           (format t \"WHOLE=~a\" (wm x y)))",
        src.to_str().unwrap(),
        fasl.to_str().unwrap(),
        fasl.to_str().unwrap()
    );
    let output = egcl_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("WHOLE=(X Y)"),
        "compiled &whole must bind the whole call form (rest -> (X Y)); got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// PROGV dynamically binds a runtime list of special variables to a runtime
/// list of values for the extent of its body, restoring them afterward (bliss-66ny;
/// needed by bordeaux-threads v2's ESTABLISH-DYNAMIC-ENV).
#[test]
fn progv_binds_and_restores_special_variables() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defvar *pv* 1)",
            "--eval",
            "(princ (progv (list '*pv*) (list 42) (symbol-value '*pv*)))",
            "--eval",
            "(princ *pv*)",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42") && stdout.trim_end().ends_with('1'),
        "PROGV should bind *pv* to 42 in its body and restore 1 after; got: '{stdout}'"
    );
}

/// A COMPILED macro whose lambda list has `&environment` and that passes the env
/// to MACROEXPAND-1, when invoked FROM THE INTERPRETER, must receive a usable
/// lexical environment. The bytecode macro-invocation path previously bound
/// `&environment` to NIL, so `(macroexpand-1 form env)` failed with "MACROEXPAND:
/// invalid lexical environment" — which blocked bordeaux-threads v2's
/// WITH-LOCK-HELD (bliss-66ny).
#[test]
fn compiled_macro_environment_reaches_macroexpand() {
    let dir = std::env::temp_dir().join(format!("egcl_test_menv_{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create menv test dir");
    let src = dir.join("m.lisp");
    std::fs::write(
        &src,
        "(defmacro inner (x) (list 'quote (list :ex x)))\n\
         (defmacro outer (x &environment env) (macroexpand-1 (list 'inner x) env))\n",
    )
    .expect("write env macro source");
    let fasl = dir.join("m.fasl");
    let prog = format!(
        "(progn (compile-file #P\"{}\" :output-file #P\"{}\") (load #P\"{}\") \
           (format t \"MENV=~a\" (outer 9)))",
        src.to_str().unwrap(),
        fasl.to_str().unwrap(),
        fasl.to_str().unwrap()
    );
    let output = egcl_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run egcl");
    let _ = std::fs::remove_dir_all(&dir);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("MENV=(EX 9)"),
        "compiled &environment must reach MACROEXPAND-1 from the interpreter; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// (DEFVAR name) with no value only proclaims NAME special — it must NOT assign
/// a value (NAME stays unbound). bordeaux-threads v2's CURRENT-THREAD relies on
/// `(boundp '*current-thread*)` being NIL after `(defvar *current-thread*)`
/// (bliss-66ny).
#[test]
fn defvar_without_value_leaves_variable_unbound() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defvar *no-val*)",
            "--eval",
            "(format t \"BOUND=~a\" (boundp '*no-val*))",
            "--eval",
            "(defvar *with-val* 7)",
            "--eval",
            "(format t \" WV=~a\" *with-val*)",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("BOUND=NIL") && stdout.contains("WV=7"),
        "(defvar x) must leave x unbound; (defvar x v) must set it; got: '{stdout}'"
    );
}

/// A CLOS class name is a type and is NOT shadowed by a same-BARE-name DEFTYPE
/// in a different package: TYPEP of an instance against its own class must be T
/// even if another package DEFTYPE'd that bare name to a disjoint type. egcl's
/// deftype registry is keyed by bare name, so bordeaux-threads v1's
/// `(deftype thread () 'integer)` otherwise made `(typep v2-thread 'thread)` NIL
/// (bliss-66ny).
#[test]
fn class_type_not_shadowed_by_same_name_deftype_in_other_package() {
    let output = egcl_bin()
        .args([
            "--no-init",
            "--eval",
            "(defpackage :pa (:use :cl))",
            "--eval",
            "(defpackage :pb (:use :cl))",
            "--eval",
            "(deftype pa::widget () 'integer)",
            "--eval",
            "(defclass pb::widget () ())",
            "--eval",
            "(format t \"INST=~a NONINST=~a\" \
               (typep (make-instance 'pb::widget) 'pb::widget) \
               (typep 5 'pb::widget))",
        ])
        .output()
        .expect("run egcl");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("INST=T") && stdout.contains("NONINST=NIL"),
        "a class must not be shadowed by a same-bare-name deftype in another package; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// CLHS 3.3.4: a `(declare (special v))` governs *references* only until an
/// inner binding of `v` intervenes. egcl kept the name dynamic for the whole
/// body, so an inner LET's lexical binding was invisible — even `(setq x 9)`
/// inside it did not stick — and conversely a `LOCALLY` special declaration
/// (which binds nothing and exists precisely to bypass a lexical binding) was
/// discarded outright. ansi LET.6 / LET*.6; bliss-9kww.
///
/// Both evaluation paths are pinned: the bare form goes through the bytecode
/// lowerer, the `(eval '…)` form through the tree-walker. The two carry
/// independent implementations of this rule and only the tree-walker's is
/// exercised by the ansi harness, which is how the bug outlived the first fix.
#[test]
fn special_declaration_scope_versus_inner_lexical_binding() {
    let cases = [
        // An inner LET binds lexically; the enclosing special declaration must
        // not redirect the reference to the dynamic cell.
        ("(let ((x 0)) (declare (special x)) (let ((x 1)) x))", "1"),
        // ...and the lexical binding is assignable.
        // ...and the lexical binding is assignable, without disturbing the
        // dynamic one the outer reference still reads. (Deliberately avoids
        // SYMBOL-VALUE, which has its own lexical-fallback bug — bliss-qui9.)
        (
            "(let ((x 0)) (declare (special x)) \
               (list (let ((x 1)) (setq x 9) x) x))",
            "(9 0)",
        ),
        // LOCALLY declares without binding: the reference reads the dynamic
        // value even though a lexical binding is in scope. This is ansi LET.6.
        (
            "(let ((x 0)) (declare (special x)) \
               (let ((x 1)) (multiple-value-list \
                 (values x (locally (declare (special x)) x)))))",
            "(1 0)",
        ),
        // The symmetric case: a lexical binding *inside* a LOCALLY body shadows
        // that declaration in turn. Fixing the above without this regresses it.
        (
            "(let ((x 0)) (declare (special x)) \
               (locally (declare (special x)) (let ((x 1)) x)))",
            "1",
        ),
        // LET* differs from LET in whether a later init sees the new binding:
        // in LET* it does (lexical 1), in parallel LET the inits run in the
        // outer scope, where the declaration still governs (dynamic 0).
        (
            "(let ((x 0)) (declare (special x)) (let* ((x 1) (y x)) y))",
            "1",
        ),
        (
            "(let ((x 0)) (declare (special x)) (let ((x 1) (y x)) y))",
            "0",
        ),
        // The binding really is dynamic when nothing shadows it: a callee sees
        // it. (The behaviour the declaration exists for; must not regress.)
        (
            "(progn (defun spc-peek () (symbol-value 'sv)) \
               (let ((sv 5)) (declare (special sv)) (spc-peek)))",
            "5",
        ),
        // LOCALLY without a special declaration is still a plain progn.
        ("(locally (declare (optimize speed)) 1 2 3)", "3"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// A `(declare (special v))` at the head of a *function* body was ignored
/// entirely, in both of its CLHS 3.3.4 roles. When v is a parameter the
/// declaration makes that binding DYNAMIC, which is the whole point — egcl
/// bound it lexically, so a callee reading the variable signalled unbound. When
/// v is not a parameter the declaration is *free* and applies to the body only,
/// not to the lambda-list init-forms — so an `&aux`/`&optional`/`&key` default
/// sees the enclosing lexical value while the body sees the dynamic one.
/// ansi DEFUN.5/6/7; bliss-g97k.
///
/// The declaration is found through the implicit BLOCK that DEFUN wraps a body
/// in at definition time. Without that, a DEFUN silently kept the old behaviour
/// while a bare LAMBDA was fixed — which is how this looked half-fixed.
#[test]
fn special_declaration_in_a_function_body() {
    let cases = [
        // Bound declaration: the parameter is a dynamic binding, so a callee
        // reached from the body sees it.
        (
            "(progn (defun g97-peek () (symbol-value 'g97v)) \
               (defun g97-a (g97v) (declare (special g97v)) (g97-peek)) \
               (g97-a 7))",
            "7",
        ),
        // ...through a bare LAMBDA as well as a DEFUN (different code paths:
        // only DEFUN bodies carry the implicit block).
        (
            "(progn (defun g97-peek2 () (symbol-value 'g97w)) \
               (funcall (lambda (g97w) (declare (special g97w)) (g97-peek2)) 8))",
            "8",
        ),
        // ...and it is unwound on exit.
        (
            "(progn (defun g97-b (g97x) (declare (special g97x)) g97x) \
               (list (g97-b 3) (boundp 'g97x)))",
            "(3 NIL)",
        ),
        // Free declaration: init-form sees the lexical binding, body sees the
        // dynamic one. These are ansi DEFUN.5 / .6 / .7 — they differ only in
        // which lambda-list keyword carries the init-form.
        (
            "(let ((x 1)) (declare (special x)) \
               (let ((x 2)) (defun g97-aux (&aux (y x)) (declare (special x)) \
                 (multiple-value-list (values y x))) (g97-aux)))",
            "(2 1)",
        ),
        (
            "(let ((x 1)) (declare (special x)) \
               (let ((x 2)) (defun g97-opt (&optional (y x)) (declare (special x)) \
                 (multiple-value-list (values y x))) (g97-opt)))",
            "(2 1)",
        ),
        (
            "(let ((x 1)) (declare (special x)) \
               (let ((x 2)) (defun g97-key (&key (y x)) (declare (special x)) \
                 (multiple-value-list (values y x))) (g97-key)))",
            "(2 1)",
        ),
        // The minimal free case, with no lambda list at all.
        (
            "(let ((x 1)) (declare (special x)) \
               (let ((x 2)) (funcall (lambda () (declare (special x)) x))))",
            "1",
        ),
        // A body with no special declaration keeps ordinary lexical scoping.
        (
            "(progn (defun g97-plain (v) (declare (ignorable v)) v) (g97-plain 4))",
            "4",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// The `special` declaration's *binding* role, in the forms bliss-g97k did not
/// reach. Surveying every binding form turned up four gaps — and one of them was
/// worse than a missing declaration: MULTIPLE-VALUE-BIND discarded special
/// bindings ENTIRELY. TakeValuesToLocals puts every value in a local slot, but a
/// reference to a special name compiles as a dynamic load, so the slot was
/// written and never read and `(multiple-value-bind (*v*) (values 42) *v*)`
/// quietly returned the global value. No declaration required. bliss-ge3g.
///
/// PROG was a different fault: its macroexpansion left the body's declarations
/// inside the TAGBODY, where CLHS puts them on the LET instead — so they were
/// not declarations at all.
///
/// DO, DO*, DOLIST, DOTIMES and DESTRUCTURING-BIND were already correct (they
/// expand into LET) and are included to keep them that way.
#[test]
fn special_binding_role_across_binding_forms() {
    let peek = "(defun sbr-pk (s) (symbol-value s))";
    let cases = [
        // The silent wrong answer: an earmuffed special bound by M-V-B.
        (
            "(progn (defvar *sbr-v* 0) (defun sbr-g () *sbr-v*) \
               (multiple-value-bind (*sbr-v*) (values 42) (list *sbr-v* (sbr-g))))",
            "(42 42)",
        ),
        // ...and it is unwound on exit.
        (
            "(progn (defvar *sbr-w* 0) (multiple-value-bind (*sbr-w*) (values 42) nil) *sbr-w*)",
            "0",
        ),
        // A declared-special M-V-B variable binds dynamically...
        (
            "(multiple-value-bind (mv) (values 3) (declare (special mv)) \
               (list mv (sbr-pk 'mv)))",
            "(3 3)",
        ),
        // ...while an undeclared one in the same form stays lexical.
        (
            "(multiple-value-bind (ma mb) (values 1 2) (declare (special ma)) \
               (list ma mb (sbr-pk 'ma)))",
            "(1 2 1)",
        ),
        // FLET and LABELS parameters.
        (
            "(flet ((ff (fv) (declare (special fv)) (sbr-pk 'fv))) (ff 8))",
            "8",
        ),
        (
            "(labels ((lf (lv) (declare (special lv)) (sbr-pk 'lv))) (lf 9))",
            "9",
        ),
        // PROG / PROG*: the declaration belongs to the LET, not the TAGBODY.
        (
            "(prog ((pv 10)) (declare (special pv)) (return (list pv (sbr-pk 'pv))))",
            "(10 10)",
        ),
        (
            "(prog* ((pw 11)) (declare (special pw)) (return (list pw (sbr-pk 'pw))))",
            "(11 11)",
        ),
        // A PROG with no declarations still runs its body as a tagbody.
        (
            "(prog ((a 1)) (go skip) (return :wrong) skip (return a))",
            "1",
        ),
        // Already correct; must stay correct.
        (
            "(destructuring-bind (db) '(4) (declare (special db)) (list db (sbr-pk 'db)))",
            "(4 4)",
        ),
        (
            "(do ((dv 5 (1+ dv))) (nil) (declare (special dv)) (return (list dv (sbr-pk 'dv))))",
            "(5 5)",
        ),
        (
            "(dolist (dl '(7)) (declare (special dl)) (return (list dl (sbr-pk 'dl))))",
            "(7 7)",
        ),
        (
            "(dotimes (dt 1) (declare (special dt)) (return (list dt (sbr-pk 'dt))))",
            "(0 0)",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args([
                "--eval",
                &format!("(progn {peek} (cl:format t \"~S~%\" {expr}))"),
            ])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// The bliss-9kww shadow rule, in the M-V-B lowering. An enclosing
/// `(declare (special z))` puts Z in the lowerer's declared_special, so every
/// reference compiles as a dynamic load; LET clears the entry for a name it
/// binds lexically, but MULTIPLE-VALUE-BIND did not — so its own binding was
/// written and the reference read the outer dynamic cell instead. bliss-7v68.
///
/// Bytecode-only: the tree-walker was already right, so the bare form and the
/// `(eval '…)` form must both be checked.
#[test]
fn mvb_lexical_binding_shadows_enclosing_special_declaration() {
    let cases = [
        // The M-V-B does not declare Z special, so its binding is lexical.
        (
            "(let ((z 0)) (declare (special z)) (multiple-value-bind (z) (values 3) z))",
            "3",
        ),
        // A declared one in the same form stays dynamic, and an undeclared
        // sibling stays lexical.
        (
            "(progn (defun mvs-pk (s) (symbol-value s)) \
               (let ((a 0) (b 0)) (declare (special a b)) \
                 (multiple-value-bind (a b) (values 1 2) (declare (special a)) \
                   (list a b (mvs-pk 'a) (mvs-pk 'b)))))",
            "(1 2 1 0)",
        ),
        // The outer dynamic binding is untouched by the inner lexical one.
        (
            "(progn (defun mvs-pk2 (s) (symbol-value s)) \
               (let ((z 0)) (declare (special z)) \
                 (multiple-value-bind (z) (values 3) (list z (mvs-pk2 'z)))))",
            "(3 0)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `(setf (apply #'f a1 … an) v)` — CLHS 5.1.2.5 — expands to
/// `(apply #'(setf f) v a1 … an)`, where the last argument is the spread list.
/// egcl signalled PROGRAM-ERROR for every such place. ansi SETF-APPLY.1/2/3/4.
///
/// The `(setf f)` writers were already callable for AREF, so the feature is the
/// rewrite plus real `(setf bit)` / `(setf sbit)` functions — BIT and SBIT index
/// a bit array exactly as AREF does. bliss-hzen.
#[test]
fn setf_of_apply() {
    let cases = [
        // Subscripts entirely in the spread list.
        (
            "(let ((x (vector 0 1 2))) (setf (apply #'aref x '(0)) 10) x)",
            "#(10 1 2)",
        ),
        // ...and spread across fixed arguments with a trailing NIL.
        (
            "(let ((a (make-array '(2 2) :initial-element 0))) \
               (setf (apply #'aref a 1 1 nil) 7) (aref a 1 1))",
            "7",
        ),
        (
            "(let ((bv (copy-seq #*0000))) (setf (apply #'bit bv 2 nil) 1) bv)",
            "#*0010",
        ),
        (
            "(let ((bv (copy-seq #*0000))) (setf (apply #'sbit bv 2 nil) 1) bv)",
            "#*0010",
        ),
        // The quoted-name form CLHS also permits.
        (
            "(let ((x (vector 0 1 2))) (setf (apply 'aref x '(2)) 5) x)",
            "#(0 1 5)",
        ),
        // A user-defined writer works the same way — nothing is special-cased.
        (
            "(progn (defun (setf sa-w) (new v i) (setf (aref v i) new)) \
               (let ((x (vector 0 0))) (setf (apply #'sa-w x '(1)) 4) x))",
            "#(0 4)",
        ),
        // SETF returns the stored value.
        ("(let ((x (vector 0))) (setf (apply #'aref x '(0)) 3))", "3"),
        // The ordinary (bit …) place still works, and still returns the value:
        // defining a writer function must not divert it.
        (
            "(let ((bv (copy-seq #*0000))) (setf (bit bv 2) 1) bv)",
            "#*0010",
        ),
        ("(let ((bv (copy-seq #*0000))) (setf (bit bv 2) 1))", "1"),
        // ...with the CLHS 5.1.1.1 order: place subform before the new value.
        (
            "(let ((bv (copy-seq #*0000)) (log nil)) \
               (setf (bit bv (progn (push :sub log) 2)) (progn (push :val log) 1)) log)",
            "(:VAL :SUB)",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// A wrong-argument-count call to a builtin is a PROGRAM-ERROR (CLHS 3.5.1).
/// Neither APPLY nor GET-SETF-EXPANSION was in `fixed_arity_builtin`, so both
/// accepted any count. `(apply)` was the worst: with no arguments it took the
/// missing first one as the function name and reported *that* undefined, so the
/// diagnostic pointed somewhere unrelated to the mistake.
/// ansi APPLY.ERROR.1, GET-SETF-EXPANSION.ERROR.1/2; bliss-g5pa.
#[test]
fn wrong_arity_apply_and_get_setf_expansion_signal_program_error() {
    // `apply function &rest args+` requires at least one argument after the
    // function, so one argument is an error too — confirmed against SBCL.
    let errors = [
        "(apply)",
        "(apply #'list)",
        "(get-setf-expansion)",
        "(get-setf-expansion 'x nil nil)",
    ];
    for expr in errors {
        let output = egcl_bin()
            .args([
                "--eval",
                &format!(
                    "(cl:format t \"~S~%\" (handler-case (progn (eval '{expr}) :no-error) \
                       (program-error () :program-error) (error (c) (type-of c))))"
                ),
            ])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            ":PROGRAM-ERROR",
            "expected PROGRAM-ERROR from: {expr}"
        );
    }
    // Well-formed calls must keep working: the spread list may be empty, and
    // arguments may be split between fixed positions and the list.
    let ok = [
        ("(apply #'+ (list 1 2))", "3"),
        ("(apply #'+ 1 2 (list 3))", "6"),
        ("(apply #'list nil)", "NIL"),
        (
            "(multiple-value-list (get-setf-expansion 'x))",
            "(NIL NIL (#:NEW) (SETQ X #:NEW) X)",
        ),
    ];
    for (expr, expected) in ok {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        let got = String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        if expr.starts_with("(multiple-value-list") {
            // The store variable is a gensym, so only its shape is stable.
            assert!(
                got.starts_with("(NIL NIL (") && got.contains("SETQ X") && got.ends_with(" X)"),
                "get-setf-expansion shape: {expr} gave {got}"
            );
        } else {
            assert_eq!(got, expected, "case: {expr}");
        }
    }
}

/// CLHS 5.1.2.3: SETF of a VALUES place returns, as multiple values, everything
/// it stored. egcl truncated to the primary — the store half was always right,
/// and EGCL::%SETF-VALUES already yielded all the values deliberately; the
/// evaluator discarded them on the way out, because SETF is not in the
/// multiple-values operator allowlist and so counts as a single-value context.
///
/// Adding SETF to that allowlist would be wrong: `(setf x (gethash k h))` must
/// return ONE value, not the two GETHASH leaves in env.mv. The decision is made
/// from the form — only a SETF whose LAST place is a VALUES place is
/// value-transparent. ansi SETF-VALUES.1/6 and VALUES.21; bliss-prdk.
#[test]
fn setf_of_values_place_returns_all_stored_values() {
    let cases = [
        (
            "(let ((x nil) (y nil) (z nil)) (multiple-value-list (setf (values x y z) (values 1 2 3))))",
            "(1 2 3)",
        ),
        // Storing still works, and a short value list fills with NIL.
        (
            "(let ((x nil) (y nil) (z nil)) (setf (values x y z) (values 1 2 3)) (list x y z))",
            "(1 2 3)",
        ),
        (
            "(let ((x nil) (y nil)) (setf (values x y) (values 1)) (list x y))",
            "(1 NIL)",
        ),
        // (setf (values) form) returns NO values, not NIL.
        ("(multiple-value-list (setf (values) (values)))", "NIL"),
        // Only the LAST place decides: a VALUES place earlier in the form does
        // not make a later plain place multi-valued.
        (
            "(let ((a nil) (x nil) (y nil)) (multiple-value-list (setf a 1 (values x y) (values 8 9))))",
            "(8 9)",
        ),
        (
            "(let ((a nil) (x nil) (y nil)) (multiple-value-list (setf (values x y) (values 8 9) a (floor 7 2))))",
            "(3)",
        ),
        // The leak guards: SETF is otherwise a single-value context.
        (
            "(let ((h (make-hash-table)) (x nil)) (setf (gethash 'k h) 7) (multiple-value-list (setf x (gethash 'k h))))",
            "(7)",
        ),
        (
            "(let ((x nil)) (multiple-value-list (setf x (floor 7 2))))",
            "(3)",
        ),
        ("(let ((x nil)) (multiple-value-list (setf x 5)))", "(5)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// Variable documentation was never stored: DEFVAR/DEFPARAMETER/DEFCONSTANT
/// docstrings were swallowed by the definers' `&rest`, and DOCUMENTATION itself
/// returned NIL unconditionally while `(setf (documentation …))` accepted the
/// store and dropped it — in BOTH backends, the bytecode lowering carrying an
/// explicit no-op branch mirroring the tree-walker's.
///
/// The store lives in boot.lisp, not cli.rs: it is one EQUAL-keyed table over
/// (name . doc-type), so it is shared with the Lisp definers that record into
/// it and covers every doc-type rather than just VARIABLE.
/// ansi DEFVAR.4/5, DEFPARAMETER.4/5, DEFCONSTANT.4; bliss-61u1.
#[test]
fn variable_documentation_is_retained() {
    let cases = [
        (
            "(progn (defvar *doc-a* 200 \"Whatever.\") (documentation '*doc-a* 'variable))",
            "\"Whatever.\"",
        ),
        (
            "(progn (defparameter *doc-b* 200 \"Param.\") (documentation '*doc-b* 'variable))",
            "\"Param.\"",
        ),
        (
            "(progn (defconstant +doc-c+ 5 \"Const.\") (documentation '+doc-c+ 'variable))",
            "\"Const.\"",
        ),
        // The writer, which previously accepted the store and discarded it.
        (
            "(progn (defvar *doc-d* 1) (setf (documentation '*doc-d* 'variable) \"Set.\") \
               (documentation '*doc-d* 'variable))",
            "\"Set.\"",
        ),
        // ...and it returns the assigned value.
        (
            "(progn (defvar *doc-e* 1) (setf (documentation '*doc-e* 'variable) \"R\"))",
            "\"R\"",
        ),
        // Absent documentation is NIL, and the doc-type is part of the key.
        (
            "(progn (defvar *doc-f* 1) (documentation '*doc-f* 'variable))",
            "NIL",
        ),
        (
            "(progn (defvar *doc-g* 1 \"V\") (list (documentation '*doc-g* 'variable) \
               (documentation '*doc-g* 'function)))",
            "(\"V\" NIL)",
        ),
        // DOCUMENTATION returns exactly one value (it reads through GETHASH,
        // which returns two).
        (
            "(progn (defvar *doc-h* 1 \"V\") (multiple-value-list (documentation '*doc-h* 'variable)))",
            "(\"V\")",
        ),
        // The definers keep their own semantics: DEFVAR returns the name, does
        // not reassign an already-bound variable, and still binds the value.
        (
            "(progn (defvar *doc-i* 1 \"D\") (defvar *doc-i* 99 \"D2\") *doc-i*)",
            "1",
        ),
        (
            "(progn (makunbound '*doc-j*) (defvar *doc-j* 200 \"D\"))",
            "*DOC-J*",
        ),
        (
            "(progn (defparameter *doc-k* 1 \"D\") (defparameter *doc-k* 2 \"D\") *doc-k*)",
            "2",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS DEFSETF: the long form's body is enclosed in an implicit BLOCK named
/// after the access-fn, like DEFUN's and DEFMACRO's. egcl established no block,
/// so a RETURN-FROM in the body signalled CONTROL-ERROR. ansi DEFSETF.5A;
/// bliss-ye6f.
#[test]
fn long_form_defsetf_body_has_an_implicit_block() {
    let cases = [
        // RETURN-FROM the access-fn exits the expander and its value is the
        // store form.
        (
            "(progn (defun dsb (x) (car x)) \
               (defsetf dsb (y) (val) (return-from dsb `(setf (car ,y) ,val))) \
               (let ((x (cons 'a 'b))) (list (setf (dsb x) 'c) x)))",
            "(C (C . B))",
        ),
        // A body that does not return early is unaffected.
        (
            "(progn (defun dsc (x) (car x)) \
               (defsetf dsc (y) (val) `(setf (car ,y) ,val)) \
               (let ((x (cons 'a 'b))) (list (setf (dsc x) 'c) x)))",
            "(C (C . B))",
        ),
        // The block wraps the whole body, so forms after the RETURN-FROM are
        // skipped rather than deciding the expansion.
        (
            "(progn (defun dsd (x) (car x)) \
               (defsetf dsd (y) (val) (return-from dsd `(setf (car ,y) ,val)) \
                 (error \"unreached\")) \
               (let ((x (cons 'a 'b))) (setf (dsd x) 'c) x))",
            "(C . B)",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// CLHS 5.1.2.7: a place whose operator is a macro is macroexpanded, and SETF
/// tries again on the expansion. egcl handled only MACROLET-bound macros; a
/// global macro place fell through to "unsupported place".
///
/// The ordering matters and is tested both ways: when a name has BOTH a macro
/// definition and a DEFSETF, the DEFSETF wins. That case (ansi SETF-MACRO.2)
/// passing on its own is what made the global-macro gap look like it did not
/// exist. ansi SETF-MACRO.1/3, SETF.7; bliss-w5wf.
#[test]
fn setf_expands_a_macro_place() {
    let cases = [
        // A plain global macro place.
        (
            "(progn (defmacro sm-a (x) `(car ,x)) \
               (let ((x (list 1))) (list (setf (sm-a x) 2) (1+ (car x)))))",
            "(2 3)",
        ),
        // Expansion repeats: a macro whose expansion is another macro.
        (
            "(progn (defmacro sm-b (x) `(car ,x)) (defmacro sm-c (x) `(sm-b ,x)) \
               (let ((x (list 1))) (list (setf (sm-c x) 2) (1+ (car x)))))",
            "(2 3)",
        ),
        // A MACROLET place still works.
        (
            "(macrolet ((%m (y) `(car ,y))) \
               (let ((x (list 1))) (list (setf (%m x) 2) (1+ (car x)))))",
            "(2 3)",
        ),
        // A DEFSETF on a macro name overrides the macro expansion: the update
        // function runs (observable through the special it sets), rather than
        // the place expanding to (car x).
        (
            "(progn (defun sm-upd (x y) (declare (special *sm-x*)) \
                       (setf (car x) y) (setf *sm-x* 'boo) y) \
               (defmacro sm-d (x) `(car ,x)) (defsetf sm-d sm-upd) \
               (let ((x (list 1)) (*sm-x* nil)) (declare (special *sm-x*)) \
                 (list (setf (sm-d x) 2) *sm-x* (1+ (car x)))))",
            "(2 BOO 3)",
        ),
        // A non-macro, non-place operator still errors rather than silently
        // succeeding.
        (
            "(handler-case (eval '(setf (sm-not-a-place 1) 2)) (error () :error))",
            ":ERROR",
        ),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// FBOUNDP disagreed with FDEFINITION about what is fbound. FDEFINITION consults
/// is_ansi_special_operator and is_ansi_standard_macro so `(fdefinition 'cond)`
/// does not signal; FBOUNDP consulted neither, so `(fboundp 'defun)` was NIL
/// while `(fdefinition 'defun)` succeeded. It also keyed a `(setf f)` name with
/// val_as_str instead of the "(SETF f)" spelling the writer registry uses, so a
/// `(defun (setf f) …)` writer was never found.
///
/// ansi DCF-FUNS and DCF-MACROS are the useful tests: each walks a whole
/// section's worth of standard names, so they check the tables rather than one
/// symbol. bliss-uy2q.
#[test]
fn fboundp_and_macro_function_see_evaluator_implemented_operators() {
    let cases = [
        // Standard macros implemented as evaluator arms.
        ("(not (not (fboundp 'defun)))", "T"),
        ("(not (not (fboundp 'defsetf)))", "T"),
        ("(not (not (macro-function 'defsetf)))", "T"),
        ("(not (not (macro-function 'define-setf-expander)))", "T"),
        // Special operators are fbound but are NOT macros.
        ("(not (not (fboundp 'if)))", "T"),
        ("(macro-function 'if)", "NIL"),
        // Builtins that FBOUNDP reported as unbound.
        ("(not (not (fboundp 'function-lambda-expression)))", "T"),
        ("(not (not (fboundp 'get-setf-expansion)))", "T"),
        // A (setf f) writer is found under the registry's own key...
        (
            "(progn (defun (setf fbx) (v x) (setf (car x) v)) (not (not (fboundp '(setf fbx)))))",
            "T",
        ),
        // ...and a (setf <gensym>) with no writer is still NIL (ansi FBOUNDP.7).
        ("(let ((g (gensym))) (fboundp (list 'setf g)))", "NIL"),
        // FBOUNDP and FDEFINITION must agree — the disagreement was the defect.
        (
            "(let ((names '(defun defsetf if cond get-setf-expansion car))) \
               (list (remove-if #'fboundp names) \
                     (remove-if (lambda (n) (ignore-errors (fdefinition n))) names)))",
            "(NIL NIL)",
        ),
        // Undefined names stay unbound.
        ("(fboundp (gensym))", "NIL"),
        ("(macro-function (gensym))", "NIL"),
    ];
    for (expr, expected) in cases {
        let output = egcl_bin()
            .args(["--eval", &format!("(cl:format t \"~S~%\" {expr})")])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "case errored: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "case: {expr}"
        );
    }
}

/// AND, OR and COND and the value count they yield (CLHS). The rule that governs
/// all of it: ALL the values of the LAST form propagate, and only the last —
/// a short-circuiting earlier form contributes exactly one value.
///
/// egcl leaked a non-last form's extra values out of OR, and returned the wrong
/// value COUNT from COND: `(cond ((values)))` produced no values at all rather
/// than NIL, and a bodiless clause passed its test's extra values through.
///
/// The last-form half is what makes this delicate, and my first attempt broke it:
/// clearing unconditionally on a false form turned `(and (values 1 nil) (values
/// nil 2))` from NIL,2 into NIL and `(and (values))` from no values into NIL.
/// Both directions are therefore pinned here. ansi AND.5/8/9, OR.5/6, COND.12;
/// bliss-0xrp.
#[test]
fn and_or_cond_value_counts() {
    let cases = [
        // Short-circuiting on a NON-last form yields exactly one value...
        ("(multiple-value-list (and (values nil t) t))", "(NIL)"),
        ("(multiple-value-list (or (values t nil) 'a))", "(T)"),
        // ...but the LAST form's values all propagate, even when it is false...
        (
            "(multiple-value-list (and (values 1 nil) (values nil 2)))",
            "(NIL 2)",
        ),
        ("(multiple-value-list (or nil (values nil 5)))", "(NIL 5)"),
        // ...and even when it yields none.
        ("(multiple-value-list (and (values)))", "NIL"),
        ("(multiple-value-list (or (values)))", "NIL"),
        // The ordinary tail cases.
        ("(multiple-value-list (and t (values 1 2)))", "(1 2)"),
        ("(multiple-value-list (or nil (values 1 2)))", "(1 2)"),
        // No forms at all: exactly one value.
        ("(multiple-value-list (and))", "(T)"),
        ("(multiple-value-list (or))", "(NIL)"),
        // COND: a test yielding no values is false, and COND itself yields NIL —
        // one value, not zero.
        ("(multiple-value-list (cond ((values))))", "(NIL)"),
        ("(multiple-value-list (cond))", "(NIL)"),
        // A clause with no forms returns the test's PRIMARY value...
        ("(multiple-value-list (cond ((values 7 8))))", "(7)"),
        // ...while a clause WITH forms propagates all of the body's values.
        ("(multiple-value-list (cond (t (values 1 2))))", "(1 2)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// An implicit progn with NO forms yields exactly NIL — one value. egcl left
/// `env.mv` holding whatever the enclosing form had produced, so those values
/// escaped as the body's own: `(multiple-value-bind () (values 1 2 3 4 5))`
/// returned all five. ansi MULTIPLE-VALUE-BIND.12/13; bliss-p3gu.
///
/// Fixed in eval_progn / lower_progn rather than in M-V-B, because the rule
/// holds for every empty body — which is also why the non-empty cases are pinned
/// here: that path is used by nearly every binding and control form.
#[test]
fn an_empty_body_yields_exactly_nil() {
    let cases = [
        (
            "(multiple-value-list (multiple-value-bind () (values)))",
            "(NIL)",
        ),
        (
            "(multiple-value-list (multiple-value-bind () (values 1 2 3 4 5)))",
            "(NIL)",
        ),
        // Other forms with an empty body, same rule.
        ("(multiple-value-list (let () ))", "(NIL)"),
        ("(multiple-value-list (progn))", "(NIL)"),
        ("(multiple-value-list (when t))", "(NIL)"),
        ("(multiple-value-list (let ((x (values 1 2)))))", "(NIL)"),
        // Non-empty bodies must still propagate the tail form's values.
        (
            "(multiple-value-list (multiple-value-bind (a) (values 1 2) (values a 9)))",
            "(1 9)",
        ),
        ("(multiple-value-list (progn (values 1 2)))", "(1 2)"),
        ("(multiple-value-list (let () (values 1 2)))", "(1 2)"),
        ("(multiple-value-list (when t (values 1 2)))", "(1 2)"),
        // ...and a non-final form's values are still discarded.
        ("(multiple-value-list (progn (values 1 2) 3))", "(3)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// MAKE-ARRAY must return a FRESH array (CLHS). Its :initial-contents branch
/// coerced the source to the target type, and COERCE legitimately returns its
/// ARGUMENT when that is already of the type — SBCL's
/// `(eq s (coerce s 'string))` is T too — so the "new" array WAS the source.
/// When the source was an immutable literal, a later store died with "cannot
/// modify an interned string literal".
/// ansi EVERY.22, SOME.22, NOTANY.22, NOTEVERY.22; bliss-4fbq.
///
/// Tree-walker-only in effect, which is why the bare form passed and only
/// `(eval '…)` failed — so every case runs on both backends.
#[test]
fn make_array_copies_its_initial_contents() {
    let cases = [
        // The array is not the source...
        (
            "(let* ((s \"abcde\") (v (make-array '(5) :initial-contents s))) (eq s v))",
            "NIL",
        ),
        (
            "(let* ((s \"abcde\") (v (make-array '(5) :initial-contents s :element-type 'base-char))) (eq s v))",
            "NIL",
        ),
        (
            "(let* ((s (vector 1 2 3)) (v (make-array '(3) :initial-contents s))) (eq s v))",
            "NIL",
        ),
        // ...so storing into it works even when the source is a literal...
        (
            "(let ((v (make-array '(5) :initial-contents \"abcde\" :element-type 'base-char))) \
               (setf (aref v 2) #\\0) v)",
            "\"ab0de\"",
        ),
        (
            "(let ((v (make-array '(5) :initial-contents \"abcde\" :element-type 'character))) \
               (setf (aref v 2) #\\0) v)",
            "\"ab0de\"",
        ),
        // ...and leaves the source untouched. Note the result is a general
        // VECTOR of characters, not a string: :element-type defaults to T, so
        // MAKE-ARRAY does not inherit the source's element type. Matches SBCL.
        (
            "(let* ((s (copy-seq \"abcde\")) (v (make-array '(5) :initial-contents s))) \
               (setf (aref v 0) #\\z) (list v s))",
            "(#(#\\z #\\b #\\c #\\d #\\e) \"abcde\")",
        ),
        // The contents are still correct, from every source type.
        ("(make-array '(3) :initial-contents '(1 2 3))", "#(1 2 3)"),
        (
            "(make-array '(3) :initial-contents (vector 1 2 3))",
            "#(1 2 3)",
        ),
        (
            "(make-array '(5) :initial-contents \"abcde\")",
            "#(#\\a #\\b #\\c #\\d #\\e)",
        ),
        (
            "(make-array '(5) :initial-contents \"abcde\" :element-type 'character)",
            "\"abcde\"",
        ),
        (
            "(make-array '(3) :initial-contents '(1 0 1) :element-type 'bit)",
            "#*101",
        ),
        // Other MAKE-ARRAY paths are unaffected.
        (
            "(let ((v (make-array '(3) :initial-element 0))) (setf (aref v 0) 9) v)",
            "#(9 0 0)",
        ),
        ("(make-array 3 :initial-element 7)", "#(7 7 7)"),
        (
            "(let ((v (make-array 3 :initial-contents '(1 2 3) :fill-pointer 2))) \
               (list v (fill-pointer v)))",
            "(#(1 2) 2)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS 5.3: within the scope of one FLET/LABELS binding, `#'name` denotes the
/// single function that binding established, so `(eq #'f #'f)` is true. The
/// tree-walker minted a fresh closure object on every `#'` evaluation.
/// ansi LABELS.37/38/39; bliss-1e8t.
///
/// Tree-walker-only — the bytecode path was already correct — which is why these
/// were on a "context-dependent, passes standalone" list for most of the session:
/// the bare form compiles and passes. Every case runs on both backends.
///
/// The cache is per binding INSTANCE, not per name, and the risk is that it
/// wrongly aliases, so the tests pin what must STILL differ: separate FLET forms,
/// separate invocations, and closures over different captured values.
#[test]
fn sharp_quote_of_a_local_function_is_eq_stable() {
    let cases = [
        // The identity the ansi tests check.
        ("(labels ((%f () nil)) (eq #'%f #'%f))", "T"),
        ("(labels ((%f () nil)) (eq #'%f (car (list #'%f))))", "T"),
        ("(labels ((%f () #'%f)) (eq #'%f (%f)))", "T"),
        ("(flet ((%f () nil)) (eq #'%f #'%f))", "T"),
        // A global DEFUN was already stable; must stay so.
        ("(progn (defun eqs-g () nil) (eq #'eqs-g #'eqs-g))", "T"),
        // Separate binding forms are separate functions.
        (
            "(let ((a (flet ((%f () 1)) #'%f)) (b (flet ((%f () 2)) #'%f))) (eq a b))",
            "NIL",
        ),
        // Separate invocations of the same form, likewise — the cache must not
        // leak across calls.
        (
            "(progn (defun eqs-mk () (flet ((%f () 1)) #'%f)) (eq (eqs-mk) (eqs-mk)))",
            "NIL",
        ),
        // ...and each closure keeps its own captured value.
        (
            "(progn (defun eqs-mk2 (n) (flet ((%f () n)) #'%f)) \
               (let ((f1 (eqs-mk2 1)) (f2 (eqs-mk2 2))) \
                 (list (funcall f1) (funcall f2) (eq f1 f2))))",
            "(1 2 NIL)",
        ),
        // An inner binding shadows an outer one of the same name.
        (
            "(flet ((%f () 1)) (let ((outer #'%f)) \
               (flet ((%f () 2)) (list (funcall outer) (funcall #'%f) (eq outer #'%f)))))",
            "(1 2 NIL)",
        ),
        // Calling through the cached reference still works, including recursion
        // within the scope.
        (
            "(labels ((fact (n) (if (<= n 1) 1 (* n (funcall #'fact (- n 1)))))) (fact 5))",
            "120",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// Three coupled rules about which binding a reference sees, all in the
/// tree-walker (the bytecode backend was already right on every case here).
///
///  1. A binding established DYNAMICALLY redirects the body's OWN references to
///     the value cell. eval_let wrote the cell but left the name out of
///     locally_specials, so the frame walk found an OUTER LEXICAL binding of the
///     same name and won — visible only when such an outer binding exists, which
///     is why `(let (x) (declare (special x)) x)` alone looked fine.
///     ansi LET.11 / LET*.11.
///  2. A binding established LEXICALLY shadows an enclosing declaration, in
///     M-V-B as well as LET. Adding rule 1 without this broke
///     `(let ((z 0)) (declare (special z)) (multiple-value-bind (z) (values 3) z))`.
///  3. SYMBOL-VALUE reads the DYNAMIC value only; a lexical binding is not a
///     symbol value. It resolved through the full lexical-then-global lookup, so
///     it saw an M-V-B's lexical binding. ansi MULTIPLE-VALUE-BIND.7.
///
/// bliss-s2y9 / bliss-qui9. Rules 1 and 2 are exact opposites, so both
/// directions and the interaction are pinned; `with_child_frame` restores only
/// the frame, so the leak cases check that neither edit escapes its form.
#[test]
fn dynamic_and_lexical_bindings_shadow_each_other_correctly() {
    let setup = "(defun dls-pk (s) (symbol-value s))";
    let cases = [
        // (1) the body's own reference reads the dynamic binding...
        (
            "(let ((x 1)) (list x (let (x) (declare (special x)) x) x))",
            "(1 NIL 1)",
        ),
        (
            "(let* ((x 1)) (list x (let* (x) (declare (special x)) x) x))",
            "(1 NIL 1)",
        ),
        // ...and a callee sees it too.
        (
            "(let ((v 1)) (let ((v 7)) (declare (special v)) (dls-pk 'v)))",
            "7",
        ),
        // (2) a lexical binding shadows an enclosing declaration, in LET...
        ("(let ((x 0)) (declare (special x)) (let ((x 1)) x))", "1"),
        // ...and in MULTIPLE-VALUE-BIND.
        (
            "(let ((z 0)) (declare (special z)) (multiple-value-bind (z) (values 3) z))",
            "3",
        ),
        // ...while the outer dynamic binding is untouched by it.
        (
            "(let ((z 0)) (declare (special z)) \
               (multiple-value-bind (z) (values 3) (list z (dls-pk 'z))))",
            "(3 0)",
        ),
        // (3) SYMBOL-VALUE never sees a lexical binding. This is the core of
        // ansi MULTIPLE-VALUE-BIND.7: X and Y are declared special in the M-V-B
        // so %x/%y read 1 and 2, while Z is bound lexically so %z reads the
        // OUTER dynamic 0.
        (
            "(let ((z 0) x y) (declare (special z)) \
               (flet ((%x () (symbol-value 'x)) (%y () (symbol-value 'y)) \
                      (%z () (symbol-value 'z))) \
                 (multiple-value-bind (x y z) (values 1 2 3) (declare (special x y)) \
                   (list (%x) (%y) (%z)))))",
            "(1 2 0)",
        ),
        // Neither edit may leak past its form (with_child_frame restores only
        // the frame, so both are undone explicitly).
        (
            "(let ((v 1)) (list (let ((v 2)) (declare (special v)) (dls-pk 'v)) v))",
            "(2 1)",
        ),
        (
            "(let ((v 9)) (declare (special v)) (list (let ((v 3)) v) (dls-pk 'v)))",
            "(3 9)",
        ),
        // Nested dynamic rebinding still unwinds.
        (
            "(let ((v 1)) (declare (special v)) \
               (list (let ((v 2)) (declare (special v)) (dls-pk 'v)) (dls-pk 'v)))",
            "(2 1)",
        ),
        // A free LOCALLY declaration still bypasses a lexical binding.
        (
            "(let ((x 0)) (declare (special x)) \
               (let ((x 1)) (multiple-value-list \
                 (values x (locally (declare (special x)) x)))))",
            "(1 0)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args([
                    "--eval",
                    &format!("(progn {setup} (cl:format t \"~S~%\" {form}))"),
                ])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `(destructuring-bind (&whole (a . b) c . d) '(1 . 2) (list a b c d))` left D
/// unbound. Every ingredient worked alone — dotted tails, dotted values, `&whole`
/// with a plain symbol, `&whole` with a pattern — only the combination failed.
/// ansi DESTRUCTURING-BIND.20; bliss-8st5.
///
/// Two losses, both of the dotted tail. `bind_macro_param` delegates any pattern
/// containing a lambda-list keyword to the lambda-list binder, which (a) rebuilt
/// the value with `vec_to_list(args)`, dropping a dotted tail before `&whole`
/// ever saw it, and (b) iterates `while c.is_cons()`, so a dotted tail in the
/// LAMBDA LIST was never bound at all.
///
/// The binder needs the argument list separately from `&whole`'s form: for a
/// macro call the latter is `(operator . args)`, so walking it is off by one —
/// an intermediate version of this fix bound D to the whole list for
/// `(defmacro m (a . d) …)`, which is why that case is pinned here too.
#[test]
fn destructuring_handles_dotted_tails_with_lambda_list_keywords() {
    let cases = [
        // The ansi case: &whole pattern + dotted lambda list + dotted value.
        (
            "(destructuring-bind (&whole (a . b) c . d) '(1 . 2) (list a b c d))",
            "(1 2 1 2)",
        ),
        // Each ingredient alone, so a regression says which half broke.
        ("(destructuring-bind (c . d) '(1 . 2) (list c d))", "(1 2)"),
        (
            "(destructuring-bind (a b . d) '(1 2 3 4) (list a b d))",
            "(1 2 (3 4))",
        ),
        (
            "(destructuring-bind (&whole w a b) '(1 2) (list w a b))",
            "((1 2) 1 2)",
        ),
        (
            "(destructuring-bind (&whole (a . b) c) '(1) (list a b c))",
            "(1 NIL 1)",
        ),
        // A dotted tail alongside the other lambda-list keywords.
        (
            "(destructuring-bind (&whole w a . d) '(1 2 3) (list w a d))",
            "((1 2 3) 1 (2 3))",
        ),
        (
            "(destructuring-bind (a (b . c) . d) '(1 (2 3 4) 5 6) (list a b c d))",
            "(1 2 (3 4) (5 6))",
        ),
        // Unaffected shapes.
        (
            "(destructuring-bind (a &rest r) '(1 2 3) (list a r))",
            "(1 (2 3))",
        ),
        (
            "(destructuring-bind (a &optional (b 'z)) '(1) (list a b))",
            "(1 Z)",
        ),
        (
            "(destructuring-bind (a &key b) '(1 :b 2) (list a b))",
            "(1 2)",
        ),
        (
            "(destructuring-bind (&whole w a &key b) '(1 :b 2) (list w a b))",
            "((1 :B 2) 1 2)",
        ),
        // A MACRO lambda list shares this binder, and there `&whole` is the whole
        // CALL form — so the dotted tail must be walked from the arguments, not
        // from that form. With one argument, D binds to NIL, not to the call.
        (
            "(progn (defmacro dtl-m (a . d) `(list ',a ',d)) (dtl-m 1))",
            "(1 NIL)",
        ),
        // ...and the surplus arguments a dotted tail exists to collect must not
        // trip the binder's "too many arguments" check (bliss-4ab5).
        (
            "(progn (defmacro dtl-m2 (a . d) `(list ',a ',d)) (dtl-m2 1 2 3))",
            "(1 (2 3))",
        ),
        (
            "(progn (defmacro dtl-w (&whole w a) (declare (ignore w)) `',a) (dtl-w 7))",
            "7",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// FLET/LABELS are LEXICAL, so a local function that ESCAPES its binding form
/// must still see the function namespace it was written in. The tree-walker's
/// Closure captured the lexical frame and the block/tag exits but not the
/// function namespace, so an escaped local function could not even recurse:
///
///     (funcall (labels ((f (n) (if (<= n 1) 1 (* n (f (- n 1)))))) #'f) 5)
///       =>  "The function F is undefined"   (SBCL and egcl's bytecode: 120)
///
/// Returning a recursive local function is an ordinary idiom, so this was a
/// real-code bug, not only ansi LABELS.40. bliss-5q20.
///
/// The namespace to capture differs by form and is the easy thing to get wrong:
/// a FLET function's body may NOT see itself or its siblings, a LABELS function's
/// body must see both. Every case is therefore checked escaped AND unescaped, on
/// both backends.
#[test]
fn an_escaped_local_function_keeps_its_function_namespace() {
    let setup = "(defun eln-g () :global)";
    let cases = [
        // Escaped LABELS: recurses by name, and via #'name.
        (
            "(funcall (labels ((f (n) (if (<= n 1) 1 (* n (f (- n 1)))))) #'f) 5)",
            "120",
        ),
        (
            "(funcall (labels ((f (n) (if (<= n 1) 1 (* n (funcall #'f (- n 1)))))) #'f) 5)",
            "120",
        ),
        // ansi LABELS.40: #'f inside the escaped body is the same object.
        (
            "(let ((x (labels ((f () #'f)) #'f))) (eql x (funcall x)))",
            "T",
        ),
        // Escaped LABELS sees its siblings.
        ("(funcall (labels ((a () :a) (b () (a))) #'b))", ":A"),
        // Escaped FLET must NOT see itself — (eln-g) in the body is the GLOBAL.
        ("(funcall (flet ((eln-g () (eln-g))) #'eln-g))", ":GLOBAL"),
        // ...nor its siblings.
        (
            "(handler-case (funcall (flet ((a () :a) (b () (a))) #'b)) (error () :undefined))",
            ":UNDEFINED",
        ),
        // The same rules unescaped, so a regression says which half broke.
        ("(flet ((eln-g () :local)) (funcall #'eln-g))", ":LOCAL"),
        ("(flet ((eln-g () (eln-g))) (funcall #'eln-g))", ":GLOBAL"),
        ("(labels ((a () :a) (b () (a))) (funcall #'b))", ":A"),
        (
            "(labels ((f (n) (if (<= n 1) 1 (* n (f (- n 1)))))) (funcall #'f 5))",
            "120",
        ),
        // Invoking an escaped closure must not disturb the CALLER's namespace.
        (
            "(let ((esc (labels ((f () :inner)) #'f))) \
               (flet ((f () :outer)) (list (funcall esc) (f))))",
            "(:INNER :OUTER)",
        ),
        // A closure that captures no local functions still calls globals.
        ("(funcall (lambda () (eln-g)))", ":GLOBAL"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args([
                    "--eval",
                    &format!("(progn {setup} (cl:format t \"~S~%\" {form}))"),
                ])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// DEFINE-MODIFY-MACRO expanded to `(setf PLACE (fn PLACE args…))`, putting the
/// place in TWICE — once for the read and once for the write — so its subforms
/// were evaluated twice and any side effect ran twice:
///
///     (define-modify-macro new-incf (&optional (delta 1)) +)
///     (let ((a (vector 0 0 0 0 0)) (i 1)) (new-incf (aref a (incf i))) i)
///       =>  3, must be 2
///
/// The stored value was correct, so this was a silent wrong answer rather than
/// an error. The expansion now goes through GET-SETF-EXPANSION and binds the
/// temporaries first, which also gives CLHS 5.1.1.1's order: place subforms left
/// to right, then the argument forms. ansi DEFINE-MODIFY-MACRO.3/4; bliss-v8f3.
#[test]
fn define_modify_macro_evaluates_place_subforms_once() {
    let setup = "(progn (define-modify-macro dmm-incf (&optional (delta 1)) +) \
                        (define-modify-macro dmm-app (&rest xs) append))";
    let cases = [
        // The ansi cases: I must advance once per mention, not twice.
        (
            "(let ((a (vector 0 0 0 0 0)) (i 1)) (list (dmm-incf (aref a (incf i))) a i))",
            "(1 #(0 0 1 0 0) 2)",
        ),
        (
            "(let ((a (vector 0 0 0 0 0)) (i 1)) (list (dmm-incf (aref a (incf i)) (incf i)) a i))",
            "(3 #(0 0 3 0 0) 3)",
        ),
        // Ordinary uses keep working: symbol place, explicit argument, &rest.
        ("(let ((x 5)) (dmm-incf x) x)", "6"),
        ("(let ((x 5)) (dmm-incf x 3) x)", "8"),
        (
            "(let ((x (list 1))) (dmm-app x (list 2) (list 3)) x)",
            "(1 2 3)",
        ),
        // The built-in modify macros share this machinery and must also evaluate
        // a place subform exactly once.
        (
            "(let ((a (vector 0 0 0)) (i 0)) (incf (aref a (incf i))) (list a i))",
            "(#(0 1 0) 1)",
        ),
        (
            "(let ((a (vector 5 5 5)) (i 0)) (decf (aref a (incf i))) (list a i))",
            "(#(5 4 5) 1)",
        ),
        (
            "(let ((a (vector nil nil nil)) (i 0)) (push 9 (aref a (incf i))) (list a i))",
            "(#(NIL (9) NIL) 1)",
        ),
        (
            "(let ((a (vector nil nil nil)) (i 0)) (pushnew 9 (aref a (incf i))) (list a i))",
            "(#(NIL (9) NIL) 1)",
        ),
        // Other place kinds still store correctly.
        ("(let ((pl (list :a 1))) (incf (getf pl :a)) pl)", "(:A 2)"),
        ("(let ((c (cons 1 2))) (incf (car c)) c)", "(2 . 2)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args([
                    "--eval",
                    &format!("(progn {setup} (cl:format t \"~S~%\" {form}))"),
                ])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// Two independent long-form DEFSETF gaps, both reproducing on both backends.
///
/// The expander must be defined in the LEXICAL environment the DEFSETF appears
/// in — the ansi test file says so in a comment — so its body may close over the
/// surrounding bindings. SetfExpander::LongUpdate stored no environment and the
/// body ran in the CALLER's, leaving such a reference unbound (DEFSETF.6A,
/// bliss-30i6). MacroDef already captured a frame for the same reason.
///
/// And a place whose expansion declares SEVERAL store variables takes them from
/// the value form's MULTIPLE VALUES (CLHS 5.5.5). Only the first was bound, so
/// the expansion referenced a gensym nothing had bound (DEFSETF.7A, bliss-669r).
#[test]
fn long_form_defsetf_environment_and_multiple_store_variables() {
    let cases = [
        // 6A: the expander closes over Z from its defining LET.
        (
            "(progn (defun dsf-a (x) (car x)) \
               (let ((z 'car)) (eval `(defsetf dsf-a (y) (val) (list 'setf (list ',z y) val)))) \
               (let ((x (cons 'a 'b))) (list (setf (dsf-a x) 'c) x)))",
            "(C (C . B))",
        ),
        // 7B: two store variables, fed from the value form's two values.
        (
            "(progn (eval '(defsetf dsf-b (x) (v1 v2) `(list ,x ,v1 ,v2))) \
               (eval '(setf (dsf-b 1) (values 2 3))))",
            "(1 2 3)",
        ),
        // ...and a missing second value fills with NIL rather than erroring.
        (
            "(progn (eval '(defsetf dsf-c (x) (v1 v2) `(list ,x ,v1 ,v2))) \
               (eval '(setf (dsf-c 1) (values 2))))",
            "(1 2 NIL)",
        ),
        // The ordinary shapes must keep working: long form, short form, the
        // implicit block, INCF over a long-form place, and subform-once order.
        (
            "(progn (defun dsf-d (x) (car x)) (defsetf dsf-d (y) (val) `(setf (car ,y) ,val)) \
               (let ((x (cons 1 2))) (list (setf (dsf-d x) 9) x)))",
            "(9 (9 . 2))",
        ),
        (
            "(progn (defun dsf-e (x) (car x)) (defun dsf-e-set (x v) (setf (car x) v) v) \
               (defsetf dsf-e dsf-e-set) (let ((x (cons 1 2))) (list (setf (dsf-e x) 9) x)))",
            "(9 (9 . 2))",
        ),
        (
            "(progn (defun dsf-f (x) (car x)) \
               (defsetf dsf-f (y) (val) (return-from dsf-f `(setf (car ,y) ,val))) \
               (let ((x (cons 1 2))) (list (setf (dsf-f x) 9) x)))",
            "(9 (9 . 2))",
        ),
        (
            "(progn (defun dsf-g (x) (car x)) (defsetf dsf-g (y) (val) `(setf (car ,y) ,val)) \
               (let ((x (cons 1 2))) (incf (dsf-g x)) x))",
            "(2 . 2)",
        ),
        (
            "(progn (defun dsf-h (v i) (aref v i)) \
               (defsetf dsf-h (v i) (val) `(setf (aref ,v ,i) ,val)) \
               (let ((v (vector 0 0 0)) (i 0)) (setf (dsf-h v (incf i)) 7) (list v i)))",
            "(#(0 7 0) 1)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// PSETF and ROTATEF assumed ONE store variable per place, so a `(values …)`
/// place — whose expansion declares one per sub-place — kept only the primary:
///
///     (psetf (values a b c) (values 1 2 3))   stored nothing at all
///     (rotatef (values a b) (values c d))     gave (3 NIL 1 NIL), not (3 4 1 2)
///
/// Two causes. PSETF called GET-SETF-EXPANSION directly, which treats VALUES as
/// an ordinary accessor and lifts its sub-places into temporaries, so the stores
/// landed in the temporaries; it now goes through %SETF-EXPANSIONS, which keeps
/// them as places. And a LET* binding keeps only the primary value, so a VALUES
/// place's values now round-trip through a list — only for VALUES places, so the
/// ordinary single-value path is unchanged. ansi PSETF.41; bliss-hsn7.
#[test]
fn psetf_and_rotatef_handle_values_places() {
    let cases = [
        // ansi PSETF.41: two VALUES places, both fed from multiple values.
        (
            "(let ((y 2) (z 3) u x a b c) \
               (psetf (values a b c) (values 1 2 3) (values u x) (values y z)) (list a b c u x))",
            "(1 2 3 2 3)",
        ),
        // ROTATEF across VALUES places, two-way and three-way.
        (
            "(let ((a 1) (b 2) (c 3) (d 4)) (rotatef (values a b) (values c d)) (list a b c d))",
            "(3 4 1 2)",
        ),
        (
            "(let ((a 1) (b 2) (c 3) (d 4) (e 5) (f 6)) \
               (rotatef (values a b) (values c d) (values e f)) (list a b c d e f))",
            "(3 4 5 6 1 2)",
        ),
        // Plain uses unchanged: PSETF is parallel, ROTATEF rotates.
        ("(let ((a 1) (b 2)) (psetf a b b a) (list a b))", "(2 1)"),
        (
            "(let ((a 1) (b 2) (c 3)) (psetf a b b c c a) (list a b c))",
            "(2 3 1)",
        ),
        (
            "(let ((a 1) (b 2) (c 3)) (rotatef a b c) (list a b c))",
            "(2 3 1)",
        ),
        // Other place kinds still store, and still evaluate a subform once.
        (
            "(let ((v (vector 1 2 3))) (psetf (aref v 0) 9 (aref v 1) 8) v)",
            "#(9 8 3)",
        ),
        (
            "(let ((v (vector 0 0 0)) (i 0)) (psetf (aref v (incf i)) 7) (list v i))",
            "(#(0 7 0) 1)",
        ),
        (
            "(let ((v (vector 1 2 3))) (rotatef (aref v 0) (aref v 2)) v)",
            "#(3 2 1)",
        ),
        (
            "(let ((v (vector 1 2 3)) (i 0)) (rotatef (aref v (incf i)) (aref v 0)) (list v i))",
            "(#(2 1 3) 1)",
        ),
        (
            "(let ((pl (list :a 1 :b 2))) (psetf (getf pl :a) 9 (getf pl :b) 8) pl)",
            "(:A 9 :B 8)",
        ),
        // SHIFTF and nested VALUES places share the helper; must not regress.
        ("(let ((a 1) (b 2)) (list (shiftf a b 9) a b))", "(1 2 9)"),
        (
            "(let (a b c) (setf (values a (values b c)) (values 1 2 3)) (list a b c))",
            "(1 2 NIL)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `(setf (get sym key) val)` prepended a fresh pair instead of updating the
/// property already there, so a symbol's plist grew without bound as a property
/// was re-set:
///
///     (setf (get s :a) 1) (setf (get s :a) 2) (symbol-plist s)
///       =>  (:A 2 :A 1), must be (:A 2)
///
/// GET returns the FIRST match, so the value read back correctly and nothing
/// looked wrong until the plist itself was inspected — which is what ansi
/// ROTATEF.32 does. REMPROP then removed only one of the copies. bliss-vy96.
#[test]
fn setf_get_updates_an_existing_property_in_place() {
    let cases = [
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (setf (get s :a) 2) (symbol-plist s))",
            "(:A 2)",
        ),
        // Other properties are preserved, and the updated one keeps its position.
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (setf (get s :b) 2) (setf (get s :a) 9) \
               (symbol-plist s))",
            "(:B 2 :A 9)",
        ),
        // The value still reads back, and INCF over the place behaves.
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (setf (get s :a) 2) (get s :a))",
            "2",
        ),
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (incf (get s :a)) (symbol-plist s))",
            "(:A 2)",
        ),
        // REMPROP now clears the property completely rather than one duplicate.
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (setf (get s :a) 2) \
               (list (not (not (remprop s :a))) (symbol-plist s)))",
            "(T NIL)",
        ),
        // A fresh indicator still prepends, and a missing one still defaults.
        (
            "(let ((s (gensym))) (setf (get s :a) 1) (symbol-plist s))",
            "(:A 1)",
        ),
        ("(let ((s (gensym))) (get s :missing :none))", ":NONE"),
        // ansi ROTATEF.32: rotating through GET places leaves single entries.
        (
            "(let* ((x (gensym)) (y (gensym)) (z 17)) \
               (setf (get x :foo) 1 (get y :bar) 2) \
               (rotatef (get x :foo) (get y :bar) z) \
               (list (symbol-plist x) (symbol-plist y) z))",
            "((:FOO 2) (:BAR 17) 1)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `(setf (find-class name) class)` had no SETF place at all — reading a class
/// worked, writing one signalled PROGRAM-ERROR. CLHS makes this the standard way
/// to give a class a second name, and ansi PSETF.35 / ROTATEF.35 build places out
/// of gensym-named classes with it. bliss-3ypy.
///
/// It binds the NAME only: the class keeps the name it was defined with. Wiring
/// this to the existing set_find_class instead renamed the class in its own
/// metaobject, which silently broke `(typep (make-instance 'c) 'c)` for the
/// original name — hence the CLASS-NAME and TYPEP cases here.
#[test]
fn setf_of_find_class_binds_a_second_name() {
    let cases = [
        // The alias resolves to the very same class object...
        (
            "(let ((n1 (gensym)) (n3 (gensym))) (eval `(defclass ,n1 () ())) \
               (setf (find-class n3) (find-class n1)) (eq (find-class n1) (find-class n3)))",
            "T",
        ),
        // ...and the class is NOT renamed by acquiring one.
        (
            "(progn (defclass sfc-a () ()) (setf (find-class 'sfc-b) (find-class 'sfc-a)) \
               (class-name (find-class 'sfc-a)))",
            "SFC-A",
        ),
        // ...so instances of the original still answer TYPEP under their OWN
        // name. (TYPEP against the ALIAS is a separate gap this feature makes
        // observable for the first time — it matches the class-precedence list by
        // name, and the class keeps its own name. Filed as bliss-34cr; not
        // asserted here, since pinning the current answer would pin a bug.)
        (
            "(progn (defclass sfc-c () ()) (setf (find-class 'sfc-d) (find-class 'sfc-c)) \
               (typep (make-instance 'sfc-c) 'sfc-c))",
            "T",
        ),
        // ansi PSETF.35: the place works under the updating macros.
        (
            "(let ((n1 (gensym)) (n2 (gensym)) (n3 (gensym)) (n4 (gensym))) \
               (eval `(defclass ,n1 () ())) (eval `(defclass ,n2 () ())) \
               (psetf (find-class n3) (find-class n1) (find-class n4) (find-class n2)) \
               (list (eq (find-class n1) (find-class n3)) (eq (find-class n2) (find-class n4))))",
            "(T T)",
        ),
        (
            "(let ((a (gensym)) (b (gensym))) (eval `(defclass ,a () ())) (eval `(defclass ,b () ())) \
               (let ((ca (find-class a)) (cb (find-class b))) \
                 (rotatef (find-class a) (find-class b)) \
                 (list (eq (find-class a) cb) (eq (find-class b) ca))))",
            "(T T)",
        ),
        // Reading is unchanged, including the errorp=NIL form, and DEFCLASS with
        // slots and accessors still works.
        ("(find-class 'no-such-class-xyz nil)", "NIL"),
        (
            "(progn (defclass sfc-e () ((s :initarg :s :accessor sfc-e-s))) \
               (sfc-e-s (make-instance 'sfc-e :s 7)))",
            "7",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS: in `(case key (KEYS form…))` a KEYS designator of NIL is the EMPTY list
/// of keys, so that clause can never match — to match the object NIL you write
/// `(nil)`. egcl treated it as the single key NIL:
///
///     (case nil (nil 'matched) (t 'fell-through))   =>  MATCHED, must fall through
///     (ecase nil (nil 'matched))                    =>  MATCHED, must signal
///
/// The tree-walker already handled CASE correctly and the bytecode lowering did
/// not — another backend divergence — while ECASE and CCASE were wrong on both
/// paths, which is what ansi CCASE.9 / ECASE.9 exercise. bliss-gm4h.
#[test]
fn nil_keys_designator_is_an_empty_key_list() {
    let cases = [
        // NIL as the designator never matches...
        (
            "(case nil (nil 'matched) (t 'fell-through))",
            "FELL-THROUGH",
        ),
        ("(case 1 (nil 'matched) (t 'fell-through))", "FELL-THROUGH"),
        // ...but (nil) matches the object NIL.
        ("(case nil ((nil) 'matched) (t 'fell-through))", "MATCHED"),
        // ECASE/CCASE signal, since no clause can match and NIL contributes no
        // key to the expected type.
        (
            "(handler-case (ecase nil (nil 'matched)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // CCASE needs a real PLACE as its keyform (its STORE-VALUE restart
        // assigns to it), so this uses a variable as ansi CCASE.9 does.
        (
            "(let ((x nil)) (handler-case (ccase x (nil 'matched)) (type-error () :type-error)))",
            ":TYPE-ERROR",
        ),
        ("(ecase nil ((nil) 'matched))", "MATCHED"),
        // Everything else is unchanged: single keys, key lists, OTHERWISE/T,
        // and a normal ECASE hit and miss.
        ("(case 2 (1 'one) (2 'two) (t 'other))", "TWO"),
        ("(case 3 ((1 2) 'low) ((3 4) 'high))", "HIGH"),
        ("(case 99 (1 'one) (otherwise 'other))", "OTHER"),
        ("(ecase 2 (1 'one) (2 'two))", "TWO"),
        (
            "(handler-case (ecase 9 (1 'one) (2 'two)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `#'(setf place)` returned the designator SYMBOL rather than the writer's
/// function object, so it was callable but failed FUNCTIONP and
/// `(typep … 'function)`:
///
///     (defun (setf sfa) (v x) (setf (car x) v))
///     (functionp #'(setf sfa))   =>  NIL, must be T
///     (type-of #'(setf sfa))     =>  SYMBOL, must be a function type
///
/// `(defun (setf place) …)` already installs a real function object on the
/// mangled %SETF-WRITER-place symbol (for the compiled SETF path), so both
/// `#'(setf place)` and `(fdefinition '(setf place))` now answer with it. The
/// designator symbol stays as the fallback for writers that live only in
/// GLOBAL_SETF_FNS. ansi FUNCTION.7 / FUNCTIONP.7; bliss-sqvh.
#[test]
fn sharp_quote_of_a_setf_name_is_a_function_object() {
    let setup = "(defun (setf sqs) (v x) (setf (car x) v))";
    let cases = [
        ("(not (not (functionp #'(setf sqs))))", "T"),
        ("(not (not (typep #'(setf sqs) 'function)))", "T"),
        ("(not (not (functionp (fdefinition '(setf sqs)))))", "T"),
        // Still callable, and still actually writes.
        (
            "(let ((c (cons 1 2))) (funcall #'(setf sqs) 9 c) c)",
            "(9 . 2)",
        ),
        (
            "(let ((c (cons 1 2))) (apply #'(setf sqs) 9 (list c)) c)",
            "(9 . 2)",
        ),
        // The SETF place itself keeps working through the writer.
        ("(let ((c (cons 1 2))) (setf (sqs c) 7) c)", "(7 . 2)"),
        // Ordinary function names are unaffected.
        ("(not (not (functionp #'car)))", "T"),
        ("(not (not (functionp (fdefinition 'car))))", "T"),
        // A (setf name) with no writer defined is still not fbound.
        ("(fboundp '(setf sqs-undefined))", "NIL"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args([
                    "--eval",
                    &format!("(progn {setup} (cl:format t \"~S~%\" {form}))"),
                ])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS 3.1.2.1.2.2: a SYMBOL used as a function DESIGNATOR denotes the symbol's
/// GLOBAL function. FLET/LABELS bindings are lexical and reachable only through
/// `#'name`, so `(funcall 'f …)` calls the global even when a local `f` is in
/// scope. The tree-walker resolved the designator through the lexical map:
///
///     (defun xc (x y) (cons x y))
///     (flet ((xc (x y) (list y x))) (funcall 'xc 1 2))
///       =>  (2 1), must be (1 . 2)
///
/// The bytecode backend was already right — another single-backend divergence,
/// where the bare form passes and only `(eval '…)` fails. SYMBOL-FUNCTION and
/// FDEFINITION name the global definition for the same reason and had the same
/// gap. ansi FUNCALL.7; bliss-c1dr.
///
/// Operator position is the opposite rule and must NOT change, so it is pinned
/// here: `(flet ((xc …)) (xc 1 2))` and `#'xc` both call the LOCAL.
#[test]
fn a_symbol_designator_denotes_the_global_function() {
    let setup = "(defun sdg (x y) (cons x y))";
    let cases = [
        // Designators see the global...
        (
            "(flet ((sdg (x y) (list y x))) (funcall 'sdg 1 2))",
            "(1 . 2)",
        ),
        (
            "(flet ((sdg (x y) (list y x))) (apply 'sdg '(1 2)))",
            "(1 . 2)",
        ),
        (
            "(labels ((sdg (x y) (list y x))) (funcall 'sdg 1 2))",
            "(1 . 2)",
        ),
        (
            "(flet ((sdg (x y) (list y x))) (funcall (symbol-function 'sdg) 1 2))",
            "(1 . 2)",
        ),
        (
            "(flet ((sdg (x y) (list y x))) (funcall (fdefinition 'sdg) 1 2))",
            "(1 . 2)",
        ),
        (
            "(flet ((sdg (x y) (list y x))) (car (mapcar 'sdg '(1) '(2))))",
            "(1 . 2)",
        ),
        // ...while operator position and #' see the LOCAL.
        ("(flet ((sdg (x y) (list y x))) (sdg 1 2))", "(2 1)"),
        (
            "(flet ((sdg (x y) (list y x))) (funcall #'sdg 1 2))",
            "(2 1)",
        ),
        (
            "(flet ((sdg (x y) (list y x))) (car (mapcar #'sdg '(1) '(2))))",
            "(2 1)",
        ),
        // Recursion through LABELS still resolves to the local.
        (
            "(labels ((f (n) (if (<= n 1) 1 (* n (f (- n 1)))))) (f 5))",
            "120",
        ),
        // With no local in scope, all spellings agree.
        ("(funcall 'sdg 1 2)", "(1 . 2)"),
        ("(funcall #'sdg 1 2)", "(1 . 2)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args([
                    "--eval",
                    &format!("(progn {setup} (cl:format t \"~S~%\" {form}))"),
                ])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// BLOCK and RETURN-FROM are LEXICAL: a local function's `(return-from name …)`
/// exits the block it was WRITTEN inside, regardless of which same-named block
/// happens to be active when it runs.
///
///     (block done
///       (flet ((%f (x) (return-from done x)))
///         (block done (mapcar #'%f '(good bad bad))))
///       'bad)
///       =>  BAD in the tree-walker, must be GOOD
///
/// `local_fn_closure` captured the block stack as it stood when `#'%f` was
/// EVALUATED — inside the inner `done` — rather than where %f was DEFINED. The
/// FunDef now records its defining exits. ansi BLOCK.10; bliss-wfxx.
///
/// Bytecode was already correct, so each case runs on both backends.
#[test]
fn return_from_in_a_local_function_is_lexical() {
    let cases = [
        // ansi BLOCK.10: the inner same-named block must not capture it.
        (
            "(block done (flet ((%f (x) (return-from done x))) \
               (block done (mapcar #'%f '(good bad bad)))) 'bad)",
            "GOOD",
        ),
        // The bliss-4u5u shape: a (return) in a closure invoked inside another
        // LOOP's implicit `block nil` must still exit the outer one.
        (
            "(block nil (flet ((%g () (return :outer))) \
               (loop for i in '(1 2 3) do (funcall #'%g)) :not-reached))",
            ":OUTER",
        ),
        // Ordinary exits still work.
        (
            "(block b (flet ((%h () (return-from b :ok))) (funcall #'%h) :no))",
            ":OK",
        ),
        (
            "(flet ((%i () (block b (return-from b :inner)))) (funcall #'%i))",
            ":INNER",
        ),
        (
            "(labels ((%j (n) (if (< n 0) (return-from %j :neg) n))) (list (%j 5) (%j -1)))",
            "(5 :NEG)",
        ),
        (
            "(progn (defun rfl-db () (return-from rfl-db :done)) (rfl-db))",
            ":DONE",
        ),
        // TAGBODY exits travel with the same capture.
        (
            "(flet ((%k () (block nil (tagbody (go done) done (return :tagged))))) (funcall #'%k))",
            ":TAGGED",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS: two ARRAYS are EQUALP when they have the same dimensions and
/// corresponding elements are EQUALP — the element TYPE is irrelevant. egcl
/// excluded strings from its vector comparison, so a string never compared equal
/// to a general vector:
///
///     (equalp "ab" #(#\a #\b))                        =>  NIL, must be T
///     (equalp (make-array '(0) :element-type nil) #()) =>  NIL, must be T
///
/// The second is ansi EQUALP.5; the first shows the gap was not only about empty
/// arrays. Two strings still take the case-insensitive fast path above the
/// vector branch. bliss-4gcm.
#[test]
fn equalp_compares_arrays_across_element_types() {
    let cases = [
        ("(equalp \"ab\" #(#\\a #\\b))", "T"),
        ("(equalp \"\" #())", "T"),
        ("(equalp (make-array '(0) :element-type nil) #())", "T"),
        ("(equalp #*10 #(1 0))", "T"),
        // Case-insensitivity survives the element-wise route.
        ("(equalp \"AB\" #(#\\a #\\b))", "T"),
        // Still NIL where it should be: a list is not an array, and lengths must
        // match.
        ("(equalp #(1 2) '(1 2))", "NIL"),
        ("(equalp #(1) #(1 2))", "NIL"),
        ("(equalp #\\a 97)", "NIL"),
        // The neighbouring EQUALP behaviours are unchanged.
        (
            "(list (equalp \"AB\" \"ab\") (equal \"AB\" \"ab\"))",
            "(T NIL)",
        ),
        (
            "(list (equalp #*101 #*101) (equalp #*101 #*100))",
            "(T NIL)",
        ),
        ("(equalp (vector \"ab\" 1) (vector \"AB\" 1.0))", "T"),
        (
            "(let ((v (make-array 4 :fill-pointer 2 :initial-contents '(1 2 3 4)))) (equalp v #(1 2)))",
            "T",
        ),
        (
            "(equalp (make-array '(2 2) :initial-element 0) (make-array '(2 2) :initial-element 0))",
            "T",
        ),
        // EQUALP hash tables use the same predicate.
        (
            "(let ((h (make-hash-table :test 'equalp))) (setf (gethash \"ab\" h) 1) (gethash \"AB\" h))",
            "1",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// A restart body runs after unwinding to the establishing construct's dynamic
/// extent, so a `(go tag)` in it targets a tag that is lexically visible THERE.
/// That is exactly how the correctable macros retry: CCASE/CTYPECASE/ASSERT's
/// STORE-VALUE clause assigns the place and jumps back to re-dispatch.
///
/// egcl ran the body with whatever exits the HANDLER carried — and a
/// handler-bind lambda is written outside the tagbody, so it has none of them —
/// and the GO signalled a PROGRAM-ERROR. RestartEntry now records the exits
/// visible where the restart was established. ansi CCASE.31 / CTYPECASE.12;
/// bliss-jf2b.
///
/// The restart mechanism itself was never broken, which is what made this look
/// like a missing restart at first: plain RESTART-CASE + INVOKE-RESTART works,
/// and so does a restart body that simply returns a value. Only the GO failed.
#[test]
fn a_restart_body_can_go_to_its_establishing_tagbody() {
    let cases = [
        // ansi CCASE.31: STORE-VALUE supplies a matching key and the CCASE retries.
        (
            "(handler-bind ((type-error (lambda (c) (store-value 7 c)))) \
               (let ((x 0)) (ccase x (1 :bad) (7 :good) (2 nil))))",
            ":GOOD",
        ),
        // ansi CTYPECASE.12, same shape through a type dispatch.
        (
            "(let ((x 1)) (handler-bind ((type-error (lambda (c) (store-value 'a c)))) \
               (ctypecase x (symbol x))))",
            "A",
        ),
        // The underlying shape, isolated: a GO out of a restart body.
        (
            "(let ((n 0)) \
               (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart 'r 7)))) \
                 (block nil (tagbody top \
                   (return (if (> n 0) :good \
                               (restart-case (error \"x\") (r (v) (setq n v) (go top)))))))))",
            ":GOOD",
        ),
        // A restart body that just returns a value still works.
        (
            "(handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart 'r 7)))) \
               (block nil (tagbody top (return (restart-case (error \"x\") (r (v) v))))))",
            "7",
        ),
        // The general restart machinery is unchanged.
        (
            "(restart-case (handler-bind ((error (lambda (c) (declare (ignore c)) (invoke-restart 'my-r 5)))) \
               (error \"boom\")) (my-r (v) v))",
            "5",
        ),
        (
            "(restart-case (handler-bind ((error (lambda (c) (invoke-restart (find-restart 'my-r c) 5)))) \
               (error \"boom\")) (my-r (v) v))",
            "5",
        ),
        (
            "(restart-case (handler-bind ((error (lambda (c) (declare (ignore c)) (continue)))) \
               (error \"boom\")) (continue () :continued))",
            ":CONTINUED",
        ),
        // A normal CCASE hit does not go near the restart.
        ("(let ((x 7)) (ccase x (1 :bad) (7 :good)))", ":GOOD"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// MACROLET and SYMBOL-MACROLET take body declarations, and a free
/// `(declare (special v))` there redirects references in the body to the dynamic
/// value — bypassing an intervening lexical binding, exactly as in LOCALLY:
///
///     (let ((x :good)) (declare (special x))
///       (let ((x :bad)) (macrolet () (declare (special x)) x)))
///       =>  :BAD, must be :GOOD
///
/// LOCALLY already did this (bliss-9kww); these two did not. Both lose the
/// declaration the same way — the wrapper is macroexpanded away and its
/// declarations go with it — so the fix is to read them off the wrapper first
/// and keep them in force over the expansion. ansi MACROLET.47; bliss-vmge.
#[test]
fn macrolet_bodies_honour_a_free_special_declaration() {
    let cases = [
        (
            "(let ((x :good)) (declare (special x)) (let ((x :bad)) (macrolet () (declare (special x)) x)))",
            ":GOOD",
        ),
        (
            "(let ((x :good)) (declare (special x)) \
               (let ((x :bad)) (symbol-macrolet () (declare (special x)) x)))",
            ":GOOD",
        ),
        // LOCALLY, which already worked, must keep working.
        (
            "(let ((x :good)) (declare (special x)) (let ((x :bad)) (locally (declare (special x)) x)))",
            ":GOOD",
        ),
        // A lexical binding INSIDE the body still shadows the declaration.
        (
            "(let ((x :good)) (declare (special x)) \
               (macrolet () (declare (special x)) (let ((x :inner)) x)))",
            ":INNER",
        ),
        // Nested with LOCALLY, both reading the dynamic value.
        (
            "(let ((x :good)) (declare (special x)) \
               (let ((x :bad)) (macrolet () (declare (special x)) \
                 (list x (locally (declare (special x)) x)))))",
            "(:GOOD :GOOD)",
        ),
        // The macros themselves still work, and a body with no declaration keeps
        // ordinary lexical scoping.
        ("(macrolet ((%m (a) `(list ,a ,a))) (%m 3))", "(3 3)"),
        ("(symbol-macrolet ((v 42)) (+ v 1))", "43"),
        ("(let ((x :lex)) (macrolet () x))", ":LEX"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// FMAKUNBOUND could not unbind a `(setf place)` name: it keyed the lookup with
/// val_as_str rather than the "(SETF place)" spelling the writer registry uses,
/// so it found nothing to remove.
///
///     (defun (setf g) (v x) …) (fmakunbound '(setf g)) (fboundp '(setf g))
///       =>  T, must be NIL
///
/// A writer lives in TWO places — GLOBAL_SETF_FNS under that key, and a function
/// object on the mangled %SETF-WRITER-place symbol installed for the compiled
/// SETF path — so both are cleared. Clearing one would trade a false negative for
/// a false positive, the same trap as the plain-symbol case.
/// ansi FMAKUNBOUND.4; bliss-xg1x.
#[test]
fn fmakunbound_unbinds_a_setf_name() {
    let cases = [
        (
            "(let* ((g (gensym)) (n (list 'setf g))) \
               (eval (list 'defun n '(v x) '(setf (car x) v))) \
               (list (not (not (fboundp n))) (progn (fmakunbound n) (fboundp n))))",
            "(T NIL)",
        ),
        // Plain names, gensyms and the return value are unaffected.
        (
            "(progn (defun fmu-a () 1) \
               (list (not (not (fboundp 'fmu-a))) (progn (fmakunbound 'fmu-a) (fboundp 'fmu-a))))",
            "(T NIL)",
        ),
        (
            "(let ((g (gensym))) (setf (symbol-function g) #'car) \
               (list (not (not (fboundp g))) (progn (fmakunbound g) (fboundp g))))",
            "(T NIL)",
        ),
        (
            "(progn (defun fmu-b () 1) (eq (fmakunbound 'fmu-b) 'fmu-b))",
            "T",
        ),
        // A writer that was never unbound still works as a SETF place.
        (
            "(progn (defun (setf fmu-c) (v x) (setf (car x) v)) \
               (let ((c (cons 1 2))) (setf (fmu-c c) 9) c))",
            "(9 . 2)",
        ),
        // ...and after FMAKUNBOUND the place no longer has a writer.
        (
            "(progn (defun (setf fmu-d) (v x) (setf (car x) v)) (fmakunbound '(setf fmu-d)) \
               (fboundp '(setf fmu-d)))",
            "NIL",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS: if any variable in a PSETQ refers to a SYMBOL MACRO, PSETQ behaves as
/// PSETF. egcl expanded to `(let ((tmp val)…) (setq var tmp)…)`, which assigns
/// without lifting the PLACE's subforms first, so a symbol-macro variable such as
/// `(aref a (incf i))` ran its subform at assignment time and the parallel
/// semantics came out wrong. ansi PSETQ.7; bliss-t00q.
///
/// Only the symbol-macro case delegates, deliberately: PSETF's expansion uses
/// DOLIST, which expands through DO to PSETQ, so delegating unconditionally makes
/// the two macros expand each other forever — that blew the control stack on the
/// first test that used one. The DO family is pinned here for that reason.
#[test]
fn psetq_behaves_as_psetf_for_symbol_macros() {
    let cases = [
        (
            "(symbol-macrolet ((x (aref a (incf i))) (y (aref a (incf i)))) \
               (let ((a (copy-seq #(0 1 2 3 4 5 6 7 8 9))) (i 0)) \
                 (psetq x (aref a (incf i)) y (aref a (incf i))) (list a i)))",
            "(#(0 2 2 4 4 5 6 7 8 9) 4)",
        ),
        // PSETF with the same places already agreed with SBCL; it must still.
        (
            "(symbol-macrolet ((x (aref a (incf i))) (y (aref a (incf i)))) \
               (let ((a (copy-seq #(0 1 2 3 4 5 6 7 8 9))) (i 0)) \
                 (psetf x (aref a (incf i)) y (aref a (incf i))) (list a i)))",
            "(#(0 2 2 4 4 5 6 7 8 9) 4)",
        ),
        // Ordinary PSETQ is parallel, and SETQ on a symbol macro is unchanged.
        ("(let ((p 1) (q 2)) (psetq p q q p) (list p q))", "(2 1)"),
        (
            "(symbol-macrolet ((x (aref a (incf i)))) \
               (let ((a (copy-seq #(0 1 2 3))) (i 0)) (setq x 9) (list a i)))",
            "(#(0 9 2 3) 1)",
        ),
        // PSETQ still rejects a non-variable, unlike PSETF.
        (
            "(handler-case (eval '(psetq (car x) 1)) (error () :error))",
            ":ERROR",
        ),
        // The DO family expands through PSETQ — the recursion hazard.
        (
            "(do ((i 0 (1+ i)) (acc nil (cons i acc))) ((= i 3) (reverse acc)))",
            "(0 1 2)",
        ),
        (
            "(do ((a 1 b) (b 2 a) (n 0 (1+ n))) ((> n 3) (list a b)))",
            "(1 2)",
        ),
        (
            "(do* ((i 0 (1+ i)) (j i (1+ j))) ((= i 3) (list i j)))",
            "(3 3)",
        ),
        ("(let ((n 0)) (dotimes (i 4 n) (incf n i)))", "6"),
        (
            "(let ((acc nil)) (dolist (x '(1 2 3) (reverse acc)) (push x acc)))",
            "(1 2 3)",
        ),
        ("(loop for i from 1 to 3 collect i)", "(1 2 3)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS DEFINE-SETF-EXPANDER: the body is enclosed in an implicit BLOCK named
/// after the ACCESS-FN, so `(return-from access-fn …)` exits the expander — the
/// same block DEFUN, DEFMACRO and long-form DEFSETF establish. egcl established
/// none, so such a body signalled CONTROL-ERROR.
///
/// The same gap bliss-ye6f fixed for DEFSETF, one construct over. An expander
/// that does not return early was unaffected, which is why only the ansi test
/// using RETURN-FROM caught it. ansi DEFINE-SETF-EXPANDER.5; bliss-0f76.
#[test]
fn define_setf_expander_body_has_an_implicit_block() {
    let cases = [
        // RETURN-FROM the access-fn exits the expander with its five values.
        (
            "(progn (defun dsx-a (x) (car x)) \
               (define-setf-expander dsx-a (x) \
                 (let ((s (gensym)) (tmp (gensym))) \
                   (return-from dsx-a \
                     (values (list tmp) (list x) (list s) \
                             `(setf (car ,tmp) ,s) `(car ,tmp))))) \
               (let ((c (cons 1 2))) (list (setf (dsx-a c) 9) c)))",
            "(9 (9 . 2))",
        ),
        // An expander that simply returns its values is unaffected.
        (
            "(progn (defun dsx-b (x) (car x)) \
               (define-setf-expander dsx-b (x) \
                 (let ((s (gensym)) (tmp (gensym))) \
                   (values (list tmp) (list x) (list s) \
                           `(setf (car ,tmp) ,s) `(car ,tmp)))) \
               (let ((c (cons 1 2))) (list (setf (dsx-b c) 9) c)))",
            "(9 (9 . 2))",
        ),
        // The expander also drives the updating macros.
        (
            "(progn (defun dsx-c (x) (car x)) \
               (define-setf-expander dsx-c (x) \
                 (let ((s (gensym)) (tmp (gensym))) \
                   (return-from dsx-c \
                     (values (list tmp) (list x) (list s) \
                             `(setf (car ,tmp) ,s) `(car ,tmp))))) \
               (let ((c (cons 1 2))) (incf (dsx-c c)) c))",
            "(2 . 2)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// FDEFINITION names any FUNCTION NAME, which includes `(setf place)`, so
/// `(setf (fdefinition '(setf f)) fn)` installs a writer. egcl's place accepted
/// only a symbol and signalled a TYPE-ERROR, and a writer installed that way was
/// invisible to FBOUNDP because writer discovery consulted GLOBAL_SETF_FNS and
/// the bytecode registry but not the mangled symbol's function cell.
/// ansi FDEFINITION.5 (partly — see below); bliss-rg32.
///
/// SYMBOL-FUNCTION keeps rejecting a non-symbol, which is the reason the two
/// accessors could not simply share one path.
#[test]
fn setf_of_fdefinition_accepts_a_setf_name() {
    let cases = [
        // The place accepts the name and FBOUNDP sees the result.
        (
            "(let* ((s (gensym)) (n (list 'setf s))) \
               (list (fboundp n) (progn (setf (fdefinition n) (fdefinition 'cons)) \
                                        (not (not (fboundp n))))))",
            "(NIL T)",
        ),
        (
            "(progn (setf (fdefinition '(setf sfd-a)) (fdefinition 'cons)) (not (not (fboundp '(setf sfd-a)))))",
            "T",
        ),
        // Plain symbols are unaffected.
        (
            "(let ((g (gensym))) (setf (fdefinition g) #'car) (funcall g '(1 2)))",
            "1",
        ),
        (
            "(let ((g (gensym))) (setf (symbol-function g) #'car) (funcall g '(3 4)))",
            "3",
        ),
        // SYMBOL-FUNCTION still refuses a non-symbol.
        (
            "(handler-case (eval '(setf (symbol-function '(setf sfd-b)) #'cons)) (error () :error))",
            ":ERROR",
        ),
        // An ordinary (defun (setf f) …) writer is untouched by the discovery
        // change: it still runs and still writes. (What SETF RETURNS through a
        // writer is shape-dependent in egcl and tracked separately as
        // bliss-9298; not asserted here, since pinning today's answer would pin
        // a bug.)
        (
            "(progn (defun (setf sfd-c) (v x) (setf (car x) v))                (let ((c (cons 1 2))) (setf (sfd-c c) 9) c))",
            "(9 . 2)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// SETF yields the STORE FORM's value, which for a `(setf f)` writer is the
/// writer's own return. The branch that calls a LEXICAL writer from FLET/LABELS
/// discarded that value and fell through to the new value:
///
///     (flet (((setf f) (&rest args) (declare (ignore args)) 'a)) (setf (f) 10))
///       =>  10, must be A
///
/// The writer ran correctly all along — a logging writer showed the right
/// arguments — so only the result was wrong, which is why this reads as "SETF
/// ignored the binding" until you look. ansi FLET.51 / LABELS.26; bliss-n0dc.
#[test]
fn setf_yields_a_lexical_writers_value() {
    let cases = [
        (
            "(flet (((setf lw-a) (&rest args) (declare (ignore args)) 'a)) (setf (lw-a) 10))",
            "A",
        ),
        (
            "(labels (((setf lw-b) (&rest args) (declare (ignore args)) 'a)) (setf (lw-b) 10))",
            "A",
        ),
        (
            "(flet (((setf lw-c) (v x) (list :local v x))) (setf (lw-c 'p) 'q))",
            "(:LOCAL Q P)",
        ),
        // The writer still runs, with the new value first then the subforms.
        (
            "(let ((log nil)) \
               (flet (((setf lw-d) (v x) (push (list :called v x) log) :writer-val)) \
                 (list (setf (lw-d 'p) 'q) log)))",
            "(:WRITER-VAL ((:CALLED Q P)))",
        ),
        // A lexical writer shadows a global one of the same name.
        (
            "(progn (defun (setf lw-e) (v x) (list :global v x)) \
               (flet (((setf lw-e) (v x) (list :local v x))) (setf (lw-e 'p) 'q)))",
            "(:LOCAL Q P)",
        ),
        // ...and outside the FLET the global one is used again.
        (
            "(progn (defun (setf lw-f) (v x) (list :global v x)) \
               (flet (((setf lw-f) (v x) (list :local v x))) (setf (lw-f 'p) 'q)) \
               (setf (lw-f 'p) 'q))",
            "(:GLOBAL Q P)",
        ),
        // Ordinary local functions and ordinary places are unaffected.
        ("(flet ((f (x) (* x 2))) (f 4))", "8"),
        ("(let ((c (cons 1 2))) (setf (car c) 9) c)", "(9 . 2)"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CLHS 3.1.2.1.2.2: operator position consults the LEXICAL function namespace
/// first, so an FLET/LABELS binding shadows a global macro of the same name just
/// as it shadows a global function. The tree-walker expanded the macro instead:
///
///     (defmacro f () :bad) (flet ((f () :good)) (f))   =>  :BAD, must be :GOOD
///
/// A MACROLET macro lives in that same lexical namespace, so it still wins — only
/// a GLOBAL macro yields to the binding. ansi FLET.73 / LABELS.51; bliss-k2ia.
///
/// These two pass standalone and failed only in the chapter, because the ansi
/// file defines the shadowed macro immediately before the test — a probe without
/// that DEFMACRO cannot reproduce them.
#[test]
fn a_lexical_binding_shadows_a_global_macro() {
    let cases = [
        (
            "(progn (defmacro shm-a () :bad) (flet ((shm-a () :good)) (shm-a)))",
            ":GOOD",
        ),
        (
            "(progn (defmacro shm-b () :bad) (labels ((shm-b () :good)) (shm-b)))",
            ":GOOD",
        ),
        // A MACROLET macro still shadows a global macro.
        (
            "(progn (defmacro shm-c () :bad) (macrolet ((shm-c () :good)) (shm-c)))",
            ":GOOD",
        ),
        // A lexical binding still shadows a global FUNCTION.
        (
            "(progn (defun shm-d () :bad) (flet ((shm-d () :good)) (shm-d)))",
            ":GOOD",
        ),
        // Outside the binding the global macro applies again.
        (
            "(progn (defmacro shm-e () :bad) (flet ((shm-e () :good)) (shm-e)) (shm-e))",
            ":BAD",
        ),
        // A global macro with no lexical binding still expands, and ordinary
        // macros keep working — this edits the hot operator dispatch.
        (
            "(progn (defmacro shm-f (x) `(list ,x ,x)) (shm-f 3))",
            "(3 3)",
        ),
        ("(let ((x 5)) (incf x) x)", "6"),
        ("(when t :yes)", ":YES"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// FLET/LABELS functions and MACROLET macros share ONE lexical namespace, so the
/// INNERMOST binding of a name wins:
///
///     (macrolet ((%f () :bad)) (flet ((%f () :good)) (%f)))   =>  :BAD, must be :GOOD
///
/// egcl keeps the two in separate maps with no ordering between them, so
/// whichever was consulted first won regardless of nesting. Shadowing is now
/// modelled at BINDING time — binding a function drops any lexical macro of that
/// name and vice versa — so "innermost wins" falls out of the maps and the lookup
/// order stops mattering. ansi MACROLET.37; bliss-9pqw.
#[test]
fn the_innermost_lexical_binding_wins() {
    let cases = [
        // Either nesting: the inner one wins.
        (
            "(macrolet ((%f () :bad)) (flet ((%f () :good)) (%f)))",
            ":GOOD",
        ),
        (
            "(macrolet ((%f () :bad)) (labels ((%f () :good)) (%f)))",
            ":GOOD",
        ),
        (
            "(flet ((%g () :bad)) (macrolet ((%g () :good)) (%g)))",
            ":GOOD",
        ),
        // The outer binding applies again outside the inner one.
        (
            "(macrolet ((%h () :outer)) (list (flet ((%h () :inner)) (%h)) (%h)))",
            "(:INNER :OUTER)",
        ),
        (
            "(flet ((%i () :outer)) (list (macrolet ((%i () :inner)) (%i)) (%i)))",
            "(:INNER :OUTER)",
        ),
        // And both still shadow their global counterparts (bliss-k2ia).
        (
            "(progn (defmacro tilb-a () :bad) (flet ((tilb-a () :good)) (tilb-a)))",
            ":GOOD",
        ),
        (
            "(progn (defun tilb-b () :bad) (macrolet ((tilb-b () :good)) (tilb-b)))",
            ":GOOD",
        ),
        // Ordinary nesting of distinct names is unaffected.
        (
            "(macrolet ((%m (x) `(list ,x))) (flet ((%n (y) (* y 2))) (%m (%n 3))))",
            "(6)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// MULTIPLE-VALUE-SETQ assigned by NAME, so an uninterned (gensym) variable was
/// never written: LET binds such a variable by symbol INDEX, and a gensym is not
/// in the name registry.
///
///     (let ((g (gensym))) (eval `(let (,g) (multiple-value-setq (,g) (values 7)) ,g)))
///       =>  NIL, must be 7
///
/// The form RETURNED the right value the whole time, so the assignment was lost
/// silently. SETQ, LET and MULTIPLE-VALUE-BIND all handled gensyms already — this
/// was specific to M-V-SETQ's store, which now uses the same by-symbol setter
/// SETQ does. ansi MULTIPLE-VALUE-SETQ.12; bliss-4rot.
#[test]
fn multiple_value_setq_assigns_to_an_uninterned_variable() {
    let cases = [
        (
            "(let ((g (gensym))) (eval `(let (,g) (multiple-value-setq (,g) (values 7)) ,g)))",
            "7",
        ),
        // The ansi shape: several gensym variables at once.
        (
            "(let ((a (gensym)) (b (gensym))) \
               (eval `(let (,a ,b) \
                        (and (eql (multiple-value-setq (,a ,b) (values-list '(2 1))) 2) \
                             (equal (list ,a ,b) '(2 1))))))",
            "T",
        ),
        // Ordinary variables, extra and missing values, and the returned value.
        (
            "(let (a b) (list (multiple-value-setq (a b) (values 1 2)) a b))",
            "(1 1 2)",
        ),
        (
            "(let (a b) (list (multiple-value-setq (a b) (values 1)) a b))",
            "(1 1 NIL)",
        ),
        (
            "(let (a) (list (multiple-value-setq (a) (values 1 2 3)) a))",
            "(1 1)",
        ),
        (
            "(let (a) (list (multiple-value-setq (a) (values)) a))",
            "(NIL NIL)",
        ),
        ("(multiple-value-setq nil :good)", ":GOOD"),
        // A symbol-macro variable is still assigned through its expansion, and a
        // special variable through its value cell.
        (
            "(symbol-macrolet ((sm (car c))) (let ((c (cons 1 2))) \
               (multiple-value-setq (sm) (values 9)) c))",
            "(9 . 2)",
        ),
        (
            "(progn (defvar *mvsu* 0) (multiple-value-setq (*mvsu*) (values 5)) *mvsu*)",
            "5",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// MAKE-ARRAY's plain simple-vector branch built an n-element LIST with
/// MAKE-LIST and then `(apply #'vector …)` it, which cost one Lisp-level CONS
/// call per element and spread the whole list as arguments: `(make-array
/// 100000)` took ~980ms, about 11x the 100k-iteration `(setf (aref a i) i)`
/// loop that fills it, and was what actually blew the ansi sequences chapter's
/// time budget (bliss-3o0r). It now allocates the storage in one step.
///
/// This pins the SHAPE, which is what a one-step allocator could plausibly get
/// wrong: the result must be indistinguishable from the vector the old
/// list-then-apply path produced.
#[test]
fn make_array_builds_a_plain_simple_vector() {
    let cases = [
        ("(make-array 3)", "#(NIL NIL NIL)"),
        ("(make-array 0)", "#()"),
        ("(make-array 3 :initial-element 7)", "#(7 7 7)"),
        // A list dimension of rank 1 takes the same branch.
        ("(make-array '(3) :initial-element 'a)", "#(A A A)"),
        ("(length (make-array 5))", "5"),
        ("(array-dimensions (make-array 5))", "(5)"),
        ("(array-rank (make-array 5))", "1"),
        (
            "(list (simple-vector-p (make-array 4)) (vectorp (make-array 4)) (arrayp (make-array 4)))",
            "(T T T)",
        ),
        // Not adjustable and no fill pointer, exactly like `(vector …)`.
        (
            "(list (adjustable-array-p (make-array 4)) (array-has-fill-pointer-p (make-array 4)))",
            "(NIL NIL)",
        ),
        (
            "(eq (type-of (make-array 4)) (type-of (vector nil nil nil nil)))",
            "T",
        ),
        // Writable, and every cell holds the SAME initial element (CLHS).
        (
            "(let ((a (make-array 3))) (setf (aref a 1) 9) a)",
            "#(NIL 9 NIL)",
        ),
        (
            "(let* ((c (cons 1 2)) (a (make-array 2 :initial-element c))) (eq (aref a 0) (aref a 1)))",
            "T",
        ),
        // A big one, to exercise the large-object allocation path.
        (
            "(let ((a (make-array 5000 :initial-element 3))) (list (length a) (aref a 4999)))",
            "(5000 3)",
        ),
        // The other branches must be untouched.
        ("(make-array 3 :initial-contents '(1 2 3))", "#(1 2 3)"),
        ("(make-array 3 :element-type 'character)", "\"   \""),
        ("(length (make-array 4 :fill-pointer 2))", "2"),
        ("(make-array '(2 2) :initial-element 0)", "#2A((0 0) (0 0))"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CONS and 1+/1- reached through FUNCALL/APPLY/MAPCAR — i.e. every call a
/// compiled function makes through the c2i adapter — used to be synthesized
/// back into `(CONS 'a 'b)` and re-evaluated, costing ~4.3us and ~5.1us a call
/// against ~2.9us for a builtin already on the fast path (bliss-3o0r). They now
/// dispatch on the already-evaluated arguments through the SAME kernels
/// operator position uses (`arena_cons`, `fold_arith_vals`, `sub_vals`), so the
/// tiers must stay bit-identical — including the numeric tower and the
/// arity/type errors, which is what this pins.
#[test]
fn cons_and_unary_arithmetic_match_operator_position() {
    let cases = [
        (
            "(list (cons 1 2) (funcall #'cons 1 2) (apply #'cons '(1 2)))",
            "((1 . 2) (1 . 2) (1 . 2))",
        ),
        ("(mapcar #'cons '(1 2) '(3 4))", "((1 . 3) (2 . 4))"),
        (
            "(list (1+ 5) (funcall #'1+ 5) (1- 5) (funcall #'1- 5))",
            "(6 6 4 4)",
        ),
        // The numeric tower: fixnum->bignum at the boundary, ratios, floats,
        // complex — a fixnum-only fast path would diverge here.
        ("(funcall #'1+ 4611686018427387903)", "4611686018427387904"),
        (
            "(funcall #'1- -4611686018427387904)",
            "-4611686018427387905",
        ),
        ("(list (funcall #'1+ 1/2) (funcall #'1- 1/2))", "(3/2 -1/2)"),
        ("(funcall #'1+ 2.5)", "3.5"),
        ("(funcall #'1- #c(1 2))", "#C(0 2)"),
        // CONS is exactly binary; 1+/1- are exactly unary and reject non-numbers.
        (
            "(handler-case (funcall #'cons 1) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (funcall #'cons 1 2 3) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (funcall #'1+ 'a) (type-error () :te))",
            ":TE",
        ),
        // A user redefinition must still win over the fast path.
        ("(progn (defun my1+ (x) (funcall #'1+ x)) (my1+ 10))", "11"),
        // A lexical function whose name is NOT a CL symbol still wins, and the
        // fast path is not consulted for it. (FLET-binding CL:CONS itself is
        // undefined consequences per CLHS 11.1.2.1.2, and the two backends
        // disagree about it — filed separately, not pinned here.)
        (
            "(flet ((my-cons (a b) (list :shadowed a b))) (my-cons 1 2))",
            "(:SHADOWED 1 2)",
        ),
        (
            "(progn (defun my-cons (a b) (cons b a)) (list (my-cons 1 2) (funcall #'my-cons 1 2)))",
            "((2 . 1) (2 . 1))",
        ),
        // MAKE-LIST, the heaviest CONS caller in the stdlib.
        ("(make-list 3 :initial-element 'x)", "(X X X)"),
        ("(length (make-list 500))", "500"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `%match-at`, SEARCH's inner loop, called `(nth i list)` for EVERY pattern
/// element, re-traversing the list from its head each time — so one SEARCH cost
/// O(plen * n^2) cdr steps instead of O(plen * n). SEARCH coerces both
/// arguments to lists, so every SEARCH on a string or vector paid it
/// (bliss-3o0r). It now walks with a single NTHCDR plus CDR.
///
/// The rewrite has to keep SEARCH's exact answers, including which end wins,
/// the empty-pattern cases, a pattern longer than what remains, and the
/// bounding/`:key`/`:test` keywords.
#[test]
fn search_finds_the_same_positions_after_the_walk_rewrite() {
    let cases = [
        ("(search \"ab\" \"xxabyyab\")", "2"),
        ("(search \"ab\" \"xxabyyab\" :from-end t)", "6"),
        // Empty pattern: START2 normally, END2 from the end (CLHS).
        ("(search \"\" \"abc\")", "0"),
        ("(search \"\" \"abc\" :from-end t)", "3"),
        ("(search \"\" \"abc\" :start2 1)", "1"),
        // Pattern longer than the target, and longer than the remaining tail —
        // the walk must run out of list and fail, not error.
        ("(search \"abc\" \"ab\")", "NIL"),
        ("(search \"bcd\" \"abcd\" :start2 2)", "NIL"),
        ("(search '(1 2 3) '(1 2))", "NIL"),
        // Lists and vectors, both ends.
        ("(search '(1 2) '(0 1 2 3 1 2))", "1"),
        ("(search '(1 2) '(0 1 2 3 1 2) :from-end t)", "4"),
        ("(search #(1 2) #(0 1 2 3 1 2))", "1"),
        // Bounding indices on both sequences.
        ("(search \"ab\" \"xxabyy\" :start2 3)", "NIL"),
        ("(search \"ab\" \"xxabyyab\" :end2 4)", "2"),
        ("(search \"bc\" \"abcd\" :start1 1)", "2"),
        ("(search \"xbc\" \"abcd\" :start1 1)", "1"),
        // :test and :key.
        ("(search \"AB\" \"xxabyy\" :test #'char-equal)", "2"),
        (
            "(search '(a) \"0\" :key (lambda (x) (if (eql x #\\0) 'a x)))",
            "0",
        ),
        ("(search '(1 2) '(9 1 2) :test-not #'eql)", "0"),
        // A match at the very last position, which an off-by-one in the walk
        // would drop.
        ("(search \"cd\" \"abcd\")", "2"),
        ("(search '(3) '(1 2 3))", "2"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// Operator dispatch memoizes a symbol's BARE (qualifier-stripped, upcased)
/// name by symbol index, because `symbol_bare_name` allocates a fresh String
/// every call and dispatch called it twice per evaluated form plus once more
/// per `apply_function` — 18% of all instructions on dispatch-heavy interpreted
/// code (bliss-edzd).
///
/// A cache is only safe with its invalidation, so this pins the two things that
/// can change what a symbol's name resolves to: RENAME-PACKAGE (which rewrites
/// the qualifier) and a symbol reached under several spellings. The
/// arity/shadowing decisions that dispatch makes from the bare name must come
/// out the same before and after.
#[test]
fn dispatch_sees_the_right_bare_name_across_package_renames() {
    let cases = [
        // The bare name survives a package rename, and the symbol keeps its
        // identity, so dispatch decisions are unchanged.
        (
            "(progn (defpackage :bnc1 (:use :cl)) \
               (let ((s (intern \"MYSYM\" :bnc1))) \
                 (rename-package :bnc1 :bnc2) \
                 (list (symbol-name s) (package-name (symbol-package s)) \
                       (eq s (intern \"MYSYM\" :bnc2)))))",
            "(\"MYSYM\" \"BNC2\" T)",
        ),
        // A function defined in a package still dispatches after the rename.
        (
            "(progn (defpackage :bnc3 (:use :cl)) (in-package :bnc3) \
               (defun bnc-f (x) (* x 2)) \
               (let ((before (bnc-f 4))) \
                 (rename-package :bnc3 :bnc4) \
                 (list before (bnc-f 5))) )",
            "(8 10)",
        ),
        // The fixed-arity builtin guard reads the bare name: it must still fire
        // for a genuine builtin, and still stand down for a shadowing
        // definition, on both the operator and the funcall path.
        ("(handler-case (cons 1) (program-error () :pe))", ":PE"),
        (
            "(handler-case (funcall #'cons 1) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (consp 'a 'b) (program-error () :pe))", ":PE"),
        (
            "(progn (defun bnc-cons (a b) (cons b a)) (bnc-cons 1 2))",
            "(2 . 1)",
        ),
        // Qualified spellings of one symbol reach the same dispatch decision.
        (
            "(list (cl:car '(1 2)) (car '(1 2)) (funcall #'cl:car '(1 2)))",
            "(1 1 1)",
        ),
        // A gensym has no interned index; its name must not be cached under a
        // colliding slot.
        (
            "(let ((g (gensym))) (eval (list 'let (list (list g 7)) g)))",
            "7",
        ),
        ("(let ((g (gensym \"PFX\"))) (eq g (car (list g))))", "T"),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// `&environment` objects are registered in a side table so a handle can travel
/// through Lisp as a value, and that table used to only ever GROW — every macro
/// expansion with an `&environment` parameter minted an entry and nothing ever
/// removed one. Two costs compounded: the entries were never freed, and the GC's
/// root scan walks EVERY entry on EVERY collection, so each expansion made all
/// later collections slower. On the ansi sequences workload that was 1.33G of
/// host RSS against a nearly empty Lisp heap (bliss-7x7o).
///
/// CLHS 3.1.1.4 gives environment objects dynamic extent, so they are now
/// dropped when the expander returns. What must keep working is everything an
/// expander legitimately does with one WHILE it runs — including a nested
/// expansion, whose own scope must not reclaim the outer environment.
#[test]
fn environment_objects_survive_the_expansion_that_uses_them() {
    let cases = [
        // MACROEXPAND-1 through the passed environment sees a MACROLET macro.
        (
            "(progn (defmacro me (x &environment e) (list 'quote (macroexpand-1 x e))) \
               (macrolet ((inner () :from-macrolet)) (me (inner))))",
            ":FROM-MACROLET",
        ),
        // ... and a SYMBOL-MACROLET expansion.
        (
            "(progn (defmacro me (x &environment e) (list 'quote (macroexpand-1 x e))) \
               (symbol-macrolet ((s 9)) (me s)))",
            "9",
        ),
        // Two MACROLET macros expanded through one environment.
        (
            "(progn (defmacro me (x &environment e) (list 'quote (macroexpand-1 x e))) \
               (macrolet ((a () :a)) (macrolet ((b () :b)) (list (me (a)) (me (b))))))",
            "(:A :B)",
        ),
        // A NESTED expansion: the inner macro's own scope must not reclaim the
        // environment the outer expander is still using.
        (
            "(progn (defmacro inner-m (x &environment e) (list 'quote (macroexpand-1 x e))) \
               (defmacro outer-m (x &environment e) \
                 (list 'list (list 'quote (macroexpand-1 x e)) (list 'inner-m x))) \
               (macrolet ((q () :deep)) (outer-m (q))))",
            "(:DEEP :DEEP)",
        ),
        // Repeated expansions all still resolve — a stale-id bug would make a
        // later one miss.
        (
            "(progn (defmacro me (x &environment e) (list 'quote (macroexpand-1 x e))) \
               (macrolet ((r () :r)) (list (me (r)) (me (r)) (me (r)) (me (r)))))",
            "(:R :R :R :R)",
        ),
        // An expander that never touches its environment still works.
        (
            "(progn (defmacro ign (x &environment e) (declare (ignore e)) (list 'quote x)) (ign (a b)))",
            "(A B)",
        ),
        // MACROEXPAND (not -1) and a non-macro form through the environment.
        (
            "(progn (defmacro me2 (x &environment e) (list 'quote (macroexpand x e))) \
               (macrolet ((c () '(d))) (macrolet ((d () :d)) (me2 (c)))))",
            ":D",
        ),
        (
            "(progn (defmacro me (x &environment e) (list 'quote (macroexpand-1 x e))) (me (+ 1 2)))",
            "(+ 1 2)",
        ),
    ];
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", expr.to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// CONCATENATE and MAP classify their result type independently — MAP carried
/// its own smaller copy inline in the interpreter, which recognized neither
/// compound specifiers like `(simple-string 3)`, nor SIMPLE-BASE-STRING, nor any
/// bit-vector type, and silently answered a LIST for all of them (ansi
/// CONCATENATE.10-15/35-40, MAP.38-47, MAP.FILL.5). They now share one stdlib
/// builder, so the two cannot drift apart again.
#[test]
fn concatenate_and_map_honour_every_result_type() {
    let cases = [
        // Bit-vector result types, bare and compound, including the empty case.
        ("(concatenate 'bit-vector nil)", "#*"),
        ("(concatenate 'bit-vector)", "#*"),
        (
            "(concatenate 'bit-vector '(0 1 1) nil #(1 0 1) #())",
            "#*011101",
        ),
        (
            "(concatenate 'simple-bit-vector '(0 1 1) nil #(1 0 1) #())",
            "#*011101",
        ),
        ("(map 'bit-vector #'identity '(0 1 1))", "#*011"),
        ("(map 'simple-bit-vector #'identity '(0 0 0))", "#*000"),
        // A bit vector named through the ELEMENT TYPE rather than the head.
        ("(map '(vector bit) #'identity '(0 0 0 0 0 1))", "#*000001"),
        (
            "(map '(vector bit 6) #'identity '(0 0 0 0 0 1))",
            "#*000001",
        ),
        (
            "(map '(vector bit *) #'identity '(0 0 0 0 0 1))",
            "#*000001",
        ),
        (
            "(map '(bit-vector 6) #'identity '(0 0 0 0 0 1))",
            "#*000001",
        ),
        (
            "(map '(simple-bit-vector *) #'identity '(0 0 0 0 0 1))",
            "#*000001",
        ),
        // ... but a non-BIT element type is still a general vector.
        (
            "(map '(simple-vector *) #'identity '(0 0 0 0 0 1))",
            "#(0 0 0 0 0 1)",
        ),
        (
            "(map '(simple-vector 6) #'identity '(0 0 0 0 0 1))",
            "#(0 0 0 0 0 1)",
        ),
        ("(map '(vector t) #'identity '(1 2))", "#(1 2)"),
        // A non-bit element is a TYPE-ERROR, not a silently coerced 1.
        (
            "(handler-case (concatenate 'bit-vector '(0 2)) (type-error () :te))",
            ":TE",
        ),
        // Compound string specifiers, with and without a length.
        (
            "(concatenate '(simple-string) \"abc\" \"def\")",
            "\"abcdef\"",
        ),
        (
            "(concatenate '(simple-string *) \"abc\" \"def\")",
            "\"abcdef\"",
        ),
        (
            "(concatenate '(simple-string 6) \"abc\" \"def\")",
            "\"abcdef\"",
        ),
        ("(concatenate '(string) \"abc\" \"def\")", "\"abcdef\""),
        ("(concatenate '(string 6) \"abc\" \"def\")", "\"abcdef\""),
        (
            "(map '(simple-string 3) #'identity '(#\\a #\\b #\\c))",
            "\"abc\"",
        ),
        (
            "(map '(base-string) #'identity '(#\\a #\\b #\\c))",
            "\"abc\"",
        ),
        (
            "(map 'simple-base-string #'identity '(#\\a #\\b #\\c))",
            "\"abc\"",
        ),
        (
            "(map '(simple-base-string *) #'identity '(#\\a #\\b #\\c))",
            "\"abc\"",
        ),
        // The cases that already worked must not move.
        ("(concatenate 'list \"ab\" '(1))", "(#\\a #\\b 1)"),
        ("(concatenate 'vector '(1 2))", "#(1 2)"),
        ("(concatenate 'string \"ab\" \"cd\")", "\"abcd\""),
        ("(map 'list #'1+ '(1 2))", "(2 3)"),
        ("(map 'vector #'1+ '(1 2))", "#(2 3)"),
        ("(map 'string #'identity \"ab\")", "\"ab\""),
        // (map nil …) is for effect and returns NIL — the one case the shared
        // builder cannot express, since NIL also names the empty list.
        ("(map nil #'identity '(1 2))", "NIL"),
        // Too few arguments to MAP is a PROGRAM-ERROR, not an internal error.
        ("(handler-case (map 'list) (program-error () :pe))", ":PE"),
        (
            "(handler-case (map 'list #'null) (program-error () :pe))",
            ":PE",
        ),
        // A result type that names no concrete sequence representation is an
        // error. SEQUENCE is a valid type specifier but does not determine a
        // representation, so it is rejected too.
        (
            "(handler-case (concatenate 'sequence '(a b c)) (error () :err))",
            ":ERR",
        ),
        (
            "(handler-case (concatenate 'fixnum '(a b c)) (error () :err))",
            ":ERR",
        ),
        (
            "(handler-case (map 'symbol #'identity '(a b c)) (type-error () :te))",
            ":TE",
        ),
        // A length the specifier DECLARES must match what was produced.
        (
            "(handler-case (concatenate '(vector * 3) '(a b c d e)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (map '(vector * 8) #'identity '(a b c)) (type-error () :te))",
            ":TE",
        ),
        ("(concatenate '(vector * 2) '(1 2))", "#(1 2)"),
    ];
    run_expression_cases(&cases);
}

/// REMOVE-DUPLICATES, DELETE-DUPLICATES and MAKE-SEQUENCE took `&rest keys` and
/// read them with GETF, which silently accepts an odd-length keyword list, a
/// non-keyword in a keyword position, and an unrecognized keyword — all three
/// are PROGRAM-ERRORs per CLHS. They now declare real `&key` lambda lists, and
/// the binder (which already gets this right) does the checking (ansi
/// REMOVE-DUPLICATES.ERROR.2/4/5/6, DELETE-DUPLICATES.ERROR.*,
/// MAKE-SEQUENCE.ERROR.9-12).
#[test]
fn keyword_argument_lists_are_validated() {
    let cases = [
        (
            "(handler-case (remove-duplicates nil :start) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (remove-duplicates nil 'bad t) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (remove-duplicates nil 1 2) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (delete-duplicates nil :start) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (delete-duplicates nil 'bad t) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (make-sequence 'list 10 :bad t) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (make-sequence 'list 10 :initial-element) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (make-sequence 'list 10 0 0) (program-error () :pe))",
            ":PE",
        ),
        // :ALLOW-OTHER-KEYS still suppresses the unknown-keyword check.
        (
            "(handler-case (make-sequence 'list 2 :bad t :allow-other-keys t) (error () :err))",
            "(NIL NIL)",
        ),
        // Every keyword these functions actually accept must still work, and
        // REMOVE-DUPLICATES keeps the LAST of each duplicate unless :from-end.
        ("(remove-duplicates '(1 2 1 3))", "(2 1 3)"),
        ("(remove-duplicates '(1 2 1 3) :from-end t)", "(1 2 3)"),
        ("(remove-duplicates '(1 2 1 3) :key #'identity)", "(2 1 3)"),
        ("(remove-duplicates '(1 2 1 3) :start 1)", "(1 2 1 3)"),
        ("(remove-duplicates '(1 2 1 3) :test #'eql)", "(2 1 3)"),
        ("(delete-duplicates (list 1 2 1))", "(2 1)"),
        ("(make-sequence 'list 3 :initial-element 7)", "(7 7 7)"),
        ("(make-sequence 'vector 2)", "#(NIL NIL)"),
        // SORT and STABLE-SORT hand-parsed their trailing arguments and
        // silently IGNORED anything malformed, so these all sorted happily.
        // Both also require a sequence AND a predicate.
        ("(handler-case (sort) (program-error () :pe))", ":PE"),
        ("(handler-case (sort nil) (program-error () :pe))", ":PE"),
        ("(handler-case (stable-sort) (program-error () :pe))", ":PE"),
        (
            "(handler-case (stable-sort nil) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (sort nil #'< :key) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (sort nil #'< 'bad t) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (sort nil #'< 1 2) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (stable-sort nil #'< :key) (program-error () :pe))",
            ":PE",
        ),
        ("(sort (list 3 1 2) #'<)", "(1 2 3)"),
        ("(sort (list 3 1 2) #'< :key #'identity)", "(1 2 3)"),
        ("(stable-sort (list 3 1 2) #'<)", "(1 2 3)"),
        ("(sort (vector 2 1) #'<)", "#(1 2)"),
        (
            "(sort (list 3 1 2) #'< :bad t :allow-other-keys t)",
            "(1 2 3)",
        ),
    ];
    run_expression_cases(&cases);
}

/// Calling a standard function with the wrong number of arguments is a
/// PROGRAM-ERROR (CLHS). These sequence functions had no arity entry, so the
/// call reached the body and answered a TYPE-ERROR about NIL — or nothing at
/// all (ansi ELT.ERROR.1/2/3, LENGTH.ERROR.1/2, REVERSE.ERROR.1/2,
/// SUBSEQ.ERROR.2/3, CONCATENATE.ERROR.3).
#[test]
fn sequence_functions_signal_program_error_on_bad_arity() {
    let cases = [
        ("(handler-case (elt) (program-error () :pe))", ":PE"),
        ("(handler-case (elt nil) (program-error () :pe))", ":PE"),
        (
            "(handler-case (elt nil 0 nil) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (length) (program-error () :pe))", ":PE"),
        (
            "(handler-case (length nil nil) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (reverse nil nil) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (nreverse nil nil) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (subseq nil) (program-error () :pe))", ":PE"),
        (
            "(handler-case (subseq nil 0 0 0) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (concatenate) (program-error () :pe))", ":PE"),
        // The well-formed calls must be untouched, including SUBSEQ's optional
        // end and CONCATENATE with a single argument.
        (r#"(length "ab")"#, "2"),
        ("(elt '(1 2) 1)", "2"),
        ("(reverse '(1 2))", "(2 1)"),
        (r#"(subseq "abcd" 1)"#, r#""bcd""#),
        (r#"(subseq "abcd" 1 3)"#, r#""bc""#),
        ("(concatenate 'list '(1))", "(1)"),
    ];
    run_expression_cases(&cases);
}

/// MAKE-SEQUENCE never checked its result type: `(make-sequence 'symbol 10)`
/// built a vector, and a length the specifier declared was ignored, so
/// `(make-sequence '(string 4) 3)` built a 3-character string. CLHS makes both a
/// TYPE-ERROR (ansi MAKE-SEQUENCE.ERROR.1-16).
#[test]
fn make_sequence_validates_its_result_type() {
    let cases = [
        (
            "(handler-case (make-sequence 'symbol 10) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence 'null 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence 'cons 0) (type-error () :te))",
            ":TE",
        ),
        // A declared length must agree with SIZE, in either direction, and in
        // both the (vector et size) and (string size) positions.
        (
            "(handler-case (make-sequence '(vector * 4) 3) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence '(vector * 2) 3) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence '(string 4) 3) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence '(simple-string 2) 3) (type-error () :te))",
            ":TE",
        ),
        // An agreeing or unspecified length is fine.
        (
            "(make-sequence '(vector * 3) 3 :initial-element 0)",
            "#(0 0 0)",
        ),
        (
            "(make-sequence '(string 3) 3 :initial-element #\\z)",
            "\"zzz\"",
        ),
        ("(make-sequence '(vector *) 2 :initial-element 1)", "#(1 1)"),
        // The ordinary cases.
        ("(make-sequence 'list 3 :initial-element 'x)", "(X X X)"),
        ("(make-sequence 'vector 3 :initial-element 1)", "#(1 1 1)"),
        ("(make-sequence 'string 3 :initial-element #\\a)", "\"aaa\""),
        ("(make-sequence 'null 0)", "NIL"),
        ("(make-sequence 'cons 2 :initial-element 5)", "(5 5)"),
        ("(make-sequence 'bit-vector 4)", "#*0000"),
        // A CLASS object designates its name.
        (
            "(make-sequence (find-class 'cons) 4 :initial-element 'x)",
            "(X X X X)",
        ),
        // The TYPE-ERROR must be truthful: its datum must NOT satisfy its own
        // expected-type. Reporting the result type against SEQUENCE failed this
        // — a compound specifier like (VECTOR * 4) is a list, hence itself a
        // sequence — so the size is reported against the length demanded.
        (
            "(handler-case (make-sequence '(vector * 4) 3)             (type-error (c) (list (type-error-datum c) (type-error-expected-type c))))",
            "(3 (EQL 4))",
        ),
        (
            "(handler-case (make-sequence '(vector * 4) 3)             (type-error (c) (typep (type-error-datum c) (type-error-expected-type c))))",
            "NIL",
        ),
        (
            "(handler-case (make-sequence 'null 1)             (type-error (c) (typep (type-error-datum c) (type-error-expected-type c))))",
            "NIL",
        ),
        (
            "(handler-case (make-sequence 'cons 0)             (type-error (c) (typep (type-error-datum c) (type-error-expected-type c))))",
            "NIL",
        ),
        (
            "(handler-case (make-sequence 'symbol 10)             (type-error (c) (typep (type-error-datum c) (type-error-expected-type c))))",
            "NIL",
        ),
    ];
    run_expression_cases(&cases);
}

/// Run `(expr, expected-printed-form)` pairs through BOTH backends: compiled as
/// written, and tree-walked via EVAL. Nine of the fixes in the
/// data-and-control-flow grind were single-backend divergences, so every case
/// has to be checked twice.
fn run_expression_cases(cases: &[(&str, &str)]) {
    for (expr, expected) in cases {
        for (path, form) in [
            ("compiled", (*expr).to_string()),
            ("tree-walked", format!("(eval '{expr})")),
        ] {
            let output = egcl_bin()
                .args(["--eval", &format!("(cl:format t \"~S~%\" {form})")])
                .output()
                .expect("failed to run egcl");
            assert_eq!(
                output.status.code(),
                Some(0),
                "{path} case errored: {expr}\nstderr: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim(),
                *expected,
                "{path} case: {expr}"
            );
        }
    }
}

/// Deep self-recursion must raise a catchable STORAGE-CONDITION, not run off the
/// C stack. T2's direct self-call skips c2i_call_args and with it the
/// native_depth_cap() check that is the only bound on recursion depth in
/// compiled code — the T2 prologue has no stack guard — so a deeply recursive
/// function returns a WRONG ANSWER instead of signalling: `(deep 400000)`
/// answers 30, and `(deep 200000)` answers a raw stack address (bliss-b4fd).
///
/// Fixed by guarding the self-call against a published stack limit: past it the
/// call takes the c2i path, which enforces the cap and drops to a flat T0 `run()`.
/// Dropping the optimization was not an option — without it fib(30) goes from
/// 3ms to 540ms — and the guard costs about 10% on tight self-recursion.
///
/// The `default` row is the regression test proper; the others pin that every
/// path which never took the direct self-call still behaves, so a future change
/// cannot quietly fix one and break another.
#[test]
fn deep_self_recursion_signals_rather_than_running_off_the_stack() {
    let program = "(progn \
        (defun deep (n) (if (= n 0) 0 (1+ (deep (1- n))))) \
        (dotimes (i 200) (deep 50)) \
        (cl:format t \"~S~%\" (handler-case (deep 400000) \
                                (storage-condition () :storage-condition) \
                                (error () :error))))";
    for (label, extra_env) in [
        // T2 with its direct self-call ACTIVE — the case that used to answer 30,
        // or a raw stack address, or segfault. The stack guard now sends it to
        // c2i once the stack runs low, which enforces the depth cap.
        ("default", vec![]),
        // And every tier that never took the direct self-call in the first place.
        ("t0", vec![("EGCL_FORCE_TIER", "t0")]),
        ("t1", vec![("EGCL_FORCE_TIER", "t1")]),
        ("no-t2", vec![("EGCL_DISABLE_T2", "1")]),
        (
            "no-direct-self-call",
            vec![("EGCL_NO_DIRECT_SELF_CALL", "1")],
        ),
    ] {
        let mut cmd = egcl_bin();
        for (k, v) in &extra_env {
            cmd.env(k, v);
        }
        let output = cmd
            .args(["--eval", program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "{label}: egcl did not exit cleanly\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            ":STORAGE-CONDITION",
            "{label}: deep recursion must signal, not return a value"
        );
    }
    // The guard must not disturb recursion that fits: these are the depths real
    // code uses, and they must still compute, not signal.
    let shallow = "(progn \
        (defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) \
        (defun deep (n) (if (= n 0) 0 (1+ (deep (1- n))))) \
        (dotimes (i 300) (fib 15) (deep 100)) \
        (cl:format t \"~S~%\" (list (fib 25) (deep 1000) (deep 50000))))";
    let output = egcl_bin()
        .args(["--eval", shallow])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .next()
            .unwrap_or("")
            .trim(),
        "(75025 1000 50000)",
        "recursion that fits on the stack must still compute"
    );
}

/// A callee redefined AFTER its caller has been compiled and warmed must be
/// picked up by that caller.
///
/// This is the invariant any direct compiled-to-compiled call has to preserve:
/// once a caller stops going through c2i_call_args and calls a callee's entry
/// itself, a stale entry means jumping into code the program has replaced — or
/// freed. None of the existing suites exercise redefinition after warm-up, so a
/// missed invalidation would be invisible until a program did exactly this
/// (bliss-x5y.27).
///
/// All three ways a function can change identity are covered: DEFUN over it,
/// SETF of SYMBOL-FUNCTION, and FMAKUNBOUND.
#[test]
fn a_callee_redefined_after_warmup_is_picked_up_by_its_caller() {
    // Each form must be its OWN top-level form — a redefinition nested inside
    // the same form as the caller is not in effect when that form runs, which
    // is a property of evaluation order, not of the call path under test.
    let output = egcl_bin()
        .args([
            "--eval",
            "(defun g (n) (* n 2))",
            "--eval",
            "(defun f (n) (g n))",
            // Warm f (and with it the call to g) well past every tier threshold.
            "--eval",
            "(let ((acc 0)) (dotimes (i 300000) (setq acc (f 3))) acc)",
            "--eval",
            "(defvar *before* (f 3))",
            "--eval",
            "(defun g (n) (* n 100))",
            "--eval",
            "(defvar *after-defun* (f 3))",
            "--eval",
            "(setf (symbol-function 'g) (lambda (n) (* n 1000)))",
            "--eval",
            "(defvar *after-setf* (f 3))",
            "--eval",
            "(fmakunbound 'g)",
            "--eval",
            "(defvar *after-fmakunbound* \
                         (handler-case (f 3) \
                           (undefined-function () :undefined) \
                           (error () :error)))",
            "--eval",
            "(cl:format t \"~S~%\" (list *before* *after-defun* \
                                                  *after-setf* *after-fmakunbound*))",
        ])
        .output()
        .expect("failed to run egcl");
    assert_eq!(
        output.status.code(),
        Some(0),
        "redefinition-after-warmup errored\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Each --eval echoes its value, so the printed list is the LAST line.
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(6 300 3000 :UNDEFINED)"),
        "a warmed caller must follow its callee's redefinition; got:\n{stdout}"
    );
}

/// MAP-INTO ignores a result vector's FILL POINTER when deciding how many
/// elements to store — the limit is the vector's capacity — and sets the fill
/// pointer to the number actually stored (CLHS map-into).
///
/// Both halves were missing. LENGTH of a fill-pointer vector is its fill
/// pointer, so a result with fill-pointer 3 could never take more than three
/// elements; and the pointer was left wherever it started, so mapping two
/// elements into it still showed three (ansi MAP-INTO-ARRAY.8/9/10).
#[test]
fn map_into_fills_to_capacity_and_sets_the_fill_pointer() {
    let cases = [
        // Fewer elements than the starting fill pointer: it moves DOWN to 2.
        (
            "(let ((a (make-array 6 :initial-element 'x :fill-pointer 3))) \
            (map-into a #'identity '(1 2)))",
            "#(1 2)",
        ),
        // More than the starting fill pointer but within capacity: it moves UP.
        (
            "(let ((a (make-array 6 :initial-element 'x :fill-pointer 3))) \
            (map-into a #'identity '(1 2 3 4 5)))",
            "#(1 2 3 4 5)",
        ),
        // No source sequences: fill the whole capacity.
        (
            "(let ((a (make-array 6 :initial-element 'x :fill-pointer 3))) \
            (map-into a (lambda () 'y)))",
            "#(Y Y Y Y Y Y)",
        ),
        // The result is still the same object, now with the new fill pointer.
        (
            "(let ((a (make-array 4 :initial-element 0 :fill-pointer 4))) \
            (list (eq a (map-into a #'identity '(9))) (fill-pointer a)))",
            "(T 1)",
        ),
        // Sequences without fill pointers are unaffected.
        ("(map-into (list 0 0 0) #'1+ '(1 2 3))", "(2 3 4)"),
        ("(map-into (vector 0 0) #'1+ '(5 6))", "#(6 7)"),
        ("(map-into (make-string 3) #'identity \"abc\")", "\"abc\""),
        // Shortest input still wins, and multiple sequences still work.
        (
            "(map-into (vector 0 0 0) #'+ '(1 2 3) '(10 20))",
            "#(11 22 0)",
        ),
    ];
    run_expression_cases(&cases);
}

/// COUNT, COUNT-IF and COUNT-IF-NOT accept :FROM-END. The count itself cannot
/// depend on direction, but the ORDER in which :KEY and the test are applied
/// does, and ansi checks it — so omitting the keyword was not a harmless
/// simplification: every such call was a PROGRAM-ERROR for an unrecognized
/// keyword argument, 32 tests from one missing parameter (ansi COUNT-*.7/.9/.12,
/// COUNT*.ORDER.1, and the COUNT-IF* .16/.17 family).
#[test]
fn the_count_family_accepts_from_end() {
    let cases = [
        // Same answer either way, across every sequence kind.
        ("(count 1 '(2 1 1 2 3 1 4 1 7 6 1 8) :from-end t)", "5"),
        ("(count 1 #(2 1 1 2 3 1 4 1 7 6 1 8) :from-end t)", "5"),
        ("(count 1 #*00101100011011000 :from-end t)", "7"),
        ("(count #\\1 \"00101100011011000\" :from-end t)", "7"),
        ("(count-if #'evenp #(0 1 2 3 4) :from-end t)", "3"),
        ("(count-if-not #'oddp #(0 1 2 3 4) :from-end t)", "3"),
        // The visible difference: :KEY is applied back-to-front. Pushing onto a
        // list reverses it, so a forward walk leaves (3 2 1) and :from-end (1 2 3).
        (
            "(let ((acc nil)) (count 1 '(1 2 3) :key (lambda (x) (push x acc) x)) acc)",
            "(3 2 1)",
        ),
        (
            "(let ((acc nil)) (count 1 '(1 2 3) :from-end t :key (lambda (x) (push x acc) x)) acc)",
            "(1 2 3)",
        ),
        (
            "(let ((acc nil)) (count-if #'evenp '(1 2 3) :from-end t :key (lambda (x) (push x acc) x)) acc)",
            "(1 2 3)",
        ),
        // :from-end composes with the bounding indices.
        ("(count 1 '(1 1 1 1) :start 1 :end 3 :from-end t)", "2"),
        (
            "(count-if #'evenp #(0 2 4 6) :start 1 :end 3 :from-end t)",
            "2",
        ),
        // Everything that worked before still does.
        ("(count 1 '(1 2 1))", "2"),
        ("(count 1 '(1 2 1) :start 1)", "1"),
        ("(count-if #'evenp '(1 2 4))", "2"),
        ("(count-if-not #'evenp '(1 2 4))", "1"),
        ("(count 'a '(a b a) :test #'eq)", "2"),
        ("(count 'a '(a b a) :test-not #'eq)", "1"),
    ];
    run_expression_cases(&cases);
}

/// NSUBSTITUTE, NSUBSTITUTE-IF and NSUBSTITUTE-IF-NOT are DESTRUCTIVE: they
/// modify the sequence and return it. All three delegated to the
/// non-destructive version, so they answered a fresh sequence and left the
/// original untouched (ansi NSUBSTITUTE-LIST.2, NSUBSTITUTE-VECTOR.3 and the
/// -IF / -IF-NOT pairs).
#[test]
fn the_nsubstitute_family_modifies_in_place() {
    let cases = [
        (
            "(let ((x (copy-seq '(a b a c)))) (nsubstitute 'b 'a x) x)",
            "(B B B C)",
        ),
        (
            "(let ((x (copy-seq #(a b a c)))) (nsubstitute 'b 'a x) x)",
            "#(B B B C)",
        ),
        (
            "(let ((x (copy-seq \"abac\"))) (nsubstitute #\\b #\\a x) x)",
            "\"bbbc\"",
        ),
        (
            "(let ((x (copy-seq '(a b a c)))) (nsubstitute-if 'b (lambda (e) (eq e 'a)) x) x)",
            "(B B B C)",
        ),
        (
            "(let ((x (copy-seq #(a b a c)))) (nsubstitute-if 'b (lambda (e) (eq e 'a)) x) x)",
            "#(B B B C)",
        ),
        (
            "(let ((x (copy-seq '(a b a c)))) \
            (nsubstitute-if-not 'b (lambda (e) (not (eq e 'a))) x) x)",
            "(B B B C)",
        ),
        // The sequence returned is the very object passed in.
        ("(let ((x (list 1 2))) (eq x (nsubstitute 9 1 x)))", "T"),
        // The keyword semantics are unchanged, since one implementation still
        // computes the answer.
        (
            "(let ((x (copy-seq '(a a a)))) (nsubstitute 'b 'a x :count 2) x)",
            "(B B A)",
        ),
        (
            "(let ((x (copy-seq '(a a a)))) (nsubstitute 'b 'a x :start 1) x)",
            "(A B B)",
        ),
        (
            "(let ((x (copy-seq '(a a a)))) (nsubstitute 'b 'a x :count 1 :from-end t) x)",
            "(A A B)",
        ),
        (
            "(let ((x (copy-seq '(1 2 3)))) (nsubstitute 9 2 x :test #'eql) x)",
            "(1 9 3)",
        ),
        // The non-destructive versions must NOT have become destructive.
        (
            "(let ((x (copy-seq '(a b a c)))) (list (substitute 'b 'a x) x))",
            "((B B B C) (A B A C))",
        ),
    ];
    run_expression_cases(&cases);
}

/// SUBSEQ, COPY-SEQ and REVERSE of a bit vector answer a BIT VECTOR. The test
/// for that only recognized the SIMPLE representation, so a fill-pointer,
/// adjustable or displaced bit vector fell through to the general-vector branch
/// and `(copy-seq …)` gave #(0 0 1) instead of #*001 (ansi COPY-SEQ.12/13/14).
#[test]
fn bit_vector_ness_survives_subseq_copy_and_reverse() {
    let cases = [
        // Simple — this already worked and must keep working.
        (
            "(copy-seq (make-array '(4) :initial-contents '(0 0 1 0) :element-type 'bit))",
            "#*0010",
        ),
        // Fill-pointer: the copy is the fill-pointer's worth, still bits.
        (
            "(copy-seq (make-array '(4) :initial-contents '(0 0 1 0) \
                                :element-type 'bit :fill-pointer 3))",
            "#*001",
        ),
        // Displaced.
        (
            "(let ((a (make-array '(6) :initial-contents '(0 1 0 1 1 1) :element-type 'bit))) \
            (copy-seq (make-array 4 :element-type 'bit :displaced-to a)))",
            "#*0101",
        ),
        // SUBSEQ and REVERSE take the same path.
        (
            "(subseq (make-array '(4) :initial-contents '(0 0 1 0) \
                             :element-type 'bit :fill-pointer 4) 1 3)",
            "#*01",
        ),
        (
            "(reverse (make-array '(3) :initial-contents '(0 1 1) \
                              :element-type 'bit :fill-pointer 3))",
            "#*110",
        ),
        (
            "(bit-vector-p (copy-seq (make-array '(2) :initial-contents '(1 0) \
                                              :element-type 'bit :fill-pointer 2)))",
            "T",
        ),
        // Sequences that are not bit vectors are untouched.
        ("(copy-seq '(1 2))", "(1 2)"),
        ("(copy-seq #(1 2))", "#(1 2)"),
        ("(copy-seq \"ab\")", "\"ab\""),
        ("(reverse #(1 2 3))", "#(3 2 1)"),
        ("(subseq \"abcd\" 1 3)", "\"bc\""),
        // A fill-pointer vector of non-bits stays a general vector.
        (
            "(copy-seq (make-array 3 :initial-contents '(a b c) :fill-pointer 2))",
            "#(A B)",
        ),
    ];
    run_expression_cases(&cases);
}

/// Two things SUBSTITUTE and everything built on it got wrong.
///
/// A BIT VECTOR must come back as one: `%coerce-like` had no bit-vector clause,
/// so `(substitute 1 0 #*0101010101 …)` answered a general vector (ansi
/// SUBSTITUTE-BIT-VECTOR.24/25 and the NSUBSTITUTE pairs built on it).
///
/// A NEGATIVE :count means no matches at all — CLHS says the behaviour is as if
/// count were 0. Unclamped it reached LAST and SUBSEQ with a negative argument
/// and substituted everything instead of nothing (ansi SUBSTITUTE-*.14).
#[test]
fn substitute_keeps_bit_vectors_and_clamps_a_negative_count() {
    let cases = [
        ("(substitute 1 0 #*0101)", "#*1111"),
        (
            "(substitute 1 0 #*0101010101 :count 3 :from-end t)",
            "#*0101111111",
        ),
        ("(bit-vector-p (substitute 1 0 #*0101))", "T"),
        // Negative counts do nothing, on every sequence kind.
        ("(substitute 1 0 #*0101 :count -1)", "#*0101"),
        ("(substitute 'x 'a '(a a) :count -1)", "(A A)"),
        ("(substitute #\\x #\\a \"aa\" :count -1)", "\"aa\""),
        ("(substitute 'x 'a #(a a) :count -1)", "#(A A)"),
        // Zero behaves the same, which is the rule the clamp appeals to.
        ("(substitute 'x 'a '(a a) :count 0)", "(A A)"),
        // And the ordinary cases are unmoved, including which end :count takes.
        ("(substitute 'x 'a '(a b a))", "(X B X)"),
        ("(substitute 'x 'a '(a b a) :count 1)", "(X B A)"),
        (
            "(substitute 'x 'a '(a b a) :count 1 :from-end t)",
            "(A B X)",
        ),
        ("(substitute #\\x #\\a \"aba\")", "\"xbx\""),
        ("(substitute 'x 'a #(a b a))", "#(X B X)"),
        // The destructive variant inherits both fixes.
        (
            "(let ((x (copy-seq #*0101))) (nsubstitute 1 0 x :count -1) x)",
            "#*0101",
        ),
    ];
    run_expression_cases(&cases);
}

/// :FROM-END changes the ORDER the test is applied in, not just which matches
/// are kept.
///
/// ansi checks this with a STATEFUL :test — one that decrements a counter on
/// every call — so which element matches depends on the order it was called in.
/// Walking forward and then taking the last COUNT gave the right shape and the
/// wrong element (ansi SUBSTITUTE-*.21/.23, and the REMOVE/POSITION families
/// built on the same position picker).
#[test]
fn from_end_reverses_the_order_the_test_is_applied_in() {
    let cases = [
        // The ansi case itself: c starts at 5 and the test decrements it per
        // call, so only a back-to-front walk matches the 8.
        (
            "(let* ((orig '(1 2 3 4 5 6 7 8 9)) (x (copy-seq orig)) (c 5) \
                 (result (substitute 'x 9 x :test (lambda (a b) (incf c -2) (= (+ b c) a)) \
                                     :from-end t))) \
            (list (equal orig x) result))",
            "(T (1 2 3 4 5 6 7 X 9))",
        ),
        // Directly observable: pushing each element the test sees reverses it,
        // so a forward walk leaves (3 2 1) and :from-end leaves (1 2 3).
        (
            "(let ((acc nil)) \
            (substitute 0 9 '(1 2 3) :test (lambda (a b) (declare (ignore a)) (push b acc) nil)) acc)",
            "(3 2 1)",
        ),
        (
            "(let ((acc nil)) \
            (substitute 0 9 '(1 2 3) :test (lambda (a b) (declare (ignore a)) (push b acc) nil) \
                        :from-end t) acc)",
            "(1 2 3)",
        ),
        // Which matches are kept is still right, in both directions.
        ("(substitute 'x 'a '(a b a) :count 1)", "(X B A)"),
        (
            "(substitute 'x 'a '(a b a) :count 1 :from-end t)",
            "(A B X)",
        ),
        ("(substitute 'x 'a '(a b a))", "(X B X)"),
        // The families that share the position picker are unmoved.
        ("(remove 'a '(a b a))", "(B)"),
        ("(remove 'a '(a b a) :count 1)", "(B A)"),
        ("(remove 'a '(a b a) :count 1 :from-end t)", "(A B)"),
        ("(position 'a '(a b a) :from-end t)", "2"),
        ("(position 'a '(a b a))", "0"),
        ("(substitute 1 0 #*0101)", "#*1111"),
    ];
    run_expression_cases(&cases);
}

/// A string result type can be named through its ELEMENT TYPE as well as its
/// head: `(vector character)`, and `(array nil (*))` — an element type of NIL
/// holds no elements and is a STRING subtype (CLHS 15.1.2.2), which MAKE-ARRAY
/// already treats that way. CONCATENATE and MAP looked only at the head, so
/// `(concatenate '(array nil (*)))` answered NIL rather than "" (ansi
/// CONCATENATE.32).
#[test]
fn a_string_result_type_can_be_named_by_its_element_type() {
    let cases = [
        ("(concatenate '(array nil (*)))", "\"\""),
        ("(concatenate '(vector nil))", "\"\""),
        (
            "(concatenate '(vector character) \"ab\" \"cd\")",
            "\"abcd\"",
        ),
        ("(concatenate '(vector base-char) \"ab\")", "\"ab\""),
        ("(map '(vector character) #'identity \"ab\")", "\"ab\""),
        // A non-character element type is still what it was: BIT stays a bit
        // vector and T stays a general vector, so the new clause cannot
        // over-reach.
        ("(concatenate '(vector bit) '(0 1))", "#*01"),
        ("(concatenate '(vector t) '(1 2))", "#(1 2)"),
        ("(map '(vector bit) #'identity '(1 0))", "#*10"),
        // The head-named forms are unchanged.
        ("(concatenate 'string \"ab\" \"cd\")", "\"abcd\""),
        ("(concatenate 'vector '(1 2))", "#(1 2)"),
        ("(concatenate 'list '(1 2))", "(1 2)"),
    ];
    run_expression_cases(&cases);
}

/// REMOVE-IF and REMOVE-IF-NOT honour their full keyword set. They took
/// `&rest keys` with `(declare (ignore keys))`, so :KEY, :COUNT, :START, :END
/// and :FROM-END were silently dropped and every match was removed — the old
/// comment in boot.lisp admitted it as "a separate gap" (ansi REMOVE-IF.ORDER.1,
/// and the DELETE-IF pair that delegates here).
#[test]
fn remove_if_honours_its_keywords() {
    let cases = [
        // :FROM-END with :COUNT takes the TRAILING match, not the leading one.
        (
            "(remove-if (lambda (x) (eq x 'a)) '(a b c d a f) :from-end t :count 1)",
            "(A B C D F)",
        ),
        (
            "(remove-if (lambda (x) (eq x 'a)) '(a b a) :count 1)",
            "(B A)",
        ),
        (
            "(remove-if (lambda (x) (eq x 'a)) '(a b a) :count 1 :from-end t)",
            "(A B)",
        ),
        // Bounding indices.
        ("(remove-if #'evenp '(1 2 3 4) :start 2)", "(1 2 3)"),
        ("(remove-if #'evenp '(1 2 3 4) :end 2)", "(1 3 4)"),
        // :KEY is applied before the predicate.
        (
            "(remove-if #'zerop '(1 2) :key (lambda (x) (- x 1)))",
            "(2)",
        ),
        ("(remove-if-not #'evenp '(1 2 3 4) :count 1)", "(2 3 4)"),
        (
            "(remove-if-not #'zerop '(1 2) :key (lambda (x) (- x 1)))",
            "(1)",
        ),
        // The keywordless behaviour, on every sequence kind, is unchanged.
        ("(remove-if #'evenp '(1 2 3 4))", "(1 3)"),
        ("(remove-if #'evenp #(1 2 3 4))", "#(1 3)"),
        ("(remove-if (lambda (c) (eql c #\\a)) \"aba\")", "\"b\""),
        ("(remove-if-not #'evenp '(1 2 3 4))", "(2 4)"),
        // DELETE-IF / DELETE-IF-NOT delegate here and inherit the fix.
        ("(delete-if #'evenp (list 1 2 3 4))", "(1 3)"),
        ("(delete-if-not #'evenp (list 1 2 3 4))", "(2 4)"),
        (
            "(delete-if (lambda (x) (eq x 'a)) (list 'a 'b 'a) :count 1 :from-end t)",
            "(A B)",
        ),
    ];
    run_expression_cases(&cases);
}

/// MERGE must reject a result type that cannot hold the merged elements. Most
/// visibly `(merge 'null (list 1 2 3) (list 4 5 6) #'<)`, which quietly answered
/// the six-element list instead of signalling (ansi MERGE.ERROR.1/6).
///
/// It is the same obligation MAKE-SEQUENCE has, so both now call one checker —
/// including the part that makes the TYPE-ERROR truthful, reporting the SIZE
/// against the length the specifier demands rather than the specifier against
/// SEQUENCE (a compound specifier is a list, hence itself a sequence).
#[test]
fn merge_validates_its_result_type() {
    let cases = [
        (
            "(handler-case (merge 'null (list 1 2 3) (list 4 5 6) #'<) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (merge 'symbol (list 1) (list 2) #'<) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (merge '(vector * 3) (list 1 2) (list 3 4) #'<) (type-error () :te))",
            ":TE",
        ),
        // An agreeing declared length is fine, and so is NULL for nothing at all.
        (
            "(merge '(vector * 4) (list 1 3) (list 2 4) #'<)",
            "#(1 2 3 4)",
        ),
        ("(merge 'null nil nil #'<)", "NIL"),
        // The ordinary merges are unchanged.
        ("(merge 'list (list 1 3) (list 2 4) #'<)", "(1 2 3 4)"),
        ("(merge 'vector (list 1 3) (list 2 4) #'<)", "#(1 2 3 4)"),
        ("(merge 'string \"ac\" \"bd\" #'char<)", "\"abcd\""),
        ("(merge 'list (list 1) nil #'<)", "(1)"),
        (
            "(merge 'list (list '(2 b)) (list '(1 a)) #'< :key #'car)",
            "((1 A) (2 B))",
        ),
        // MAKE-SEQUENCE, which now shares the checker, is unmoved.
        (
            "(handler-case (make-sequence '(vector * 4) 3) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-sequence 'symbol 10) (type-error () :te))",
            ":TE",
        ),
        ("(make-sequence 'list 3 :initial-element 7)", "(7 7 7)"),
    ];
    run_expression_cases(&cases);
}

/// REPLACE on OVERLAPPING ranges of the same object must behave as if the source
/// were copied first (CLHS replace). Writing straight through clobbered the
/// source as it went (ansi REPLACE-LIST.20, REPLACE-VECTOR/STRING/BIT-VECTOR.21).
///
/// FILL's :START and :END are bounding index designators, so a negative one, one
/// past the sequence, or a start after the end is a TYPE-ERROR rather than
/// something to silently clamp — `(fill a 'x :end -1)` quietly did nothing
/// (ansi ARRAY-FILL-9 and its FIXNUM / UNSIGNED-BYTE8 variants).
#[test]
fn replace_handles_overlap_and_fill_checks_its_bounds() {
    let cases = [
        // The destination range starts one past the source range, so a forward
        // copy would smear the first element across all of it.
        (
            "(let* ((x (copy-seq (list 'a 'b 'c 'd 'e 'f))) \
                 (r (replace x x :start1 1 :end1 4 :start2 0 :end2 3))) \
            (list (eq x r) r))",
            "(T (A A B C E F))",
        ),
        (
            "(let ((x (copy-seq \"abcdef\"))) (replace x x :start1 1 :end1 4 :start2 0 :end2 3))",
            "\"aabcef\"",
        ),
        (
            "(let ((x (copy-seq #(a b c d e f)))) (replace x x :start1 1 :end1 4 :start2 0 :end2 3))",
            "#(A A B C E F)",
        ),
        // Overlap the other way round, where a forward copy happens to be safe.
        (
            "(let ((x (copy-seq (list 'a 'b 'c 'd)))) (replace x x :start1 0 :end1 2 :start2 2))",
            "(C D C D)",
        ),
        // FILL's bounds.
        (
            "(handler-case (fill (make-array 5) 'x :end -1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (fill (make-array 5) 'x :start -1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (fill (make-array 5) 'x :end 99) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (fill (make-array 5) 'x :start 3 :end 1) (type-error () :te))",
            ":TE",
        ),
        // Ordinary uses of both are unchanged.
        ("(replace (list 1 2 3) (list 9 9))", "(9 9 3)"),
        ("(replace (copy-seq \"abc\") \"xy\")", "\"xyc\""),
        ("(replace (list 1 2 3) (list 9) :start1 2)", "(1 2 9)"),
        ("(fill (list 1 2 3) 0)", "(0 0 0)"),
        ("(fill (copy-seq \"abc\") #\\z :start 1)", "\"azz\""),
        (
            "(fill (make-array 3 :initial-element 1) 7 :end 2)",
            "#(7 7 1)",
        ),
        (
            "(fill (make-array 3 :initial-element 1) 7 :start 3)",
            "#(1 1 1)",
        ),
    ];
    run_expression_cases(&cases);
}

/// A sequence must be a PROPER list, and a FUNCTION is never a sequence however
/// cons-shaped the interpreter's closure representation is.
///
/// Both guards live in the ONE traversal every sequence function shares, plus
/// the two that walk for themselves (LENGTH and SORT). Putting them in LENGTH
/// alone was net zero on the chapter — it fixed the functions written over
/// LENGTH and broke REVERSE, SORT, REDUCE and the duplicate-removers, which
/// carry their own walks (reverted in b1f8073, redone here).
///
/// The datum differs by case ON PURPOSE, because ansi checks both: for a
/// non-sequence it must BE the argument passed (CHECK-TYPE-ERROR), and for a
/// dotted list it must be the improper TAIL, since a dotted list is itself
/// `typep` LIST and blaming it would be self-contradicting (SIGNALS-ERROR).
#[test]
fn a_function_is_not_a_sequence_and_a_dotted_list_is_not_either() {
    let cases = [
        // Dotted lists: the datum is the tail.
        (
            "(handler-case (length '(a b c . d)) \
            (type-error (c) (list (type-error-datum c) \
                                  (typep (type-error-datum c) (type-error-expected-type c)))))",
            "(D NIL)",
        ),
        (
            "(handler-case (find 'e '(a b c . d)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (position 'e '(a b c . d)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (find-if #'null '(a b c . d)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (reverse '(a b . c)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (sort '(3 1 . d) #'<) (type-error () :te))",
            ":TE",
        ),
        // Closures: the datum is the function itself, across every walk.
        (
            "(handler-case (length (lambda (x) x)) (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (reverse (lambda (x) x)) (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (sort (lambda (x) x) #'<) (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (stable-sort (lambda (x) x) #'<) \
            (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (reduce #'cons (lambda (x) x)) \
            (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (remove-duplicates (lambda (x) x)) \
            (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        (
            "(handler-case (find 1 (lambda (x) x)) (type-error (c) (functionp (type-error-datum c))))",
            "T",
        ),
        // Proper sequences of every kind are untouched.
        (
            "(list (length nil) (length '(1 2 3)) (length \"abc\") (length #(1 2)) (length #*101))",
            "(0 3 3 2 3)",
        ),
        (
            "(list (reverse '(1 2)) (sort (list 2 1) #'<) (reduce #'+ '(1 2 3)))",
            "((2 1) (1 2) 6)",
        ),
        ("(length (make-array 4 :fill-pointer 2))", "2"),
    ];
    run_expression_cases(&cases);
}

/// `(setf (elt list n))` past the end must signal, not silently do nothing.
///
/// The list branch had NO else at all: the store was skipped and the value
/// returned as if it had worked, which is the worst shape a bug can take —
/// the call looks successful and the data is unchanged (ansi ELT.10).
///
/// The datum is the INDEX against the valid range rather than the list, so the
/// error is checkable: ansi asserts a type error's datum does not satisfy its
/// own expected-type, and the list would satisfy LIST.
#[test]
fn setf_elt_past_the_end_signals() {
    let cases = [
        (
            "(handler-case (let ((x (list 'a 'b 'c))) (setf (elt x 4) 'd)) \
            (type-error (c) (list (type-error-datum c) \
                                  (typep (type-error-datum c) (type-error-expected-type c)))))",
            "(4 NIL)",
        ),
        (
            "(handler-case (let ((x (list 1))) (setf (elt x 1) 9)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (let ((x (vector 1))) (setf (elt x 4) 9)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (let ((x (copy-seq \"ab\"))) (setf (elt x 5) #\\z)) (type-error () :te))",
            ":TE",
        ),
        // Stores that are in range are untouched, at both ends and in the middle.
        ("(let ((x (list 1 2 3))) (setf (elt x 0) 9) x)", "(9 2 3)"),
        ("(let ((x (list 1 2 3))) (setf (elt x 1) 9) x)", "(1 9 3)"),
        ("(let ((x (list 1 2 3))) (setf (elt x 2) 9) x)", "(1 2 9)"),
        ("(let ((x (vector 1 2))) (setf (elt x 1) 9) x)", "#(1 9)"),
        (
            "(let ((x (copy-seq \"ab\"))) (setf (elt x 1) #\\z) x)",
            "\"az\"",
        ),
    ];
    run_expression_cases(&cases);
}

/// NOT and NULL take exactly one argument. The operator-position handlers check
/// that themselves, but nothing did on the funcall/apply path, so
/// `(funcall #'not 1 2)` quietly answered NIL.
///
/// That is not academic: a :TEST-NOT of #'NOT is called with TWO arguments, so
/// it has to be a PROGRAM-ERROR — which is exactly what ansi checks
/// (POSITION.ERROR.13 and its FIND / COUNT siblings).
#[test]
fn not_and_null_take_exactly_one_argument_on_every_path() {
    let cases = [
        (
            "(handler-case (funcall #'not 1 2) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (funcall #'null 1 2) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (apply #'not '(1 2)) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (not 1 2) (program-error () :pe))", ":PE"),
        (
            "(handler-case (funcall #'not) (program-error () :pe))",
            ":PE",
        ),
        // The case that motivated it: a two-argument call through :TEST-NOT.
        (
            "(handler-case (position 'b '(a b c d) :test-not #'not) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (find 'b '(a b c d) :test-not #'not) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (count 'b '(a b c d) :test-not #'not) (program-error () :pe))",
            ":PE",
        ),
        // One-argument uses are untouched, on every path.
        (
            "(list (not nil) (not 1) (null nil) (null 1))",
            "(T NIL T NIL)",
        ),
        ("(list (funcall #'not nil) (funcall #'null 1))", "(T NIL)"),
        ("(mapcar #'not '(nil 1))", "(T NIL)"),
        ("(remove-if #'null '(1 nil 2))", "(1 2)"),
    ];
    run_expression_cases(&cases);
}

/// FLOOR/CEILING/TRUNCATE/ROUND/MOD/REM on two fixnums take a native-integer
/// fast path (bliss-mwpb) instead of building reduced `BigRat`s. It must agree
/// with the general rational path exactly, so pin the cases that distinguish
/// the four rounding modes — sign of the quotient, MOD taking the sign of the
/// divisor while REM takes the dividend's, and ROUND breaking a tie toward the
/// even neighbour — plus bignum/ratio/float operands, which still go the
/// general way.
#[test]
fn integer_division_rounding_modes_are_exact() {
    let cases = [
        // FLOOR rounds toward -inf, CEILING toward +inf, TRUNCATE toward zero.
        ("(floor -7 2)", "-4"),
        ("(ceiling -7 2)", "-3"),
        ("(truncate -7 2)", "-3"),
        ("(floor 7 2)", "3"),
        ("(ceiling 7 2)", "4"),
        ("(truncate 7 2)", "3"),
        // The second value is the remainder a - q*b in every mode.
        ("(multiple-value-list (floor -7 2))", "(-4 1)"),
        ("(multiple-value-list (ceiling -7 2))", "(-3 -1)"),
        ("(multiple-value-list (truncate -7 2))", "(-3 -1)"),
        // ROUND is round-half-to-EVEN, not half-away-from-zero.
        ("(round 5 2)", "2"),
        ("(round 7 2)", "4"),
        ("(round -5 2)", "-2"),
        ("(round -7 2)", "-4"),
        ("(round 3 2)", "2"),
        // MOD takes the sign of the divisor, REM the sign of the dividend.
        ("(mod -7 3)", "2"),
        ("(rem -7 3)", "-1"),
        ("(mod 7 -3)", "-2"),
        ("(rem 7 -3)", "1"),
        ("(mod 6 3)", "0"),
        ("(rem 6 3)", "0"),
        // An exact division must not round away from the exact quotient.
        ("(multiple-value-list (floor -6 3))", "(-2 0)"),
        ("(multiple-value-list (ceiling -6 3))", "(-2 0)"),
        // Bignum, ratio and float operands keep the general path.
        ("(floor (expt 10 30) 7)", "142857142857142857142857142857"),
        ("(mod (expt 10 30) 7)", "1"),
        ("(floor 7/2)", "3"),
        ("(mod 7.5 2)", "1.5"),
        // Division by zero still signals rather than dividing natively.
        ("(handler-case (mod 1 0) (arithmetic-error () :ae))", ":AE"),
        (
            "(handler-case (floor 1 0) (arithmetic-error () :ae))",
            ":AE",
        ),
    ];
    run_expression_cases(&cases);
}

/// MOD, REM and the four rounding operators reached through FUNCALL/APPLY (and
/// so through the c2i fallback from compiled code) take apply_builtin_fast's
/// direct arms rather than the synthesize-`(MOD 'a 'b)`-and-re-evaluate detour
/// (bliss-mwpb). They must stay bit-identical to operator position, including
/// the second value, so pin both spellings against each other.
#[test]
fn integer_division_through_funcall_matches_operator_position() {
    let cases = [
        (
            "(list (mod -7 3) (funcall #'mod -7 3) (apply #'mod '(-7 3)))",
            "(2 2 2)",
        ),
        (
            "(list (rem -7 3) (funcall #'rem -7 3) (apply #'rem '(-7 3)))",
            "(-1 -1 -1)",
        ),
        ("(list (mod 7 -3) (funcall #'mod 7 -3))", "(-2 -2)"),
        // The rounding operators must carry their SECOND value through funcall.
        (
            "(list (multiple-value-list (floor -7 2)) (multiple-value-list (funcall #'floor -7 2)))",
            "((-4 1) (-4 1))",
        ),
        (
            "(list (multiple-value-list (round 7 2)) (multiple-value-list (funcall #'round 7 2)))",
            "((4 -1) (4 -1))",
        ),
        (
            "(list (multiple-value-list (ceiling -7 2)) (multiple-value-list (funcall #'ceiling -7 2)))",
            "((-3 -1) (-3 -1))",
        ),
        // One-argument FLOOR family through funcall divides by 1.
        ("(multiple-value-list (funcall #'truncate 7/2))", "(3 1/2)"),
        // MAPCAR routes through the same dispatch.
        ("(mapcar #'mod '(-7 7 6) '(3 -3 3))", "(2 -2 0)"),
        // Bignum and ratio operands still reach the general rational path.
        ("(funcall #'mod (expt 10 30) 7)", "1"),
        ("(multiple-value-list (funcall #'floor 7/2))", "(3 1/2)"),
        // CL MOD takes the sign of the DIVISOR for floats too (CLHS 12.1.4.1);
        // this answered 2.0 and 1.0 before bliss-mwpb.
        ("(mod -7.0 -3.0)", "-1.0"),
        ("(mod 7.0 -3.0)", "-2.0"),
        ("(mod -7.0 3.0)", "2.0"),
        ("(funcall #'mod -7.0 -3.0)", "-1.0"),
        // REM keeps the sign of the dividend.
        ("(rem -7.0 3.0)", "-1.0"),
        ("(funcall #'rem -7.0 3.0)", "-1.0"),
        // Division by zero still signals on the fast path.
        (
            "(handler-case (funcall #'mod 1 0) (arithmetic-error () :ae))",
            ":AE",
        ),
        (
            "(handler-case (funcall #'floor 1 0) (arithmetic-error () :ae))",
            ":AE",
        ),
    ];
    run_expression_cases(&cases);
}

/// CLHS 5.1.1.1: a place's subforms are evaluated left-to-right and the new
/// value LAST. `(setf (subseq ...) ...)` evaluated the value form first, so
/// ansi SUBSEQ.ORDER.3/4 saw the value's side effect happen before the
/// sequence, start and end subforms.
#[test]
fn setf_subseq_evaluates_value_form_last() {
    let cases = [
        // SUBSEQ.ORDER.3: with an explicit end, the order is seq, start, end,
        // then value -- so a=1 b=2 c=3 d=4, not d=1.
        (
            "(let ((i 0) a b c d (s (copy-seq \"abcdefgh\")))
               (list (setf (subseq (progn (setf a (incf i)) s)
                                   (progn (setf b (incf i)) 1)
                                   (progn (setf c (incf i)) 4))
                           (progn (setf d (incf i)) \"xyz\"))
                     s i a b c d))",
            "(\"xyz\" \"axyzefgh\" 4 1 2 3 4)",
        ),
        // SUBSEQ.ORDER.4: without an end, the order is seq, start, then value.
        (
            "(let ((i 0) a b c (s (copy-seq \"abcd\")))
               (list (setf (subseq (progn (setf a (incf i)) s)
                                   (progn (setf b (incf i)) 1))
                           (progn (setf c (incf i)) \"xyz\"))
                     s i a b c))",
            "(\"xyz\" \"axyz\" 3 1 2 3)",
        ),
        // The store itself must be unchanged: strings with and without an end,
        // lists, vectors, and a package-qualified spelling of the accessor
        // (deferring the value relies on the bare name, while the store path
        // matches the symbol's full name -- if those disagreed the value would
        // silently store as NIL).
        (
            "(let ((s (copy-seq \"abcdef\"))) (setf (cl:subseq s 1 4) \"XYZ\") s)",
            "\"aXYZef\"",
        ),
        (
            "(let ((s (copy-seq \"abcdef\"))) (setf (subseq s 2) \"PQ\") s)",
            "\"abPQef\"",
        ),
        (
            "(let ((l (list 1 2 3 4 5))) (setf (subseq l 1 3) (list 9 9)) l)",
            "(1 9 9 4 5)",
        ),
        (
            "(let ((v (vector 1 2 3 4 5))) (setf (subseq v 0 2) (vector 7 7)) v)",
            "#(7 7 3 4 5)",
        ),
    ];
    run_expression_cases(&cases);
}

/// The fixnum division fast path (bliss-mwpb) computes in i128 and falls back
/// to the general BigRat path when a result leaves the 61-bit fixnum range.
/// Exercise the extremes, where that fallback is what keeps the answer right:
/// check the defining identity a = q*b + r in exact arithmetic, and cross-check
/// against the same division scaled by 2^70, which is quotient-preserving and
/// forces the general path.
#[test]
fn integer_division_is_exact_at_the_fixnum_boundary() {
    let cases = [(
        "(let* ((mx (1- (expt 2 60))) (mn (- (expt 2 60))) (k (expt 2 70))
                (bad 0) (n 0))
           (dolist (a (list mx mn (1- mx) (1+ mn) (expt 2 59) (- (expt 2 59)) 0 1 -1))
             (dolist (b (list 1 -1 2 -2 3 -3 mx mn))
               (dolist (op (list #'floor #'ceiling #'truncate #'round))
                 (incf n)
                 (multiple-value-bind (q r) (funcall op a b)
                   (unless (= a (+ (* q b) r)) (incf bad))
                   (multiple-value-bind (bq br) (funcall op (* a k) (* b k))
                     (unless (and (= q bq) (= (* r k) br)) (incf bad)))))))
           (list n bad))",
        "(288 0)",
    )];
    run_expression_cases(&cases);
}

/// A ratio literal's components must become BIGNUMs when they leave the 61-bit
/// fixnum range. `EgclVal::from_fixnum` shifts left by 3 without checking, so
/// the reader used to wrap them into the sign bit -- silently, with no error:
/// `1152921504606846976/7` (numerator 2^60) READ AS `-1152921504606846976/7`,
/// and `7/1152921504606846976` read with a negative, unnormalized denominator
/// (bliss-mwpb). CLHS 2.3.2.3 also requires the literal to be reduced with a
/// positive denominator.
#[test]
fn big_ratio_literals_read_without_wrapping() {
    let cases = [
        // 2^60 and above in either component: previously sign-flipped.
        ("1152921504606846976/7", "1152921504606846976/7"),
        ("7/1152921504606846976", "7/1152921504606846976"),
        ("-1152921504606846976/7", "-1152921504606846976/7"),
        // The denominator must be positive and the value reduced.
        ("(denominator 7/1152921504606846976)", "1152921504606846976"),
        ("(numerator -1152921504606846976/7)", "-1152921504606846976"),
        // Just inside the fixnum range still reduces to a fixnum integer.
        ("1152921504606846975/7", "164703072086692425"),
        ("7/1152921504606846975", "1/164703072086692425"),
        // A big numerator that reduces to an integer must still collapse.
        ("2305843009213693952/4", "576460752303423488"),
        // Ordinary reduction and sign normalization are unchanged.
        ("6/4", "3/2"),
        ("4/2", "2"),
        ("-6/4", "-3/2"),
        ("6/-4", "-3/2"),
        // The value must equal numerator/denominator, computed.
        ("(= 1152921504606846976/7 (/ 1152921504606846976 7))", "T"),
        ("(= 7/1152921504606846976 (/ 7 1152921504606846976))", "T"),
        // And arithmetic on it must work.
        (
            "(floor (- 1152921504606846976/7) 1152921504606846976/7)",
            "-1",
        ),
    ];
    run_expression_cases(&cases);
}

/// CLHS 2.3.2.3: a radix prefix applies to a RATIO as well as an integer, so
/// `#x1F/2` is the rational 31/2. read_radix_integer scanned only alphanumeric
/// characters, so it stopped at the `/`, read the numerator alone and left
/// `/2` in the stream -- `#x1F/2` silently read as 31 (bliss-mwpb).
#[test]
fn radix_prefixed_ratio_literals_read_as_ratios() {
    let cases = [
        ("#x1F/2", "31/2"),
        ("#b101/11", "5/3"),
        ("#o17/4", "15/4"),
        ("#16rFF/3", "85"),
        ("#x-1F/2", "-31/2"),
        // Reduction matches the unprefixed path. (A sign on the DENOMINATOR is
        // not valid ratio syntax per CLHS 2.3.2.3 -- `sign` appears only at the
        // front -- and `#x1F/-2` is a reader error here. The base-10 path is
        // more lenient and accepts `6/-4`; that inconsistency is pre-existing
        // and deliberately not changed by this test.)
        ("#xFF/3", "85"),
        // A 2^60 numerator must become a bignum here too, not wrap negative.
        ("#x1000000000000000/7", "1152921504606846976/7"),
        // Plain radix integers, including bignums, are unaffected.
        ("#x1F", "31"),
        ("#b1011", "11"),
        ("#xFFFFFFFFFFFFFFFF", "18446744073709551615"),
        ("#36rZZ", "1295"),
        // An out-of-range radix is still a reader error.
        (
            "(handler-case (read-from-string \"#37rZ\") (error () :err))",
            ":ERR",
        ),
    ];
    run_expression_cases(&cases);
}

/// CONCATENATE and MAP must return a FRESH sequence.
///
/// The string result path used the interning constructor, so two calls producing
/// the same characters answered the SAME object — `(eq (concatenate 'string "ab"
/// "cd") (concatenate 'string "ab" "cd"))` was T. Sharing is worse than a wrong
/// answer here: mutating either result corrupts the other, so code that builds a
/// string this way can break something unrelated much later. That is how it
/// surfaced — running the ansi search-string tests made a later
/// SUBSTITUTE-STRING test fail, and the two have nothing to do with each other.
///
/// SUBSEQ, COPY-SEQ and REVERSE already used the fresh constructor for exactly
/// this reason; this path did not.
#[test]
fn concatenate_and_map_return_fresh_sequences() {
    let cases = [
        (
            "(eq (concatenate 'string \"ab\" \"cd\") (concatenate 'string \"ab\" \"cd\"))",
            "NIL",
        ),
        (
            "(eq (map 'string #'identity \"ab\") (map 'string #'identity \"ab\"))",
            "NIL",
        ),
        // Mutating one result must not touch another with the same contents.
        (
            "(let ((a (concatenate 'string \"ab\" \"cd\")) (b (concatenate 'string \"ab\" \"cd\"))) \
            (setf (char a 0) #\\Q) (list a b))",
            "(\"Qbcd\" \"abcd\")",
        ),
        (
            "(let ((a (map 'string #'identity \"ab\")) (b (map 'string #'identity \"ab\"))) \
            (setf (char a 0) #\\Q) (list a b))",
            "(\"Qb\" \"ab\")",
        ),
        // ... nor the literal it was built from.
        (
            "(let ((a (concatenate 'string \"ab\"))) (setf (char a 0) #\\Q) (list a (concatenate 'string \"ab\")))",
            "(\"Qb\" \"ab\")",
        ),
        // The values themselves are unchanged.
        ("(concatenate 'string \"ab\" \"cd\")", "\"abcd\""),
        ("(concatenate 'list '(1) '(2))", "(1 2)"),
        ("(concatenate 'vector '(1 2))", "#(1 2)"),
        ("(map 'string #'char-upcase \"ab\")", "\"AB\""),
        ("(equal (concatenate 'string \"ab\" \"cd\") \"abcd\")", "T"),
    ];
    run_expression_cases(&cases);
}

/// Compiled and interpreted call sites dispatch known builtins DIRECTLY,
/// skipping apply_function's resolve-the-callee-by-name preamble
/// (bliss-x5y.27). The shortcut is only sound while the name still means the
/// builtin, so pin the invalidation paths and the results themselves.
///
/// Lexical FLET/LABELS shadowing of a builtin is NOT asserted here: it is
/// broken independently of this work (bliss-ccgu), and behaves identically on
/// binaries built before and after it. Asserting it either way would bake a
/// pre-existing bug into the suite.
#[test]
fn direct_builtin_dispatch_respects_shadowing() {
    let cases = [
        // The unshadowed results, including VARIADIC arities. These have no
        // declared arity range, so an arity gate would silently exclude exactly
        // the builtins this table exists to speed up.
        (
            "(list (car (list 1 2)) (cdr (list 1 2)) (cons 1 2))",
            "(1 (2) (1 . 2))",
        ),
        (
            "(list (+ 1 2 3) (- 10 1) (* 2 3) (1+ 5) (1- 5))",
            "(6 9 6 6 4)",
        ),
        ("(list (< 1 2 3) (= 2 2) (/= 1 2) (eq 'a 'a))", "(T T T T)"),
        (
            "(list (aref (vector 9 8 7) 1) (elt (list 1 2 3) 1) (mod -7 3) (rem -7 3))",
            "(8 2 2 -1)",
        ),
        ("(multiple-value-list (floor -7 2))", "(-4 1)"),
        (
            "(list (logand 12 10) (logior 12 10) (lognot 0) (null nil) (not 1))",
            "(8 14 -1 T NIL)",
        ),
        // A hot loop must compute the same answer as a cold one.
        (
            "(let ((v (make-array 16 :initial-element 3)))
               (list (let ((s 0)) (dotimes (i 10 s) (setq s (+ s (aref v (mod i 16))))))
                     (let ((s 0)) (dotimes (i 200000 s) (setq s (+ s (aref v (mod i 16))))))))",
            "(30 600000)",
        ),
        // Wrong argument counts must still signal rather than reaching a kernel
        // that ignores the extras.
        ("(handler-case (car 1 2) (error () :err))", ":ERR"),
        ("(handler-case (cons 1) (error () :err))", ":ERR"),
    ];
    run_expression_cases(&cases);
}

/// Redefining a builtin must take effect even after it has been dispatched
/// directly tens of thousands of times (bliss-x5y.27). Separate top-level
/// forms, because a redefinition nested inside ONE top-level form is not
/// picked up by that same form — pre-existing behaviour, identical on binaries
/// built before and after this work, and not what this test is about.
#[test]
fn redefining_a_directly_dispatched_builtin_takes_effect() {
    let out = egcl_bin()
        .args([
            "--eval",
            "(defun hot3 (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (car (list 1 2)))))))",
            // Run it hot enough to memoize CAR as a direct builtin many times.
            "--eval",
            "(cl:format t \"before=~a \" (hot3 50000))",
            "--eval",
            "(defun car (x) (declare (ignore x)) 99)",
            // Both the hot function and a fresh call site must see the new one.
            "--eval",
            "(cl:format t \"after=~a direct=~a~%\" (hot3 10) (car (list 1 2)))",
        ])
        .output()
        .expect("failed to run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("before=50000")
            && stdout.contains("after=990")
            && stdout.contains("direct=99"),
        "redefinition did not take effect; got: {stdout}"
    );
}

/// CLHS lets any type specifier denoting a sequence subtype be a result type,
/// DEFTYPE aliases included. The sequence-result classifier only knew the
/// BUILT-IN names, so a user type was rejected outright as "not a sequence
/// type specifier" — which broke (asdf:load-system :babel), since babel defines
/// (deftype unicode-string () '(simple-array character (*))) and uses it.
/// Expand the alias first, in every function that takes a result type.
#[test]
fn sequence_result_types_accept_deftype_aliases() {
    let prelude = "(progn (deftype ustr () '(simple-array character (*)))
                          (deftype ubits () '(simple-array bit (*)))
                          (deftype ulist () 'list)";
    let cases = [
        // The alias must build what it EXPANDS to, not be rejected.
        (
            format!("{prelude} (map 'ustr #'identity \"ab\"))"),
            "\"ab\"".to_string(),
        ),
        (
            format!("{prelude} (concatenate 'ustr \"ab\" \"cd\"))"),
            "\"abcd\"".to_string(),
        ),
        (
            format!("{prelude} (make-sequence 'ustr 2 :initial-element #\\a))"),
            "\"aa\"".to_string(),
        ),
        (
            format!("{prelude} (make-sequence 'ubits 3))"),
            "#*000".to_string(),
        ),
        (
            format!("{prelude} (map 'ulist #'identity \"ab\"))"),
            "(#\\a #\\b)".to_string(),
        ),
        // MERGE through an alias must agree with MERGE through the expansion
        // itself. (Neither yields a STRING here: COERCE does not recognize
        // (simple-array character (*)) as a string type — a pre-existing gap,
        // tracked separately. What this fix guarantees is that the alias and
        // its expansion behave the SAME.)
        (
            format!(
                "{prelude} (equalp (merge 'ustr (list #\\a #\\c) (list #\\b) #'char<)
                                   (merge '(simple-array character (*))
                                          (list #\\a #\\c) (list #\\b) #'char<)))"
            ),
            "T".to_string(),
        ),
        (
            format!("{prelude} (coerce \"ab\" 'ulist))"),
            "(#\\a #\\b)".to_string(),
        ),
        // An alias for a NON-sequence type must still be rejected, so the
        // expansion did not simply turn the check off.
        (
            format!(
                "{prelude} (deftype unum () 'integer)
                       (handler-case (concatenate 'unum \"ab\") (error () :type-error)))"
            ),
            ":TYPE-ERROR".to_string(),
        ),
        (
            format!("{prelude} (handler-case (make-sequence 'symbol 2) (error () :type-error)))"),
            ":TYPE-ERROR".to_string(),
        ),
        // Built-in spellings are unchanged.
        (
            format!("{prelude} (concatenate 'string \"ab\" \"cd\"))"),
            "\"abcd\"".to_string(),
        ),
        (
            format!("{prelude} (make-sequence 'list 2))"),
            "(NIL NIL)".to_string(),
        ),
        (
            format!("{prelude} (merge 'string (list #\\a) (list #\\b) #'char<))"),
            "\"ab\"".to_string(),
        ),
    ];
    let refs: Vec<(&str, &str)> = cases
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    run_expression_cases(&refs);
}

/// A rejected type specifier must be named in the error. The Debug form
/// "Symbol(4941)" is what a user saw when (asdf:load-system :babel) failed, and
/// it identified nothing; the answer was BABEL::UNICODE-STRING.
#[test]
fn type_error_names_a_symbol_datum() {
    let out = egcl_bin()
        .args(["--eval", "(concatenate 'some-undefined-type \"ab\")"])
        .output()
        .expect("failed to run egcl");
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("SOME-UNDEFINED-TYPE"),
        "the rejected symbol must be named; got: {text}"
    );
    assert!(
        !text.contains("Symbol("),
        "a symbol datum must not print as its Debug form; got: {text}"
    );
}

/// `is_vector` / `is_complex_vector` no longer consult the pathname string
/// registry to decide whether a heap value is safe to dereference (bliss-edzd).
/// That guard dated from when a registered string was an FNV hash behind a
/// heap-object tag; registered values are real objects now. Pin the behaviour
/// it was protecting, so removing it cannot regress silently: registered
/// strings, pathnames, and every vector shape must still classify correctly.
#[test]
fn vector_predicates_classify_without_the_string_registry() {
    let cases = [
        // Plain vectors and strings.
        ("(aref (vector 1 2 3) 1)", "2"),
        ("(aref \"abc\" 1)", "#\\b"),
        ("(elt \"abc\" 1)", "#\\b"),
        ("(length \"abc\")", "3"),
        ("(vectorp (vector 1 2))", "T"),
        ("(vectorp \"ab\")", "T"),
        ("(vectorp (list 1 2))", "NIL"),
        ("(simple-vector-p (vector 1 2))", "T"),
        // NOTE: (simple-vector-p "ab") wrongly answers T, on binaries built
        // before AND after this change -- a pre-existing conformance bug
        // (bliss-3snc), not something the removed registry lookup was
        // preventing. Deliberately not asserted either way here.
        // Fill-pointer / adjustable vectors take the complex-array path.
        (
            "(let ((v (make-array 3 :fill-pointer 2 :adjustable t :initial-element 7)))
               (list (aref v 2) (length v) (vectorp v)))",
            "(7 2 T)",
        ),
        // Pathnames and pathname-derived strings are registry-backed values and
        // must still behave — a pathname is not a sequence, its name is.
        (
            "(namestring (make-pathname :name \"foo\" :type \"lisp\"))",
            "\"foo.lisp\"",
        ),
        ("(pathname-name (pathname \"/tmp/x.lisp\"))", "\"x\""),
        ("(vectorp (pathname \"/tmp/x.lisp\"))", "NIL"),
        ("(length (pathname-name (pathname \"/tmp/xy.lisp\")))", "2"),
        (
            "(aref (pathname-name (pathname \"/tmp/xy.lisp\")) 0)",
            "#\\x",
        ),
        // Bit vectors keep their own classification.
        (
            "(aref (make-array 3 :element-type 'bit :initial-element 1) 2)",
            "1",
        ),
        ("(subseq (vector 1 2 3) 1)", "#(2 3)"),
    ];
    run_expression_cases(&cases);
}

/// LENGTH dispatches on already-evaluated arguments (bliss-edzd). It used to
/// fall through to the synthesize-`(LENGTH 'seq)`-and-re-evaluate detour, which
/// cost 3.33us per call against 0.35us for AREF and 0.02us for CAR -- 148x CAR
/// for what is a header read on a vector. The fast path must agree with the
/// operator-position handler on every sequence representation, and must keep
/// signalling where a sequence is required.
#[test]
fn length_is_correct_on_every_sequence_representation() {
    let cases = [
        ("(length nil)", "0"),
        ("(length (list 1 2 3))", "3"),
        ("(length (vector 1 2 3))", "3"),
        ("(length \"abcd\")", "4"),
        (
            "(length (make-array 3 :element-type 'bit :initial-element 1))",
            "3",
        ),
        // A fill-pointer vector's LENGTH is its fill pointer, not its capacity.
        (
            "(length (make-array 5 :fill-pointer 2 :initial-element 0))",
            "2",
        ),
        (
            "(length (make-array 3 :adjustable t :initial-element 0))",
            "3",
        ),
        // Reached through FUNCALL/APPLY/MAPCAR, i.e. the same dispatch the
        // direct path serves.
        ("(funcall #'length (list 1 2))", "2"),
        ("(apply #'length (list (list 1 2 3)))", "3"),
        (
            "(mapcar #'length (list (list 1) \"ab\" (vector 1 2 3)))",
            "(1 2 3)",
        ),
        // Still signals for non-sequences, a dotted list, and a closure --
        // the guards the detour used to provide.
        (
            "(handler-case (length (cons 1 2)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (length #'car) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (length 'foo) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (length 5) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // Wrong arity is still a PROGRAM-ERROR.
        ("(handler-case (length) (program-error () :pe))", ":PE"),
        (
            "(handler-case (length nil nil) (program-error () :pe))",
            ":PE",
        ),
    ];
    run_expression_cases(&cases);
}

/// Type predicates dispatch on already-evaluated arguments (bliss-edzd). They
/// used to fall through to the synthesize-and-re-evaluate detour, so a tag test
/// cost 1.4-1.8us against CAR's 0.022us. Each fast arm must use the SAME
/// predicate as its operator-position handler, so assert the two spellings
/// agree across a spread of representations rather than just spot values.
#[test]
fn type_predicates_agree_between_operator_position_and_funcall() {
    let vals = "(list 1 1.5 \"ab\" 'a :kw nil t (list 1) (vector 1) \
                 (make-array '(2 2)) #\\a #'car (make-hash-table))";
    let agree = |op: &str| {
        format!(
            "(let ((vals {vals}))
               (equal (mapcar #'{op} vals) (mapcar (lambda (v) (funcall #'{op} v)) vals)))"
        )
    };
    let cases: Vec<(String, String)> = [
        "symbolp", "numberp", "stringp", "vectorp", "keywordp", "arrayp",
    ]
    .iter()
    .map(|op| (agree(op), "T".to_string()))
    .chain([
        // Spot values, so agreement cannot be vacuous.
        (
            "(list (symbolp 'a) (symbolp 1) (symbolp nil))".to_string(),
            "(T NIL T)".to_string(),
        ),
        (
            "(list (numberp 1) (numberp 1.5) (numberp \"a\"))".to_string(),
            "(T T NIL)".to_string(),
        ),
        (
            "(list (stringp \"a\") (stringp 'a) (stringp 1))".to_string(),
            "(T NIL NIL)".to_string(),
        ),
        (
            "(list (vectorp (vector 1)) (vectorp (list 1)))".to_string(),
            "(T NIL)".to_string(),
        ),
        (
            "(list (keywordp :k) (keywordp 'a))".to_string(),
            "(T NIL)".to_string(),
        ),
        (
            "(list (arrayp (vector 1)) (arrayp (make-array '(2 2))) (arrayp 1))".to_string(),
            "(T T NIL)".to_string(),
        ),
        // Wrong arity still signals, since the fast arms decline it.
        (
            "(handler-case (numberp) (program-error () :pe))".to_string(),
            ":PE".to_string(),
        ),
        (
            "(handler-case (stringp 1 2) (program-error () :pe))".to_string(),
            ":PE".to_string(),
        ),
    ])
    .collect();
    let refs: Vec<(&str, &str)> = cases
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    run_expression_cases(&refs);
}

/// EQL/EQUAL/EQUALP and MIN/MAX dispatch on already-evaluated arguments
/// (bliss-edzd), instead of taking the synthesize-and-re-evaluate detour that
/// cost EQUAL 103x and MIN 142x a CAR call. The fast arms must use the same
/// kernels as operator position, so assert the two spellings agree and pin the
/// behaviours that distinguish the three equalities.
#[test]
fn equality_and_extrema_match_operator_position() {
    let pairs = "(list (list 1 1) (list 1 1.0) (list \"a\" \"a\") (list \"a\" \"A\") \
                  (list (list 1 2) (list 1 2)) (list (vector 1) (vector 1)) \
                  (list #\\a #\\A) (list 'a 'a) (list 1/2 0.5))";
    let agree = |op: &str| {
        format!(
            "(let ((pairs {pairs}))
               (equal (mapcar (lambda (p) (funcall #'{op} (first p) (second p))) pairs)
                      (mapcar (lambda (p) (apply #'{op} p)) pairs)))"
        )
    };
    let cases: Vec<(String, String)> = ["eql", "equal", "equalp"]
        .iter()
        .map(|op| (agree(op), "T".to_string()))
        .chain([
            // EQL is not EQUAL is not EQUALP, and the differences are the point.
            ("(list (eql 1 1) (eql 1 1.0) (eql \"a\" \"a\") (eql 'a 'a))".to_string(),
             "(T NIL NIL T)".to_string()),
            ("(list (equal \"a\" \"a\") (equal \"a\" \"A\") (equal (list 1 2) (list 1 2)) (equal (vector 1) (vector 1)))".to_string(),
             "(T NIL T NIL)".to_string()),
            ("(list (equalp \"a\" \"A\") (equalp 1 1.0) (equalp (vector 1) (vector 1)) (equalp #\\a #\\A))".to_string(),
             "(T T T T)".to_string()),
            // MIN/MAX return the extreme ARGUMENT with its exact type intact.
            ("(list (min 3) (min 3 1 2) (max 3 1 2) (min 1 1.5) (max 1/2 1/3))".to_string(),
             "(3 1 3 1 1/2)".to_string()),
            ("(min (expt 10 30) 5)".to_string(), "5".to_string()),
            ("(max 5 (expt 10 30))".to_string(), "1000000000000000000000000000000".to_string()),
            // A single argument is still type-checked, and no arguments is a
            // PROGRAM-ERROR -- both are operator-position behaviours.
            ("(handler-case (min) (program-error () :pe))".to_string(), ":PE".to_string()),
            ("(handler-case (min \"a\") (error () :type-error))".to_string(), ":TYPE-ERROR".to_string()),
            ("(handler-case (equal 1) (program-error () :pe))".to_string(), ":PE".to_string()),
        ])
        .collect();
    let refs: Vec<(&str, &str)> = cases
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    run_expression_cases(&refs);
}

/// NTH dispatches on already-evaluated arguments (bliss-edzd). It was the worst
/// remaining leaf builtin on the detour at ~140x a CAR call. Its ANSI index
/// validation must be reproduced exactly: a negative or non-integer index is a
/// TYPE-ERROR rather than a saturated 0, and a non-negative BIGNUM index is
/// past the end of any real list and reads as NIL.
#[test]
fn nth_matches_operator_position_including_index_validation() {
    let cases = [
        (
            "(let ((l (list 10 20 30))) (list (nth 0 l) (nth 2 l) (nth 5 l)))",
            "(10 30 NIL)",
        ),
        ("(nth 0 nil)", "NIL"),
        // Operator position and FUNCALL must agree.
        (
            "(let ((l (list 10 20 30)))
               (equal (list (nth 0 l) (nth 1 l) (nth 9 l))
                      (list (funcall #'nth 0 l) (funcall #'nth 1 l) (funcall #'nth 9 l))))",
            "T",
        ),
        // A negative or non-integer index is a TYPE-ERROR, not element 0.
        (
            "(handler-case (nth -1 (list 1 2)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (nth \"a\" (list 1 2)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (nth 1.5 (list 1 2)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // A huge but valid index is simply past the end.
        ("(nth (expt 10 30) (list 1 2))", "NIL"),
        // Wrong arity is a PROGRAM-ERROR.
        ("(handler-case (nth 1) (program-error () :pe))", ":PE"),
        // A dotted tail stops the walk rather than erroring.
        ("(nth 1 (cons 1 (cons 2 3)))", "2"),
    ];
    run_expression_cases(&cases);
}

/// CAR and CDR must reject a closure. The interpreter represents one as the
/// cons `(EGCL::CLOSURE . id)`, and CONSP, LISTP, ATOM and TYPEP all already
/// report it as a FUNCTION rather than a CONS -- but CAR/CDR walked into the
/// representation and answered EGCL::CLOSURE or the raw id instead of
/// signalling (ansi cons CAR.ERROR.1, CDR.ERROR.1, which are two of the four
/// failures on the gate's only enabled chapter, bliss-s0tc).
#[test]
fn car_and_cdr_reject_a_closure() {
    let cases = [
        (
            "(handler-case (car (lambda (x) x)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (cdr (lambda (x) x)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (car #'car) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (cdr #'car) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // FIRST/REST share the arms and must agree.
        (
            "(handler-case (first (lambda (x) x)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (rest (lambda (x) x)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // Reached through FUNCALL, i.e. the evaluated-args path, which needed
        // the same guard as operator position.
        (
            "(handler-case (funcall #'car (lambda (x) x)) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        (
            "(handler-case (mapcar #'cdr (list (lambda (x) x))) (type-error () :type-error))",
            ":TYPE-ERROR",
        ),
        // The predicates were already right and must stay right.
        (
            "(let ((f (lambda (x) x))) (list (consp f) (listp f) (atom f) (functionp f) (typep f 'cons)))",
            "(NIL NIL T T NIL)",
        ),
        // Ordinary CAR/CDR behaviour is untouched, including on NIL.
        (
            "(list (car (list 1 2)) (cdr (list 1 2)) (car nil) (cdr nil))",
            "(1 (2) NIL NIL)",
        ),
        ("(list (first (list 9 8)) (rest (list 9 8)))", "(9 (8))"),
        ("(car (cons 1 2))", "1"),
        ("(cdr (cons 1 2))", "2"),
    ];
    run_expression_cases(&cases);
}

/// `#'<builtin>` is a reified wrapper closure whose body is literally
/// `(apply 'NAME %args)`. Interpreting that trampoline made (funcall #'equalp
/// a b) cost 9.74us against 0.61us for a direct call, a cost paid by every
/// :test, :key and higher-order sequence call. apply_function now dispatches
/// such a wrapper straight to the builtin (bliss-edzd) -- which must not change
/// any observable behaviour, including that a redefinition is still picked up.
#[test]
fn builtin_function_objects_dispatch_correctly() {
    let cases = [
        (
            "(list (funcall #'equalp \"a\" \"A\") (funcall #'car (list 1 2)) (funcall #'length \"abc\"))",
            "(T 1 3)",
        ),
        (
            "(list (apply #'equal (list \"a\" \"a\")) (apply #'min (list 3 1 2)))",
            "(T 1)",
        ),
        (
            "(mapcar #'length (list (list 1) \"ab\" (vector 1 2 3)))",
            "(1 2 3)",
        ),
        ("(sort (list 3 1 2) #'<)", "(1 2 3)"),
        (
            "(remove-duplicates (list \"a\" \"A\" \"b\") :test #'equalp)",
            "(\"A\" \"b\")",
        ),
        ("(member 2 (list 1 2 3) :test #'eql)", "(2 3)"),
        ("(find 2 (list (list 1) (list 2)) :key #'car)", "(2)"),
        // The wrapper is still a FUNCTION and still not a CONS.
        (
            "(list (functionp #'car) (consp #'car) (functionp #'equalp))",
            "(T NIL T)",
        ),
        // The shortcut must decline anything that is not a plain builtin: a
        // real lambda, and a name a user has taken over.
        (
            "(funcall (lambda (x y) (list :mine x y)) 1 2)",
            "(:MINE 1 2)",
        ),
        // A captured function OBJECT keeps its own definition: `#'f` yields the
        // functional value of `f` at that moment, and a later DEFUN installs a
        // different object rather than rewriting this one (CLHS FUNCTION /
        // FDEFINITION; bliss-evpx). This row previously expected 15 — the NEW
        // definition — on the rationale that "the wrapper dispatches at call time
        // rather than capturing code", which was describing EGCL's in-place
        // redefinition rather than the standard, and is what made the classic
        // `(defvar *saved* (fdefinition 'f))` wrapper idiom recurse to a stack
        // overflow.
        //
        // NOT a portability claim: SBCL answers 15 for this exact program and 10
        // for the same shape written with FDEFINITION, because it resolves `#'f`
        // within a SINGLE top-level form as one compilation unit. Split across two
        // top-level forms, SBCL captures too. 10 is the conforming reading of an
        // evaluated PROGN, and it is what EGCL now answers for both spellings.
        (
            "(progn (defun redefme (x) (* x 2))
                    (let ((f #'redefme)) (defun redefme (x) (* x 3)) (funcall f 5)))",
            "10",
        ),
        (
            "(progn (defun redefme2 (x) (* x 2))
                    (let ((f (fdefinition 'redefme2)))
                      (defun redefme2 (x) (* x 3))
                      (funcall f 5)))",
            "10",
        ),
        // Arities the kernel declines still reach the general path and signal.
        ("(handler-case (funcall #'car 1 2) (error () :err))", ":ERR"),
        ("(handler-case (funcall #'length) (error () :err))", ":ERR"),
    ];
    run_expression_cases(&cases);
}

/// FUNCALL and APPLY dispatch on already-evaluated arguments (bliss-edzd).
/// Without that they took the synthesize-and-re-evaluate detour on every call
/// reached through apply_function -- which is every FUNCALL in compiled code,
/// and so every :test, :key and higher-order sequence call. FUNCALL went from
/// 16x a direct call to 1.3x.
///
/// Neither is in the direct-builtin table, deliberately: both re-enter Lisp and
/// so fail the leaf rule. Reached from native code they still go out through
/// c2i, which owns the depth cap and panic barrier.
#[test]
fn funcall_and_apply_dispatch_on_evaluated_arguments() {
    let cases = [
        // FUNCALL over every designator shape.
        ("(funcall #'+ 1 2)", "3"),
        ("(funcall #'car (list 9 8))", "9"),
        ("(funcall 'car (list 7 6))", "7"),
        ("(funcall (lambda (x) (* x 2)) 21)", "42"),
        ("(funcall #'funcall #'car (list 5 4))", "5"),
        (
            "(mapcar (lambda (f) (funcall f (list 1 2))) (list #'car #'cdr #'length))",
            "(1 (2) 2)",
        ),
        // APPLY spreads only its LAST argument.
        ("(apply #'+ (list 1 2 3))", "6"),
        ("(apply #'+ 1 2 (list 3 4))", "10"),
        ("(apply #'+ nil)", "0"),
        ("(apply #'list 1 (list 2 3))", "(1 2 3)"),
        (
            "(apply (lambda (a b) (list :l a b)) 1 (list 2))",
            "(:L 1 2)",
        ),
        ("(apply '+ 1 (list 2))", "3"),
        // Errors still signal.
        ("(handler-case (funcall) (program-error () :pe))", ":PE"),
        ("(handler-case (apply) (program-error () :pe))", ":PE"),
        (
            "(handler-case (funcall 'no-such-fn-xyz 1) (error () :undefined))",
            ":UNDEFINED",
        ),
        (
            "(handler-case (funcall 'quote 1) (error () :not-a-function))",
            ":NOT-A-FUNCTION",
        ),
        // NOTE: (apply #'+ 1 2) answers 1 rather than signalling on a non-list
        // last argument. That is pre-existing -- identical on a binary built
        // before this change, because the operator-position handler does the
        // same list_to_vec -- and is tracked separately (bliss-0qbf). Asserted
        // only to catch a DIVERGENCE between the two paths, not as correct.
        ("(apply #'+ 1 2)", "1"),
    ];
    run_expression_cases(&cases);
}

/// bliss-74rl: a closure is physically the cons `(EGCL::CLOSURE . id)`, so
/// every *raw tag test* mistakes it for a CONS. The readers already excluded it
/// (CAR/CDR/CONSP/...), but the destructive stores did not, and neither did the
/// T2 emitter -- so a promoted CONSP answered T where the interpreter answered
/// NIL, RPLACA's `(unless (consp cons) (error ...))` guard let the store
/// through, and a live function object was overwritten in place.
///
/// Two properties are asserted: the predicates are correct, and the mutators
/// signal instead of corrupting. The promotion half lives in the test below.
#[test]
fn closure_is_not_a_cons_for_predicates_or_destructive_stores() {
    let cases = [
        // Predicates: a closure is a FUNCTION, never a CONS or a LIST.
        ("(consp (lambda (x) x))", "NIL"),
        ("(atom (lambda (x) x))", "T"),
        ("(listp (lambda (x) x))", "NIL"),
        ("(typep (lambda (x) x) 'cons)", "NIL"),
        ("(typep (lambda (x) x) 'list)", "NIL"),
        ("(functionp (lambda (x) x))", "T"),
        // Readers signal rather than exposing the representation.
        (
            "(handler-case (car (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (cdr (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        // Destructive stores signal rather than overwriting the closure.
        (
            "(handler-case (rplaca (lambda (x) x) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (rplacd (lambda (x) x) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (setf (car (lambda (x) x)) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (setf (cdr (lambda (x) x)) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (setf (first (lambda (x) x)) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (setf (rest (lambda (x) x)) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (setf (elt (lambda (x) x) 0) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (egcl::set-car (lambda (x) x) 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (egcl::set-cdr (lambda (x) x) 1) (type-error () :te))",
            ":TE",
        ),
        // The closure SURVIVES a rejected store -- the point of the bug.
        (
            "(let ((f (lambda (x) (* x 2))))
            (handler-case (rplaca f 1) (error () nil))
            (handler-case (setf (car f) 1) (error () nil))
            (handler-case (setf (nth 0 f) 1) (error () nil))
            (handler-case (setf (elt f 0) 1) (error () nil))
            (funcall f 21))",
            "42",
        ),
        // List-walking builtins must reject a closure BEFORE the walk: driven
        // by a raw is_cons() they descended into `(EGCL::CLOSURE . id)`,
        // compared against the symbol, and then reported the fixnum id as the
        // offending datum (ansi MEMBER.ERROR.1 checks the datum is the
        // argument; leaking an internal symbol or id is wrong regardless).
        (
            "(let ((f (lambda (x) x)))
            (handler-case (member 'a f) (type-error (e) (eq (type-error-datum e) f))))",
            "T",
        ),
        (
            "(let ((f (lambda (x) x)))
            (handler-case (assoc 'a f) (type-error (e) (eq (type-error-datum e) f))))",
            "T",
        ),
        // ... while ordinary walks, keywords and improper tails are unchanged.
        ("(member 3 (list 1 2 3 4))", "(3 4)"),
        ("(member 2 (list 1 2 3) :key #'1+)", "(1 2 3)"),
        ("(member \"a\" (list \"a\") :test #'equal)", "(\"a\")"),
        ("(cdr (assoc 'b (list (cons 'a 1) (cons 'b 2))))", "2"),
        (
            "(handler-case (member 'z (cons 1 2)) (type-error (e) (type-error-datum e)))",
            "2",
        ),
        // Real conses are unaffected: every place form still mutates.
        (
            "(let ((l (list 1 2 3))) (rplaca l :a) (rplacd (cdr l) (list :c)) l)",
            "(:A 2 :C)",
        ),
        (
            "(let ((l (list 1 2 3))) (setf (car l) :a) (setf (cdr (cdr l)) nil) l)",
            "(:A 2)",
        ),
        (
            "(let ((l (list 1 2 3))) (setf (nth 1 l) :b) (setf (elt l 2) :c) l)",
            "(1 :B :C)",
        ),
        (
            "(let ((l (list 1 2 3))) (setf (second l) :x) l)",
            "(1 :X 3)",
        ),
        (
            "(let ((l (list 1 2 3))) (setf (subseq l 0 1) (list :y)) l)",
            "(:Y 2 3)",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-74rl, the tiering half: the same predicates and guards must answer
/// IDENTICALLY once the code is promoted. Each `inner`/`outer` pair is hot
/// enough to reach T2 (where CONSP was inlined as a bare `x & 7 == 1`), and a
/// closure is then fed to the promoted code. This is the stage-5 gate property
/// -- a hot loop promoted through tiers with identical results at each tier --
/// applied to the one value whose representation lies about its type.
#[test]
fn promoted_cons_predicates_and_guards_match_the_interpreter() {
    let cases = [(
        "(progn
           (defvar *f* (lambda (x) x))
           (defun i-consp (c) (consp c))            (defun o-consp (c) (i-consp c))
           (defun i-typep (c) (typep c 'cons))      (defun o-typep (c) (i-typep c))
           (defun i-rpa (c) (handler-case (progn (rplaca c 1) :mut) (type-error () :te)))
           (defun o-rpa (c) (i-rpa c))
           (defun i-rpd (c) (handler-case (progn (rplacd c 1) :mut) (type-error () :te)))
           (defun o-rpd (c) (i-rpd c))
           ;; Direct (setf (car|cdr x) v): since bliss-fdny these dispatch
           ;; through the SET-CAR/SET-CDR direct-builtin arm rather than
           ;; apply_function's synthesize path, so the hot answer here is what
           ;; proves that arm kept the closure exclusion.
           (defun i-sca (c) (handler-case (progn (setf (car c) 1) :mut) (type-error () :te)))
           (defun o-sca (c) (i-sca c))
           (defun i-scd (c) (handler-case (progn (setf (cdr c) 1) :mut) (type-error () :te)))
           (defun o-scd (c) (i-scd c))
           (let ((cold (list (o-consp *f*) (o-typep *f*) (o-rpa *f*) (o-rpd *f*)
                             (o-sca *f*) (o-scd *f*))))
             ;; Drive past the OSR/T2 promotion thresholds.
             (dotimes (i 600000)
               (o-consp (list 1 2)) (o-typep (list 1 2))
               (o-rpa (list 1 2)) (o-rpd (list 1 2))
               (o-sca (list 1 2)) (o-scd (list 1 2)))
             (let ((hot (list (o-consp *f*) (o-typep *f*) (o-rpa *f*) (o-rpd *f*)
                              (o-sca *f*) (o-scd *f*))))
               (list (equal cold hot) cold
                     ;; still a working function, and real conses still answer T
                     (funcall *f* 7) (o-consp (list 1)) (o-typep (list 1))))))",
        "(T (NIL NIL :TE :TE :TE :TE) 7 T T)",
    )];
    run_expression_cases(&cases);
}

/// bliss-0goy: NTH walked the spine via `list_to_vec`, materialising the WHOLE
/// list on every access. The kernel is now a walk, shared by the
/// operator-position handler and the evaluated-args fast arm so the two cannot
/// drift (the bliss-x5y.9 rule). These cases pin the behaviour the walk has to
/// reproduce exactly -- the dotted-tail and past-the-end answers in particular,
/// which are where a walk could plausibly differ from indexing a vector.
#[test]
fn nth_walks_the_spine_with_unchanged_edge_cases() {
    let cases = [
        // Ordinary indexing, both dispatch paths (FUNCALL takes the fast arm).
        ("(nth 0 (list 1 2 3))", "1"),
        ("(nth 2 (list 1 2 3))", "3"),
        ("(funcall #'nth 1 (list 1 2 3))", "2"),
        // Past the end is NIL, not an error.
        ("(nth 5 (list 1 2 3))", "NIL"),
        ("(nth 0 nil)", "NIL"),
        // Dotted tails: the walk must stop exactly where list_to_vec stopped.
        ("(nth 0 (cons 1 2))", "1"),
        ("(nth 1 (cons 1 2))", "NIL"),
        ("(nth 1 (cons 1 (cons 2 3)))", "2"),
        ("(nth 2 (cons 1 (cons 2 3)))", "NIL"),
        // A bignum index arrives as usize::MAX; it must terminate on the first
        // non-cons (O(length)), not attempt to count to the index.
        ("(nth 100000000000000000000 (list 1 2))", "NIL"),
        // Index type errors are unchanged and still precede the walk.
        (
            "(handler-case (nth -1 (list 1 2)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (nth 1.0 (list 1 2)) (type-error () :te))",
            ":TE",
        ),
        // A closure is not a list. Walking one used to answer the internal
        // symbol EGCL::CLOSURE for index 0 (bliss-74rl family).
        (
            "(let ((f (lambda (x) x)))
            (handler-case (nth 0 f) (type-error (e) (eq (type-error-datum e) f))))",
            "T",
        ),
        (
            "(let ((f (lambda (x) x)))
            (handler-case (funcall #'nth 0 f) (type-error (e) (eq (type-error-datum e) f))))",
            "T",
        ),
        // Long list: correct element, and the walk is the only way this stays
        // cheap enough to run in a test.
        (
            "(nth 9999 (let ((l nil)) (dotimes (i 10000) (push i l)) l))",
            "0",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi CHARACTER.1: a symbol character designator is defined by its
/// SYMBOL-NAME. COERCE-to-character used `symbol_bare_name`, which upper-cases
/// AND strips a package prefix at a colon -- so `(character (make-symbol "a"))`
/// answered #\A, and an uninterned symbol whose NAME is ":" was reduced to the
/// empty string and type-errored instead of yielding #\:.
#[test]
fn character_designator_uses_symbol_name_exactly() {
    let cases = [
        // Uninterned symbols: the name is taken verbatim, case and colons.
        ("(character (make-symbol \":\"))", "#\\:"),
        ("(character (make-symbol \"a\"))", "#\\a"),
        ("(character (make-symbol \"Q\"))", "#\\Q"),
        (
            "(handler-case (character (make-symbol \"ab\")) (type-error () :te))",
            ":TE",
        ),
        // Interned: the READER upper-cases, so 'a designates #\A -- but an
        // escaped |b| keeps its case, and |:| is a one-character name.
        ("(character '|:|)", "#\\:"),
        ("(character 'a)", "#\\A"),
        ("(character '|b|)", "#\\b"),
        // The other designators, and non-designators, are unchanged.
        ("(character \"x\")", "#\\x"),
        ("(character #\\y)", "#\\y"),
        ("(handler-case (character 65) (type-error () :te))", ":TE"),
        (
            "(handler-case (character \"xy\") (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (character (list 1)) (type-error () :te))",
            ":TE",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi STRING=.4: the string-comparison family takes string DESIGNATORS, and
/// a symbol designates its SYMBOL-NAME. `string_compare_bounds` used
/// `val_as_str`, which returns a symbol's registry key -- so an interned symbol
/// stringified to "COMMON-LISP-USER::ABC" and `(string= 'abc "ABC")` was NIL
/// while `(string= 'abc "COMMON-LISP-USER::ABC")` was T. An UNINTERNED symbol
/// has no prefix, which is why that case already worked and hid the bug, and
/// STRING/STRING-EQUAL resolve the name correctly -- the family had drifted.
#[test]
fn string_comparison_takes_symbol_name_as_the_designator() {
    let cases = [
        ("(string= 'abc \"ABC\")", "T"),
        ("(string= (make-symbol \"xY\") \"xY\")", "T"),
        ("(string= #\\a \"a\")", "T"),
        // The registry key must no longer compare equal.
        ("(string= 'abc \"COMMON-LISP-USER::ABC\")", "NIL"),
        // The ansi case itself: an escaped symbol keeps its case.
        ("(not (string= '|abc| (copy-seq \"abc\")))", "NIL"),
        // Ordering predicates share the same designator path.
        ("(string< 'abc \"ABD\")", "2"),
        ("(string> 'abd \"ABC\")", "2"),
        ("(string<= 'abc \"ABC\")", "3"),
        ("(string/= 'abc \"ABC\")", "NIL"),
        // Regressions: plain strings and the bounding keywords are unchanged.
        ("(string= \"abc\" \"abc\")", "T"),
        ("(string= \"abc\" \"abd\")", "NIL"),
        ("(string= \"xabcy\" \"abc\" :start1 1 :end1 4)", "T"),
        ("(string-equal 'abc \"ABC\")", "T"),
        ("(string 'abc)", "\"ABC\""),
    ];
    run_expression_cases(&cases);
}

/// bliss-l3d7: a SIMPLE_BASE_STRING's payload is one BYTE per character, but
/// `alloc_pinned_name` copied a symbol name's UTF-8 BYTES into one. Every
/// non-ASCII symbol name therefore decoded back as Latin-1 mojibake -- two
/// characters for a 2-byte code point, three for a 3-byte one -- and
/// FIND-SYMBOL could not match the key it had just interned under, so package
/// lookups on such names were broken, not merely cosmetic.
///
/// Names are now stored at the narrowest width that actually HOLDS the code
/// points, which is what the reader already did for string literals (which is
/// why those were always correct while symbol names were not).
#[test]
fn symbol_names_preserve_non_ascii_characters() {
    let cases = [
        // 2-byte, 3-byte and mixed code points: the name length is in
        // CHARACTERS, not bytes.
        (
            "(length (symbol-name (make-symbol (string (code-char 955)))))",
            "1",
        ),
        (
            "(length (symbol-name (make-symbol (string (code-char 8364)))))",
            "1",
        ),
        ("(length (symbol-name (make-symbol \"a\u{3bb}b\")))", "3"),
        (
            "(length (symbol-name (make-symbol \"\u{3bb}\u{3bc}\u{20ac}\")))",
            "3",
        ),
        // The character round-trips, not just the count.
        (
            "(char (symbol-name (make-symbol (string (code-char 955)))) 0)",
            "#\\\u{3bb}",
        ),
        // A symbol read from source agrees with one built by MAKE-SYMBOL.
        ("(length (symbol-name '|\u{3bb}|))", "1"),
        (
            "(string= (symbol-name '|\u{3bb}|) (string (code-char 955)))",
            "T",
        ),
        // The key actually interned under: FIND-SYMBOL must locate it.
        (
            "(progn (intern (string (code-char 955)))
                 (not (null (find-symbol (string (code-char 955))))))",
            "T",
        ),
        (
            "(let ((s (string (code-char 955)))) (eq (intern s) (find-symbol s)))",
            "T",
        ),
        // ASCII names are unchanged (still the compact base-string width).
        ("(length (symbol-name (make-symbol \"abc\")))", "3"),
        ("(symbol-name (make-symbol \"abc\"))", "\"abc\""),
        (
            "(progn (intern \"FOOBAR\") (not (null (find-symbol \"FOOBAR\"))))",
            "T",
        ),
        ("(find-symbol \"no-such-symbol-xyz\")", "NIL"),
    ];
    run_expression_cases(&cases);
}

/// bliss-ljmk: SECOND and THIRD each open-coded their own access -- and THIRD
/// called `list_to_vec`, materialising the entire list to read one element,
/// the same cost bliss-0goy removed from NTH. Both now route to `nth_element`,
/// the kernel NTH uses, so there is one walk rather than three copies of one.
///
/// Every answer below is unchanged from before that refactor EXCEPT the two
/// closure cases: `(second f)` answered NIL, which disagreed with egcl's own
/// CDR (SECOND is `(car (cdr x))`, and `(cdr <function>)` signals).
#[test]
fn ordinal_accessors_share_the_nth_kernel() {
    let cases = [
        ("(second (list 1 2 3))", "2"),
        ("(third (list 1 2 3))", "3"),
        ("(funcall #'second (list 1 2))", "2"),
        ("(funcall #'third (list 1 2 3))", "3"),
        // Short lists and NIL answer NIL rather than signalling.
        ("(second (list 1))", "NIL"),
        ("(third (list 1 2))", "NIL"),
        ("(second nil)", "NIL"),
        ("(third nil)", "NIL"),
        // Dotted tails stop where the walk stops.
        ("(second (cons 1 2))", "NIL"),
        ("(third (cons 1 (cons 2 (cons 3 4))))", "3"),
        // A non-list argument SIGNALS (bliss-swi5). These previously answered
        // NIL: the walk simply found no cons and fell out, so (second 5) and
        // (nth 1 "abc") reported "no such element" for something that has no
        // elements to begin with. SBCL type-errors on each of these, as CLHS
        // requires -- SECOND..TENTH and NTH all specify a LIST argument.
        ("(handler-case (second 5) (type-error () :te))", ":TE"),
        ("(handler-case (third \"abc\") (type-error () :te))", ":TE"),
        ("(handler-case (nth 1 5) (type-error () :te))", ":TE"),
        ("(handler-case (nth 0 'sym) (type-error () :te))", ":TE"),
        // A closure signals too, whether it is a heap object or the cons-shaped
        // (EGCL::CLOSURE . id): require_list rejects both.
        (
            "(handler-case (second (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (third (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (nth 1 (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        // NIL and a too-short list still answer NIL -- absent element, not a
        // bad argument -- so the new check must not swallow these.
        ("(nth 5 (list 1 2 3))", "NIL"),
        ("(nth 0 nil)", "NIL"),
        // FOURTH..TENTH are boot.lisp defuns and are unaffected.
        ("(fourth (list 1 2 3 4))", "4"),
        ("(tenth (list 1 2 3 4 5 6 7 8 9 10))", "10"),
    ];
    run_expression_cases(&cases);
}

/// bliss-wukf: a signalled error did not STOP execution in native code. The c2i
/// helper stashes the error and returns NIL for `run_native` to re-raise on
/// exit, and the emitted call sequence had no post-call check -- so native
/// execution carried on into code that must not run:
///
///   (defun h (x) (when (eq x :bad) (error "boom")) (setq *ran* t) :done)
///   cold      -> *ran* NIL      correct
///   hot (T1)  -> *ran* T        WRONG
///
/// T1 now follows HotSpot's UNCOMMON TRAP: a call to ERROR is not compiled at
/// all, it deopts to T0 at that bytecode index, and T0 signals with proper
/// unwinding. Free -- a branch that signals is cold by construction.
///
/// THE ITERATION COUNTS MATTER. The T1 window is roughly 50k-350k invocations;
/// probes using only a large count miss it entirely, which is how an earlier
/// sweep of this area came back clean against a broken build.
///
/// T2 IS NOT FIXED: see the bead. T2's deopt re-runs the whole function rather
/// than resuming at a bytecode index, so the same trick would duplicate side
/// effects performed before the error. The cases below are the ones T1 governs.
#[test]
fn a_signalled_error_stops_execution_in_native_code() {
    let cases = [
        // The wrong-error symptom: two validation clauses, warm up on good
        // input, then hit bad input. The FIRST error must be the one raised.
        (
            "(progn (defun wukf-g (end len)\
            (unless (and (integerp end) (<= 0 end len)) (error \"first\"))\
            (unless (<= 0 end) (error \"second\")) :ok)\
          (defun wukf-p () (multiple-value-bind (v e) (ignore-errors (wukf-g 'a 5))\
            (declare (ignore v)) (format nil \"~a\" e)))\
          (let ((cold (wukf-p)))\
            (dotimes (i 200000) (wukf-g 3 5))\
            (list cold (wukf-p))))",
            "(\"first\" \"first\")",
        ),
        // ...and the clause BODIES must not both run.
        (
            "(progn (defvar *wukf-tr* nil)\
          (defun wukf-h (end len)\
            (unless (and (integerp end) (<= 0 end len)) (push :b1 *wukf-tr*) (error \"first\"))\
            (unless (<= 0 end) (push :b2 *wukf-tr*) (error \"second\")) :ok)\
          (dotimes (i 200000) (wukf-h 3 5))\
          (setq *wukf-tr* nil)\
          (ignore-errors (wukf-h 'a 5))\
          (reverse *wukf-tr*))",
            "(:B1)",
        ),
        // The T2 shape: a branch that is NEVER taken while the function warms
        // up, whose body calls ERROR. T1 deopts at such a call; T2 compiles the
        // call and gives it a Trap terminator, so the block has no fall-through
        // and its successor gets no edge. Getting that bookkeeping wrong panicked
        // the SSA builder ("trying to get a VReg before observing its class") --
        // and getting the SEMANTICS wrong let the code after the ERROR run. 600k
        // iterations clears the OSR/T2 promotion window, which 200k does not.
        (
            "(progn (defvar *wukf-t2* nil)\
          (defun wukf-t2 (x)\
            (when (eq x :bad) (error \"boom\"))\
            (push :after *wukf-t2*) :done)\
          (dotimes (i 600000) (wukf-t2 :good))\
          (setq *wukf-t2* nil)\
          (multiple-value-bind (v e) (ignore-errors (wukf-t2 :bad))\
            (declare (ignore v))\
            (list (format nil \"~a\" e) (and *wukf-t2* t))))",
            "(\"boom\" NIL)",
        ),
    ];
    run_expression_cases(&cases);
}

/// TYPEP silently ignored a MALFORMED numeric bound, so a value was reported as
/// being of a type that cannot be constructed:
///
///     (typep 0 '(integer 0 a))   =>  T     SBCL signals
///     (typep 0 '(integer a 5))   =>  T     SBCL signals
///
/// Both bound checkers answered `true` for any bound they could not use --
/// carried over from the original `num_val`-based code and preserved verbatim
/// when the REAL arm was rewritten to compare exactly. Treating an unusable
/// bound as "no constraint" is the worst of the options: it turns a malformed
/// specifier into a permissive one.
///
/// It also broke ansi ARRAY-FILL-10 and its FIXNUM / UNSIGNED-BYTE8 siblings,
/// which is how it surfaced. SIGNALS-ERROR validates a signalled condition by
/// checking that its DATUM is NOT of its EXPECTED-TYPE; with a malformed
/// expected-type answering T for everything, a correctly-signalled TYPE-ERROR
/// was rejected as bogus.
///
/// A BIGNUM bound is NOT malformed -- it is merely outside fixnum range -- so
/// those still answer rather than signal. alexandria's ARRAY-INDEX,
/// `(integer 0 (array-dimension-limit))`, depends on that.
#[test]
fn typep_rejects_a_malformed_numeric_bound() {
    let cases = [
        (
            "(handler-case (typep 0 '(integer 0 a)) (error () :signalled))",
            ":SIGNALLED",
        ),
        (
            "(handler-case (typep 0 '(integer a 5)) (error () :signalled))",
            ":SIGNALLED",
        ),
        (
            "(handler-case (typep 0 '(real 0 a)) (error () :signalled))",
            ":SIGNALLED",
        ),
        (
            "(handler-case (typep 0.5 '(float a 5)) (error () :signalled))",
            ":SIGNALLED",
        ),
        // Well-formed bounds are unaffected.
        ("(typep 5 '(integer 0 10))", "T"),
        ("(typep 50 '(integer 0 10))", "NIL"),
        ("(typep 3 '(integer 0 *))", "T"),
        ("(typep 3 '(integer * 5))", "T"),
        ("(typep 3 '(integer 0))", "T"),
        ("(typep 3 '(integer (2) 5))", "T"),
        ("(typep 2 '(integer (2) 5))", "NIL"),
        ("(typep 3.0 '(real 0 5))", "T"),
        ("(typep 5 '(mod 10))", "T"),
        ("(typep 5 '(unsigned-byte 8))", "T"),
        // A bignum bound is out of fixnum range, NOT malformed.
        ("(typep 5 '(integer 0 1000000000000000000000))", "T"),
        ("(typep 5 '(integer -1000000000000000000000 10))", "T"),
        ("(typep 5 '(integer 1000000000000000000000 *))", "NIL"),
        // The alexandria ARRAY-INDEX shape: exclusive bignum-ish upper bound.
        ("(typep 5 '(integer 0 (1152921504606846975)))", "T"),
    ];
    run_expression_cases(&cases);
}

/// bliss-7lqe: REMOVE-DUPLICATES was O(n) with no :test and O(n^2) pairwise
/// funcalls with one, because any :test fell to %REMOVE-DUPLICATES-GENERAL.
/// EQ, EQL, EQUAL and EQUALP are exactly the four standard HASH-TABLE tests, so
/// those now take the same O(n) hash path. Measured on 873 short sublists:
/// :test #'equal 1404 ms -> 6 ms, :test #'equalp 905 ms -> 4 ms.
///
/// Substituting hashing for pairwise comparison is sound only because those
/// four are genuine EQUIVALENCE RELATIONS. An arbitrary :test need not be
/// symmetric or transitive, so it still takes the general path -- asserted
/// below with a non-transitive parity test, which must keep its old answer.
///
/// Every expectation here was diffed against SBCL: all 18 shapes identical.
#[test]
fn remove_duplicates_hashes_the_standard_tests() {
    let cases = [
        ("(remove-duplicates (list 1 2 1 3 2))", "(1 3 2)"),
        (
            "(remove-duplicates (list (list 1) (list 2) (list 1)) :test #'equal)",
            "((2) (1))",
        ),
        // A symbol designator must work too, not just a function object.
        (
            "(remove-duplicates (list (list 1) (list 2) (list 1)) :test 'equal)",
            "((2) (1))",
        ),
        (
            "(remove-duplicates (list \"a\" \"A\" \"b\") :test #'equalp)",
            "(\"A\" \"b\")",
        ),
        // EQUAL is case-SENSITIVE on strings; EQUALP is not. The hash table's
        // test must match the :test, not merely be "some string test".
        (
            "(remove-duplicates (list \"a\" \"A\" \"b\") :test #'equal)",
            "(\"a\" \"A\" \"b\")",
        ),
        ("(remove-duplicates (list 'a 'b 'a) :test #'eq)", "(B A)"),
        // :from-end and :key interact with the hash path.
        (
            "(remove-duplicates (list 1 2 1 3 2) :from-end t)",
            "(1 2 3)",
        ),
        (
            "(remove-duplicates (list (list 1) (list 2) (list 1)) :from-end t :test #'equal)",
            "((1) (2))",
        ),
        (
            "(remove-duplicates (list (list 1 :a) (list 2 :b) (list 1 :c)) :key #'car :test #'equal)",
            "((2 :B) (1 :C))",
        ),
        (
            "(remove-duplicates (list (list 1 :a) (list 2 :b) (list 1 :c)) :key #'car :from-end t)",
            "((1 :A) (2 :B))",
        ),
        // These must still take the GENERAL path -- bounds, :test-not, and a
        // test that is not an equivalence relation.
        ("(remove-duplicates (list 1 1 2 1) :start 1)", "(1 2 1)"),
        ("(remove-duplicates (list 1 1 2 1) :end 2)", "(1 2 1)"),
        ("(remove-duplicates (list 1 2 1) :test-not #'eql)", "(1)"),
        (
            "(remove-duplicates (list 1 2 3 4) :test (lambda (a b) (= (mod a 2) (mod b 2))))",
            "(3 4)",
        ),
        // Non-list sequences keep their type.
        ("(remove-duplicates \"abcabc\" :test #'char=)", "\"abc\""),
        ("(remove-duplicates (vector 1 2 1) :test #'eql)", "#(2 1)"),
        // DELETE-DUPLICATES delegates here, so it inherits the fast path.
        (
            "(delete-duplicates (list (list 1) (list 1)) :test #'equal)",
            "((1))",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-hb0q: `#'<builtin>` was not EQ-stable.
///
///     (eq #'car #'car)      egcl NIL,  SBCL T
///     (eq #'equal #'equal)  egcl NIL,  SBCL T
///     (eq #'myf #'myf)      egcl T     -- user functions already were
///
/// `builtin_fn_wrapper` reused the wrapper's closure ID but returned a FRESH
/// `arena_cons` every call, so two references to the same builtin were distinct
/// objects -- and every `#'<builtin>` reference allocated. CLHS 5.3 makes #'car
/// denote "the function named car"; two such references should be identical.
///
/// The fix caches the wrapper VALUE, which is only safe because the cache is
/// visited by `scan_evaluator_global_roots` -- the surrounding tables key on
/// stable u32/u64 ids precisely because an unvisited cached EgclVal would go
/// stale under the moving collector. The churn case below exists to exercise
/// that: it forces allocation between taking `#'car` and comparing it.
#[test]
fn builtin_function_objects_are_eq_stable() {
    let cases = [
        ("(eq #'car #'car)", "T"),
        ("(eq #'equal #'equal)", "T"),
        ("(eq #'equalp #'equalp)", "T"),
        ("(eq (fdefinition 'equal) (fdefinition 'equal))", "T"),
        ("(eq (symbol-function 'car) (symbol-function 'car))", "T"),
        // User-defined functions were already stable and must stay so.
        ("(progn (defun hb0q-f (x) x) (eq #'hb0q-f #'hb0q-f))", "T"),
        // DISTINCT builtins must remain distinct -- the cache is per name.
        ("(eq #'equal #'equalp)", "NIL"),
        ("(eq #'car #'cdr)", "NIL"),
        // Identity must survive allocation churn, i.e. a relocating collection.
        (
            "(let ((f #'car)) (dotimes (i 20000) (list i i)) (eq f #'car))",
            "T",
        ),
        (
            "(let ((f #'equal)) (dotimes (i 20000) (list i i)) (funcall f 3 3))",
            "T",
        ),
        // Calling behaviour is unchanged.
        ("(funcall #'car (list 1 2))", "1"),
        ("(apply #'+ (list 1 2 3))", "6"),
        ("(mapcar #'1+ (list 1 2))", "(2 3)"),
        ("(functionp #'car)", "T"),
        // MAKE-HASH-TABLE recognises #'equal through the wrapper (bliss-uuh);
        // the cache must not break that.
        (
            "(let ((h (make-hash-table :test #'equal))) (setf (gethash \"a\" h) 1)\
            (nth-value 1 (gethash (copy-seq \"a\") h)))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// SUBTYPEP returned "cannot determine" for a MEMBER type against an ordinary
/// supertype, where the answer is trivially decidable:
///
///     (subtypep '(member #\a #\b) 'character)   =>  NIL NIL,  want T T
///
/// `member_subtypep` only compared a MEMBER against ANOTHER MEMBER, so nothing
/// answered this shape. It is not cosmetic: the ansi random differential tests
/// pick an element through a `cond` over `(subtypep* '(member #\a … #\h) type)`,
/// so the indefinite answer made their helper signal "Can't get random element
/// of type CHARACTER" and took out eight tests (REMOVE-RANDOM and siblings,
/// RANDOM-REMOVE-DUPLICATES, RANDOM-DELETE-DUPLICATES).
///
/// The last two cases are DELIBERATELY left undecided. Only elements decidable
/// exactly get a definite answer; `(NIL NIL)` is always conforming while a
/// wrong definite answer is not. SBCL answers (T T) to the (integer 0 3) case,
/// so egcl is less precise there -- not incorrect. Everything else matches
/// SBCL exactly.
#[test]
fn subtypep_decides_member_types_against_ordinary_supertypes() {
    let cases = [
        (
            "(multiple-value-list (subtypep '(member #\\a #\\b) 'character))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(member a b) 'symbol))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(member 1 2) 'integer))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(eql #\\a) 'character))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(member nil) 'symbol))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(member 1000000000000000000000) 'integer))",
            "(T T)",
        ),
        // The empty MEMBER type is the empty type: a subtype of everything.
        (
            "(multiple-value-list (subtypep '(member) 'character))",
            "(T T)",
        ),
        // A member that fails the supertype gives a DEFINITE no.
        (
            "(multiple-value-list (subtypep '(member 1 #\\a) 'integer))",
            "(NIL T)",
        ),
        // Unrelated types still behave.
        (
            "(multiple-value-list (subtypep 'character 'character))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep 'fixnum 'character))",
            "(NIL T)",
        ),
        // Deliberately undecided -- see the note above.
        (
            "(multiple-value-list (subtypep '(member #\\a) 'standard-char))",
            "(NIL NIL)",
        ),
        (
            "(multiple-value-list (subtypep '(member 1 2) '(integer 0 3)))",
            "(NIL NIL)",
        ),
    ];
    run_expression_cases(&cases);
}

/// THE with a COMPOUND type specifier signalled a TYPE-ERROR naming the SPEC
/// as the bad datum, instead of returning the value:
///
///     (the (integer 0 9) 5)   =>  "Cons(...) is not of type type specifier"
///
/// The tree-walker's THE arm reads
///
///     if value_satisfies_declared_type(type_form, value)?
///         || typep_matches(env, value, type_form)?
///
/// and its own comment says the TYPEP fallback exists so "bounded/compound
/// specs are honoured rather than rejected". But the quick check returned Err
/// for any spec it could not canonicalise -- anything that is not a bare symbol
/// -- and `?` propagated it, so the fallback it documents never ran. A guard
/// meant to fall through instead bailed out.
///
/// The quick check now answers Ok(false) ("cannot tell") for those, and the
/// error path prints the spec rather than re-raising. Found while clearing the
/// last ansi sequences failures: LENGTH.STRING.5 and LENGTH.BIT-VECTOR.5 use
/// (the (simple-string 5) ...) to assert the subform is evaluated exactly once.
#[test]
fn the_accepts_compound_type_specifiers() {
    let cases = [
        ("(the (integer 0 9) 5)", "5"),
        ("(the (simple-string 5) \"abcde\")", "\"abcde\""),
        ("(the (vector t) (vector 1 2))", "#(1 2)"),
        ("(the (simple-bit-vector 5) #*00110)", "#*00110"),
        ("(the (array t (2)) (vector 1 2))", "#(1 2)"),
        // Bare symbols were already fine and must stay so.
        ("(the simple-string \"abcde\")", "\"abcde\""),
        ("(the integer 5)", "5"),
        ("(the list (list 1 2))", "(1 2)"),
        // The declared type must still be CHECKED, not just accepted: a value
        // that genuinely fails a compound spec still signals.
        (
            "(handler-case (funcall (compile nil '(lambda () (the (integer 0 9) 99)))) (type-error () :te))",
            ":TE",
        ),
        // THE must evaluate its subform EXACTLY ONCE -- the property the ansi
        // LENGTH tests actually assert.
        (
            "(let ((i 0)) (flet ((f () (incf i) (make-string 5 :initial-element #\\a))) (list (length (the (simple-string 5) (f))) i)))",
            "(5 1)",
        ),
        // NOTE: no `\` line-continuations inside these Lisp strings -- the
        // continuation eats the newline AND the indentation, so 'bit and
        // :initial-contents glue into the package reference bit:initial-contents.
        (
            "(let ((i 0)) (flet ((f () (incf i) (make-array 5 :element-type 'bit :initial-contents '(0 0 1 1 0)))) (list (length (the (simple-bit-vector 5) (f))) i)))",
            "(5 1)",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-c02n: STRINGP gave a different ANSWER once it promoted a tier.
///
/// The inlined T2 STRING test proves the heap tag and then accepts only
/// SIMPLE_BASE_STRING / SIMPLE_CHARACTER_STRING. A string with a fill pointer,
/// an adjustable one, or a DISPLACED one is a COMPLEX_ARRAY, so the inline
/// answered NIL where the interpreter -- which consults is_complex_vector +
/// cvec_is_string -- answers T.
///
/// The consequence was a wrong answer, not a slow path. boot.lisp's
/// %COERCE-LIKE is `(cond ((stringp orig) ...) ... (t (coerce list 'vector)))`
/// and REMOVE / REMOVE-IF / DELETE-IF / SUBSTITUTE / REMOVE-DUPLICATES all end
/// in it, so once STRINGP flipped they returned a general vector instead of a
/// string. It showed up only under the ansi harness because nothing else called
/// them enough times to promote -- which is why the bead recorded, wrongly,
/// that it would not reproduce standalone.
///
/// THE ITERATION COUNT IS LOAD-BEARING. Below the promotion threshold this
/// test passes against the BROKEN build and proves nothing. 1.5M is comfortably
/// past where the flip was observed (~1.2M) and costs ~5s on the debug binary.
/// Every case compares the value BEFORE and AFTER the loop, so a build that
/// never promotes cannot quietly pass only the easy half.
#[test]
fn stringp_keeps_its_answer_across_tier_promotion() {
    let setup = concat!(
        "(defvar *s0* (make-array 10 :element-type 'character ",
        "  :initial-contents (concatenate 'string \"XX\" \"ab1c2\" \"YYY\"))) ",
        "(defvar *sd* (make-array 5 :element-type 'character ",
        "  :displaced-to *s0* :displaced-index-offset 2)) ",
        "(defvar *fp* (make-array 9 :element-type 'character :fill-pointer 5 ",
        "  :initial-contents (coerce \"ab1c2ZZZZ\" 'list))) ",
        "(defvar *ad* (make-array 5 :element-type 'character :adjustable t ",
        "  :initial-contents (coerce \"ab1c2\" 'list))) ",
        "(defun sp (x) (stringp x)) ",
        "(defun tp (x) (typep x 'string)) ",
        "(defun hot (s) (remove-if #'alpha-char-p s))"
    );
    let cases: [(&str, &str); 8] = [
        // Cold, then hot: the two must agree.
        (
            "(list (sp *sd*) (progn (dotimes (i 1500000) (sp *sd*)) (sp *sd*)))",
            "(T T)",
        ),
        (
            "(list (sp *fp*) (progn (dotimes (i 1500000) (sp *fp*)) (sp *fp*)))",
            "(T T)",
        ),
        (
            "(list (sp *ad*) (progn (dotimes (i 1500000) (sp *ad*)) (sp *ad*)))",
            "(T T)",
        ),
        // TYPEP shares the emitter, so it must hold too.
        (
            "(list (tp *sd*) (progn (dotimes (i 1500000) (tp *sd*)) (tp *sd*)))",
            "(T T)",
        ),
        // The end-to-end consequence: REMOVE-IF must still answer a STRING
        // after its own promotion, not a general vector.
        (
            "(progn (dotimes (i 300000) (hot *sd*)) (hot *sd*))",
            "\"12\"",
        ),
        (
            "(progn (dotimes (i 300000) (hot *sd*)) (stringp (hot *sd*)))",
            "T",
        ),
        (
            "(progn (dotimes (i 300000) (hot *sd*)) (array-element-type (hot *sd*)))",
            "CHARACTER",
        ),
        // The fix must not make the predicate permissive the other way.
        (
            "(list (sp (vector 1 2)) (progn (dotimes (i 1500000) (sp (vector 1 2))) (sp (vector 1 2))))",
            "(NIL NIL)",
        ),
    ];
    for (expr, expected) in cases {
        let program = format!("(progn {setup} (cl:format t \"~S\" {expr}))");
        let output = egcl_bin()
            .args(["--eval", &program])
            .output()
            .expect("failed to run egcl");
        assert_eq!(
            output.status.code(),
            Some(0),
            "tier-promotion case failed: {expr}\nstderr: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("")
                .trim(),
            expected,
            "tier-promotion case: {expr}"
        );
    }
}

/// bliss-sci0: assigning to a constant variable PANICKED rather than
/// signalling. NIL and T are special immediates rather than TAG_SYMBOL values;
/// `is_symbol()` accepts them anyway ("tag 101 or special NIL/T"), but the
/// accessors that take the symbol index -- `sym_name`, `as_symbol_index` --
/// assert the strict tag, so an assignment to NIL reached one and panicked.
///
/// A panic is the worst failure mode available: no handler can run, and across
/// the `extern "C"` c2i boundary it aborts. It did not even need the form to
/// RUN -- CCASE's STORE-VALUE restart expands to `(setf <keyplace> ...)`, so
/// merely LOWERING `(ccase nil ...)` killed the process. That case is asserted
/// below: it must produce a function, not a corpse.
///
/// Three more targets did not panic but silently SUCCEEDED, which is its own
/// wrong answer -- a keyword, a DEFCONSTANT name, and a non-symbol all
/// returned the value as though something had been assigned. CLHS 3.1.2.1.1.3
/// makes all of these constant variables; SBCL signals PROGRAM-ERROR for every
/// row below, which is what these were checked against.
#[test]
fn assigning_to_a_constant_variable_signals_rather_than_panicking() {
    let cases = [
        (
            "(handler-case (eval '(setf nil 5)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (eval '(setq nil 5)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (eval '(setf t 5)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (eval '(setq t 5)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (eval '(setq :kw 5)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (eval '(setf 5 6)) (program-error () :pe))",
            ":PE",
        ),
        (
            "(progn (defconstant +sci0+ 7)\
           (handler-case (eval '(setq +sci0+ 9)) (program-error () :pe)))",
            ":PE",
        ),
        // The motivating case: LOWERING a malformed place must not kill the
        // process, even though the form never runs.
        ("(functionp (eval '(lambda () (ccase nil (1 :one)))))", "T"),
        // The value form must NOT be evaluated -- the check precedes it.
        (
            "(let ((hit nil))\
            (handler-case (eval '(setf nil (setq hit t))) (program-error () nil))\
            hit)",
            "NIL",
        ),
        // Ordinary assignment is untouched.
        ("(progn (setq sci0-x 5) sci0-x)", "5"),
        ("(progn (setf sci0-y 6) sci0-y)", "6"),
        (
            "(progn (setq sci0-a 1 sci0-b 2) (list sci0-a sci0-b))",
            "(1 2)",
        ),
        ("(let ((v 1)) (setq v 2) v)", "2"),
        // Real SETF places are not variable names and must be unaffected.
        ("(let ((l (list 1 2))) (setf (car l) 9) l)", "(9 2)"),
        (
            "(let ((v (vector 1 2))) (setf (aref v 0) 7) (aref v 0))",
            "7",
        ),
        (
            "(let ((h (make-hash-table))) (setf (gethash :k h) 5) (gethash :k h))",
            "5",
        ),
        // A symbol macro expands BEFORE the guard, so SETQ through one works.
        (
            "(let ((l (list 1 2))) (symbol-macrolet ((m (car l))) (setq m 42)) (car l))",
            "42",
        ),
        // bliss-aoj7: every row above reaches the guard through `(eval '...)`,
        // i.e. the TREE-WALKER only. sci0 guarded that evaluator and not the
        // bytecode lowerer, so these same forms, compiled, still panicked in
        // `store_to_symbol_place`. Written WITHOUT `eval`, they lower for real.
        ("(handler-case (setq nil 5) (program-error () :pe))", ":PE"),
        ("(handler-case (setf nil 5) (program-error () :pe))", ":PE"),
        ("(handler-case (setq t 5) (program-error () :pe))", ":PE"),
        ("(handler-case (setq :kw 5) (program-error () :pe))", ":PE"),
        // A DEFCONSTANT name the compiler already knows about. (The constant
        // must predate THIS form's compilation: the check is made when the
        // store is lowered, so a name that only becomes constant later is not
        // caught in code already compiled -- see bliss-p1a5.)
        ("(handler-case (setq pi 3) (program-error () :pe))", ":PE"),
        (
            "(handler-case (setq most-positive-fixnum 3) (program-error () :pe))",
            ":PE",
        ),
        // The motivating form, compiled rather than merely lowered: CCASE's
        // STORE-VALUE restart is a `(setf <keyplace> ...)` on NIL.
        (
            "(handler-case (ccase nil (1 :one)) (type-error () :te))",
            ":TE",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-im5c: DEFCONSTANT expanded to `(progn (setq name value)
/// (%mark-constant 'name))` -- a bare SETQ. Re-evaluating a DEFCONSTANT form is
/// normal and legal (COMPILE-FILE evaluates it per CLHS 3.2.2.3, then the fasl
/// evaluates it again at load), but by the second evaluation the name is marked,
/// so the SETQ became an assignment to an established constant. As soon as
/// bliss-sci0 stopped silently permitting that, the expansion refused itself:
/// `(asdf:load-system :babel)` died on alexandria's DEFINE-CONSTANT.
///
/// DEFCONSTANT is precisely the operator permitted to write a constant, so both
/// expansion sites (the lib/boot.lisp macro and the fasl lowering, which had
/// duplicated the shape) now emit one `%defconstant` that assigns and marks.
#[test]
fn re_evaluating_a_defconstant_form_is_not_an_illegal_assignment() {
    let cases = [
        // The direct shape: defining the same constant twice must not signal.
        (
            "(progn (defconstant +im5c-a+ 5) (defconstant +im5c-a+ 5) +im5c-a+)",
            "5",
        ),
        // alexandria DEFINE-CONSTANT's shape -- a non-EQL value each time,
        // funnelled through the old value when an EQUALP test accepts it. This
        // is what babel's +EBCDIC-ENCODE-TABLE+ does.
        (
            "(progn (defconstant +im5c-b+ (vector 1 2))\
           (defconstant +im5c-b+ (if (equalp +im5c-b+ (vector 1 2)) +im5c-b+ (vector 1 2)))\
           (coerce +im5c-b+ 'list))",
            "(1 2)",
        ),
        // The constant is still a constant, and still holds its value.
        (
            "(progn (defconstant +im5c-c+ 3) (defconstant +im5c-c+ 3)\
           (list (constantp '+im5c-c+) +im5c-c+))",
            "(T 3)",
        ),
        // ...so assigning to a constant is still refused (the sci0 guard is
        // intact). Uses a constant that predates this form's compilation; see
        // bliss-p1a5 for why a same-form DEFCONSTANT is not caught when
        // compiled.
        ("(handler-case (setq pi 3) (program-error () :pe))", ":PE"),
        // DEFVAR and DEFPARAMETER keep their own semantics.
        (
            "(progn (defvar *im5c-v* 1) (defvar *im5c-v* 99) *im5c-v*)",
            "1",
        ),
        (
            "(progn (defparameter *im5c-p* 1) (defparameter *im5c-p* 99) *im5c-p*)",
            "99",
        ),
        // DEFCONSTANT still returns its name and takes a doc string.
        ("(defconstant +im5c-e+ 8 \"doc\")", "+IM5C-E+"),
    ];
    run_expression_cases(&cases);
}

/// bliss-h21k: the two gaps left open by bliss-wzfm, both the same shape as it.
///
/// `(vector bit)` names a bit vector through its ELEMENT TYPE, and COERCE's
/// head-symbol dispatch sent it to the general VECTOR arm -- `#(1 0)` where
/// SBCL answers `#*10`. `result_type_is_bit_vector` already had the rule (MAP
/// and CONCATENATE use it); COERCE now shares it, exactly as it now shares the
/// string predicate.
///
/// And an element type of NIL holds NO elements, so only an empty sequence can
/// be coerced to it. `result_type_is_string` counts `(array nil (*))` as a
/// string subtype -- right for classification (CLHS 15.1.2.2), but it must not
/// let a non-empty sequence through. bliss-wzfm moved that case from one wrong
/// answer to another; this is the guard it needed.
///
/// Verified against SBCL.
#[test]
fn coerce_builds_bit_vectors_and_rejects_a_nil_element_type() {
    let cases = [
        ("(coerce '(1 0) '(vector bit))", "#*10"),
        ("(coerce '(1 0) '(simple-array bit (*)))", "#*10"),
        ("(coerce '(1 0) '(array bit (*)))", "#*10"),
        ("(coerce '(1 0) '(vector bit 2))", "#*10"),
        // Bare-symbol specs were already right and must stay so.
        ("(coerce '(1 0) 'bit-vector)", "#*10"),
        ("(coerce '(1 0) 'simple-bit-vector)", "#*10"),
        ("(bit-vector-p (coerce '(1 0) '(vector bit)))", "T"),
        ("(coerce nil '(vector bit))", "#*"),
        ("(coerce #*10 '(vector bit))", "#*10"),
        // A non-bit element still signals, and a declared length still checked.
        (
            "(handler-case (coerce '(1 2) '(vector bit)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (coerce '(1 0) '(vector bit 5)) (error () :err))",
            ":ERR",
        ),
        // A T element type is NOT a bit vector.
        ("(coerce '(1 0) '(vector t))", "#(1 0)"),
        // Element type NIL: empty is fine, non-empty signals.
        (
            "(handler-case (coerce '(#\\a #\\b) '(array nil (*))) (type-error () :te))",
            ":TE",
        ),
        ("(coerce nil '(array nil (*)))", "\"\""),
        // The siblings sharing these predicates must still agree.
        ("(concatenate 'bit-vector '(0 1 1))", "#*011"),
        ("(concatenate '(vector bit) '(0 1))", "#*01"),
        ("(make-sequence 'bit-vector 2)", "#*00"),
        // And the string half of the shared machinery is untouched.
        ("(coerce '(#\\a #\\b) '(vector character))", "\"ab\""),
    ];
    run_expression_cases(&cases);
}

/// bliss-wzfm: COERCE reduced a type specifier to its HEAD symbol, so a
/// compound spec naming a string through its ELEMENT TYPE --
/// `(simple-array character (*))`, `(vector base-char)` -- dispatched on
/// SIMPLE-ARRAY/VECTOR and built a general vector: `#(#\a #\b)` where SBCL
/// answers `"ab"`.
///
/// The rule was not restated for COERCE. `result_type_is_string` in
/// egcl-stdlib already had it (CONCATENATE needed the same thing), so COERCE
/// now shares that predicate -- which is the point: COERCE, CONCATENATE and
/// MAKE-SEQUENCE agreeing is what keeps this from regressing one function at a
/// time. The last three cases check the siblings still agree.
///
/// Expectations verified against SBCL.
#[test]
fn coerce_recognizes_a_string_named_by_its_element_type() {
    let cases = [
        (
            "(coerce '(#\\a #\\b) '(simple-array character (*)))",
            "\"ab\"",
        ),
        ("(coerce '(#\\a #\\b) '(vector character))", "\"ab\""),
        ("(coerce '(#\\a #\\b) '(array character (*)))", "\"ab\""),
        (
            "(coerce '(#\\a #\\b) '(simple-array base-char (*)))",
            "\"ab\"",
        ),
        ("(coerce '(#\\a #\\b) '(vector base-char))", "\"ab\""),
        (
            "(coerce '(#\\a #\\b) '(simple-array standard-char (*)))",
            "\"ab\"",
        ),
        ("(coerce '(#\\a #\\b) '(vector character 2))", "\"ab\""),
        // Bare symbol specs were already right and must stay so.
        ("(coerce '(#\\a #\\b) 'string)", "\"ab\""),
        ("(coerce '(#\\a #\\b) 'simple-string)", "\"ab\""),
        // A NON-character element type must NOT be treated as a string.
        ("(coerce '(1 2) '(vector t))", "#(1 2)"),
        ("(coerce '(1 2) '(simple-array t (*)))", "#(1 2)"),
        ("(coerce '(1 2) 'vector)", "#(1 2)"),
        ("(coerce '(#\\a #\\b) 'list)", "(#\\a #\\b)"),
        // Already a string, and other sequence inputs.
        ("(coerce \"ab\" '(vector character))", "\"ab\""),
        ("(coerce #(#\\a #\\b) '(vector character))", "\"ab\""),
        ("(coerce nil '(vector character))", "\"\""),
        (
            "(stringp (coerce '(#\\a #\\b) '(simple-array character (*))))",
            "T",
        ),
        // The siblings that share the predicate must still agree.
        (
            "(concatenate '(vector character) \"ab\" \"cd\")",
            "\"abcd\"",
        ),
        (
            "(make-sequence '(vector character) 2 :initial-element #\\x)",
            "\"xx\"",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-w0ae: MEMBER and ASSOC were the two worst builtins left on the
/// synthesize-and-re-evaluate detour (6.3us and 5.9us against 0.23us for CAR).
/// They are excluded from the direct-builtin table because :test/:key take
/// arbitrary Lisp functions and the table requires LEAF builtins -- but the
/// no-keyword shape cannot re-enter Lisp, so it qualifies.
///
/// The risk in specialising a shape is that the specialised and general paths
/// drift, so operator position delegates to the SAME kernel for this shape
/// rather than keeping a second copy. These cases pin both halves: the
/// no-keyword answers, and that every keyword form still reaches the general
/// path unchanged. Expectations verified against SBCL.
#[test]
fn member_and_assoc_no_keyword_fast_path_matches_the_general_path() {
    let cases = [
        // No keywords -- the specialised shape.
        ("(member 3 (list 1 2 3 4))", "(3 4)"),
        ("(member 9 (list 1 2 3))", "NIL"),
        ("(member 1 nil)", "NIL"),
        // A match found BEFORE the improper tail is reached is not an error...
        ("(member 2 (cons 1 (cons 2 3)))", "(2 . 3)"),
        // ...but reaching that tail without a match is.
        (
            "(handler-case (member 5 (cons 1 2)) (type-error () :te))",
            ":TE",
        ),
        ("(handler-case (member 1 5) (type-error () :te))", ":TE"),
        // A closure is physically a cons in the interpreter and must never be
        // walked as a list (the bliss-74rl family).
        (
            "(handler-case (member 1 (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        // eql_values allocates for bignums; the kernel must root across it.
        (
            "(member 1000000000000000000000 (list 1000000000000000000000))",
            "(1000000000000000000000)",
        ),
        ("(assoc 'b '((a . 1) (b . 2)))", "(B . 2)"),
        ("(assoc 'z '((a . 1)))", "NIL"),
        ("(assoc 'a nil)", "NIL"),
        // NIL entries are skipped; a non-NIL non-cons entry is a TYPE-ERROR.
        ("(assoc 'a (list nil (cons 'a 1)))", "(A . 1)"),
        (
            "(handler-case (assoc 'a (list 5)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (assoc 'a (cons (cons 'b 1) 7)) (type-error () :te))",
            ":TE",
        ),
        ("(handler-case (assoc 'a 5) (type-error () :te))", ":TE"),
        // Keyword forms must still reach the general path and behave exactly as
        // before -- the fast arm has to DECLINE, not mishandle them.
        ("(member 3 (list 1 2 3 4) :test #'eql)", "(3 4)"),
        ("(member 2.0 (list 1 2 3) :test #'=)", "(2 3)"),
        ("(member 'b (list (list 'a) (list 'b)) :key #'car)", "((B))"),
        ("(member 3 (list 1 2 3) :test-not #'eql)", "(1 2 3)"),
        ("(assoc 2.0 '((2 . b)) :test #'=)", "(2 . B)"),
        ("(assoc 'b '((a . 1) (b . 2)) :key #'identity)", "(B . 2)"),
        ("(assoc 'a '((a . 1)) :test-not #'eql)", "NIL"),
        // A bad keyword tail is still a PROGRAM-ERROR, not silently ignored.
        (
            "(handler-case (member 1 (list 1) :bogus 1) (program-error () :pe))",
            ":PE",
        ),
        // An explicit NIL :key is not "a :key was supplied" -- it must behave
        // like the no-keyword case, which is how the general path reads it.
        ("(member 3 (list 1 2 3) :key nil)", "(3)"),
    ];
    run_expression_cases(&cases);
}

/// bliss-ccgu: an FLET/LABELS binding shadows a BUILTIN, and the two tiers
/// must agree about it. The tree-walker dispatched builtin names without
/// consulting the lexical function environment, so
/// `(flet ((cons (a b) (list :shadowed a b))) (cons 1 2))` answered
/// `(:SHADOWED 1 2)` compiled and `(1 . 2)` tree-walked -- the same form, two
/// answers.
///
/// CLHS 11.1.2.1.2 leaves binding a CL symbol undefined, so neither answer is
/// wrong in isolation; the tiers agreeing is the property that matters, and the
/// compiled answer is the one that respects the binding.
///
/// `run_expression_cases` runs every case through BOTH backends, which is
/// exactly the check this needs.
#[test]
fn flet_and_labels_shadow_builtins_identically_in_both_tiers() {
    let cases = [
        (
            "(flet ((cons (a b) (list :shadowed a b))) (cons 1 2))",
            "(:SHADOWED 1 2)",
        ),
        ("(flet ((car (x) :shadowed)) (car (list 1)))", ":SHADOWED"),
        ("(flet ((1+ (x) :shadowed)) (1+ 1))", ":SHADOWED"),
        (
            "(labels ((length (x) :shadowed)) (length (list 1 2)))",
            ":SHADOWED",
        ),
        // Builtins moved to Rust in the character work inherit the same rule.
        (
            "(flet ((char= (a b) :shadowed)) (char= #\\a #\\b))",
            ":SHADOWED",
        ),
        (
            "(flet ((char-code (c) :shadowed)) (char-code #\\a))",
            ":SHADOWED",
        ),
        (
            "(flet ((char-equal (a b) :shadowed)) (char-equal #\\a #\\b))",
            ":SHADOWED",
        ),
        // SPECIAL OPERATORS are excluded: IF is not a function and cannot be
        // called, so the special form still wins -- as it does in the lowerer.
        ("(flet ((if (a b) :shadowed)) (if t 1 2))", "1"),
        ("(flet ((quote (x) :shadowed)) (quote a))", "A"),
        ("(flet ((let (x) :shadowed)) (let ((v 5)) v))", "5"),
        // An unshadowed call is untouched -- the guard must not leak out of the
        // FLET, and the binding must not outlive its scope.
        ("(char= #\\a #\\a)", "T"),
        ("(car (list 7))", "7"),
        (
            "(progn (flet ((car (x) :shadowed)) (car (list 1))) (car (list 7)))",
            "7",
        ),
        // A nested rebinding still resolves to the innermost one.
        (
            "(flet ((car (x) :outer)) (flet ((car (x) :inner)) (car (list 1))))",
            ":INNER",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-7oa5: the character comparison family moved from boot.lisp `&rest`
/// defuns to Rust builtins over egcl-stdlib::characters (37-200x faster).
/// These assert the SEMANTICS the move had to preserve exactly -- the arity
/// rules are the easy part to get wrong, and every expectation here was checked
/// against SBCL first.
#[test]
fn character_comparisons_keep_their_arity_and_type_rules() {
    let cases = [
        // One argument is T; zero is a PROGRAM-ERROR, not NIL and not T.
        ("(char= #\\a)", "T"),
        ("(char/= #\\a)", "T"),
        ("(handler-case (char/=) (program-error () :pe))", ":PE"),
        ("(handler-case (char-equal) (program-error () :pe))", ":PE"),
        // Monotonic predicates compare ADJACENT pairs.
        ("(char= #\\a #\\a #\\a)", "T"),
        ("(char= #\\a #\\a #\\b)", "NIL"),
        ("(char<= #\\a #\\b #\\c)", "T"),
        ("(char<= #\\a #\\c #\\b)", "NIL"),
        // cl-ppcre's char-class matcher needs the 3-argument range test (bliss-omw).
        ("(char<= #\\a #\\m #\\z)", "T"),
        ("(char<= #\\a #\\Z #\\z)", "NIL"),
        // /= is PAIRWISE distinct, NOT the negation of = beyond two arguments:
        // no two NEIGHBOURS match here, yet the answer is NIL.
        ("(char/= #\\a #\\b #\\a)", "NIL"),
        ("(char/= #\\a #\\b #\\c)", "T"),
        // Case-insensitive family.
        ("(char-equal #\\a #\\A)", "T"),
        ("(char-equal #\\a #\\A #\\a)", "T"),
        ("(char-equal #\\a #\\b)", "NIL"),
        ("(char-lessp #\\a #\\B)", "T"),
        // Case-folded, so B > A is T -- NOT the code-point answer, where
        // #\B (66) < #\a (97) would give NIL.
        ("(char-greaterp #\\B #\\a)", "T"),
        ("(char-greaterp #\\a #\\B)", "NIL"),
        ("(char-not-equal #\\a #\\A)", "NIL"),
        // A non-character argument is a TYPE-ERROR in every position, including
        // one an early answer might have let it skip.
        ("(handler-case (char= #\\a 5) (type-error () :te))", ":TE"),
        ("(handler-case (char= 5 #\\a) (type-error () :te))", ":TE"),
        (
            "(handler-case (char= #\\a #\\b 5) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (char-equal #\\a 5) (type-error () :te))",
            ":TE",
        ),
        ("(handler-case (char-upcase 5) (type-error () :te))", ":TE"),
        // Case conversion, including a caseless character.
        ("(char-upcase #\\a)", "#\\A"),
        ("(char-upcase #\\A)", "#\\A"),
        ("(char-upcase #\\1)", "#\\1"),
        ("(char-downcase #\\A)", "#\\a"),
        // Wrong arity is a PROGRAM-ERROR. As boot.lisp defuns the lambda-list
        // binder gave this for free; as builtins nothing did until an explicit
        // fixed_arity entry was added (ansi CHAR-UPCASE.ERROR.1/2).
        ("(handler-case (char-upcase) (program-error () :pe))", ":PE"),
        (
            "(handler-case (char-upcase #\\a #\\a) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (char-downcase) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (char-downcase #\\a #\\a) (program-error () :pe))",
            ":PE",
        ),
        // Case handling is ASCII-only and must stay CONSISTENT with
        // LOWER-CASE-P: ansi CHAR-UPCASE.2 walks all 65536 code points
        // asserting that anything CHAR-UPCASE changes is a character
        // LOWER-CASE-P calls lower. A Unicode CHAR-UPCASE over an ASCII
        // LOWER-CASE-P breaks that (see the separate Unicode-case bead).
        ("(char-upcase (code-char 233))", "#\\\u{e9}"),
        ("(lower-case-p (code-char 233))", "NIL"),
        ("(char-code #\\a)", "97"),
        ("(code-char 97)", "#\\a"),
        // Reachable as functions, not just in operator position -- these are
        // FBOUNDP-visible and funcallable, which moving them out of boot.lisp
        // initially broke for the case-insensitive names.
        ("(funcall #'char= #\\a #\\a)", "T"),
        ("(funcall #'char-equal #\\a #\\A)", "T"),
        (
            "(and (fboundp 'char-lessp) (fboundp 'char-not-greaterp)\
               (fboundp 'char-not-lessp) (fboundp 'char-greaterp)\
               (fboundp 'char-not-equal) t)",
            "T",
        ),
        ("(apply #'char< (list #\\a #\\b))", "T"),
    ];
    run_expression_cases(&cases);
}

/// bliss-7efw: a class-allocated slot's `:initarg` was silently dropped unless
/// the slot-name symbol already had a BARE internal name.
///
/// `eval_defclass` stored the slot name as `sym_name` gave it -- package
/// qualified -- while `lookup_slot_def` compared against the bare name callers
/// pass. The lookup missed, the initarg was treated as an instance initarg, the
/// shared class cell was never written, and the slot read back UNBOUND.
///
/// This hid behind an accident for a long time: it only bites names that
/// boot.lisp does not itself mention, and the older test for this area used a
/// slot named CS while boot.lisp had `&rest cs` parameters interning CS bare.
/// Every slot name below is deliberately one boot.lisp never uses, so the test
/// cannot pass on that coincidence again.
#[test]
fn class_allocated_initargs_work_for_unfamiliar_slot_names() {
    let cases = [
        (
            "(progn (defclass q1 () ((zebra :allocation :class :reader q1z :initarg :zebra)))\
           (make-instance 'q1 :zebra 99) (q1z (make-instance 'q1)))",
            "99",
        ),
        // Value is shared with a subclass instance.
        (
            "(progn (defclass q2 () ((quokka :allocation :class :reader q2q :initarg :quokka)))\
           (defclass q2sub (q2) ()) (make-instance 'q2 :quokka 'shared)\
           (q2q (make-instance 'q2sub)))",
            "SHARED",
        ),
        // An instance-allocated initarg with an equally unfamiliar name still works.
        (
            "(progn (defclass q3 () ((narwhal :initarg :narwhal :reader q3n)))\
           (q3n (make-instance 'q3 :narwhal 7)))",
            "7",
        ),
        // A class slot with an initform and no initarg is unaffected.
        (
            "(progn (defclass q4 () ((platypus :allocation :class :initform 5 :reader q4p)))\
           (q4p (make-instance 'q4)))",
            "5",
        ),
        // An explicit initarg overrides the initform on a class slot.
        (
            "(progn (defclass q5 () ((wombat :allocation :class :initform 1 :initarg :wombat :reader q5w)))\
           (make-instance 'q5 :wombat 42) (q5w (make-instance 'q5)))",
            "42",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-3snc: SIMPLE-VECTOR-P shared VECTORP's dispatch arm, so it inherited
/// VECTORP's answer and reported T for everything rank-1 -- strings, bit
/// vectors, fill-pointer and adjustable arrays. CLHS scopes it to a SIMPLE
/// one-dimensional array of element type T, so a specialized element type
/// (CHARACTER, BIT) and a complex array (fill pointer / adjustable /
/// displaced) each disqualify it independently.
///
/// It also contradicted TYPEP: `(typep "ab" 'simple-vector)` already answered
/// NIL via the strict helper while `(simple-vector-p "ab")` answered T, and
/// CLHS requires the two to be equivalent. Every case below is asserted three
/// ways -- operator position, funcall (the evaluated-args fast path), and
/// TYPEP -- because the bug was precisely that those paths disagreed.
///
/// Expectations verified against SBCL.
#[test]
fn simple_vector_p_excludes_specialized_and_complex_vectors() {
    let subjects = [
        ("(vector 1 2)", "T"),
        ("(make-array 3)", "T"),
        // Specialized element type: not element type T.
        ("\"ab\"", "NIL"),
        ("#*101", "NIL"),
        ("(make-array 3 :element-type 'bit)", "NIL"),
        ("(make-array 3 :element-type 'character)", "NIL"),
        // Complex array: not simple.
        ("(make-array 3 :fill-pointer 2)", "NIL"),
        ("(make-array 3 :adjustable t)", "NIL"),
        // Not a rank-1 array at all.
        ("(make-array '(2 2))", "NIL"),
        ("nil", "NIL"),
        ("'(1 2)", "NIL"),
        ("5", "NIL"),
    ];
    let mut cases: Vec<(String, String)> = Vec::new();
    for (subject, want) in subjects {
        cases.push((format!("(simple-vector-p {subject})"), want.to_string()));
        cases.push((
            format!("(funcall #'simple-vector-p {subject})"),
            want.to_string(),
        ));
        cases.push((
            format!("(typep {subject} 'simple-vector)"),
            want.to_string(),
        ));
    }
    // VECTORP keeps its own, broader answer -- the fix must not narrow it.
    for (subject, want) in [
        ("\"ab\"", "T"),
        ("#*101", "T"),
        ("(vector 1 2)", "T"),
        ("(make-array 3 :fill-pointer 2)", "T"),
        ("'(1 2)", "NIL"),
    ] {
        cases.push((format!("(vectorp {subject})"), want.to_string()));
    }
    let refs: Vec<(&str, &str)> = cases
        .iter()
        .map(|(e, w)| (e.as_str(), w.as_str()))
        .collect();
    run_expression_cases(&refs);
}

/// bliss-swi5: LAST's loop exits immediately on a non-cons and returns its
/// trailing pointer, which still points at the ORIGINAL argument -- so (last 5)
/// answered 5, reporting a non-list as the last cons of itself. The bead frames
/// this as closures walking their cons representation, but the cause is the
/// missing argument check: every atom slipped through, and the heap-object
/// closure that prompted the report is not a cons at all.
///
/// SBCL gets this from (defknown last (list &optional unsigned-byte) ...);
/// verified against it that each erroring case below errors there and each
/// preserved case returns the same value.
#[test]
fn last_requires_a_list_argument() {
    let cases = [
        // Non-lists signal rather than answering themselves.
        ("(handler-case (last 5) (type-error () :te))", ":TE"),
        ("(handler-case (last \"abc\") (type-error () :te))", ":TE"),
        ("(handler-case (last 'sym) (type-error () :te))", ":TE"),
        (
            "(handler-case (last (lambda (x) x)) (type-error () :te))",
            ":TE",
        ),
        // A dotted list IS a list: unchanged.
        ("(last (cons 1 2))", "(1 . 2)"),
        ("(last (list 1 2 3))", "(3)"),
        ("(last nil)", "NIL"),
        // The N argument keeps its own check and its own behaviour.
        ("(last (list 1 2 3) 2)", "(2 3)"),
        ("(last (list 1 2 3) 0)", "NIL"),
        ("(last (list 1 2 3) 99)", "(1 2 3)"),
        (
            "(handler-case (last (list 1 2) -1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (last (list 1 2) 'x) (type-error () :te))",
            ":TE",
        ),
        // The list check must come after N's, so a bad N on a bad list still
        // reports N -- both are type-errors, but N is checked first.
        (
            "(handler-case (last 5 -1) (type-error (c) (type-error-datum c)))",
            "-1",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-7kmw: STRING is a UNION type -- CLHS defines it as the union of
/// (vector character), (vector base-char) and (vector nil) -- but the array
/// lattice modelled it as a single specialized array type with element type
/// CHARACTER, the same one BASE-STRING got. Element types are compared by
/// equality (correct: arrays are invariant in their upgraded element type), so
/// STRING and BASE-STRING came out as MUTUAL subtypes, and an element type of
/// NIL had no representation at all.
///
/// Note the second value was T in every one of the seven ansi failures, i.e.
/// SUBTYPEP claimed certainty while answering wrongly.
#[test]
fn string_types_form_a_union_not_one_specialized_array() {
    let cases = [
        // STRING is NOT a subtype of the narrower base-char types.
        (
            "(multiple-value-list (subtypep 'string 'base-string))",
            "(NIL T)",
        ),
        (
            "(multiple-value-list (subtypep 'simple-string 'simple-base-string))",
            "(NIL T)",
        ),
        (
            "(multiple-value-list (subtypep 'simple-string '(simple-array character (*))))",
            "(NIL T)",
        ),
        (
            "(multiple-value-list (subtypep 'simple-string '(simple-array base-char (*))))",
            "(NIL T)",
        ),
        // ... but each arm of the union IS a subtype of it, including (array nil).
        (
            "(multiple-value-list (subtypep '(array nil (*)) 'string))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(array nil 1) 'string))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(simple-array nil (*)) 'simple-string))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep 'base-string 'string))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep 'simple-base-string 'simple-string))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(vector character) 'string))",
            "(T T)",
        ),
        // Ordinary relations are unchanged.
        (
            "(multiple-value-list (subtypep 'simple-string 'string))",
            "(T T)",
        ),
        ("(multiple-value-list (subtypep 'string 'string))", "(T T)"),
        ("(multiple-value-list (subtypep 'string 'vector))", "(T T)"),
        ("(multiple-value-list (subtypep 'string 'array))", "(T T)"),
        (
            "(multiple-value-list (subtypep 'string 'sequence))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep '(vector bit) 'bit-vector))",
            "(T T)",
        ),
        (
            "(multiple-value-list (subtypep 'simple-vector 'vector))",
            "(T T)",
        ),
        // The empty element type is NOT a universal bottom: arrays stay
        // invariant in their upgraded element type outside the string union.
        (
            "(multiple-value-list (subtypep '(array nil (*)) '(array bit (*))))",
            "(NIL T)",
        ),
        // TYPEP is unaffected -- only the SUBTYPEP lattice changed.
        ("(typep \"abc\" 'string)", "T"),
        ("(typep \"abc\" 'simple-string)", "T"),
        ("(typep 5 'string)", "NIL"),
    ];
    run_expression_cases(&cases);
}

/// bliss-ljmk: ABS had no evaluated-args kernel, so every call through
/// FUNCALL/APPLY or a compiled call site paid apply_function's
/// synthesize-and-re-evaluate detour. Its four branches are now extracted into
/// `abs_value` and shared with the operator-position handler -- extracted
/// rather than transcribed, because the exact-type preservation below is easy
/// to get subtly wrong in a second copy.
#[test]
fn abs_preserves_exact_type_on_both_dispatch_paths() {
    let cases = [
        ("(abs 5)", "5"),
        ("(abs -5)", "5"),
        ("(abs 0)", "0"),
        ("(abs -5.5)", "5.5"),
        ("(abs -5.5d0)", "5.5d0"),
        // A ratio stays a ratio and a bignum stays a bignum -- the generic
        // compare/subtract cores exist to preserve that (bliss-apr).
        ("(abs -1/2)", "1/2"),
        ("(abs 1/2)", "1/2"),
        ("(abs -100000000000000000000)", "100000000000000000000"),
        ("(type-of (abs -5))", "FIXNUM"),
        ("(type-of (abs -5.5d0))", "DOUBLE-FLOAT"),
        // ABS of a complex is its magnitude (bliss-dhrx), and the magnitude
        // takes the PARTS' float format -- this boxed as a single-float
        // unconditionally, so a double-float complex lost both precision and
        // type: |#C(-3.0d0 4.0d0)| answered 5.0 rather than 5.0d0 (ansi
        // ABS.20-21).
        ("(abs (complex 3 4))", "5.0"),
        ("(abs (complex -3 -4))", "5.0"),
        ("(abs (complex -3.0d0 4.0d0))", "5.0d0"),
        ("(type-of (abs (complex -3.0d0 4.0d0)))", "DOUBLE-FLOAT"),
        ("(type-of (abs (complex -3.0 4.0)))", "SINGLE-FLOAT"),
        // The evaluated-args path must agree with operator position.
        ("(funcall #'abs -7)", "7"),
        ("(funcall #'abs -1/2)", "1/2"),
        ("(mapcar #'abs (list -1 -2.5 -1/2))", "(1 2.5 1/2)"),
        // Non-numbers and wrong arity still signal.
        ("(handler-case (abs \"x\") (type-error () :te))", ":TE"),
        ("(handler-case (abs nil) (type-error () :te))", ":TE"),
        ("(handler-case (abs) (program-error () :pe))", ":PE"),
        ("(handler-case (abs 1 2) (program-error () :pe))", ":PE"),
        // ansi ABS.1 requires (abs x) be REAL and never MINUSP for every x.
        // |MOST-NEGATIVE-FIXNUM| is 2^60, one PAST fixnum range, and
        // `from_fixnum` shifts left without checking -- so this wrapped into
        // the sign bit and answered a NEGATIVE number. Unary minus already
        // promoted to a bignum here; ABS now does too.
        ("(minusp (abs most-negative-fixnum))", "NIL"),
        ("(realp (abs most-negative-fixnum))", "T"),
        (
            "(= (abs most-negative-fixnum) (- most-negative-fixnum))",
            "T",
        ),
        ("(abs (1+ most-negative-fixnum))", "1152921504606846975"),
        ("(abs most-positive-fixnum)", "1152921504606846975"),
        (
            "(loop for x in (list 0 1 -1 most-positive-fixnum most-negative-fixnum
                               -5.5 1/2 -1/2 -100000000000000000000)
            for a = (abs x) always (and (realp a) (not (minusp a))))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-0dtf: a ratio literal whose numerator OR denominator exceeded i64 was
/// not parsed at all -- the token fell through and came back as a SYMBOL, so
/// `(numberp <literal>)` was NIL and SYMBOLP answered T. Reducing such a
/// literal to lowest terms with a positive denominator (CLHS 2.3.2.3) needs
/// bignum GCD and exact division, which lived in the interpreter where the
/// reader could not reach them; 63bdc4c moved them to egcl-rt.
#[test]
fn ratio_literals_with_bignum_components_read_as_rationals() {
    let cases = [
        // The literal from the ansi failures: a number, not a symbol.
        (
            "(numberp (read-from-string
            \"189729874978126783786123/1234678123487612347896123467851234671234\"))",
            "T",
        ),
        (
            "(symbolp (read-from-string
            \"189729874978126783786123/1234678123487612347896123467851234671234\"))",
            "NIL",
        ),
        (
            "(rationalp (read-from-string
            \"189729874978126783786123/1234678123487612347896123467851234671234\"))",
            "T",
        ),
        (
            "(numerator (read-from-string
            \"189729874978126783786123/1234678123487612347896123467851234671234\"))",
            "189729874978126783786123",
        ),
        (
            "(denominator (read-from-string
            \"189729874978126783786123/1234678123487612347896123467851234671234\"))",
            "1234678123487612347896123467851234671234",
        ),
        // Reduced to lowest terms, collapsing to an integer when the
        // denominator reduces to 1.
        (
            "(read-from-string
            \"246913578024691357802469135780/123456789012345678901234567890\")",
            "2",
        ),
        // The sign is normalised onto the numerator.
        (
            "(read-from-string \"1/-123456789012345678901234567890\")",
            "-1/123456789012345678901234567890",
        ),
        // A zero denominator is still an error, not a symbol.
        (
            "(handler-case (read-from-string \"5/0\") (error () :err))",
            ":ERR",
        ),
        // Everything the i64 path already handled is unchanged.
        ("1/2", "1/2"),
        ("-3/4", "-3/4"),
        ("6/4", "3/2"),
        ("4/2", "2"),
        ("#x1F/2", "31/2"),
        // The 61-bit fixnum boundary cases that bliss-mwpb fixed stay fixed.
        ("1152921504606846976/7", "1152921504606846976/7"),
        ("7/1152921504606846976", "7/1152921504606846976"),
        // And such a literal is usable as a number, not just readable.
        (
            "(* 2 (read-from-string \"1/123456789012345678901234567890\"))",
            "1/61728394506172839450617283945",
        ),
    ];
    run_expression_cases(&cases);
}

/// bliss-bd9c: the transcendental functions ran their argument through
/// `num_val`, which rejects a COMPLEX -- so `(sin #C(0 1))` answered
/// "#C(0 1) is not of type number". Complex arguments now take a dedicated
/// branch for the seven functions with direct, branch-cut-free formulas.
///
/// Values are checked against the closed forms: sin(i) = i*sinh(1),
/// cos(i) = cosh(1), tan(i) = i*tanh(1), tanh(i) = i*tan(1).
#[test]
fn transcendentals_accept_complex_arguments() {
    let cases = [
        ("(realpart (exp (complex 1.0 0.0)))", "2.7182817"),
        // sin(i) = i sinh 1 = 1.1752012i
        ("(imagpart (sin (complex 0.0 1.0)))", "1.1752012"),
        ("(realpart (sin (complex 0.0 1.0)))", "0.0"),
        // cos(i) = cosh 1 = 1.5430807
        ("(realpart (cos (complex 0.0 1.0)))", "1.5430807"),
        // sinh(i) = i sin 1 ; cosh(i) = cos 1
        ("(imagpart (sinh (complex 0.0 1.0)))", "0.84147096"),
        ("(realpart (cosh (complex 0.0 1.0)))", "0.5403023"),
        // tan(i) = i tanh 1 ; tanh(i) = i tan 1
        ("(imagpart (tan (complex 0.0 1.0)))", "0.7615942"),
        ("(imagpart (tanh (complex 0.0 1.0)))", "1.5574077"),
        // A general argument: sin(1+i) = 1.2984576 + 0.6349639i
        ("(realpart (sin (complex 1.0 1.0)))", "1.2984576"),
        ("(imagpart (sin (complex 1.0 1.0)))", "0.6349639"),
        // Each returns a COMPLEX, and reals are untouched.
        ("(complexp (sin (complex 0.0 1.0)))", "T"),
        ("(sin 0)", "0.0"),
        ("(cos 0)", "1.0"),
        ("(exp 1)", "2.7182817"),
        ("(tan 0)", "0.0"),
        ("(sinh 0)", "0.0"),
        ("(cosh 0)", "1.0"),
        ("(tanh 0)", "0.0"),
        // ZEROP accepts any number, a complex included. That alone was
        // blocking SIGNUM, whose boot.lisp definition opens with
        // `(if (zerop n) n (/ n (abs n)))` -- ABS and `/` already worked.
        ("(zerop (complex 0.0 0.0))", "T"),
        ("(zerop (complex 0.0 -0.0))", "T"),
        ("(zerop (complex 1.0 1.0))", "NIL"),
        ("(signum (complex 3.0 4.0))", "#C(0.6 0.8)"),
        ("(zerop (signum (complex 0.0 0.0)))", "T"),
        ("(signum 5)", "1"),
        ("(signum -2.5)", "-1.0"),
        ("(zerop 0)", "T"),
        ("(zerop 1)", "NIL"),
        // PLUSP/MINUSP stay real-only: CLHS defines them by comparison with
        // zero, which a complex has no ordering for.
        (
            "(handler-case (plusp (complex 1.0 1.0)) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (minusp (complex 1.0 1.0)) (type-error () :te))",
            ":TE",
        ),
        // The inverse functions were a deferred follow-up when this test was
        // written -- a complex argument type-errored rather than being
        // silently mis-answered. They are implemented now (bliss-i13d), so
        // this asserts the CURRENT behaviour; the detailed closed-form and
        // round-trip checks live in
        // `inverse_transcendentals_take_complex_and_out_of_domain_arguments`.
        ("(complexp (asin (complex 1.0 1.0)))", "T"),
    ];
    run_expression_cases(&cases);
}

/// ansi EXP.ERROR.4-11 / EXPT.ERROR.4-11: CLHS 12.1.4.3 makes a float
/// operation that overflows to an infinity, or underflows to zero from a
/// nonzero exact result, signal a condition rather than return inf/0.0.
///
/// The subtle part is WHERE to test: the arithmetic runs in f64, so
/// most-positive-SINGLE-float squared is still finite and only becomes an
/// infinity when the result is narrowed to f32. Checking the f64 intermediate
/// caught every double-float case and missed every single-float one.
#[test]
fn exp_and_expt_signal_float_range_conditions() {
    let cases = [
        // Overflow, both float formats.
        (
            "(handler-case (exp (+ (log most-positive-single-float) 100))
            (floating-point-overflow () :ovf))",
            ":OVF",
        ),
        (
            "(handler-case (exp (+ (log most-positive-double-float) 100))
            (floating-point-overflow () :ovf))",
            ":OVF",
        ),
        (
            "(handler-case (expt most-positive-single-float 2)
            (floating-point-overflow () :ovf))",
            ":OVF",
        ),
        (
            "(handler-case (expt most-positive-double-float 2)
            (floating-point-overflow () :ovf))",
            ":OVF",
        ),
        // Underflow, both float formats.
        (
            "(handler-case (exp (- (log least-positive-single-float) 100))
            (floating-point-underflow () :unf))",
            ":UNF",
        ),
        (
            "(handler-case (expt least-positive-single-float 2)
            (floating-point-underflow () :unf))",
            ":UNF",
        ),
        (
            "(handler-case (expt least-positive-double-float 2)
            (floating-point-underflow () :unf))",
            ":UNF",
        ),
        // Both are ARITHMETIC-ERRORs.
        (
            "(handler-case (expt most-positive-single-float 2)
            (arithmetic-error () :ae))",
            ":AE",
        ),
        // Ordinary results are untouched, including the integer EXPT path and
        // (expt 0.0 2), whose exact result IS zero and so is not an underflow.
        ("(exp 1)", "2.7182817"),
        ("(exp 0)", "1.0"),
        ("(exp -1)", "0.36787945"),
        ("(exp 1.0d0)", "2.718281828459045d0"),
        ("(expt 2 10)", "1024"),
        ("(expt 2.0 10)", "1024.0"),
        ("(expt 0.0 2)", "0.0"),
        ("(expt 2 0.5)", "1.4142135"),
        ("(expt -8 1/3)", "#C(1.0 1.7320508)"),
    ];
    run_expression_cases(&cases);
}

/// ansi CL-CONSTANT-SYMBOLS.1: every symbol in *CL-CONSTANT-SYMBOLS* must be
/// both BOUNDP and CONSTANTP. MULTIPLE-VALUES-LIMIT and
/// INTERNAL-TIME-UNITS-PER-SECOND were seeded into their value cells by
/// cli.rs's seed_standard_constant but never marked constant, so they were
/// bound yet not CONSTANTP -- the only two the test collected.
///
/// The DEFCONSTANT literals must match the cli.rs seeds, which is what the
/// value assertions here are for.
#[test]
fn standard_limit_constants_are_constantp() {
    let cases = [
        ("(constantp 'multiple-values-limit)", "T"),
        ("(constantp 'internal-time-units-per-second)", "T"),
        ("(boundp 'multiple-values-limit)", "T"),
        ("(boundp 'internal-time-units-per-second)", "T"),
        // Values must still agree with the interpreter's seeds.
        ("internal-time-units-per-second", "1000"),
        ("(= multiple-values-limit most-positive-fixnum)", "T"),
        // The neighbouring constants are unaffected.
        ("(constantp 'call-arguments-limit)", "T"),
        ("(constantp 'lambda-parameters-limit)", "T"),
        ("(constantp 'most-positive-fixnum)", "T"),
        ("(constantp 'pi)", "T"),
        // The ansi clause itself, over the four seeded limits.
        (
            "(loop for s in '(internal-time-units-per-second multiple-values-limit
                           call-arguments-limit lambda-parameters-limit)
            when (or (not (boundp s)) (not (constantp s))) collect s)",
            "NIL",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi LOGEQV.2-4: LOGEQV was defined as a flat `(lognot (logxor ...))`, which
/// is right for TWO arguments and off by a complement for every ODD count --
/// `(logeqv 1231)` answered -1232. EQV is associative and the complement
/// cancels pairwise:
///
///   (logeqv a b)     = ~(a^b)
///   (logeqv a b c)   = ~(~(a^b) ^ c) = a^b^c
///   (logeqv a b c d) = ~(a^b^c^d)
///
/// so it is LOGXOR of everything, complemented only on an even count. The
/// two-argument case being correct is exactly why this survived; each arity is
/// pinned here rather than inferred from it.
#[test]
fn logeqv_is_correct_at_every_arity() {
    let cases = [
        ("(logeqv)", "-1"),
        ("(logeqv 1231)", "1231"),
        ("(logeqv -198)", "-198"),
        ("(logeqv 0)", "0"),
        ("(logeqv 12 10)", "-7"),
        ("(= (logeqv 12 10) (lognot (logxor 12 10)))", "T"),
        ("(= (logeqv 12 10 6) (logxor 12 10 6))", "T"),
        ("(= (logeqv 12 10 6 3) (lognot (logxor 12 10 6 3)))", "T"),
        // A bignum argument goes through the same path.
        (
            "(logeqv 123456789012345678901234567890)",
            "123456789012345678901234567890",
        ),
        // BOOLE-EQV passes two arguments and is unaffected; so are the other
        // derived log ops.
        ("(= (boole boole-eqv 12 10) (lognot (logxor 12 10)))", "T"),
        ("(lognand 12 10)", "-9"),
        ("(lognor 12 10)", "-15"),
        ("(logand 5)", "5"),
        ("(logior 5)", "5"),
        ("(logxor 5)", "5"),
    ];
    run_expression_cases(&cases);
}

/// ansi EXPT.7 and EXPT.18: `(expt x 0)` is 1 for EVERY x. That has to be
/// decided before EXPT's complex branch, which fires on any complex base and
/// on a negative real base with a non-fixnum exponent -- both reached it with a
/// zero exponent and answered a complex one.
///
/// `(expt 5 0.0)` was already correct, so only NEGATIVE bases were wrong;
/// EXPT.18 catches it because its loop runs i from -1000.
#[test]
fn expt_of_zero_exponent_is_one_for_every_base() {
    let cases = [
        // (expt x 0) is 1 OF THE RESULT TYPE, not the integer 1. These four
        // pin all of it together, because a fix that returns a blanket integer
        // 1 passes the first two and breaks the last two (it did, and ansi
        // EXPT.3/10 caught it):
        //   rational-complex base  -> integer 1  (#C(1 0) canonicalises)
        //   float base             -> 1.0
        //   double-complex base    -> #C(1.0d0 0.0d0)
        ("(eql (expt (complex 3 3) 0) 1)", "T"),
        ("(eql (expt 3.0 0) 1.0)", "T"),
        (
            "(loop for i = (random 1000.0) repeat 100 always (eql (expt i 0) 1.0))",
            "T",
        ),
        (
            "(loop for i = (random 1.0d10) for c = (complex i i) repeat 100
            always (eql (expt c 0) (complex 1.0d0 0.0d0)))",
            "T",
        ),
        ("(expt (complex 1.0 1.0) 0)", "#C(1.0 0.0)"),
        // A FLOAT zero exponent on a rational complex contagiously gives a
        // float complex -- that one was already right and must stay so.
        ("(expt (complex 3 3) 0.0)", "#C(1.0 0.0)"),
        (
            "(loop for i from -50 to 50 for c = (complex i i) always (eql (expt c 0) 1))",
            "T",
        ),
        // A float zero gives a float 1 in the EXPONENT's format, for negative
        // bases as well as positive ones.
        ("(expt -5 0.0)", "1.0"),
        ("(expt -5 0.0d0)", "1.0d0"),
        ("(expt 5 0.0)", "1.0"),
        (
            "(loop for zero in (list 0.0 0.0d0) always
            (loop for i from -50 to 50 always
              (or (zerop i) (eql (expt i zero) (float 1 zero)))))",
            "T",
        ),
        // Rational, bignum and zero bases keep the exact integer 1.
        ("(expt 5 0)", "1"),
        ("(expt 1/2 0)", "1"),
        ("(expt 123456789012345678901234567890 0)", "1"),
        ("(expt 0 0)", "1"),
        // Nonzero exponents are untouched, including the paths that legitimately
        // produce a complex result.
        ("(expt 2 10)", "1024"),
        ("(expt -2 3)", "-8"),
        ("(expt 2.0 10)", "1024.0"),
        ("(expt -8 1/3)", "#C(1.0 1.7320508)"),
        ("(expt (complex 0 1) 2)", "-1"),
    ];
    run_expression_cases(&cases);
}

/// ansi LOG.5-8 (and COSH.6/SINH.6/TANH.6): TYPEP's compound dispatch has arms
/// for OR, AND, ARRAY, INTEGER, REAL, MEMBER, SATISFIES and friends but had
/// NONE for COMPLEX, so every `(complex TYPE)` specifier fell through and
/// answered NIL.
///
/// Worth recording how this presented: LOG.5 looks like a LOG bug, and the
/// complex values LOG returns are in fact CORRECT -- `(log -0.5)` is
/// #C(-0.6931 3.1416) with single-float parts. The test's real assertion is
/// `(typep y '(complex single-float))`, which is what was broken.
#[test]
fn typep_handles_compound_complex_specifiers() {
    let cases = [
        ("(typep (log -0.5) '(complex single-float))", "T"),
        ("(typep (log -0.5d0) '(complex double-float))", "T"),
        // ... and rejects the wrong part type.
        ("(typep (log -0.5) '(complex double-float))", "NIL"),
        ("(typep (log -0.5d0) '(complex single-float))", "NIL"),
        // A bare COMPLEX, and the `*` / no-argument forms, mean any complex.
        ("(typep (complex 1 2) 'complex)", "T"),
        ("(typep (complex 1 2) '(complex))", "T"),
        ("(typep (complex 1 2) '(complex *))", "T"),
        // Rational parts.
        ("(typep (complex 1 2) '(complex integer))", "T"),
        ("(typep (complex 1 2) '(complex float))", "NIL"),
        ("(typep (complex 1.0 2.0) '(complex float))", "T"),
        // Non-complex objects never match, whatever the part spec.
        ("(typep 5 '(complex integer))", "NIL"),
        ("(typep 5.0 '(complex single-float))", "NIL"),
        ("(typep \"x\" '(complex t))", "NIL"),
        // The ansi shape itself: LOG of a negative float is ONE value, of type
        // (complex <that float format>).
        (
            "(let* ((x -0.5) (r (multiple-value-list (log x))))
            (and (null (cdr r)) (typep (car r) '(complex single-float))))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi PLUS.3 / MINUS.3: `#C(a b)` denotes `(complex a b)` (CLHS 2.4.8.11),
/// but the READER applied neither of COMPLEX's rules, so a literal read
/// differently from the same value constructed by `(complex a b)`. ansi seeds
/// *NUMBERS* from such literals, so `(eql x (+ x 0))` failed for them -- the
/// addition normalised what the reader had not.
///
/// Both rules are pinned here: float contagion to the widest format, and
/// canonicalisation of a RATIONAL complex with a zero imaginary part. A FLOAT
/// zero must NOT canonicalise.
#[test]
fn complex_literals_read_the_same_as_the_complex_function() {
    let cases = [
        // Float contagion, including the double/bignum/ratio parts that a
        // fixnum-and-single-float-only coercion would have skipped.
        ("(read-from-string \"#C(1.0 3.0d0)\")", "#C(1.0d0 3.0d0)"),
        ("(read-from-string \"#C(1.0d0 3.0)\")", "#C(1.0d0 3.0d0)"),
        ("(read-from-string \"#C(1 2.0)\")", "#C(1.0 2.0)"),
        ("(read-from-string \"#C(2.0 1)\")", "#C(2.0 1.0)"),
        ("(read-from-string \"#C(1/2 3.0)\")", "#C(0.5 3.0)"),
        // Canonicalisation of a rational complex with a zero imaginary part.
        ("(read-from-string \"#C(1 0)\")", "1"),
        ("(read-from-string \"#C(1/2 0)\")", "1/2"),
        (
            "(read-from-string \"#C(123456789012345678901234567890 0)\")",
            "123456789012345678901234567890",
        ),
        // ... but a FLOAT zero imaginary part stays complex.
        ("(read-from-string \"#C(1.0 0.0)\")", "#C(1.0 0.0)"),
        // An all-rational complex is untouched.
        ("(read-from-string \"#C(1 2)\")", "#C(1 2)"),
        // The literal and the constructor must agree, which is the property
        // the ansi tests actually rely on.
        (
            "(let ((r (read-from-string \"#C(1.0 3.0d0)\")))
            (eql r (complex (realpart r) (imagpart r))))",
            "T",
        ),
        // ... and identity under adding zero, which is PLUS.3's assertion.
        (
            "(loop for x in (list (read-from-string \"#C(1.0 3.0d0)\")
                               (read-from-string \"#C(1/2 3.0)\")
                               (read-from-string \"#C(1 2)\") 1 1.5 1/2)
            always (and (eql x (+ x 0)) (eql x (+ 0 x))))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi GCD.2-3 / LCM.2-3 / COMPLEX.ERROR.1-2.
///
/// GCD and LCM folded with REDUCE and no :initial-value. REDUCE hands back a
/// lone element UNTOUCHED -- it never calls the function -- so the `(abs a)`
/// inside never ran for a ONE-argument call and `(gcd -12)` answered -12.
/// Seeding with each operation's identity (0 for GCD, since gcd(0,x) = |x|;
/// 1 for LCM) puts every element through the function, including the only one.
/// This is the same single-element fold shape as the LOGEQV bug.
///
/// COMPLEX with a wrong argument COUNT reported "NIL is not of type real", a
/// TYPE-ERROR where CLHS 3.5.1 requires a PROGRAM-ERROR.
#[test]
fn gcd_lcm_one_argument_and_complex_arity() {
    let cases = [
        // The one-argument case is |x|, for either sign.
        ("(gcd -12)", "12"),
        ("(gcd 12)", "12"),
        ("(lcm -12)", "12"),
        ("(lcm 12)", "12"),
        // Identities and the two-argument cases are unchanged.
        ("(gcd)", "0"),
        ("(lcm)", "1"),
        ("(gcd 0 -12)", "12"),
        ("(lcm 1 -12)", "12"),
        ("(gcd 12 18)", "6"),
        ("(gcd -12 18)", "6"),
        ("(gcd 12 -18)", "6"),
        ("(lcm 4 6)", "12"),
        ("(lcm -4 6)", "12"),
        ("(lcm 0 5)", "0"),
        // Three arguments, and bignums.
        ("(gcd 12 18 24)", "6"),
        ("(lcm 4 6 10)", "60"),
        ("(gcd (- (expt 2 70)) (expt 2 68))", "295147905179352825856"),
        // COMPLEX: wrong COUNT is a PROGRAM-ERROR ...
        ("(handler-case (complex) (program-error () :pe))", ":PE"),
        (
            "(handler-case (complex 1 2 3) (program-error () :pe))",
            ":PE",
        ),
        // ... while a wrong argument TYPE stays a TYPE-ERROR.
        ("(handler-case (complex \"x\") (type-error () :te))", ":TE"),
        // Valid calls are untouched, including the canonicalising one.
        ("(complex 1 2)", "#C(1 2)"),
        ("(complex 3)", "3"),
        ("(complex 1.0 2)", "#C(1.0 2.0)"),
        ("(complex 1 0)", "1"),
    ];
    run_expression_cases(&cases);
}

/// ansi ASH.ERROR.4-5, ISQRT.ERROR.5, BOOLE.ERROR.5-7, RANDOM.ERROR.1-3: four
/// builtins signalling the wrong condition, or none at all.
///
/// ASH and ISQRT had NO type check on their early-exit paths -- ASH's
/// `(zerop count)` branch and ISQRT's `(< n 2)` branch each returned the
/// argument unchanged, so `(ash " " 0)` answered " " and `(isqrt 1.2)`
/// answered 1.2. BOOLE signalled a SIMPLE-ERROR for an unrecognised op.
/// RANDOM had its two error kinds SWAPPED: a missing argument gave a
/// TYPE-ERROR and a non-positive limit gave a PROGRAM-ERROR, where CLHS 3.5.1
/// wants a PROGRAM-ERROR for a wrong argument COUNT and a TYPE-ERROR for a
/// limit outside (REAL (0)).
#[test]
fn integer_builtins_signal_the_right_conditions() {
    let cases = [
        // ASH: both arguments must be integers, including on the count-0 path.
        ("(handler-case (ash \" \" 0) (type-error () :te))", ":TE"),
        ("(handler-case (ash 1.5 0) (type-error () :te))", ":TE"),
        ("(handler-case (ash nil 0) (type-error () :te))", ":TE"),
        ("(handler-case (ash 1 1.5) (type-error () :te))", ":TE"),
        // ISQRT: a NON-NEGATIVE INTEGER; a negative one is a TYPE-ERROR too,
        // not the bare SIMPLE-ERROR it used to signal.
        ("(handler-case (isqrt 1.2) (type-error () :te))", ":TE"),
        ("(handler-case (isqrt 3/5) (type-error () :te))", ":TE"),
        ("(handler-case (isqrt -1) (type-error () :te))", ":TE"),
        // BOOLE: an op outside the constants is a TYPE-ERROR.
        ("(handler-case (boole nil 1 2) (type-error () :te))", ":TE"),
        ("(handler-case (boole 99 1 2) (type-error () :te))", ":TE"),
        // ... and both OPERANDS must be integers even for the ops that IGNORE
        // one of them. BOOLE-1/2/C2/CLR/SET returned without touching the
        // argument, so `(boole boole-1 nil 1)` answered NIL (ansi
        // BOOLE.ERROR.6-7 collect exactly those five names).
        (
            "(handler-case (boole boole-1 nil 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (boole boole-2 1 nil) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (boole boole-clr nil 1) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (boole boole-set 1 nil) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (boole boole-c2 nil 1) (type-error () :te))",
            ":TE",
        ),
        ("(boole boole-1 12 10)", "12"),
        ("(boole boole-clr 12 10)", "0"),
        ("(boole boole-set 12 10)", "-1"),
        // MAKE-RANDOM-STATE: the argument must be NIL, T or a RANDOM-STATE; it
        // was ignored entirely (ansi MAKE-RANDOM-STATE.ERROR.4).
        (
            "(handler-case (make-random-state 0) (type-error () :te))",
            ":TE",
        ),
        (
            "(handler-case (make-random-state \"x\") (type-error () :te))",
            ":TE",
        ),
        ("(random-state-p (make-random-state))", "T"),
        // TYPEP had NO arm for RANDOM-STATE, so it answered NIL for egcl's own
        // random state while RANDOM-STATE-P answered T -- a predicate
        // disagreeing with the type it tests. ansi MAKE-RANDOM-STATE.ERROR.4
        // sees the mismatch: its TYPEF (which uses TYPEP) says the value is not
        // a RANDOM-STATE and expects a rejection, while MAKE-RANDOM-STATE
        // accepts it via RANDOM-STATE-P.
        ("(typep *random-state* 'random-state)", "T"),
        (
            "(let ((r (make-random-state)))
            (and (typep r 'random-state) (random-state-p r)))",
            "T",
        ),
        ("(typep 0 'random-state)", "NIL"),
        ("(typep \"x\" 'random-state)", "NIL"),
        ("(typep (list 1 2) 'random-state)", "NIL"),
        ("(typep nil 'random-state)", "NIL"),
        ("(random-state-p (make-random-state t))", "T"),
        ("(random-state-p (make-random-state *random-state*))", "T"),
        // RANDOM: count vs type, no longer swapped.
        ("(handler-case (random) (program-error () :pe))", ":PE"),
        (
            "(handler-case (random 10 *random-state* 3) (program-error () :pe))",
            ":PE",
        ),
        ("(handler-case (random -5) (type-error () :te))", ":TE"),
        ("(handler-case (random 0) (type-error () :te))", ":TE"),
        ("(handler-case (random 0.0) (type-error () :te))", ":TE"),
        // Every ordinary call is unchanged.
        ("(ash 1 4)", "16"),
        ("(ash 16 -2)", "4"),
        ("(ash 0 0)", "0"),
        ("(ash -8 -1)", "-4"),
        ("(isqrt 16)", "4"),
        ("(isqrt 0)", "0"),
        ("(isqrt (expt 10 40))", "100000000000000000000"),
        ("(boole boole-and 12 10)", "8"),
        ("(boole boole-ior 12 10)", "14"),
        (
            "(let ((v (random 10))) (and (integerp v) (>= v 0) (< v 10)))",
            "T",
        ),
        ("(floatp (random 10.0))", "T"),
    ];
    run_expression_cases(&cases);
}

/// ansi LOG.7-8: `(log <complex>)` type-errored -- `num_val` rejects a complex,
/// so only log of a NEGATIVE REAL ever produced one. ln z = ln|z| + i*arg z,
/// with arg the PRINCIPAL value in (-pi, pi], which is what atan2 gives.
///
/// This is also the prerequisite for the inverse transcendentals (bliss-i13d):
/// asin/acos/atan/asinh/acosh/atanh are all expressed through complex log and
/// sqrt, and their branch cuts come from arg's principal range.
#[test]
fn log_accepts_complex_arguments() {
    let cases = [
        // ln(i) = i*pi/2 ; ln(-i) = -i*pi/2 -- the principal-value SIGN matters.
        ("(realpart (log (complex 0.0 1.0)))", "0.0"),
        ("(imagpart (log (complex 0.0 1.0)))", "1.5707964"),
        ("(imagpart (log (complex 0.0 -1.0)))", "-1.5707964"),
        // ln(1+i) = ln(sqrt 2) + i*pi/4
        ("(realpart (log (complex 1.0 1.0)))", "0.3465736"),
        ("(imagpart (log (complex 1.0 1.0)))", "0.7853982"),
        // ln(-1+0i) = i*pi, agreeing with the negative-real branch.
        ("(imagpart (log (complex -1.0 0.0)))", "3.1415927"),
        ("(realpart (log (complex 2.0 0.0)))", "0.6931472"),
        // Round-trip: exp(log z) = z.
        ("(exp (log (complex 1.0 1.0)))", "#C(1.0 1.0)"),
        // Reals are untouched, including the negative-real complex result and
        // the two-argument base form.
        ("(log 1.0)", "0.0"),
        ("(log -1.0)", "#C(0.0 3.1415927)"),
        ("(log 8 2)", "3.0"),
        ("(log 100 10)", "2.0"),
        ("(complexp (log (complex 1.0 1.0)))", "T"),
    ];
    run_expression_cases(&cases);
}

/// bliss-i13d: the six INVERSE transcendentals accepted neither a complex
/// argument nor a real one outside their real domain -- `(asin 2)` answered
/// NaN and `(asin #C(0 1))` type-errored.
///
/// Implemented from CLHS 12.1.5.3's formulas VERBATIM rather than
/// algebraically simplified equivalents: the standard states them in the form
/// that places the principal-value branch cuts correctly, and a rearrangement
/// that is valid as pure algebra can move a cut and yield plausible WRONG
/// numbers with nothing to signal.
///
/// The out-of-real-domain case is not hand-derived per function either: when
/// the real kernel returns NaN from a finite input, the same complex formula
/// is used. That avoids six separate domain conditions, each a chance to get a
/// sign wrong.
#[test]
fn inverse_transcendentals_take_complex_and_out_of_domain_arguments() {
    let cases = [
        // Real arguments outside the real domain -> complex principal values.
        ("(asin 2.0)", "#C(1.5707964 -1.316958)"),
        ("(acos 2.0)", "#C(0.0 1.316958)"),
        ("(acosh 0.0)", "#C(0.0 1.5707964)"),
        ("(atanh 2.0)", "#C(0.54930615 -1.5707964)"),
        ("(asin -2.0)", "#C(-1.5707964 1.316958)"),
        // Complex arguments, against closed forms:
        // asin(i) = i*asinh(1), asinh(i) = i*pi/2, atanh(i) = i*pi/4.
        ("(asin (complex 0.0 1.0))", "#C(0.0 0.8813736)"),
        ("(asinh (complex 0.0 1.0))", "#C(0.0 1.5707964)"),
        ("(atanh (complex 0.0 1.0))", "#C(0.0 0.7853982)"),
        ("(atan (complex 0.0 0.5))", "#C(0.0 0.54930615)"),
        // ATAN has its OWN arm (for the two-argument atan2 form) and so needs
        // its own complex branch -- it bypassed the shared one entirely.
        ("(complexp (atan (complex 1.0 1.0)))", "T"),
        ("(atan 1.0 1.0)", "0.7853982"),
        ("(atan 0.0 -1.0)", "3.1415927"),
        // Round-trip identities -- independent of how the formulas were
        // transcribed, so these are the real check.
        (
            "(sin (asin (complex 0.5 0.5)))",
            "#C(0.49999997 0.49999997)",
        ),
        ("(cosh (acosh (complex 2.0 1.0)))", "#C(2.0 1.0)"),
        // In-domain reals are untouched.
        ("(asin 0.5)", "0.5235988"),
        ("(acos 0.5)", "1.0471976"),
        ("(atan 1.0)", "0.7853982"),
        ("(asinh 1.0)", "0.8813736"),
        ("(acosh 2.0)", "1.316958"),
        ("(atanh 0.5)", "0.54930615"),
        // Every result is a complex of the argument's float format.
        ("(typep (asin 2.0) '(complex single-float))", "T"),
        ("(typep (atanh 2.0) '(complex single-float))", "T"),
    ];
    run_expression_cases(&cases);
}

/// ansi ARITHMETIC-ERROR.3 and the four ARITHMETIC-ERROR-OPERATION/OPERANDS
/// arity tests: the ARITHMETIC-ERROR condition already HAD its OPERATION and
/// OPERANDS slots (see cli.rs's condition table); only the two CLHS readers
/// were missing, so calling either was an UNDEFINED-FUNCTION even on a
/// condition explicitly built with :operation and :operands.
///
/// The slots carry no initform, so the readers guard with SLOT-BOUNDP: egcl's
/// internal arithmetic signallers do not record the operation or operands yet,
/// and reading an unbound slot would raise inside a handler rather than answer.
#[test]
fn arithmetic_error_readers() {
    let cases = [
        // Values supplied at construction round-trip, which is what
        // ARITHMETIC-ERROR.3 checks.
        (
            "(let ((a (make-condition 'arithmetic-error
                     :operation '/ :operands '(0 0))))
            (list (arithmetic-error-operation a) (arithmetic-error-operands a)))",
            "(/ (0 0))",
        ),
        // A SIGNALLED condition reports NIL rather than erroring on an unbound
        // slot -- egcl does not record them yet.
        (
            "(handler-case (/ 1 0)
            (division-by-zero (e) (list (arithmetic-error-operation e)
                                        (arithmetic-error-operands e))))",
            "(NIL NIL)",
        ),
        (
            "(handler-case (exp 1000.0)
            (floating-point-overflow (e) (arithmetic-error-operation e)))",
            "NIL",
        ),
        // Wrong argument count is a PROGRAM-ERROR.
        (
            "(handler-case (arithmetic-error-operation) (program-error () :pe))",
            ":PE",
        ),
        (
            "(handler-case (arithmetic-error-operands) (program-error () :pe))",
            ":PE",
        ),
        // The condition hierarchy is unchanged.
        ("(subtypep 'division-by-zero 'arithmetic-error)", "T"),
        ("(subtypep 'floating-point-overflow 'arithmetic-error)", "T"),
    ];
    run_expression_cases(&cases);
}

/// ansi TANH.3 / SINH.3: `(coerce x '(complex TYPE))` must give a complex whose
/// PARTS are of TYPE. The imaginary part was a fixnum 0 regardless of the spec,
/// so for a RATIONAL value the result canonicalised straight back to the real:
/// `(coerce 0 '(complex single-float))` answered the integer 0 rather than
/// #C(0.0 0.0). Those tests build their complex zero exactly that way and then
/// compare against it, so the zero being an integer broke the comparison.
#[test]
fn coerce_to_complex_honours_the_part_type() {
    let cases = [
        ("(coerce 0 '(complex single-float))", "#C(0.0 0.0)"),
        ("(coerce 0 '(complex double-float))", "#C(0.0d0 0.0d0)"),
        ("(coerce 5 '(complex single-float))", "#C(5.0 0.0)"),
        ("(coerce 1/2 '(complex single-float))", "#C(0.5 0.0)"),
        (
            "(typep (coerce 0 '(complex single-float)) '(complex single-float))",
            "T",
        ),
        (
            "(type-of (realpart (coerce 0 '(complex double-float))))",
            "DOUBLE-FLOAT",
        ),
        // A bare (complex) or (complex *) specifies no part type, so a rational
        // stays rational and canonicalises -- unchanged behaviour.
        ("(coerce 0 'complex)", "0"),
        ("(coerce 0 '(complex *))", "0"),
        // An argument that is already complex passes through.
        (
            "(coerce (complex 1.0 2.0) '(complex single-float))",
            "#C(1.0 2.0)",
        ),
        // Ordinary coercions are untouched.
        ("(coerce 0 'single-float)", "0.0"),
        ("(coerce 1/2 'double-float)", "0.5d0"),
        ("(coerce 5 'float)", "5.0"),
        (
            "(handler-case (coerce \"x\" 'complex) (type-error () :te))",
            ":TE",
        ),
    ];
    run_expression_cases(&cases);
}

/// ansi BIGNUM.FLOAT.COMPARE.7-8: FLOOR/CEILING/TRUNCATE/ROUND of a float at or
/// beyond the 61-bit fixnum range answered silently NEGATIVE integers. The
/// float path went through `as i64` (which SATURATES) and then `from_fixnum`
/// (which WRAPS), with no range check:
///
///     (floor 2^60)                       => -1152921504606846976
///     (floor 2^70)                       => -1
///     (floor most-positive-single-float) => -1
///
/// Those tests take `(floor x)` of every real and compare against it, so the
/// comparisons were against garbage. Same class as the `(abs
/// most-negative-fixnum)` wrap.
///
/// Two subtleties, both found by testing the boundary rather than a value well
/// past it:
///   * the bound must be 2^60, not `FIXNUM_MAX as f64` -- FIXNUM_MAX is 2^60-1,
///     which needs 61 mantissa bits and ROUNDS UP to exactly 2^60 in f64, so a
///     `>` test against it was false at the boundary and 2^60 still wrapped;
///   * `round_half_even` had its own `as i64`, so ROUND still saturated (to
///     2^63) even once the caller promoted large quotients correctly.
#[test]
fn float_rounding_promotes_past_the_fixnum_range() {
    let cases = [
        // At the boundary and beyond, the exact integer.
        ("(floor (float (expt 2 60) 1.0d0))", "1152921504606846976"),
        (
            "(floor (float (expt 2 70) 1.0d0))",
            "1180591620717411303424",
        ),
        (
            "(ceiling (float (expt 2 70) 1.0d0))",
            "1180591620717411303424",
        ),
        (
            "(truncate (float (expt 2 70) 1.0d0))",
            "1180591620717411303424",
        ),
        (
            "(round (float (expt 2 70) 1.0d0))",
            "1180591620717411303424",
        ),
        // Agreement with RATIONAL, which was always exact -- this is what the
        // ansi comparison tests actually rely on.
        (
            "(= (floor most-positive-single-float) (rational most-positive-single-float))",
            "T",
        ),
        (
            "(= (floor most-negative-single-float) (rational most-negative-single-float))",
            "T",
        ),
        ("(plusp (floor most-positive-single-float))", "T"),
        ("(minusp (floor most-negative-single-float))", "T"),
        // Ordinary rounding is untouched, ties-to-even included.
        ("(floor 3.7)", "3"),
        ("(floor -3.7)", "-4"),
        ("(ceiling 3.2)", "4"),
        ("(truncate -3.7)", "-3"),
        ("(round 2.5)", "2"),
        ("(round 3.5)", "4"),
        ("(round -2.5)", "-2"),
        // Two-argument and exact-rational forms are untouched.
        ("(floor 7 2)", "3"),
        ("(floor 7.0 2)", "3"),
        ("(round 7 2)", "4"),
        ("(floor 2/3)", "0"),
        ("(mod -7 3)", "2"),
        ("(rem -7 3)", "-1"),
    ];
    run_expression_cases(&cases);
}

/// ansi RATIONAL.*.RANDOM.COMPARE and BIGNUM.*.RANDOM.COMPARE: an exponent
/// marker straight after a bare decimal point was rejected, so the whole token
/// became a SYMBOL -- `1.s0` read as |1.S0| and the tests died with "The
/// variable |1.S0| is unbound" while writing their bounds.
///
/// CLHS 2.3.1 admits `[sign] {digit}+ [decimal-point {digit}*] exponent`, i.e.
/// ZERO fractional digits before the marker. The parser required the character
/// immediately before the marker to be a digit, which consuming the decimal
/// point had cleared; requiring only that SOME digit has appeared still
/// rejects `.s0` and a bare `e0`, which is what that guard was for.
#[test]
fn exponent_marker_after_a_bare_decimal_point() {
    let cases = [
        // Every marker, with no fractional digits. s/f force single, d/l
        // double, e the default format.
        ("(read-from-string \"1.s0\")", "1.0"),
        ("(read-from-string \"1.S0\")", "1.0"),
        ("(read-from-string \"1.f0\")", "1.0"),
        ("(read-from-string \"1.e0\")", "1.0"),
        ("(read-from-string \"1.d0\")", "1.0d0"),
        ("(read-from-string \"1.l0\")", "1.0d0"),
        ("(read-from-string \"-2.e3\")", "-2000.0"),
        // Forms that already worked stay working.
        ("(read-from-string \"1s0\")", "1.0"),
        ("(read-from-string \"1.0s0\")", "1.0"),
        ("(read-from-string \"1.5d0\")", "1.5d0"),
        // ... and non-floats are still symbols, which is what the guard
        // protected: no digit before the marker at all.
        ("(symbolp (read-from-string \".s0\"))", "T"),
        ("(symbolp (read-from-string \"e0\"))", "T"),
        ("(symbolp (read-from-string \"abc\"))", "T"),
        ("(symbolp (read-from-string \"A.S0\"))", "T"),
        ("(symbolp (read-from-string \"1.2.3\"))", "T"),
    ];
    run_expression_cases(&cases);
}

/// ansi IMAGPART.4 and EXPT.29.
///
/// (imagpart real) is (* 0 real), so the zero takes the argument's FORMAT. The
/// double-float case fell into the rational branch and answered the integer 0
/// rather than 0.0d0 -- single-float was already handled, which is why it went
/// unnoticed.
///
/// EXPT reached its complex path for a complex BASE but not a complex
/// EXPONENT, so `num_val` rejected the exponent and `(expt 2.0 #C(2 2))`
/// type-errored.
#[test]
fn imagpart_zero_format_and_complex_exponent() {
    let cases = [
        // The zero matches (* 0 x) for every real format -- IMAGPART.4's
        // actual assertion.
        ("(imagpart 1.0d0)", "0.0d0"),
        ("(imagpart 1.0)", "0.0"),
        // (imagpart real) is LITERALLY (* 0 real), SIGN included: the zero of a
        // negative float is -0.0, which no hand-built zero gives. Routing
        // through the multiply kernel gets format, sign and the
        // rational/float distinction together.
        ("(imagpart -1.0)", "-0.0"),
        ("(imagpart -1.0d0)", "-0.0d0"),
        (
            "(loop for x in (list -3.4028235e38 -1.0e-45 -1.0 1.0 -1.0d0 1.0d0 5 -1/2 0.0)
            unless (eql (* 0 x) (imagpart x)) collect x)",
            "NIL",
        ),
        ("(imagpart 5)", "0"),
        ("(imagpart 1/2)", "0"),
        (
            "(loop for x in (list 3.141592653589793d0 1.0 5 1/2 1.0d0)
            always (eql (* 0 x) (imagpart x)))",
            "T",
        ),
        // REALPART and the complex case are unchanged.
        ("(realpart 1.0d0)", "1.0d0"),
        ("(imagpart (complex 1 2))", "2"),
        // A complex EXPONENT: 2^(2+2i) = e^((2+2i) ln 2).
        ("(expt 2.0 (complex 2 2))", "#C(0.7338279 3.932111)"),
        ("(complexp (expt (complex 1 1) (complex 2 2)))", "T"),
        // ansi EXPT.29 asserts `(eql (* x y) (expt x y))` for every zero base
        // and every exponent with a positive real part -- so (expt 0 y) simply
        // IS (* x y), which makes the type follow contagion for free. The
        // integer 0 stays an integer (because #C(0 0) canonicalises) while a
        // float or complex zero keeps its own format. "Zero" has to include a
        // COMPLEX zero, or those bases fall through to the float path.
        ("(expt 0 (complex 2 2))", "0"),
        ("(expt 0.0 (complex 2 2))", "#C(0.0 0.0)"),
        (
            "(loop for x in (list 0 0.0 0.0d0 (complex 0.0 0.0) (complex 0.0d0 0.0d0))
            append (loop for y in (list 2 2.0 2.0d0 (complex 2 2) (complex 2 1/2)
                                        (complex 2.0 0.5) (complex 2.0d0 0.5d0))
                     unless (eql (* x y) (expt x y)) collect (list x y)))",
            "NIL",
        ),
        // A zero EXPONENT still wins over the zero-base rule, and a negative
        // exponent on a zero base still divides by zero.
        ("(expt 0 0)", "1"),
        ("(expt 0 0.0)", "1.0"),
        (
            "(handler-case (expt 0 -1) (division-by-zero () :div0))",
            ":DIV0",
        ),
        // Ordinary EXPT is untouched.
        ("(expt 2 10)", "1024"),
        ("(expt 2.0 0.5)", "1.4142135"),
        ("(expt -8 1/3)", "#C(1.0 1.7320508)"),
        ("(expt 5 0)", "1"),
    ];
    run_expression_cases(&cases);
}

/// ansi REAL.1-4: a numeric type's bounds were compared in f64. Beyond 2^53 an
/// f64 cannot represent consecutive integers -- 2^53 and 2^53+1 are the SAME
/// double -- so the comparison could not separate them:
///
///     (typep (1+ 2^53) '(real 0 2^53))     => T    wrong
///     (typep (1+ 2^53) '(integer 0 2^53))  => NIL  correct
///     (= (1+ 2^53) 2^53)                   => NIL  correct
///
/// `=` and the INTEGER path were right because they compare exactly; only this
/// membership test round-tripped through f64. It now uses `numeric_cmp`, the
/// same comparison `=` and `<` use, which converts a float to the rational it
/// denotes (CLHS 12.1.4.1) rather than the reverse.
///
/// SBCL reaches the same answers structurally instead: it translates
/// (REAL lo hi) into (OR (FLOAT lo hi) (RATIONAL lo hi)) at parse time, so an
/// integer is tested against an exact rational bound.
#[test]
fn numeric_type_bounds_compare_exactly() {
    let cases = [
        // The 2^53 boundary and beyond, including a large bignum bound.
        (
            "(typep (1+ 9007199254740992) '(real 0 9007199254740992))",
            "NIL",
        ),
        ("(typep 9007199254740992 '(real 0 9007199254740992))", "T"),
        (
            "(let ((m (expt 2 60))) (typep (1+ m) (list 'real 0 m)))",
            "NIL",
        ),
        (
            "(let ((m (expt 10 30))) (typep (1+ m) (list 'real 0 m)))",
            "NIL",
        ),
        // REAL.1's own loop: 200 doublings, every clause.
        (
            "(loop for i = 1 then (ash i 1) for tp = (list 'real 0 i) repeat 200
            unless (and (not (typep -1 tp)) (not (typep -0.0001 tp)) (typep 0 tp)
                        (typep 0.0001 tp) (typep 1 tp) (typep i tp)
                        (not (typep (1+ i) tp)))
            collect i)",
            "NIL",
        ),
        // Float, ratio and mixed bounds still behave.
        ("(typep 0.5 '(real 0.0 1.0))", "T"),
        ("(typep 1.5 '(real 0.0 1.0))", "NIL"),
        ("(typep 1/2 '(real 0 1))", "T"),
        ("(typep 1/2 '(real 0 1/3))", "NIL"),
        // Exclusive bounds, written (n).
        ("(typep 10 (list 'real 0 (list 10)))", "NIL"),
        ("(typep 9 (list 'real 0 (list 10)))", "T"),
        ("(typep 0 (list 'real (list 0) 10))", "NIL"),
        // Every numeric head keeps its own membership rule.
        ("(typep 5 '(integer 0 10))", "T"),
        ("(typep 5.0 '(integer 0 10))", "NIL"),
        ("(typep 1/2 '(rational 0 1))", "T"),
        ("(typep 0.5 '(rational 0 1))", "NIL"),
        ("(typep 0.5 '(float 0.0 1.0))", "T"),
        ("(typep 0.5d0 '(double-float 0.0d0 1.0d0))", "T"),
        ("(typep 0.5 '(single-float 0.0 1.0))", "T"),
        // `*` bounds and the bare head.
        ("(typep 5 '(real 0 *))", "T"),
        ("(typep -5 '(real 0 *))", "NIL"),
        ("(typep 5 'real)", "T"),
    ];
    run_expression_cases(&cases);
}

/// EGCL must be able to SET an environment variable, not just read one
/// (bliss-po88). `EGCL-EXT:GETENV` existed from the start, but nothing could
/// write, so ASDF's `(defsetf getenv …)` table fell through to
/// `(not-implemented-error '(setf getenv))` on EGCL — and with it every
/// UIOP/ASDF path that configures a child process or a library through the
/// environment. Found through pure-tls, whose trust-store tests pin
/// SSL_CERT_FILE around each case.
///
/// `EGCL-EXT:SETENV` takes POSIX setenv's shape (name, value, optional
/// overwrite, as `sb-posix:setenv` has) so the `#+egcl` arm of that table is a
/// one-liner, and returns the value so `(setf (uiop:getenv …) v)` yields `v` the
/// way SETF requires. It mutates the real process environment via std rather
/// than a EGCL-private table, so a child process and FFI code see the change;
/// std's unix shim holds an RwLock across getenv/setenv/unsetenv, so EGCL's own
/// concurrent GETENV can never read a freed value.
///
/// The name checks matter because `std::env::set_var` *panics* on an empty name,
/// a name containing `=`, or an embedded NUL — a Lisp-level mistake must not
/// take the process down.
#[test]
fn environment_variables_can_be_set_and_unset() {
    let cases = [
        // Round trip: set, then read it back.
        (
            "(progn (egcl-ext:setenv \"EGCL_PO88\" \"hello\")
               (egcl-ext:getenv \"EGCL_PO88\"))",
            "\"hello\"",
        ),
        // SETENV returns the value it stored (what SETF must yield).
        ("(egcl-ext:setenv \"EGCL_PO88\" \"v\")", "\"v\""),
        // UNSETENV removes it; GETENV then answers NIL, not "".
        (
            "(progn (egcl-ext:setenv \"EGCL_PO88\" \"v\")
               (egcl-ext:unsetenv \"EGCL_PO88\")
               (egcl-ext:getenv \"EGCL_PO88\"))",
            "NIL",
        ),
        // Unsetting a variable that was never set is not an error (POSIX).
        ("(egcl-ext:unsetenv \"EGCL_PO88_NEVER_SET\")", "NIL"),
        // A false OVERWRITE keeps an existing value ...
        (
            "(progn (egcl-ext:setenv \"EGCL_PO88\" \"first\")
               (egcl-ext:setenv \"EGCL_PO88\" \"second\" nil)
               (egcl-ext:getenv \"EGCL_PO88\"))",
            "\"first\"",
        ),
        // ... but still creates an absent one.
        (
            "(progn (egcl-ext:setenv \"EGCL_PO88\" \"only\" nil)
               (egcl-ext:getenv \"EGCL_PO88\"))",
            "\"only\"",
        ),
        // Overwriting is the default, and a later set wins.
        (
            "(progn (egcl-ext:setenv \"EGCL_PO88\" \"first\")
               (egcl-ext:setenv \"EGCL_PO88\" \"second\")
               (egcl-ext:getenv \"EGCL_PO88\"))",
            "\"second\"",
        ),
        // The three names std would panic on are program errors instead.
        (
            "(handler-case (egcl-ext:setenv \"\" \"v\")
               (program-error () :program-error))",
            ":PROGRAM-ERROR",
        ),
        (
            "(handler-case (egcl-ext:setenv \"EGCL=PO88\" \"v\")
               (program-error () :program-error))",
            ":PROGRAM-ERROR",
        ),
        (
            "(handler-case (egcl-ext:unsetenv \"\")
               (program-error () :program-error))",
            ":PROGRAM-ERROR",
        ),
        // The error carries a message, not just a class name (bliss-kk0i).
        (
            "(handler-case (egcl-ext:setenv \"\" \"v\")
               (program-error (e) (and (search \"may not be empty\"
                                               (princ-to-string e))
                                       t)))",
            "T",
        ),
    ];
    run_expression_cases(&cases);
}

/// SETF through an accessor name shared by two classes must store into the slot
/// of the INSTANCE's class, not into whichever class the accessor map happened
/// to yield first (bliss-i6ga1).
///
/// The mapping used by both the tree-walker's store path and the lowered
/// `EGCL::SET-ACCESSOR-SLOT` primitive searched `env.classes` by accessor NAME
/// alone and returned the first match. Giving one accessor name to differently
/// named slots on two classes is ordinary CLOS style, and it broke two ways:
/// a crash when the foreign slot was absent from the instance's layout, and —
/// worse — a silent store into the wrong slot when it was present.
///
/// Both cases run here, and each runs twice: the harness evaluates every case
/// compiled and again tree-walked, which is what this needs, since the two
/// tiers deliberately share the mapping and so shared the bug.
#[test]
fn setf_through_a_shared_accessor_name_writes_the_instances_own_slot() {
    run_expression_cases(&[
        // The foreign slot is ABSENT from the instance's layout: this used to
        // fail outright with "slot not present in class layout: XSLOT".
        (
            "(progn (defclass sa-a () ((xslot :initform 'a-init :accessor sa-sh)))
                    (defclass sa-b () ((yslot :initform 'b-init :accessor sa-sh)))
                    (let ((o (make-instance 'sa-b)))
                      (setf (sa-sh o) 'stored)
                      (list (slot-value o 'yslot) (sa-sh o))))",
            "(STORED STORED)",
        ),
        // The foreign slot is PRESENT, so a wrong store cannot crash. This is
        // the dangerous shape: it silently wrote SB-X and left SB-Y alone, so
        // the value read back was the stale initform and an unrelated slot had
        // been clobbered — with no error anywhere.
        (
            "(progn (defclass sb-a () ((sb-x :initform 'ax :accessor sb-sh) (sb-y :initform 'ay)))
                    (defclass sb-b () ((sb-y :initform 'by :accessor sb-sh) (sb-x :initform 'bx)))
                    (defun sb-store (o v) (setf (sb-sh o) v))
                    (let ((o (make-instance 'sb-b)))
                      (sb-store o 'new)
                      (list (sb-sh o) (slot-value o 'sb-y) (slot-value o 'sb-x))))",
            "(NEW NEW BX)",
        ),
        // An accessor inherited from a superclass still resolves, so the
        // instance-first search must fall through the precedence list rather
        // than stopping at the instance's own class.
        (
            "(progn (defclass sc-base () ((sc-v :initform 'base :accessor sc-sh)))
                    (defclass sc-derived (sc-base) ())
                    (defclass sc-other () ((sc-w :initform 'other :accessor sc-sh)))
                    (let ((o (make-instance 'sc-derived)))
                      (setf (sc-sh o) 'inherited)
                      (list (sc-sh o) (slot-value o 'sc-v))))",
            "(INHERITED INHERITED)",
        ),
        // READING through a shared accessor name is broken by the same root
        // cause at a DIFFERENT site: the accessor read fast-path scans every
        // class by name and keeps the LAST hit. Pinned separately from the
        // write, because the two use different code and the write can be
        // correct while the read is not — which is exactly how this was found.
        (
            "(progn (defclass sd-base () ((sd-v :initform 'base :accessor sd-sh)))
                    (defclass sd-derived (sd-base) ())
                    (defclass sd-other () ((sd-w :initform 'other :accessor sd-sh)))
                    (list (sd-sh (make-instance 'sd-derived))
                          (sd-sh (make-instance 'sd-other))))",
            "(BASE OTHER)",
        ),
    ]);
}
