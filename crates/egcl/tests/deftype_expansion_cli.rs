//! A DEFTYPE must be expanded wherever a type specifier is interpreted,
//! including when it names another DEFTYPE or sits in an element-type position
//! (bliss-hfn71). Babel defines (deftype unicode-string () '(vector unicode-char
//! *)) with UNICODE-CHAR a DEFTYPE of its own, and CFFI's DEFTYPE
//! FOREIGN-POINTER names the implementation's own pointer type.
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
fn a_deftype_naming_another_deftype_is_recognised() {
    assert_eq!(
        eval(
            r#"(progn (deftype small () '(integer 0 9))
                      (deftype digit () 'small)
                      (deftype numeral () 'digit)
                      (format t "RESULT:~s" (list (typep 5 'small) (typep 5 'digit) (typep 5 'numeral)
                                   (typep 50 'numeral))))"#
        ),
        "(T T T NIL)"
    );
}

#[test]
fn a_deftype_in_an_element_type_position_is_recognised() {
    // (vector my-char *) designates a string exactly as (vector character *) does.
    assert_eq!(
        eval(
            r#"(progn (deftype my-char () 'character)
                      (deftype my-string () '(vector my-char *))
                      (format t "RESULT:~s" (list (stringp (coerce '(#\a #\b) 'my-string))
                                   (stringp (concatenate 'my-string '(#\a #\b)))
                                   (coerce '(#\a #\b) 'my-string))))"#
        ),
        "(T T \"ab\")"
    );
}

#[test]
fn a_satisfies_expansion_still_reaches_its_predicate() {
    assert_eq!(
        eval(
            r#"(progn (defun evenish (x) (and (integerp x) (evenp x)))
                      (deftype even-number () '(satisfies evenish))
                      (deftype even-alias () 'even-number)
                      (format t "RESULT:~s" (list (typep 4 'even-alias) (typep 5 'even-alias))))"#
        ),
        "(T NIL)"
    );
}

#[test]
fn a_circular_deftype_does_not_hang() {
    // Nonsense, but it must answer rather than expand forever.
    assert_eq!(
        eval(
            r#"(progn (deftype loopy () 'loopy)
                      (format t "RESULT:~a" (handler-case (progn (typep 1 'loopy) :answered)
                               (error () :signalled))))"#
        ),
        "ANSWERED"
    );
}

#[test]
fn an_alias_is_expanded_wherever_a_type_specifier_is_read() {
    // Every answer here is SBCL's on the same forms.
    assert_eq!(
        eval(
            r#"(progn (deftype my-int () 'integer)
                      (deftype my-char () 'character)
                      (deftype my-string () '(vector my-char *))
                      (format t "RESULT:~s" (list (typep 5 '(or my-int string))
                                   (typep 5 '(and my-int (satisfies oddp)))
                                   (typep "x" '(not my-int))
                                   (typep "ab" 'my-string)
                                   (make-sequence 'my-string 2 :initial-element #\z))))"#
        ),
        "(T T T T \"zz\")"
    );
}
