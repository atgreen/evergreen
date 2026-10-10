// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn check(program: &str, expected: &[&str]) {
    for (tier, native, installed) in [
        ("interp", "0", 0),
        ("t0", "0", 0),
        ("t1", "0", 1),
        ("t2", "0", 2),
        ("t2", "1", 2),
    ] {
        if tier == "t2" && !cfg!(all(target_arch = "x86_64", unix)) {
            continue;
        }
        if tier == "t1" && !cfg!(target_arch = "x86_64") {
            continue;
        }
        let out = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_NATIVE_TRANSFER", native)
            .env("EGCL_LAZY_COMPILE", "0")
            .output()
            .expect("run callable binding regression");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        let tier_line = format!("TIER {installed}");
        assert!(
            out.status.success()
                && stdout.lines().any(|line| line == tier_line)
                && expected
                    .iter()
                    .all(|expected| stdout.lines().any(|line| line == *expected)),
            "tier={tier}, native={native}: {}\n{stdout}\n{stderr}",
            out.status
        );
    }
}

fn check_reader_replacement(setup: &str) {
    check(
        &format!(
            r#"
      {setup}
      (defparameter *effects* nil)
      (defun warm-reader (object) (probe-reader (progn (push :argument *effects*) object)))
      (dotimes (i 40) (warm-reader *object*))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-reader))
      (setf (symbol-function 'probe-reader)
        (lambda (object) (declare (ignore object))
          (push :replacement *effects*) (values 110 :replacement)))
      (defun cold-reader (object) (probe-reader (progn (push :argument *effects*) object)))
      (setq *effects* nil)
      (format t "READERS ~S ~S~%"
        (multiple-value-list (warm-reader *object*))
        (multiple-value-list (cold-reader *object*)))
      (format t "EFFECTS ~S~%" *effects*)
    "#
        ),
        &[
            "READERS (110 :REPLACEMENT) (110 :REPLACEMENT)",
            "EFFECTS (:REPLACEMENT :ARGUMENT :REPLACEMENT :ARGUMENT)",
        ],
    );
}

#[test]
fn class_reader_observes_replaced_function() {
    check_reader_replacement(
        r#"
      (defclass probe () ((value :initform 7 :reader probe-reader)))
      (defparameter *object* (make-instance 'probe))
    "#,
    );
}

#[test]
fn structure_reader_observes_replaced_function() {
    check_reader_replacement(
        r#"
      (defstruct (probe (:conc-name probe-)) (reader 7))
      (defparameter *object* (make-probe))
    "#,
    );
}

#[test]
fn class_reader_observes_added_around_method() {
    check(
        r#"
      (defclass probe () ((value :initform 7 :reader probe-reader)))
      (defparameter *object* (make-instance 'probe))
      (defparameter *effects* nil)
      (defun warm-reader (object) (probe-reader (progn (push :argument *effects*) object)))
      (dotimes (i 40) (warm-reader *object*))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-reader))
      (defmethod probe-reader :around ((object probe))
        (declare (ignore object)) (push :around *effects*) (values 110 :around))
      (setq *effects* nil)
      (format t "READER ~S~%" (multiple-value-list (warm-reader *object*)))
      (format t "EFFECTS ~S~%" *effects*)
    "#,
        &["READER (110 :AROUND)", "EFFECTS (:AROUND :ARGUMENT)"],
    );
}

#[test]
fn class_reader_preserves_arity_errors() {
    check(
        r#"
      (defclass probe () ((value :initform 7 :reader probe-reader)))
      (defparameter *object* (make-instance 'probe))
      (defun warm-reader (object) (probe-reader object))
      (dotimes (i 40) (warm-reader *object*))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-reader))
      (format t "ARITY ~S~%"
        (list
          (handler-case (probe-reader) (program-error () :program) (error () :wrong))
          (handler-case (probe-reader *object* *object*)
            (program-error () :program) (error () :wrong))
          (handler-case (funcall #'probe-reader)
            (program-error () :program) (error () :wrong))
          (handler-case (funcall #'probe-reader *object* *object*)
            (program-error () :program) (error () :wrong))))
    "#,
        &["ARITY (:PROGRAM :PROGRAM :PROGRAM :PROGRAM)"],
    );
}

#[test]
fn literal_memory_fence_observes_replaced_function() {
    check(
        r#"
      (defparameter *effects* nil)
      (defun warm-fence () (push :caller *effects*) (egcl::%memory-fence :full))
      (dotimes (i 40) (warm-fence))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-fence))
      (setf (symbol-function 'egcl::%memory-fence)
        (lambda (kind) (push kind *effects*) (values 330 :fence)))
      (defun cold-fence () (push :caller *effects*) (egcl::%memory-fence :full))
      (setq *effects* nil)
      (format t "FENCES ~S ~S~%"
        (multiple-value-list (warm-fence)) (multiple-value-list (cold-fence)))
      (format t "EFFECTS ~S~%" *effects*)
    "#,
        &[
            "FENCES (330 :FENCE) (330 :FENCE)",
            "EFFECTS (:FULL :CALLER :FULL :CALLER)",
        ],
    );
}

#[test]
fn class_writer_preserves_replacement_and_argument_order() {
    check(
        r#"
      (defclass probe () ((value :initform 7 :accessor probe-accessor)))
      (defparameter *object* (make-instance 'probe))
      (defparameter *effects* nil)
      (defun warm-writer (object value)
        (setf (probe-accessor (progn (push :object *effects*) object))
              (progn (push :value *effects*) value)))
      (dotimes (i 40) (warm-writer *object* 7))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-writer))
      (setf (fdefinition '(setf probe-accessor))
        (lambda (value object) (declare (ignore value object))
          (push :replacement *effects*) (values 220 :writer)))
      (setq *effects* nil)
      (format t "WRITER ~S~%" (multiple-value-list (warm-writer *object* 9)))
      (format t "EFFECTS ~S SLOT ~S~%" *effects* (slot-value *object* 'value))
    "#,
        &[
            "WRITER (220 :WRITER)",
            "EFFECTS (:REPLACEMENT :VALUE :OBJECT) SLOT 7",
        ],
    );
}

#[test]
fn memory_fence_guard_checks_the_evaluated_argument() {
    check(
        r#"
      (defparameter *effects* nil)
      (defun warm-fence (invalid)
        (egcl::%memory-fence
          (if (progn (push :argument *effects*) invalid) :invalid :full)))
      (dotimes (i 40) (warm-fence nil))
      (format t "TIER ~D~%" (egcl-ext:function-tier 'warm-fence))
      (setq *effects* nil)
      (format t "INVALID-FENCE ~S~%"
        (handler-case (warm-fence t)
          (program-error () :program-error)))
      (format t "EFFECTS ~S~%" *effects*)
    "#,
        &["INVALID-FENCE :PROGRAM-ERROR", "EFFECTS (:ARGUMENT)"],
    );
}
