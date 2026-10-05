// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compound places must use a custom place's storing form, not SETF its getter.
use std::process::Command;

fn check(body: &str) {
    check_with_setup("", body);
}

fn check_with_setup(setup: &str, body: &str) {
    let program = format!(
        r#"
        (defvar *place-events* nil)
        (define-setf-expander opaque-place (cell &environment env)
          (declare (ignore env))
          (let ((tmp (gensym)) (new (gensym)))
            (values (list tmp) (list cell) (list new)
                    `(progn (push :store *place-events*) (rplaca ,tmp ,new) ,new)
                    `(progn (push :read *place-events*) (car ,tmp)))))
        {setup}
        (defun exercise-compound-place () {body})
        (dotimes (i 3) (exercise-compound-place))
        (format t "COMPOUND-PLACE-OK~%")
        "#
    );
    for tier in ["interp", "t0", "t1"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T1_THRESHOLD", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("COMPOUND-PLACE-OK"));
    }
}

#[test]
fn getf_composes_custom_writeback_and_evaluates_subforms_once() {
    check(
        r#"
        (dolist (initial '(nil (:key 3)))
          (setf *place-events* nil)
          (let ((cell (list (copy-list initial))))
            (assert (= 42 (setf (getf (opaque-place (progn (push :cell *place-events*) cell))
                                     (progn (push :indicator *place-events*) :key)
                                     (progn (push :default *place-events*) 0))
                               (progn (push :new *place-events*) 42))))
            (assert (equal (car cell) '(:key 42)))
            (assert (equal (reverse *place-events*)
                           '(:cell :indicator :default :new :read :store)))))
        "#,
    );
}

#[test]
fn ldb_composes_custom_writeback_inside_the() {
    check(
        r#"
        (setf *place-events* nil)
        (let ((cell (list #xa0)))
          (assert (= 7 (setf (ldb (progn (push :bytes *place-events*) (byte 4 0))
                                 (the integer
                                   (opaque-place (progn (push :cell *place-events*) cell))))
                            (progn (push :new *place-events*) 7))))
          (assert (= (car cell) #xa7))
          (assert (equal (reverse *place-events*) '(:bytes :cell :new :read :store))))
        "#,
    );
}

#[test]
fn mask_field_composes_custom_writeback_and_preserves_other_bits() {
    check(
        r#"
        (setf *place-events* nil)
        (let ((cell (list #x0a)))
          (assert (= #x70 (setf (mask-field (progn (push :bytes *place-events*) (byte 4 4))
                                          (opaque-place (progn (push :cell *place-events*) cell)))
                               (progn (push :new *place-events*) #x70))))
          (assert (= (car cell) #x7a))
          (assert (equal (reverse *place-events*) '(:bytes :cell :new :read :store))))
        "#,
    );
}

#[test]
fn nested_getf_expansion_uses_all_inner_store_variables() {
    check_with_setup(
        r#"
        (define-setf-expander multiple-store-place (cell)
          (let ((tmp (gensym)) (new (gensym)) (secondary (gensym)))
            (values (list tmp) (list cell) (list new secondary)
                    `(progn (assert (null ,secondary)) (rplaca ,tmp ,new) ,new)
                    `(values (car ,tmp) :ignored))))
        "#,
        r#"
        (let ((cell (list (list :bits #xa0))))
          (assert (= 7 (setf (ldb (byte 4 0)
                                 (getf (multiple-store-place cell) :bits)) 7)))
          (assert (= (getf (car cell) :bits) #xa7))
          (assert (= #xa8 (incf (getf (multiple-store-place cell) :bits))))
          (setf (the list (getf (multiple-store-place cell) :extra)) '(9))
          (assert (equal (getf (car cell) :extra) '(9))))
        "#,
    );
}

#[test]
fn decf_composes_custom_writeback_after_the_delta() {
    check(
        r#"
        (setf *place-events* nil)
        (let ((cell (list 10)))
          (assert (= 7 (decf (opaque-place (progn (push :cell *place-events*) cell))
                             (progn (push :delta *place-events*) 3))))
          (assert (= (car cell) 7))
          (assert (equal (reverse *place-events*) '(:cell :delta :read :store))))
        "#,
    );
}

#[test]
fn remf_shiftf_and_rotatef_use_custom_storing_forms() {
    check(
        r#"
        (setf *place-events* nil)
        (let ((cell (list (list :a 1 :b 2))))
          (assert (remf (opaque-place (progn (push :cell *place-events*) cell))
                        (progn (push :indicator *place-events*) :a)))
          (assert (equal (car cell) '(:b 2)))
          (assert (equal (reverse *place-events*)
                         '(:cell :indicator :read :store))))

        (setf *place-events* nil)
        (let ((left (list 1)) (right (list 2)))
          (assert (= 1 (shiftf (opaque-place left) (opaque-place right) 3)))
          (assert (= (car left) 2))
          (assert (= (car right) 3))
          (assert (equal (reverse *place-events*)
                         '(:read :read :store :store))))

        (setf *place-events* nil)
        (let ((left (list 1)) (right (list 2)))
          (assert (null (rotatef (opaque-place left) (opaque-place right))))
          (assert (= (car left) 2))
          (assert (= (car right) 1))
          (assert (equal (reverse *place-events*)
                         '(:read :read :store :store))))
        "#,
    );
}

#[test]
fn shiftf_and_rotatef_preserve_multiple_store_values() {
    check_with_setup(
        r#"
        (define-setf-expander pair-place (cell)
          (let ((tmp (gensym)) (first (gensym)) (second (gensym)))
            (values (list tmp) (list cell) (list first second)
                    `(progn (rplaca ,tmp ,first) (rplacd ,tmp ,second)
                            (values ,first ,second))
                    `(values (car ,tmp) (cdr ,tmp)))))
        "#,
        r#"
        (let ((left (cons 1 2)) (right (cons 3 4)))
          (assert (equal (multiple-value-list
                           (shiftf (pair-place left) (pair-place right)
                                   (values 5 6)))
                         '(1 2)))
          (assert (equal left '(3 . 4)))
          (assert (equal right '(5 . 6))))
        (let ((left (cons 1 2)) (right (cons 3 4)))
          (assert (null (rotatef (pair-place left) (pair-place right))))
          (assert (equal left '(3 . 4)))
          (assert (equal right '(1 . 2))))
        "#,
    );
}

#[test]
fn psetf_and_values_setf_use_custom_storing_forms() {
    check(
        r#"
        (setf *place-events* nil)
        (let ((left (list 1)) (right (list 2)))
          (assert (null
                    (psetf (opaque-place (progn (push :left *place-events*) left))
                           (progn (push :left-value *place-events*) 10)
                           (opaque-place (progn (push :right *place-events*) right))
                           (progn (push :right-value *place-events*) 20))))
          (assert (= (car left) 10))
          (assert (= (car right) 20))
          (assert (equal (reverse *place-events*)
                         '(:left :left-value :right :right-value :store :store))))

        (setf *place-events* nil)
        (let ((left (list 1)) (right (list 2)))
          (assert (equal (multiple-value-list
                           (setf (values (opaque-place left) (opaque-place right))
                                 (values 30 40)))
                         '(30 40)))
          (assert (= (car left) 30))
          (assert (= (car right) 40))
          (assert (equal (reverse *place-events*) '(:store :store))))
        "#,
    );
}

#[test]
fn psetf_preserves_multiple_store_values() {
    check_with_setup(
        r#"
        (define-setf-expander pair-place (cell)
          (let ((tmp (gensym)) (first (gensym)) (second (gensym)))
            (values (list tmp) (list cell) (list first second)
                    `(progn (rplaca ,tmp ,first) (rplacd ,tmp ,second)
                            (values ,first ,second))
                    `(values (car ,tmp) (cdr ,tmp)))))
        "#,
        r#"
        (let ((cell (cons 1 2)))
          (assert (null (psetf (pair-place cell) (values 5 6))))
          (assert (equal cell '(5 . 6))))
        "#,
    );
}

#[test]
fn getf_returns_only_the_new_value_when_its_writer_returns_multiple_values() {
    check(
        r#"
        (define-setf-expander multiple-value-writer (cell)
          (let ((tmp (gensym)) (new (gensym)))
            (values (list tmp) (list cell) (list new)
                    `(progn (rplaca ,tmp ,new) (values ,new :extra))
                    `(car ,tmp))))
        (dolist (initial '(nil (:key 3)))
          (let ((cell (list (copy-list initial))))
            (assert (equal (multiple-value-list
                             (setf (getf (multiple-value-writer cell) :key) 42))
                           '(42)))
            (assert (equal (car cell) '(:key 42)))))
        "#,
    );
}

#[test]
fn native_class_accessor_expansion_supports_compound_updates() {
    check(
        r#"
        (defclass compound-place-cell ()
          ((value :initarg :value :accessor compound-place-value)))
        (let ((cell (make-instance 'compound-place-cell :value 41)))
          (assert (= 42 (incf (compound-place-value cell))))
          (assert (= 40 (decf (compound-place-value cell) 2)))
          (setf (compound-place-value cell) (list :bits #xa0))
          (assert (= 7 (setf (ldb (byte 4 0)
                                 (getf (compound-place-value cell) :bits)) 7)))
          (assert (= #xa7 (getf (compound-place-value cell) :bits))))
        "#,
    );
}

#[test]
fn native_structure_accessor_expansion_supports_compound_updates() {
    check(
        r#"
        (defstruct compound-place-record value)
        (let ((cell (make-compound-place-record :value 41)))
          (assert (= 42 (incf (compound-place-record-value cell))))
          (assert (= 40 (decf (compound-place-record-value cell) 2)))
          (assert (= 7 (setf (ldb (byte 4 0)
                                 (compound-place-record-value cell)) 7)))
          (assert (= 39 (compound-place-record-value cell))))
        "#,
    );
}

#[test]
fn native_accessor_expansion_preserves_an_explicit_setf_writer() {
    check(
        r#"
        (defclass explicit-writer-cell ()
          ((value :initarg :value :accessor explicit-writer-value)))
        (defun (setf explicit-writer-value) (new cell)
          (setf (slot-value cell 'value) (+ new 100))
          new)
        (let ((cell (make-instance 'explicit-writer-cell :value 41)))
          (assert (= 42 (incf (explicit-writer-value cell))))
          (assert (= 142 (slot-value cell 'value))))
        "#,
    );
}

#[test]
fn class_accessor_expansion_preserves_an_explicit_setf_method() {
    check(
        r#"
        (defclass method-writer-cell ()
          ((value :initarg :value :accessor method-writer-value)))
        (defmethod (setf method-writer-value) (new (cell method-writer-cell))
          (setf (slot-value cell 'value) (+ new 100))
          new)
        (let ((cell (make-instance 'method-writer-cell :value 41)))
          (assert (= 42 (incf (method-writer-value cell))))
          (assert (= 142 (slot-value cell 'value))))
        "#,
    );
}
