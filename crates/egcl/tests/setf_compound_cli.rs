//! Compound places must use a custom place's storing form, not SETF its getter.
use std::process::Command;

fn check(body: &str) {
    let program = format!(
        r#"
        (defvar *place-events* nil)
        (define-setf-expander opaque-place (cell &environment env)
          (declare (ignore env))
          (let ((tmp (gensym)) (new (gensym)))
            (values (list tmp) (list cell) (list new)
                    `(progn (push :store *place-events*) (rplaca ,tmp ,new) ,new)
                    `(progn (push :read *place-events*) (car ,tmp)))))
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
    check(
        r#"
        (define-setf-expander multiple-store-place (cell)
          (let ((tmp (gensym)) (new (gensym)) (secondary (gensym)))
            (values (list tmp) (list cell) (list new secondary)
                    `(progn (assert (null ,secondary)) (rplaca ,tmp ,new) ,new)
                    `(values (car ,tmp) :ignored))))
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
