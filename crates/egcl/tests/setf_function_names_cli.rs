// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! `(setf place)` is a function NAME, not only a SETF expansion (bliss-6buay).
//!
//! The places below always worked; the names did not exist, so anything that
//! takes a function designator — `#'(setf slot-value)`, FDEFINITION, APPLY, a
//! FUNCTION type check, mapping over a list of them — received the bare
//! `(SETF x)` cons and signalled a TYPE-ERROR. kitchen-sink does exactly that
//! with `(SETF SLOT-VALUE)`, which is how this was found.
//!
//! Every writer stores through a PRIMITIVE (or RPLACA/RPLACD), never as
//! `(setf (place …) new)`: whether the lowerer emits a direct store or a CALL to
//! the place's writer depends on the surrounding form, so a body that mentions
//! its own place recurses in whichever context takes the call route. The names
//! that still need a store primitive are tracked in bliss-rwpmq. Every
//! expectation below was checked against SBCL first.
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
fn every_standard_setf_writer_is_a_function() {
    // FBOUNDP of the NAME and FUNCTIONP of the designator, for one name of each
    // shape: two primitive-backed writers, a composed accessor writer, and an
    // ordinal.
    assert_eq!(
        eval(
            r#"(format t "RESULT:~s"
                 (mapcar (lambda (n) (list (and (fboundp n) t) (functionp (fdefinition n))))
                         '((setf slot-value) (setf symbol-function) (setf caddr) (setf tenth))))"#
        ),
        "((T T) (T T) (T T) (T T))"
    );
}

#[test]
fn the_slot_value_writer_stores_through_its_designator() {
    // The exact shape kitchen-sink failed on, including via APPLY.
    assert_eq!(
        eval(
            r#"(progn (defclass thing () ((a :initform 0)))
                 (let ((o (make-instance 'thing)) (p (make-instance 'thing)))
                   (funcall #'(setf slot-value) 42 o 'a)
                   (apply #'(setf slot-value) (list 7 p 'a))
                   (format t "RESULT:~s" (list (slot-value o 'a) (slot-value p 'a)))))"#
        ),
        "(42 7)"
    );
}

#[test]
fn the_function_cell_writer_installs_a_definition() {
    assert_eq!(
        eval(
            r#"(progn (funcall #'(setf symbol-function) (lambda (x) (* x 5)) 'quint)
                 (format t "RESULT:~s" (quint 4)))"#
        ),
        "20"
    );
}

#[test]
fn the_list_accessor_writers_store_in_place() {
    assert_eq!(
        eval(
            r#"(let ((l (list 1 2 3 4 5 6 7 8 9 10 11)))
                 (funcall #'(setf nth) :n 1 l)
                 (funcall #'(setf caddr) :c l)
                 (funcall #'(setf fourth) :f l)
                 (funcall #'(setf tenth) :t l)
                 (format t "RESULT:~s" l))"#
        ),
        "(1 :N :C :F 5 6 7 8 9 :T 11)"
    );
}

#[test]
fn a_four_deep_accessor_writer_stores_in_place() {
    assert_eq!(
        eval(
            r#"(let ((deep (list (list (list 1 2) (list 3 4)) 'b 'c 'd 'e)))
                 (funcall #'(setf caaar) :x deep)
                 (funcall #'(setf cdddr) (list :tail) deep)
                 (format t "RESULT:~s" deep))"#
        ),
        "(((:X 2) (3 4)) B C :TAIL)"
    );
}

#[test]
fn the_vector_and_string_writers_store_through_their_designators() {
    assert_eq!(
        eval(
            r#"(let ((s (copy-seq "abcd"))
                     (a (make-array '(2 2) :initial-element 0)))
                 (funcall #'(setf char) #\Z s 0)
                 (funcall #'(setf schar) #\Y s 1)
                 (funcall #'(setf row-major-aref) 9 a 3)
                 (format t "RESULT:~s" (list s (aref a 1 1))))"#
        ),
        "(\"ZYcd\" 9)"
    );
}

#[test]
fn a_writer_body_never_routes_back_through_its_own_place() {
    // The regression that made this rule: a writer written as
    // `(setf (place …) new)` self-recurses wherever the lowerer emits a call to
    // the writer instead of a direct store. It broke cffi-toolchain, cffi-grovel,
    // cffi-libffi and iolib, all of which had been loading. These two shapes —
    // the store at top level of a loaded file and the same store inside a LET —
    // take different routes, so both must survive.
    assert_eq!(
        eval(
            r#"(progn (defclass q () ())
                 (let ((c (find-class 'q))) (setf (find-class 'q2) c))
                 (let ((v (make-array 3 :fill-pointer 1)))
                   (let ((n 2)) (setf (fill-pointer v) n))
                   (format t "RESULT:~s" (list (and (find-class 'q2 nil) t)
                                               (fill-pointer v)))))"#
        ),
        "(T 2)"
    );
}

#[test]
fn a_setf_writer_returns_the_new_value() {
    // CLHS: a setf function returns its new-value argument.
    assert_eq!(
        eval(
            r#"(let ((l (list 1 2 3)))
                 (format t "RESULT:~s" (list (funcall #'(setf nth) :v 0 l)
                                             (funcall #'(setf car) :w l))))"#
        ),
        "(:V :W)"
    );
}
