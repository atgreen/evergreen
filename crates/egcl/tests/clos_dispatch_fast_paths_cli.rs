// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The generic-dispatch fast paths keep CLOS semantics: a DEFCLASS accessor
//! read or written directly after method selection, the per-generic mask of
//! specialized argument positions, and the per-class Gray-stream answer.
//! Each program calls the operation often enough for its callers to tier up,
//! from compiled functions as well as from top level.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str) -> String {
    let out = Command::new("timeout")
        .args(["--kill-after=5", "120", BIN, "--no-init", "--eval", program])
        .output()
        .expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    assert!(
        out.status.success(),
        "program failed: {program}\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn accessors_read_and_write_slots() {
    let out = run(r#"
        (progn
          (defclass fp-box () ((a :initarg :a :accessor fp-a)
                               (b :initform 2 :reader fp-b :writer set-fp-b)))
          (defun bump (box) (setf (fp-a box) (1+ (fp-a box))))
          (let ((box (make-instance 'fp-box :a 0)))
            (dotimes (i 500) (bump box))
            (assert (= (fp-a box) 500))
            (assert (= (setf (fp-a box) 7) 7))
            (assert (equal (multiple-value-list (fp-a box)) '(7)))
            (dotimes (i 50) (set-fp-b i box))
            (assert (= (fp-b box) 49)))
          (format t "ACCESSORS-OK~%"))
    "#);
    assert!(out.contains("ACCESSORS-OK"), "{out}");
}

#[test]
fn an_unbound_slot_still_signals_through_its_reader() {
    let out = run(r#"
        (progn
          (defclass fp-maybe () ((v :accessor fp-v)))
          (defun read-v (x) (fp-v x))
          (let ((x (make-instance 'fp-maybe)))
            (dotimes (i 50)
              (assert (eq :unbound (handler-case (read-v x) (unbound-slot () :unbound)))))
            (setf (fp-v x) 3)
            (dotimes (i 50) (assert (= (read-v x) 3))))
          (format t "UNBOUND-OK~%"))
    "#);
    assert!(out.contains("UNBOUND-OK"), "{out}");
}

#[test]
fn user_methods_on_an_accessor_still_run() {
    let out = run(r#"
        (progn
          (defclass fp-wrapped () ((v :initform 1 :accessor fp-w)))
          (defun read-w (x) (fp-w x))
          (let ((x (make-instance 'fp-wrapped)))
            (dotimes (i 50) (assert (= (read-w x) 1)))
            (defmethod fp-w :around ((x fp-wrapped)) (* 10 (call-next-method)))
            (dotimes (i 50) (assert (= (read-w x) 10)))
            (defmethod (setf fp-w) :before (new (x fp-wrapped)) (assert (integerp new)))
            (dotimes (i 50) (setf (fp-w x) i))
            (assert (= (read-w x) 490)))
          (defclass fp-replaced () ((v :initform 1 :accessor fp-r)))
          (let ((x (make-instance 'fp-replaced)))
            (dotimes (i 50) (assert (= (fp-r x) 1)))
            (defmethod fp-r ((x fp-replaced)) :replaced)
            (dotimes (i 50) (assert (eq (fp-r x) :replaced))))
          (format t "USER-METHODS-OK~%"))
    "#);
    assert!(out.contains("USER-METHODS-OK"), "{out}");
}

#[test]
fn class_slots_stay_shared_and_obsolete_instances_migrate() {
    let out = run(r#"
        (progn
          (defclass fp-shared () ((n :allocation :class :initform 0 :accessor fp-n)))
          (let ((one (make-instance 'fp-shared)) (two (make-instance 'fp-shared)))
            (dotimes (i 50) (setf (fp-n one) i))
            (assert (= (fp-n two) 49)))
          (defclass fp-old () ((a :initform 1 :accessor fp-old-a)))
          (let ((x (make-instance 'fp-old)))
            (dotimes (i 50) (setf (fp-old-a x) i))
            (defclass fp-old () ((z :initform :new) (a :initform 1 :accessor fp-old-a)))
            (setf (fp-old-a x) (list :after))
            (assert (equal (fp-old-a x) '(:after)))
            (assert (slot-exists-p x 'z)))
          (format t "SLOTS-OK~%"))
    "#);
    assert!(out.contains("SLOTS-OK"), "{out}");
}

#[test]
fn reader_edge_cases_are_unchanged() {
    let out = run(r#"
        (progn
          (defclass fp-edge () ((v :initform 1 :accessor fp-e)))
          (let ((x (make-instance 'fp-edge)))
            (dotimes (i 50) (fp-e x))
            (assert (null (funcall #'fp-e nil)))
            (assert (eq :arity (handler-case (funcall #'fp-e x x) (program-error () :arity))))
            (assert (eq :error (handler-case (funcall #'(setf fp-e) 1) (error () :error)))))
          (format t "EDGES-OK~%"))
    "#);
    assert!(out.contains("EDGES-OK"), "{out}");
}

#[test]
fn a_newly_specialized_argument_takes_part_in_dispatch() {
    let out = run(r#"
        (progn
          (defgeneric fp-pick (a b))
          (defmethod fp-pick ((a integer) b) (list :int b))
          (defun pick (a b) (fp-pick a b))
          (dotimes (i 50) (assert (equal (pick 1 "x") '(:int "x"))))
          (defmethod fp-pick ((a integer) (b string)) :string)
          (dotimes (i 50)
            (assert (eq (pick 1 "x") :string))
            (assert (equal (pick 1 :k) '(:int :k))))
          (format t "MASK-OK~%"))
    "#);
    assert!(out.contains("MASK-OK"), "{out}");
}

#[test]
fn redefining_a_class_as_a_gray_stream_takes_effect() {
    let out = run(r#"
        (progn
          (defclass fp-src () ((n :initform 0 :accessor fp-src-n)))
          (let ((x (make-instance 'fp-src)))
            (dotimes (i 20)
              (assert (eq :not-a-stream (handler-case (read-byte x) (type-error () :not-a-stream)))))
            (defclass fp-src (egcl-gray-streams:fundamental-binary-input-stream)
              ((n :initform 0 :accessor fp-src-n)))
            (defmethod egcl-gray-streams:stream-read-byte ((s fp-src)) (incf (fp-src-n s)))
            (let ((y (make-instance 'fp-src)))
              (dotimes (i 20) (read-byte y))
              (assert (= (read-byte y) 21))))
          (format t "GRAY-OK~%"))
    "#);
    assert!(out.contains("GRAY-OK"), "{out}");
}

#[test]
fn accessors_behave_the_same_after_an_image_restore() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path =
        std::env::temp_dir().join(format!("egcl-accessor-{}-{nonce}.bimg", std::process::id()));
    let image = path.to_str().unwrap();
    let save = format!(
        r#"
        (progn
          (defclass fp-saved () ((v :initform 0 :accessor fp-s)
                                 (u :accessor fp-u)))
          (defun touch (x) (setf (fp-s x) (1+ (fp-s x))))
          (let ((x (make-instance 'fp-saved))) (dotimes (i 50) (touch x)))
          (save-lisp-and-die "{image}"))
        "#
    );
    run(&save);
    let out = Command::new("timeout")
        .args([
            "--kill-after=5",
            "120",
            BIN,
            "--image",
            image,
            "--no-init",
            "--eval",
            r#"(let ((x (make-instance 'fp-saved)))
                 (dotimes (i 50) (touch x))
                 (assert (= (fp-s x) 50))
                 (assert (eq :unbound (handler-case (fp-u x) (unbound-slot () :unbound))))
                 (format t "RESTORED-OK~%"))"#,
        ])
        .output()
        .expect("run restored image");
    let _ = std::fs::remove_file(&path);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("RESTORED-OK"),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}
