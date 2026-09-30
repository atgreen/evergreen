//! A SETF place is not an expression: a compiler macro must not rewrite it
//! (CLHS 3.2.2.1, 5.1.2.7; bliss-msyk). CFFI's WITH-FOREIGN-SLOTS binds a symbol
//! macro standing for an accessor that has both a compiler macro and a SETF
//! expander, which is exactly this shape.
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

/// The accessor CFFI's shape reduces to: a reader whose compiler macro rewrites
/// it into a translation of a lower-level accessor, plus a SETF expander.
const ACCESSOR: &str = r#"
  (defvar *cell* (list 0))
  (defun %raw (c) (car c))
  (defun %set-raw (v c) (setf (car c) v) v)
  (defun translate-out (v) (list :out v))
  (defun getter (c) (translate-out (%raw c)))
  (define-compiler-macro getter (c) `(translate-out (%raw ,c)))
  (define-setf-expander getter (c &environment env)
    (declare (ignore env))
    (let ((store (gensym)) (tmp (gensym)))
      (values (list tmp) (list c) (list store)
              `(progn (%set-raw ,store ,tmp) ,store)
              `(getter ,tmp))))
  ;; The same shape, but numeric, and with a (setf …) function rather than an
  ;; expander — so INCF over it exercises the place path too.
  (defun plain (c) (%raw c))
  (define-compiler-macro plain (c) `(%raw ,c))
  (defun (setf plain) (v c) (%set-raw v c))
"#;

#[test]
fn a_place_reached_through_a_symbol_macro_keeps_its_setf_expander() {
    assert_eq!(
        eval(&format!(
            "(progn {ACCESSOR}
               (symbol-macrolet ((slot (getter *cell*)))
                 (setf slot 7))
               (format t \"RESULT:~s\" (list *cell* (getter *cell*))))"
        )),
        "((7) (:OUT 7))"
    );
}

#[test]
fn the_same_place_works_inside_a_function_and_for_several_pairs() {
    assert_eq!(
        eval(&format!(
            "(progn {ACCESSOR}
               (defvar *other* (list 0))
               (defun store-both ()
                 (symbol-macrolet ((a (getter *cell*)) (b (getter *other*)))
                   (setf a 1 b 2)))
               (store-both)
               (format t \"RESULT:~s\" (list *cell* *other*)))"
        )),
        "((1) (2))"
    );
}

#[test]
fn a_read_of_the_same_symbol_macro_still_goes_through_the_compiler_macro() {
    // The place must keep its identity; a READ of it is an ordinary call and the
    // compiler macro applies as usual.
    assert_eq!(
        eval(&format!(
            "(progn {ACCESSOR}
               (symbol-macrolet ((slot (getter *cell*)))
                 (setf slot 5)
                 (format t \"RESULT:~s\" slot)))"
        )),
        "(:OUT 5)"
    );
}

#[test]
fn place_taking_macros_over_such_an_accessor_work_too() {
    // INCF and PUSH expand into SETF, so they inherit the place handling.
    assert_eq!(
        eval(&format!(
            "(progn {ACCESSOR}
               (defun bump ()
                 (symbol-macrolet ((slot (plain *cell*)))
                   (setf slot 10)
                   (incf slot 5)
                   slot))
               (format t \"RESULT:~s\" (bump)))"
        )),
        "15"
    );
}

#[test]
fn an_ordinary_place_is_unchanged() {
    assert_eq!(
        eval(
            r#"(progn (defvar *h* (make-hash-table))
                      (setf (gethash :k *h*) 1)
                      (incf (gethash :k *h*) 2)
                      (let ((v (vector 0 0)))
                        (setf (aref v 1) 9)
                        (format t "RESULT:~s" (list (gethash :k *h*) v))))"#
        ),
        "(3 #(0 9))"
    );
}

#[test]
fn a_place_that_holds_a_place_keeps_both() {
    // THE's type is not a form, and LDB, GETF and VALUES hold places of their own.
    assert_eq!(
        eval(
            r#"(progn (defvar *n* 0)
                      (defvar *plist* (list :head (list :k 1)))
                      (defvar *a* 0) (defvar *b* 0)
                      (setf (the fixnum *n*) 7)
                      (setf (ldb (byte 4 0) *n*) 3)
                      (setf (getf (second *plist*) :k) 9)
                      (setf (values *a* *b*) (values 1 2))
                      (format t "RESULT:~s" (list *n* (getf (second *plist*) :k) *a* *b*)))"#
        ),
        "(3 9 1 2)"
    );
}
