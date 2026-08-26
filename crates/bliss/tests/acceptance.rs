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
    // Cargo builds the bin as a prerequisite of this integration test and sets
    // CARGO_BIN_EXE_bliss-cli to its path — target-aware, so this works whether
    // the default target is glibc or the static musl target (bliss-bca.5).
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| PathBuf::from(env!("CARGO_BIN_EXE_bliss-cli")))
        .as_path()
}

/// Get the path to the bliss-cli binary built by cargo.
fn bliss_bin() -> Command {
    let mut cmd = Command::new(bliss_bin_path());
    // Keep tests hermetic against the developer's real ~/.blissrc: point the init
    // file at a path that does not exist, so REPL-mode runs never load it (a
    // present init file would otherwise inject output/latency and fail the REPL
    // prompt tests). Tests that exercise init loading set BLISS_INIT_FILE
    // themselves, overriding this.
    cmd.env(
        "BLISS_INIT_FILE",
        std::env::temp_dir().join("bliss-tests-nonexistent-init.lisp"),
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
    let output = bliss_bin()
        .args([
            "--no-init",
            "--eval",
            "(defvar *seven-z-l* 41)",
            "--eval",
            "(princ (+ *seven-z-l* 1))",
        ])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0), "exit code should be 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42"),
        "second --eval should see the first's defvar (expect 42), got: {stdout}"
    );
}

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
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
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
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("SHARED SHARED (PREP COMP) 42 9"),
        "class-allocated slots + default-initargs; got: '{}', stderr: '{}'",
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

/// `&whole` binds the ENTIRE macro call form, including the macro name (CLHS
/// 3.4.4) — `(w 7)` binds `(W 7)`, not `(7)`. A `&whole` on a nested
/// destructuring sublist still binds only that sublist (no operator).
#[test]
fn eval_macro_whole_includes_operator() {
    let expr = r#"(progn
  (defmacro w (&whole form x) (declare (ignore x)) (list 'quote form))
  (defmacro nested ((&whole sub a b)) (declare (ignore a b)) (list 'quote sub))
  (format nil "~S ~S" (w 7) (nested (8 9))))"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
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
fn eval_warn_renders_asdf_format_control() {
    let expr = r#"(warn "~@<Invalid :version specifier ~S~@[ for component ~S~]~@[ in ~S~]~@[ from file ~A~]~@[, using NIL instead~]~3i~_~@:>"
                        'version "completions" nil "/tmp/completions.asd" t)"#;
    let output = bliss_bin()
        .args(["--eval", expr])
        .output()
        .expect("failed to run bliss");
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

#[test]
fn sandbox_cpu_timeout_is_catchable_timeout_condition() {
    let expr =
        "(handler-case (loop) (bliss-ext:timeout-condition (e) (declare (ignore e)) :timeout))";
    let mut child = bliss_bin()
        .env("BLISS_SANDBOX_CPU_MS", "25")
        .args(["--sandbox", "--eval", expr])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn bliss sandbox timeout check");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().expect("poll bliss timeout child") {
            let output = child
                .wait_with_output()
                .expect("collect bliss timeout child");
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
        .args([
            "--eval",
            "(let ((x 0)) (restart-case (invoke-restart 'bump) (bump () (setq x 99))) x)",
        ])
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        ("(loop for s = 100 then (1+ s) repeat 3 collect s)", "(100 101 102)"),
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
        (
            "(let ((l (list 1 2 3 4 5))) (setf (fourth l) 40) (setf (nth 4 l) 50) l)",
            "(1 2 3 40 50)",
        ),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "{expr} => expected {expected}, got: {stdout}"
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
        ("(let ((h (make-hash-table :test #'equal))) (setf (gethash (list 1 2) h) :a) (gethash (list 1 2) h))", ":A"),
        ("(let ((h (make-hash-table :test #'equal))) (setf (gethash \"s\" h) :b) (gethash \"s\" h))", ":B"),
        // #'equalp (case-insensitive).
        ("(let ((h (make-hash-table :test #'equalp))) (setf (gethash \"AbC\" h) :c) (gethash \"abc\" h))", ":C"),
        // The symbol form still works.
        ("(let ((h (make-hash-table :test 'equal))) (setf (gethash (list 9) h) :d) (gethash (list 9) h))", ":D"),
        // Default EQL: a fresh list key does NOT match (identity), fixnum does.
        ("(let ((h (make-hash-table))) (setf (gethash (list 1) h) :e) (gethash (list 1) h))", "NIL"),
        ("(let ((h (make-hash-table))) (setf (gethash 5 h) :f) (gethash 5 h))", ":F"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
    }
}

/// Regression (bliss-c0m): TYPEP recognises the real heap numeric types (bignum,
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
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
    let mut cmd = bliss_bin();
    cmd.env("BLISS_T0_T1_THRESHOLD", "2").env("BLISS_T1_T2_INVOKE_THRESHOLD", "3");
    cmd.args([
        "--eval", "(defun av-rd () av-pv)",
        "--eval", "(defun av-h () (let ((av-pv 99)) (av-rd)))",
        "--eval", "(defparameter av-pv 5)",
        "--eval", "(format t \"cold=~a~%\" (av-h))",
        "--eval", "(dotimes (i 20) (av-h))",
        "--eval", "(format t \"hot=~a~%\" (av-h))",
    ]);
    let out = cmd.output().expect("run bliss");
    assert_eq!(out.status.code(), Some(0), "should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cold=99"), "cold should be 99: {stdout}");
    assert!(stdout.contains("hot=99"), "hot must match cold (tier-consistent): {stdout}");
    // defvar likewise proclaims special; and defparameter returns the NAME (ANSI).
    let out2 = bliss_bin()
        .args(["--eval", "(defun av-rd2 () av-dv)",
               "--eval", "(defvar av-dv 5)",
               "--eval", "(print (let ((av-dv 42)) (av-rd2)))"])
        .output().expect("run bliss");
    assert!(String::from_utf8_lossy(&out2.stdout).contains("42"), "defvar var is special: {}", String::from_utf8_lossy(&out2.stdout));
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
            &["(defun ds-helper () ds-v)",
              "(defun ds-caller () (let ((ds-v 42)) (declare (special ds-v)) (ds-helper)))",
              "(defparameter ds-v 1)"],
            "(ds-caller)",
            "42",
        ),
        // The body reads/updates the dynamic binding directly.
        (
            &["(defun ds-f (x) (let ((ds-acc 0)) (declare (special ds-acc)) \
                 (setq ds-acc (+ ds-acc x)) ds-acc))"],
            "(ds-f 5)",
            "5",
        ),
        // A declared-special inner LET shadows an outer LEXICAL binding: the
        // callee sees the dynamic (inner) value, not the outer lexical.
        (
            &["(defun ds-rd () ds-x)",
              "(defun ds-g () (let ((ds-x 1)) (let ((ds-x 99)) (declare (special ds-x)) (ds-rd))))",
              "(defparameter ds-x 7)"],
            "(ds-g)",
            "99",
        ),
        // A special declaration is pervasive: an inner LET rebinding of the same
        // name is also dynamic, and the body reads that inner dynamic value.
        (
            &["(defun ds-nested () (let ((ds-z 1)) (declare (special ds-z)) \
                 (let ((ds-z 2)) (+ ds-z ds-z))))"],
            "(ds-nested)",
            "4",
        ),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = bliss_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{final_expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).rev().find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{final_expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
            &["(declaim (special na-zz))", "(defun na-rd () na-zz)", "(defparameter na-zz 1)"],
            "(let ((na-zz 99)) (na-rd))",
            "99",
        ),
        // proclaim (the run-time counterpart) → same.
        (
            &["(proclaim '(special na-qq))", "(defun na-rq () na-qq)", "(defparameter na-qq 1)"],
            "(let ((na-qq 77)) (na-rq))",
            "77",
        ),
        // declaim tolerates and ignores non-special declarations.
        (&["(declaim (optimize (speed 3)) (type fixnum na-foo))"], ":ok", ":OK"),
    ];
    for (setup, final_expr, expected) in cases {
        let mut cmd = bliss_bin();
        for form in setup {
            cmd.args(["--eval", form]);
        }
        cmd.args(["--eval", &format!("(print {final_expr})")]);
        let out = cmd.output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{final_expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        // Each `--eval` echoes its result, so the final `(print …)` value is the
        // LAST non-empty line, not the first.
        let got = stdout.lines().map(|l| l.trim()).rev().find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{final_expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
        let out = bliss_bin().args(["--eval", &format!("(print {expr})")]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        let got = stdout.lines().map(|l| l.trim()).find(|l| !l.is_empty()).unwrap_or("");
        assert_eq!(got, expected, "{expr} => expected {expected}, got: {got} (full: {stdout:?})");
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
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(expected), "{expr} => expected {expected}, got: {stdout}");
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
        ("(progn (defun ftest2 (x) x) (type-of #'ftest2))", "FUNCTION"),
        ("(typep (lambda (x) x) 'function)", "T"),
        ("(type-of (lambda (x) x))", "FUNCTION"),
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
    let dir = std::env::temp_dir().join("bliss_test_shadow");
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

    let out = bliss_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("run bliss");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);

    assert_eq!(out.status.code(), Some(0), "shadow load should exit 0 (not SIGSEGV)");
    let stdout = String::from_utf8_lossy(&out.stdout).to_uppercase();
    assert!(stdout.contains("DISTINCT=NIL"), "cl:defconstant must differ from shadowed defconstant, got: {stdout}");
    assert!(stdout.contains("HOME=\"SHTEST\""), "shadowed symbol home package must be SHTEST, got: {stdout}");
    assert!(stdout.contains("SHADOWING=(\"DEFCONSTANT\")"), "package-shadowing-symbols must list DEFCONSTANT, got: {stdout}");
    assert!(stdout.contains("VALUE=42"), "self-referential defconstant macro must terminate, got: {stdout}");
}

/// Regression (bliss-255): the tree-walker's FUNCALL and TAGBODY arms held a
/// `BlissVal` across an allocation that can fire a relocating minor GC, without
/// rooting it — the classic invariant-#1 violation (AGENTS.md "GC safety"):
///   - FUNCALL: the callee `fn_val` sat unrooted across `eval_args` (which
///     allocates), so a moved young closure went stale → "Cannot apply: Cons(..)".
///   - TAGBODY: the `items` statement `Vec<BlissVal>` was unrooted across the
///     per-statement `eval_form`, so a moved statement form read back as a zeroed
///     cons (car => Fixnum(0)) → "undefined function:".
/// Both surfaced deep in cl-ppcre's tagbody-heavy, `(funcall next-fn ..)` CPS
/// scanner closures. `BLISS_GC_STRESS` fires a minor GC on (almost) every
/// allocation, turning the load-dependent corruption into a deterministic one;
/// the callee/statements must be freshly allocated (young) at the vulnerable
/// point, and TAGBODY needs a moderate stride so its fresh form is not promoted
/// out of the nursery before the collection. Each program computes a known total
/// that only comes out right if nothing went stale.
#[test]
fn gc_roots_funcall_callee_and_tagbody_statements() {
    // FUNCALL: `g` is a fresh young closure; the `(list i i)` argument allocates
    // (firing the GC) while `g` is the in-flight callee. eval forces the
    // tree-walker path. Sum of (i+i+i) for i in 0..59 = 3*1770 = 5310.
    let funcall_prog = "(let ((s 0)) \
         (dotimes (i 60) \
           (let ((g (let ((k i)) (lambda (p) (+ k (car p) (cadr p)))))) \
             (setf s (+ s (eval '(funcall g (list i i))))))) \
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
        let out = bliss_bin()
            .env("BLISS_GC_STRESS", stride)
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run bliss");
        assert_eq!(
            out.status.code(),
            Some(0),
            "GC-stress program should exit 0, not crash (stderr: {})",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains(expected),
            "expected {expected} under BLISS_GC_STRESS={stride}, got: {stdout} (stderr: {})",
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
        let out = bliss_bin().args(["--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
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
/// `(let ((reg-scanner …)) (defmethod build-replacement-template …))`. bliss
/// previously ran the body with no captured environment, so the free lexical was
/// unbound. The capture must fire ONLY for genuine nesting, not for a top-level
/// (or nested-LOAD top-level) definition — otherwise every library function would
/// wrongly run against the load frame.
#[test]
fn defun_and_defmethod_close_over_enclosing_lexicals() {
    let dir = std::env::temp_dir().join("bliss_test_sdd");
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
    let out = bliss_bin()
        .args(["--load", file_path.to_str().unwrap()])
        .output()
        .expect("run bliss");
    let _ = std::fs::remove_file(&file_path);
    let _ = std::fs::remove_dir(&dir);
    assert_eq!(out.status.code(), Some(0), "sdd load should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout).to_uppercase();
    assert!(stdout.contains("F=42"), "defun closes over x, got: {stdout}");
    assert!(stdout.contains("CUR=2"), "two defuns share captured mutable c, got: {stdout}");
    assert!(stdout.contains("GM=15"), "defmethod closes over y, got: {stdout}");
    assert!(stdout.contains("TOP=7"), "a top-level defun still works (no spurious capture), got: {stdout}");
}

/// Regression (bliss-sdd follow-ons found via cl-ppcre regex-replace): WRITE-STRING
/// / WRITE-LINE must honor the `:start`/`:end` bounding keywords (cl-ppcre stitches
/// replacement output with them; ignoring them emitted the whole string each time),
/// and ARRAY-DIMENSION (singular, axis 0) returns a vector's length.
#[test]
fn write_string_bounds_and_array_dimension() {
    let cases = [
        ("(with-output-to-string (s) (write-string \"hello world\" s :start 2 :end 5))", "\"llo\""),
        ("(with-output-to-string (s) (write-string \"abcdef\" s :start 3))", "\"def\""),
        ("(with-output-to-string (s) (write-string \"abcdef\" s))", "\"abcdef\""),
        ("(array-dimension (vector 1 2 3 4) 0)", "4"),
        ("(array-dimension (make-array 5 :initial-element 0) 0)", "5"),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin().args(["--no-init", "--eval", expr]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{expr} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(expected), "{expr} => expected {expected}, got: {stdout}");
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
        let out = bliss_bin().args(["--no-init", "--eval", prog]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.to_uppercase().contains(&expected.to_uppercase()),
            "expected {expected}, got: {stdout}"
        );
    }
}

/// Regression (bliss-day): DEFMETHOD with the same qualifier and specializers
/// REPLACES the existing method (CLHS 7.6.2). bliss appended it, leaving the
/// stale method applicable so a redefinition never took effect.
#[test]
fn defmethod_redefinition_replaces() {
    let cases = [
        ("(progn (defgeneric r (x)) (defmethod r ((n integer)) :v1) \
            (defmethod r ((n integer)) :v2) (format t \"~s\" (r 1)))", ":V2"),
        // Replace only after the cache is warm (invalidation on redefinition).
        ("(progn (defgeneric r (x)) (defmethod r ((n integer)) :v1) (r 1) \
            (defmethod r ((n integer)) :v2) (format t \"~s\" (r 1)))", ":V2"),
        // A different specializer is a distinct method, not a replacement.
        ("(progn (defgeneric d (x)) (defmethod d ((n integer)) :int) \
            (defmethod d ((s string)) :str) (format t \"~s\" (list (d 1) (d \"x\"))))",
         "(:INT :STR)"),
    ];
    for (prog, expected) in cases {
        let out = bliss_bin().args(["--no-init", "--eval", prog]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.to_uppercase().contains(&expected.to_uppercase()), "expected {expected}, got: {stdout}");
    }
}

/// Regression (bliss-x5y.20): COND is not a macro in bliss — it is lowered
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
        ("(progn (defclass r () ((a :initform 7))) \
            (defun f (o) (with-slots (a) o (cond ((eql a 7) :yes) (t :no)))) \
            (format t \"~s\" (f (make-instance 'r))))", ":YES"),
        ("(progn (defclass r () ((mn :initform 2) (mx :initform 2))) \
            (defgeneric cm (x)) \
            (defmethod cm ((o r)) (with-slots (mn mx) o \
              (cond ((= mn mx 1) :one) ((eql mn mx) :eq) (t :other)))) \
            (format t \"~s\" (cm (make-instance 'r))))", ":EQ"),
        // Multiple slots, one used only in a clause body.
        ("(progn (defclass r () ((a :initform 3) (b :initform 9))) \
            (defun f (o) (with-slots (a b) o (cond ((eql a 3) b) (t a)))) \
            (format t \"~s\" (f (make-instance 'r))))", "9"),
    ];
    for (prog, expected) in cases {
        let out = bliss_bin().args(["--no-init", "--eval", prog]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.to_uppercase().contains(&expected.to_uppercase()), "expected {expected}, got: {stdout}");
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
        ("(format t \"~s\" (funcall (funcall (lambda () (symbol-macrolet ((m 42)) (lambda () m))))))", "42"),
        // Escaping closure over a WITH-SLOTS slot (the cl-ppcre shape).
        ("(progn (defclass k () ((s :initform 5))) \
            (defun mk (o) (with-slots (s) o (lambda () s))) \
            (format t \"~s\" (funcall (mk (make-instance 'k)))))", "5"),
        // Inner LET still shadows the symbol-macro.
        ("(format t \"~s\" (symbol-macrolet ((m 42)) (let ((m 7)) m)))", "7"),
        // QUOTE still suppresses expansion.
        ("(format t \"~s\" (symbol-macrolet ((m 42)) 'm))", "M"),
        // Common with-slots read/write unaffected.
        ("(progn (defclass k () ((s :initform 0))) \
            (defun f (o) (with-slots (s) o (setf s 9) s)) \
            (format t \"~s\" (f (make-instance 'k))))", "9"),
    ];
    for (prog, expected) in cases {
        let out = bliss_bin().args(["--no-init", "--eval", prog]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.to_uppercase().contains(&expected.to_uppercase()), "expected {expected}, got: {stdout}");
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
        ("(progn (defun helper () (declare (special v)) v) \
            (defun caller () (let ((v 42)) (declare (special v)) (helper))) \
            (format t \"~s\" (caller)))", "42"),
        // LET* form (init of a later binding calls the reader), like cl-ppcre convert.
        ("(progn (defun helper () (declare (special v)) v) \
            (defun caller () (let* ((v 7) (w (helper))) (declare (special v)) w)) \
            (format t \"~s\" (caller)))", "7"),
        // (setf (slot-value ...)) in a compiled defun and method.
        ("(progn (defclass k () ((s :initform 0))) \
            (defun f (o v) (setf (slot-value o 's) v) (slot-value o 's)) \
            (format t \"~s\" (f (make-instance 'k) 42)))", "42"),
        ("(progn (defclass k () ((s :initform 0))) (defgeneric g (o v)) \
            (defmethod g ((o k) v) (setf (slot-value o 's) (* v 2)) (slot-value o 's)) \
            (format t \"~s\" (g (make-instance 'k) 21)))", "42"),
    ];
    for (prog, expected) in cases {
        let out = bliss_bin().args(["--no-init", "--eval", prog]).output().expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(expected), "expected {expected}, got: {stdout}");
    }
}

/// Regression (bliss-x5y): lazy compilation (BLISS_LAZY_COMPILE, now the default
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
        ("(progn (defun fib (n) (if (< n 2) n (+ (fib (- n 1)) (fib (- n 2))))) \
            (format t \"~s\" (fib 20)))", "6765"),
        // Cold function (called once) stays interpreted but still correct.
        ("(progn (defun once (x) (* x x)) (format t \"~s\" (once 9)))", "81"),
        // Redefinition after warmup takes effect (lazy state cleared).
        ("(progn (defun g (x) (* x 2)) (dotimes (i 20) (g i)) \
            (defun g (x) (* x 3)) (format t \"~s\" (g 10)))", "30"),
    ];
    for (prog, expected) in cases {
        let out = bliss_bin()
            .env("BLISS_LAZY_COMPILE", "1")
            .env("BLISS_LAZY_THRESHOLD", "4")
            .args(["--no-init", "--eval", prog])
            .output()
            .expect("run bliss");
        assert_eq!(out.status.code(), Some(0), "{prog} should exit 0 (stderr: {})", String::from_utf8_lossy(&out.stderr));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains(expected), "expected {expected}, got: {stdout}");
    }
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
    let caught = bliss_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f (n) (+ 1 (f (+ n 1))))) (f 0)) \
               (storage-condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run bliss");
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
    let via_super = bliss_bin()
        .args([
            "--eval",
            "(handler-case (labels ((f () (f))) (f)) \
               (condition (e) (declare (ignore e)) :caught))",
        ])
        .output()
        .expect("run bliss");
    assert!(
        String::from_utf8_lossy(&via_super.stdout)
            .to_uppercase()
            .contains("CAUGHT"),
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
    assert!(
        String::from_utf8_lossy(&ok.stdout)
            .to_uppercase()
            .contains("DONE")
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        ("(string-trim \" \" \"  hi  \")", "\"hi\""),
        ("(string-left-trim \" x\" \"xx hi\")", "\"hi\""),
        ("(string-right-trim \" \" \"hi  \")", "\"hi\""),
    ];
    for (expr, expected) in cases {
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
        let out = bliss_bin()
            .args(["--eval", expr])
            .output()
            .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL|"),
        "reduce with :initial-value nil on empty seq should return NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setf_of_a_setf_generic_function() {
    // bliss-lb6.14: (setf (place …) v) dispatches to a (defmethod (setf place) …)
    // writer generic — ASDF uses (setf (action-status …) …).
    let prog = "(defgeneric (setf gsp) (v x)) \
                (defmethod (setf gsp) (v (x cons)) (setf (car x) v)) \
                (let ((c (list 0))) (setf (gsp c) 8) (format t \"~a\" c))";
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("(8)"),
        "setf of a (setf f) generic should mutate the place, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn setf_of_a_setf_function_place() {
    // bliss-lb6.14: (setf (place …) v) must call a (defun (setf place) …) writer
    // (CLHS 5.1.2.9) — ASDF uses (defun (setf operate-level) …).
    let prog = "(defun (setf hd) (v x) (setf (car x) v) v) \
                (let ((c (list 1 2))) (setf (hd c) 9) (format t \"~a\" c))";
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("NIL T"),
        "a pathname must be pathnamep=T, stringp=NIL, got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn load_accepts_a_pathname_designator() {
    // LOAD must accept a #P pathname, not only a string (common in ~/.blissrc).
    let dir = std::env::temp_dir().join("bliss_test_load_pathname");
    let _ = std::fs::create_dir_all(&dir);
    let file_path = dir.join("p.lisp");
    std::fs::write(&file_path, "(format t \"LOADED-VIA-PATHNAME\")\n").expect("write");
    let prog = format!("(load #P\"{}\")", file_path.to_str().unwrap());
    let output = bliss_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run bliss");
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
        "bliss_test_dynamic_load_pathname_{}",
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
    let output = bliss_bin()
        .args(["--eval", &prog])
        .output()
        .expect("run bliss");
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
    let error_output = bliss_bin()
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
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
    let output = bliss_bin()
        .args(["--eval", prog])
        .output()
        .expect("run bliss");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("42 2 NONE"),
        "return-from <function> should work from top-level and nested forms; got: '{stdout}', stderr: '{}'",
        String::from_utf8_lossy(&output.stderr)
    );
}
