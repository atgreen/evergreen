// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn run(program: &str) -> std::process::Output {
    let timeout = if std::env::var_os("EGCL_GC_STRESS").is_some() {
        "300"
    } else {
        "90"
    };
    Command::new("timeout")
        .args([
            "--kill-after=5",
            timeout,
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .expect("run atomic-operation regression")
}

#[test]
fn cas_supports_all_documented_places_and_compatibility_packages() {
    let output = run(r#"
        (defparameter *atomic-special* 10)
        (defstruct atomic-record (value 20 :type fixnum))
        (defclass atomic-object () ((value :initarg :value)))
        (let* ((record (make-atomic-record))
               (object (make-instance 'atomic-object :value 30))
               (pair (cons 40 50))
               (vector (vector 60))
               (symbol (make-symbol "ATOMIC-CELLS"))
               (evaluations 0))
          (setf (symbol-value symbol) 70
                (symbol-plist symbol) '(80))
          (assert (= 10 (egcl-ext:cas *atomic-special* 10 11)))
          (assert (= 11 (egcl-ext:cas *atomic-special* 10 12)))
          (assert (= 20 (egcl-ext:cas
                          (atomic-record-value (progn (incf evaluations) record))
                          20 21)))
          (assert (= 30 (egcl-ext:cas
                          (slot-value (progn (incf evaluations) object) 'value)
                          30 31)))
          (assert (= 40 (egcl-ext:cas
                          (car (progn (incf evaluations) pair)) 40 41)))
          (assert (= 50 (egcl-ext:cas
                          (cdr (progn (incf evaluations) pair)) 50 51)))
          (assert (= 60 (egcl-ext:cas
                          (svref (progn (incf evaluations) vector) 0) 60 61)))
          (assert (= 70 (egcl-ext:cas
                          (symbol-value (progn (incf evaluations) symbol)) 70 71)))
          (assert (equal '(80)
                         (egcl-ext:cas
                           (symbol-plist (progn (incf evaluations) symbol))
                           (symbol-plist symbol) '(81))))
          (assert (= 7 evaluations))
          (assert (= 11 *atomic-special*))
          (assert (= 21 (atomic-record-value record)))
          (assert (= 31 (slot-value object 'value)))
          (assert (equal '(41 . 51) pair))
          (assert (= 61 (svref vector 0)))
          (assert (= 71 (symbol-value symbol)))
          (assert (equal '(81) (symbol-plist symbol))))
        (dolist (package '(egcl-ext sb-ext sb-thread))
          (assert (find-package package)))
        (dolist (name '("CAS" "ATOMIC-INCF" "ATOMIC-DECF"))
          (multiple-value-bind (egcl status) (find-symbol name :egcl-ext)
            (assert (eq :external status))
            (multiple-value-bind (sb-ext sb-status) (find-symbol name :sb-ext)
              (assert (eq :external sb-status))
              (assert (eq egcl sb-ext)))
            (multiple-value-bind (sb-thread thread-status) (find-symbol name :sb-thread)
              (assert (eq :external thread-status))
              (assert (eq egcl sb-thread)))))
        (format t "ATOMIC-PLACES-OK~%")
        "#);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("ATOMIC-PLACES-OK"), "{stdout}\n{stderr}");
}

#[test]
fn atomic_arithmetic_returns_old_value_checks_overflow_and_is_concurrent() {
    let output = run(r#"
        (defparameter *atomic-counter* 0)
        (defstruct atomic-box (value 5 :type fixnum))
        (let ((box (make-atomic-box))
              (vector (make-array 1 :element-type 'fixnum :initial-element 9)))
          (let ((old (egcl-ext:atomic-incf *atomic-counter* 2)))
            (assert (= 0 old)))
          (assert (= 2 *atomic-counter*))
          (let ((old (egcl-ext:atomic-decf (atomic-box-value box) 3)))
            (assert (= 5 old)))
          (assert (= 2 (atomic-box-value box)))
          (let ((old (egcl-ext:atomic-incf (svref vector 0))))
            (assert (= 9 old)))
          (assert (= 10 (svref vector 0))))
        (defparameter *atomic-overflow* most-positive-fixnum)
        (let ((value *atomic-overflow*))
          (assert (handler-case
                      (progn (egcl-ext:atomic-incf *atomic-overflow*) nil)
                    (arithmetic-error () t)))
          (assert (= value *atomic-overflow*)))
        (setq *atomic-counter* 0)
        (let ((threads nil))
          (dotimes (worker 4)
            (declare (ignore worker))
            (push (egcl-thread:make-thread
                    (lambda ()
                      (dotimes (i 2000)
                        (declare (ignore i))
                        (egcl-ext:atomic-incf *atomic-counter*))))
                  threads))
          (dolist (thread threads) (egcl-thread:join-thread thread)))
        (assert (= 8000 *atomic-counter*))
        (defun compiled-atomic-step ()
          (egcl-ext:atomic-incf *atomic-counter*))
        (compile 'compiled-atomic-step)
        (assert (= 8000 (compiled-atomic-step)))
        (assert (= 8001 *atomic-counter*))
        (format t "ATOMIC-ARITHMETIC-OK~%")
        "#);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("ATOMIC-ARITHMETIC-OK"),
        "{stdout}\n{stderr}"
    );
}
