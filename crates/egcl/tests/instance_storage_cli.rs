// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A CLOS update never changes an instance's address (bliss-fmhj5).
//!
//! An instance is a fixed-size shell — header, wrapper, and a reference to a
//! SIMPLE_VECTOR holding its slots — so a class redefinition or CHANGE-CLASS
//! replaces the STORAGE and the wrapper, never the object. SBCL's
//! `%obsolete-instance-trap` is the same two stores:
//!
//! ```text
//! (setf (%instance-layout thing) layout)
//! (%instance-set thing sb-vm:instance-data-start slot-vector)
//! ```
//!
//! Before this, an update ALLOCATED A REPLACEMENT and forwarded the old object
//! to it, and that one mechanism produced four defects in a day: two crashes
//! from callers left holding the old address (bliss-ch80h, bliss-kbo3y), a
//! pinned class metaobject "moving" and tripping the CLOS scanner's assertion
//! (bliss-k4gev), and a forward-referenced class losing EQ when DEFCLASS had to
//! replace its placeholder outright (bliss-qbz4o). The rows below are those
//! cases; they pass because the mechanism is gone, not because it is handled.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run_with(env: &[(&str, &str)], program: &str) -> String {
    let mut command = Command::new("timeout");
    command.args(["--kill-after=10", "600", BIN, "--no-init", "--eval", program]);
    // Inherited stress settings would make these rows measure the harness.
    command
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT")
        .env_remove("EGCL_GC_POISON");
    for (k, v) in env {
        command.env(k, v);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

fn run(program: &str) -> String {
    run_with(&[], program)
}

fn expect(text: &str, rows: &[&str]) {
    for row in rows {
        assert!(text.contains(row), "missing {row:?} in:\n{text}");
    }
}

/// Identity survives every shape of update: a redefinition that adds slots, one
/// that removes them, a reordering, and CHANGE-CLASS to a class with more slots
/// (which used to take the grow-and-forward branch).
#[test]
fn an_instance_keeps_its_address_across_every_update() {
    let stdout = run(r#"
      (defclass c () ((a :initform 1)))
      (defvar *i* (make-instance 'c))
      (defvar *alias* *i*)
      (defvar *h* (make-hash-table :test 'eq))
      (setf (gethash *i* *h*) :keyed)
      (setf (slot-value *i* 'a) 5)

      ;; Grow.
      (eval '(defclass c () ((a :initform 1) (b :initform 2))))
      (format t "GROW-ALIAS ~a~%" (eq *i* *alias*))
      (format t "GROW-HASH ~a~%" (gethash *i* *h*))
      (format t "GROW-KEPT ~a~%" (slot-value *i* 'a))
      ;; Shrink.
      (eval '(defclass c () ((a :initform 1))))
      (format t "SHRINK-ALIAS ~a~%" (eq *i* *alias*))
      (format t "SHRINK-HASH ~a~%" (gethash *i* *h*))
      (format t "SHRINK-KEPT ~a~%" (slot-value *i* 'a))
      ;; Reorder, same count: values must follow their NAMES, not their slots.
      (eval '(defclass c () ((z :initform 9) (a :initform 1))))
      (format t "REORDER-KEPT ~a~%" (slot-value *i* 'a))
      (format t "REORDER-NEW ~a~%" (slot-value *i* 'z))
      (format t "REORDER-ALIAS ~a~%" (eq *i* *alias*))

      ;; CHANGE-CLASS to a wider class: the old growth path forwarded here.
      (defclass wide () ((a :initform 1) (p :initform 7) (q :initform 8)))
      (defvar *j* (make-instance 'c))
      (defvar *jalias* *j*)
      (setf (slot-value *j* 'a) 3)
      (change-class *j* 'wide)
      (format t "CHANGE-ALIAS ~a~%" (eq *j* *jalias*))
      (format t "CHANGE-KEPT ~a~%" (slot-value *j* 'a))
      (format t "CHANGE-NEW ~a~%" (list (slot-value *j* 'p) (slot-value *j* 'q)))
      (format t "STORAGE-OK~%")
    "#);
    expect(&stdout, &[
        "GROW-ALIAS T", "GROW-HASH KEYED", "GROW-KEPT 5",
        "SHRINK-ALIAS T", "SHRINK-HASH KEYED", "SHRINK-KEPT 5",
        "REORDER-KEPT 5", "REORDER-NEW 9", "REORDER-ALIAS T",
        "CHANGE-ALIAS T", "CHANGE-KEPT 3", "CHANGE-NEW (7 8)",
        "STORAGE-OK",
    ]);
}

/// bliss-k4gev: a metaclass that gains a slot gives its existing classes that
/// slot, with its initform, and the class metaobject does not move — which is
/// the part that mattered, because ClosState keys HashMaps by its address and
/// the root scanner asserts on exactly that.
///
/// Run under GC stress too: the assertion fires during a collection, so a
/// non-stress run cannot see it. Before this change that run panicked with
/// "a class metaobject moved: class_meta's keys need rehashing, not marking".
#[test]
fn a_metaclass_that_gains_a_slot_does_not_move_its_classes() {
    let program = r#"
      (defclass meta (standard-class) ((x :initform 1 :accessor meta-x)))
      (defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
      (defclass foo () () (:metaclass meta))
      (defvar *before* (find-class 'foo))
      (setf (meta-x (find-class 'foo)) 41)
      (eval '(defclass meta (standard-class)
               ((x :initform 1 :accessor meta-x) (y :initform 2 :accessor meta-y))))
      (format t "CLASS-EQ ~a~%" (eq *before* (find-class 'foo)))
      (format t "KEPT ~a~%" (meta-x (find-class 'foo)))
      (format t "GAINED ~a~%" (meta-y (find-class 'foo)))
      (format t "TYPEP ~a~%" (typep (find-class 'foo) 'meta))
      (format t "INSTANCE ~a~%" (typep (make-instance 'foo) 'foo))
      ;; Churn so collections run after the metaclass redefinition. Small on
      ;; purpose: under stress every allocation forces one (bliss-v2b6o).
      (dotimes (i 300) (list i i i))
      (format t "AFTER-GC ~a ~a~%" (eq *before* (find-class 'foo)) (meta-y (find-class 'foo)))
      (format t "METACLASS-GROWTH-OK~%")
    "#;
    let rows = [
        "CLASS-EQ T",
        "KEPT 41",
        "GAINED 2",
        "TYPEP T",
        "INSTANCE T",
        "AFTER-GC T 2",
        "METACLASS-GROWTH-OK",
    ];
    expect(&run(program), &rows);
    expect(
        &run_with(
            &[
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
                ("EGCL_GC_POISON", "1"),
            ],
            program,
        ),
        &rows,
    );
}

/// bliss-qbz4o: a reference captured BEFORE the definition stays EQ to the
/// defined class, even when the definition names a metaclass with slots.
///
/// DEFCLASS used to replace the placeholder outright, because inline slots
/// could not grow; the captured reference then named an orphan with no name and
/// no metadata.
#[test]
fn a_placeholder_captured_before_its_definition_stays_eq() {
    let stdout = run(r#"
      (defclass meta (standard-class) ((x :initform 1 :accessor meta-x)))
      (defmethod egcl-mop:validate-superclass ((c meta) (s standard-class)) t)
      (defgeneric cc (c))
      (defmethod cc ((c meta)) :meta)
      (defclass sub (undef) ())
      (defvar *ph* (find-class 'undef))
      (eval '(defclass undef () () (:metaclass meta)))
      (format t "EQ ~a~%" (eq *ph* (find-class 'undef)))
      (format t "NAME ~a~%" (class-name *ph*))
      (format t "TYPEP ~a~%" (typep (find-class 'undef) 'meta))
      ;; The metaclass's initform ran over cells that did not exist when the
      ;; placeholder was minted, and it reads through the CAPTURED reference.
      (format t "INITFORM ~a~%" (meta-x (find-class 'undef)))
      (format t "VIA-CAPTURED ~a~%" (meta-x *ph*))
      (format t "DISPATCH ~a~%" (cc *ph*))
      (format t "SUB-INHERITS ~a~%" (typep (make-instance 'sub) 'undef))
      (format t "PLACEHOLDER-OK~%")
    "#);
    expect(&stdout, &[
        "EQ T", "NAME UNDEF", "TYPEP T", "INITFORM 1",
        "VIA-CAPTURED 1", "DISPATCH META", "SUB-INHERITS T",
        "PLACEHOLDER-OK",
    ]);
}

/// A saved image carries the storage vector like any other heap object, and a
/// restored instance keeps its slots, its written values, and its EQ identity.
#[test]
fn instances_survive_an_image_round_trip() {
    let dir = std::env::temp_dir().join("egcl-instance-storage-image");
    std::fs::create_dir_all(&dir).expect("temp dir");
    let image = dir.join("round-trip.img");
    let _ = std::fs::remove_file(&image);
    let save = format!(
        r#"(progn
             (defclass c () ((a :initform 1) (b :initform 2)))
             (defvar *i* (make-instance 'c))
             (setf (slot-value *i* 'a) 42)
             (defvar *h* (make-hash-table :test 'eq))
             (setf (gethash *i* *h*) :keyed)
             (egcl-ext:save-lisp-and-die {:?}))"#,
        image.to_string_lossy()
    );
    run(&save);
    let output = Command::new("timeout")
        .args(["--kill-after=10", "600", BIN, "--image"])
        .arg(&image)
        .args([
            "--eval",
            r#"(progn
                 (format t "SLOT-A ~a~%" (slot-value *i* 'a))
                 (format t "SLOT-B ~a~%" (slot-value *i* 'b))
                 (format t "EQ-HASH ~a~%" (gethash *i* *h*))
                 (setf (slot-value *i* 'b) 7)
                 (format t "WRITE ~a~%" (slot-value *i* 'b))
                 (format t "FRESH ~a~%" (slot-value (make-instance 'c) 'a))
                 (format t "IMAGE-OK~%"))"#,
        ])
        .output()
        .expect("restore image");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(output.status.success(), "{stdout}");
    expect(&stdout, &[
        "SLOT-A 42", "SLOT-B 2", "EQ-HASH KEYED", "WRITE 7", "FRESH 1", "IMAGE-OK",
    ]);
    let _ = std::fs::remove_file(&image);
}
