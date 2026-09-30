//! A DEFUN replaces the definition, including a definition that has already
//! been promoted into the bytecode registry (bliss-96hjb).
//!
//! `lazy_compile_defun` returns early when the symbol is already registered, and
//! nothing else retired the old body, so once a function had been called enough
//! times to compile, a later DEFUN installed a new function object that every
//! subsequent call ignored: 300 calls and then `(defun u (x) (* x 3))` kept
//! answering with `(* x 2)`. SBCL takes the new definition in every shape below.
use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_torcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{stdout}\n{stderr}"))
        .trim()
        .to_string()
}

#[test]
fn a_redefinition_inside_a_binding_form_replaces_a_promoted_definition() {
    // The failing shape: the DEFUN is nested in a LET, so it records a lexical
    // capture rather than clearing one, and the 300 calls before it have put the
    // first definition in the registry.
    assert_eq!(
        eval(
            r#"(progn (defun u (x) (* x 2))
                 (let ((a 0))
                   (dotimes (i 300) (setf a (u 5)))
                   (defun u (x) (* x 3))
                   (format t "RESULT:~s" (list a (u 5)))))"#
        ),
        "(10 15)"
    );
}

#[test]
fn a_redefinition_replaces_a_promoted_definition_called_through_its_object() {
    assert_eq!(
        eval(
            r#"(progn (defun v (x) (* x 2))
                 (let ((a 0))
                   (dotimes (i 300) (setf a (funcall #'v 5)))
                   (defun v (x) (* x 3))
                   (format t "RESULT:~s" (list a (funcall #'v 5)))))"#
        ),
        "(10 15)"
    );
}

#[test]
fn a_redefinition_at_the_same_level_as_the_calls_still_replaces_it() {
    assert_eq!(
        eval(
            r#"(progn (defun w (x) (* x 2))
                 (dotimes (i 300) (w 5))
                 (defun w (x) (* x 3))
                 (format t "RESULT:~s" (w 5)))"#
        ),
        "15"
    );
}

#[test]
fn a_cold_definition_is_still_replaced() {
    assert_eq!(
        eval(
            r#"(progn (defun y (x) (* x 2))
                 (let ((a (y 5)))
                   (defun y (x) (* x 3))
                   (format t "RESULT:~s" (list a (y 5)))))"#
        ),
        "(10 15)"
    );
}

#[test]
fn a_function_recompiles_after_being_redefined() {
    // The new definition must be able to promote in its turn, not merely run
    // interpreted forever because its name was cleared.
    assert_eq!(
        eval(
            r#"(progn (defun z (x) (* x 2))
                 (dotimes (i 300) (z 5))
                 (defun z (x) (* x 3))
                 (let ((a 0))
                   (dotimes (i 300) (setf a (z 5)))
                   (format t "RESULT:~s" a)))"#
        ),
        "15"
    );
}
