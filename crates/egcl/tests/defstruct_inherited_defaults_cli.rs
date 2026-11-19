// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const SOURCE: &str = r#"
(defstruct (scalar-root (:constructor nil))
  (left nil) (flag t) (count 17 :type integer) (kind :node))
(defstruct (scalar-middle (:include scalar-root) (:constructor nil)) (level 3))
(defstruct (scalar-child (:include scalar-middle)
                         (:constructor make-scalar-boa (index))) index)
(defstruct (scalar-keyed (:include scalar-middle (count 23) (kind :child))) extra)
(let ((x (make-scalar-boa 9)) (y (make-scalar-keyed))
      (z (make-scalar-keyed :count 31 :left :supplied)))
  (assert (null (scalar-child-left x)))
  (assert (eq (scalar-child-flag x) t))
  (assert (= (scalar-child-count x) 17))
  (assert (eq (scalar-child-kind x) :node))
  (assert (= (scalar-child-level x) 3))
  (assert (= (scalar-child-index x) 9))
  (assert (= (scalar-keyed-count y) 23))
  (assert (eq (scalar-keyed-kind y) :child))
  (assert (= (scalar-keyed-count z) 31))
  (assert (eq (scalar-keyed-left z) :supplied)))
(assert (handler-case (progn (make-scalar-keyed :count :bad) nil)
          (type-error () t)))
;; The actual generated initfunctions and source metadata remain usable.
(let ((slots (egcl-mop:class-direct-slots (find-class 'scalar-root))))
  (assert (equal (mapcar #'egcl-mop:slot-definition-initform slots)
                 '(nil t 17 :node)))
  (assert (equal (mapcar (lambda (s) (funcall (egcl-mop:slot-definition-initfunction s))) slots)
                 '(nil t 17 :node))))
;; Nonliteral defaults keep the defining lexical environment, allocation
;; identity, left-to-right effects, and supplied-p suppression.
(defparameter *default-order* nil)
(let ((captured 40))
  (defstruct (effect-root (:constructor nil))
    (first (progn (push :first *default-order*) (list captured)))
    (second (progn (push :second *default-order*) (+ captured 2))))
  (setf captured 41))
(defstruct (effect-child (:include effect-root)))
(assert (null *default-order*))
(let ((a (make-effect-child)) (b (make-effect-child :second 99)))
  (assert (equal *default-order* '(:first :second :first)))
  (assert (equal (effect-child-first a) '(41)))
  (assert (equal (effect-child-first b) '(41)))
  (assert (not (eq (effect-child-first a) (effect-child-first b))))
  (assert (= (effect-child-second a) 43))
  (assert (= (effect-child-second b) 99)))
;; Literal advertised source does not replace an explicitly supplied function.
(let* ((calls 0) (function (lambda () (incf calls) 73))
       (class (egcl-mop:ensure-class 'scalar-explicit
                :direct-superclasses (list (find-class 'standard-object))
                :direct-slots (list (list :name 'value :initargs '(:value)
                                         :initform 17 :initfunction function)))))
  (let ((slot (first (egcl-mop:class-direct-slots class))))
    (assert (eq (egcl-mop:slot-definition-initfunction slot) function))
    (assert (= (egcl-mop:slot-definition-initform slot) 17)))
  (assert (= (slot-value (make-instance class) 'value) 73))
  (assert (= calls 1))
  (assert (= (slot-value (make-instance class :value 8) 'value) 8))
  (assert (= calls 1)))
(write-line "INHERITED-DEFAULTS-OK")
"#;

fn run(args: &[&str], stress: bool, markers: usize) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init").args(args);
    for key in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(key);
    }
    if stress {
        command.envs([
            ("EGCL_GC_STRESS", "1"),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ]);
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "stress={stress}: {}\n{stdout}\n{stderr}",
        output.status
    );
    assert_eq!(
        stdout
            .lines()
            .filter(|s| s.trim() == "INHERITED-DEFAULTS-OK")
            .count(),
        markers,
        "{stdout}\n{stderr}"
    );
}

#[test]
fn inherited_scalar_defaults_preserve_initialization_protocol_in_source_and_fasl() {
    let dir = std::env::temp_dir().join(format!("egcl-inherited-defaults-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("defaults.lisp");
    let artifact = dir.join("defaults.bfasl");
    fs::write(&source, SOURCE).unwrap();
    for stress in [false, true] {
        run(&["--load", source.to_str().unwrap()], stress, 1);
    }
    run(
        &[
            "--eval",
            &format!(
                "(multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?}) (declare (ignore file warnings)) (assert (not failure)))"
            ),
        ],
        false,
        0,
    );
    fs::remove_file(&source).unwrap();
    for stress in [false, true] {
        run(
            &[
                "--eval",
                &format!("(progn (load {artifact:?}) (load {artifact:?}))"),
            ],
            stress,
            2,
        );
    }
    fs::remove_dir_all(dir).unwrap();
}
