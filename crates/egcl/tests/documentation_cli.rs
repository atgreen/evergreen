//! A definition's docstring must be recorded where DOCUMENTATION can find it
//! (bliss-jre1u). CFFI's DEFCFUN passes its docstring through to DEFUN.
use std::process::Command;

/// Run PROGRAM and return what it printed after `RESULT:`. `--eval` prints the
/// form's value after whatever the form printed, so the answer is marked rather
/// than read off the whole of stdout.
fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
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
fn a_defun_docstring_is_recorded() {
    assert_eq!(
        eval(
            r#"(progn (defun shout (x) "say it louder" x) (format t "RESULT:~a" (documentation 'shout 'function)))"#
        ),
        "say it louder"
    );
}

#[test]
fn a_lone_string_body_is_the_return_value_not_a_docstring() {
    // `(defun f () "text")` returns the string; it documents nothing.
    assert_eq!(
        eval(
            r#"(progn (defun greeting () "hello") (format t "RESULT:~s" (list (greeting) (documentation 'greeting 'function))))"#
        ),
        "(\"hello\" NIL)"
    );
}

#[test]
fn redefining_replaces_the_docstring() {
    assert_eq!(
        eval(
            r#"(progn (defun f (x) "first" x) (defun f (x) "second" x)
                      (format t "RESULT:~a" (documentation 'f 'function)))"#
        ),
        "second"
    );
}

#[test]
fn a_variable_docstring_still_answers_separately() {
    // The function and variable namespaces are separate doc-types.
    assert_eq!(
        eval(
            r#"(progn (defvar *thing* 1 "a variable") (defun *thing* (x) "a function" x)
                      (format t "RESULT:~s" (list (documentation '*thing* 'variable)
                                   (documentation '*thing* 'function))))"#
        ),
        "(\"a variable\" \"a function\")"
    );
}

#[test]
fn a_defmacro_docstring_is_recorded() {
    // CFFI defines a variadic DEFCFUN binding as a macro with a docstring.
    assert_eq!(
        eval(
            r#"(progn (defmacro twice (x) "do it twice" `(progn ,x ,x)) (format t "RESULT:~a" (documentation 'twice 'function)))"#
        ),
        "do it twice"
    );
}
