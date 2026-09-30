//! DEFINE-SYMBOL-MACRO is a global definition and SYMBOL-MACROLET a lexical one
//! (bliss-cb3c7). CFFI's DEFCVAR defines its accessor as a symbol macro, and
//! CFFI-TESTS defines some of those through EVAL at load time.
use std::process::Command;

fn run(program: &str) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).trim().to_string(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// What PROGRAM printed after `RESULT:`. `--eval` prints the form's value after
/// whatever the form printed, so the answer is marked rather than read off the
/// whole of stdout.
fn eval(program: &str) -> String {
    let (ok, out, err) = run(program);
    assert!(ok, "{out}\n{err}");
    out.lines()
        .find_map(|line| line.strip_prefix("RESULT:"))
        .unwrap_or_else(|| panic!("no RESULT: line in output:\n{out}\n{err}"))
        .trim()
        .to_string()
}

#[test]
fn a_global_symbol_macro_survives_into_a_null_lexical_environment() {
    // EVAL evaluates in a null lexical environment, which drops SYMBOL-MACROLET
    // bindings but not global definitions.
    assert_eq!(
        eval(
            r#"(progn (define-symbol-macro *place* (get 'cell 'value))
                      (setf *place* 41)
                      (format t "RESULT:~s" (list (eval '*place*) (eval '(progn (setf *place* 42) *place*)))))"#
        ),
        "(41 42)"
    );
}

#[test]
fn a_definition_made_through_eval_is_visible_afterwards() {
    assert_eq!(
        eval(
            r#"(progn (eval '(define-symbol-macro *place* (get 'cell 'value)))
                      (eval '(setf *place* 7))
                      (format t "RESULT:~s" (list *place* (eval '(let ((copy *place*)) copy)))))"#
        ),
        "(7 7)"
    );
}

#[test]
fn a_definition_made_inside_a_binding_is_still_global() {
    // CFFI-TESTS defines its symbol-case DEFCVARs inside a LET of *READTABLE*.
    assert_eq!(
        eval(
            r#"(progn (let ((*readtable* (copy-readtable)))
                        (eval '(define-symbol-macro *place* 12345)))
                      (format t "RESULT:~s" (eval '*place*)))"#
        ),
        "12345"
    );
}

#[test]
fn symbol_macrolet_stays_lexical() {
    // A lexical binding shadows the global definition inside its body and
    // nowhere else, and EVAL never sees it.
    assert_eq!(
        eval(
            r#"(progn (define-symbol-macro *place* :global)
                      (format t "RESULT:~s" (list (symbol-macrolet ((*place* :lexical)) *place*) *place*)))"#
        ),
        "(:LEXICAL :GLOBAL)"
    );
    let (ok, out, err) = run(r#"(symbol-macrolet ((only-here 1)) (eval 'only-here))"#);
    assert!(
        !ok || out.contains("UNBOUND") || err.contains("unbound"),
        "a lexical symbol macro must not reach EVAL: {out}\n{err}"
    );
}
