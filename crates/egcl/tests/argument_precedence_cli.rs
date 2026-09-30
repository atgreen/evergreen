// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A generic function's :ARGUMENT-PRECEDENCE-ORDER decides which method runs
//! (CLHS 7.6.6.1.2, bliss-nj6id). CFFI relies on it: its foreign-type argument,
//! not the Lisp value, must pick the translator.
use std::process::Command;

fn run(program: &str) -> (bool, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn declared_precedence_order_decides_the_applicable_method() {
    // Left to right, (list t) wins on its first parameter; the declared order
    // compares the second parameter first, so (t inner) wins instead.
    let program = r#"
      (defclass outer () ())
      (defclass inner (outer) ())
      (defgeneric translate (value kind)
        (:argument-precedence-order kind value))
      (defmethod translate ((value list) (kind outer)) :by-value)
      (defmethod translate (value (kind inner)) :by-kind)
      (dotimes (iteration 5)
        (assert (eq (translate '(1 . 2) (make-instance 'inner)) :by-kind))
        (assert (eq (translate '(1 . 2) (make-instance 'outer)) :by-value))
        (assert (eq (translate 7 (make-instance 'inner)) :by-kind)))
      (format t "PRECEDENCE-OK~%")
    "#;
    let (ok, out, err) = run(program);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("PRECEDENCE-OK"), "{out}\n{err}");
}

#[test]
fn default_order_is_left_to_right_and_compares_one_argument_at_a_time() {
    // Without the option the first parameter decides, and a method that is more
    // specific only on a later parameter must not win: an ordering that added
    // the per-argument distances together let it (bliss-nj6id).
    let program = r#"
      (defclass base () ())
      (defclass derived (base) ())
      (defgeneric pick (a b))
      (defmethod pick ((a list) (b base)) :list-first)
      (defmethod pick (a (b derived)) :derived-second)
      (dotimes (iteration 5)
        (assert (eq (pick '(1) (make-instance 'derived)) :list-first))
        (assert (eq (pick 7 (make-instance 'derived)) :derived-second)))
      (format t "DEFAULT-ORDER-OK~%")
    "#;
    let (ok, out, err) = run(program);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("DEFAULT-ORDER-OK"), "{out}\n{err}");
}

#[test]
fn redefining_without_the_option_restores_the_default_order() {
    let program = r#"
      (defclass outer () ())
      (defclass inner (outer) ())
      (defgeneric translate (value kind)
        (:argument-precedence-order kind value))
      (defmethod translate ((value list) (kind outer)) :by-value)
      (defmethod translate (value (kind inner)) :by-kind)
      (assert (eq (translate '(1 . 2) (make-instance 'inner)) :by-kind))
      (defgeneric translate (value kind))
      (defmethod translate ((value list) (kind outer)) :by-value)
      (defmethod translate (value (kind inner)) :by-kind)
      (assert (eq (translate '(1 . 2) (make-instance 'inner)) :by-value))
      (format t "REDEFINE-OK~%")
    "#;
    let (ok, out, err) = run(program);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("REDEFINE-OK"), "{out}\n{err}");
}

#[test]
fn a_precedence_order_that_is_not_a_permutation_is_rejected() {
    for option in [
        "(:argument-precedence-order kind)",
        "(:argument-precedence-order kind kind)",
        "(:argument-precedence-order kind missing)",
    ] {
        let program = format!("(defgeneric translate (value kind) {option})");
        let (ok, out, err) = run(&program);
        assert!(!ok, "{option} was accepted: {out}\n{err}");
        assert!(
            format!("{out}{err}").contains("ARGUMENT-PRECEDENCE-ORDER"),
            "{option}: {out}\n{err}"
        );
    }
}

#[test]
fn a_parameter_named_method_is_a_parameter_not_an_option() {
    // The lambda list used to be scanned as though it were an option list.
    let program = r#"
      (defgeneric combine (method other))
      (defmethod combine ((method integer) other) (+ method 1))
      (assert (= (combine 41 :ignored) 42))
      (format t "LAMBDA-LIST-OK~%")
    "#;
    let (ok, out, err) = run(program);
    assert!(ok, "{out}\n{err}");
    assert!(out.contains("LAMBDA-LIST-OK"), "{out}\n{err}");
}
