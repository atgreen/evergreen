//! `(setf (compiler-macro-function name) fn)` is how CLHS 3.2.2.1 says a
//! compiler macro is installed, and how a library aliases one — iolib's DEFALIAS
//! copies a compiler macro from one name to another that way (bliss-vhr6e).
use std::process::Command;

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
fn a_compiler_macro_can_be_installed_as_a_function() {
    // The expander receives the whole form and an environment, and its result is
    // what runs — here it rewrites the call to a different function.
    assert_eq!(
        eval(
            r#"(progn (defun original (x) (list :original x))
                      (defun rewritten (x) (list :rewritten x))
                      (setf (compiler-macro-function 'original)
                            (lambda (form env) (declare (ignore env)) (cons 'rewritten (cdr form))))
                      (defun caller () (original 1))
                      (format t "RESULT:~s" (list (caller) (funcall #'original 2))))"#
        ),
        "((:REWRITTEN 1) (:ORIGINAL 2))"
    );
}

#[test]
fn compiler_macro_function_reads_back_what_was_installed() {
    assert_eq!(
        eval(
            r#"(progn (defun f (x) x)
                      (setf (compiler-macro-function 'f) (lambda (form env) (declare (ignore env)) form))
                      (format t "RESULT:~s" (and (compiler-macro-function 'f) t)))"#
        ),
        "T"
    );
}

#[test]
fn a_function_installed_compiler_macro_can_be_copied_to_another_name_and_removed() {
    // The DEFALIAS shape: read one name's expander and install it under another.
    // This works for an expander that IS a function object; one defined by
    // DEFINE-COMPILER-MACRO still reads back as T and so cannot be copied
    // (bliss-0g5lg) — the next test pins that.
    assert_eq!(
        eval(
            r#"(progn (defun a (x) (list :a x))
                      (defun b (x) (list :b x))
                      (setf (compiler-macro-function 'a)
                            (lambda (form env) (declare (ignore env)) (list 'list :expanded (second form))))
                      (setf (compiler-macro-function 'b) (compiler-macro-function 'a))
                      (defun call-b () (b 7))
                      (let ((copied (call-b)))
                        (setf (compiler-macro-function 'b) nil)
                        (defun call-b-again () (b 7))
                        (format t "RESULT:~s" (list copied (call-b-again)
                                                    (compiler-macro-function 'b)))))"#
        ),
        "((:EXPANDED 7) (:B 7) NIL)"
    );
}

#[test]
fn copying_a_define_compiler_macro_expander_leaves_the_alias_without_one() {
    // DEFINE-COMPILER-MACRO's expander is a host closure, so the reader answers T
    // and there is no function to copy (bliss-0g5lg). Installing that T must leave
    // the alias with no compiler macro — legal, since a compiler macro is always
    // optional — rather than installing something uncallable.
    assert_eq!(
        eval(
            r#"(progn (defun a (x) (list :a x))
                      (defun b (x) (list :b x))
                      (define-compiler-macro a (x) (list 'list :expanded x))
                      (setf (compiler-macro-function 'b) (compiler-macro-function 'a))
                      (defun call-a () (a 7))
                      (defun call-b () (b 7))
                      (format t "RESULT:~s" (list (compiler-macro-function 'a)
                                                  (call-a) (call-b)
                                                  (compiler-macro-function 'b))))"#
        ),
        "(T (:EXPANDED 7) (:B 7) NIL)"
    );
}
