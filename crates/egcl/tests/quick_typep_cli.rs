// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Constant standard type checks answered without the general TYPEP. A
//! structure slot check or a compiled `(typep x 'null)` used to reach the
//! evaluated builtin through the cold call path, about a microsecond each;
//! the leaf helper and the evaluated handlers now decide NULL, LIST, SYMBOL,
//! BOOLEAN, fixnum ranges, MEMBER and OR/AND/NOT of them from the object's
//! representation. Every answer must equal the general path's, which
//! EGCL_DISABLE_QUICK_TYPEP selects, at every tier and for both TYPEP and
//! %DECLARATION-TYPEP.
//!
//! A DEFTYPE name is decided by its expansion, which the evaluated handlers
//! record the first time they meet the name and forget when DEFTYPE or a new
//! class changes what the name could mean.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defstruct qt-point x)
  (defstruct (qt-point3 (:include qt-point)) z)
  (defclass qt-class () ())
  (defvar *qt-objects*
    (list nil t 0 -1 1 127 128 -128 -129 255 256 65535 65536
          most-positive-fixnum most-negative-fixnum
          (1+ most-positive-fixnum) (1- most-negative-fixnum)
          (expt 2 64) (- (expt 2 64)) (expt 2 130)
          #\a 1.5 1.5d0 1/2 #c(1 2) "str" (vector 1 2)
          (make-array 3 :element-type 'bit) (make-hash-table)
          'qt-sym :qt-kw '(1 . 2) (list 1 2) (lambda (x) x) #'car
          (make-qt-point) (make-qt-point3) (make-instance 'qt-class)
          *standard-output* (find-package "CL")))
  (defmacro qt-checks (&rest specs)
    `(list ,@(loop for spec in specs
                   collect `(cons (lambda (x) (typep x ',spec))
                                  (lambda (x) (egcl::%declaration-typep x ',spec))))))
  (defun qt-row ()
    (let ((checks (qt-checks
                   null boolean symbol cons list atom fixnum integer character
                   structure-object t nil keyword
                   (integer 0 10) (integer -5 (5)) (integer (0) *) (integer * 3)
                   (integer 0 18446744073709551616) integer (integer)
                   (unsigned-byte 8) (unsigned-byte 32) (unsigned-byte 64)
                   (unsigned-byte *) unsigned-byte (signed-byte 8) (signed-byte 30)
                   (signed-byte 64) signed-byte (mod 256) (mod 1)
                   (member :a :qt-kw 1 #\a) (member) (eql 1) (eql :qt-kw)
                   (or null symbol) (or null qt-point) (or fixnum character)
                   (and integer (not fixnum)) (and) (or) (not null)
                   (not (or cons null)) (or (member 1 2) (integer 100 200)))))
      (with-output-to-string (out)
        (dolist (check checks)
          (dolist (object *qt-objects*)
            (write-char (if (funcall (car check) object) #\1 #\0) out)
            (write-char (if (funcall (cdr check) object) #\1 #\0) out))
          (write-char #\/ out)))))
  (dotimes (i 30) (qt-row))
  (format t "MATRIX ~A~%" (qt-row)))
"#;

/// The program's stdout, after checking it succeeded.
fn run(program: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", program]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// The rest of the stdout line that starts with `label`.
fn line(stdout: &str, label: &str) -> String {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(label))
        .unwrap_or_else(|| panic!("no {label} line in:\n{stdout}"))
        .to_owned()
}

const TIERS: [Option<&str>; 5] = [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")];

fn tier_env(tier: Option<&str>) -> Vec<(&str, &str)> {
    tier.map(|t| ("EGCL_FORCE_TIER", t)).into_iter().collect()
}

#[test]
fn constant_type_checks_answer_as_the_general_path_at_every_tier() {
    let oracle = line(
        &run(PROGRAM, &[("EGCL_DISABLE_QUICK_TYPEP", "1")]),
        "MATRIX ",
    );
    for tier in TIERS {
        let matrix = line(&run(PROGRAM, &tier_env(tier)), "MATRIX ");
        assert_eq!(matrix, oracle, "tier {tier:?}");
    }
}

const DEFTYPE_PROGRAM: &str = r#"
(progn
  (defstruct qt-point x)
  (defvar *qt-expansions* 0)
  (deftype qt-index () (incf *qt-expansions*) '(integer 0 1000))
  (deftype qt-maybe-index () '(or qt-index null))
  (deftype qt-alias () 'qt-index)
  (deftype qt-point-or-nil () '(or qt-point null))
  (deftype qt-anything () 't)
  (deftype qt-callable () 'function)
  (deftype qt-even () '(and integer (satisfies evenp)))
  (deftype qt-later () 'fixnum)
  ;; Used only by the hot loop, so nothing has recorded QT-INNER before the
  ;; loop first meets it inside QT-OUTER's expansion.
  (deftype qt-inner () (incf *qt-expansions*) '(integer 0 1000))
  (deftype qt-outer () '(or qt-inner null))
  (defvar *qt-objects*
    (list nil t 0 -1 5 1000 1001 (expt 2 70) #\a 1.5 "str" 'qt-sym '(1 . 2)
          (lambda (x) x) #'car (make-qt-point) *standard-output*))
  (defmacro qt-checks (&rest specs)
    `(list ,@(loop for spec in specs
                   collect `(cons (lambda (x) (typep x ',spec))
                                  (lambda (x) (egcl::%declaration-typep x ',spec))))))
  (defun qt-row ()
    (let ((checks (qt-checks
                   qt-index qt-maybe-index qt-alias qt-point-or-nil qt-anything
                   qt-callable qt-even qt-later (or qt-index symbol)
                   (or null qt-alias) (not qt-index) (and qt-index (not (eql 0))))))
      (with-output-to-string (out)
        (dolist (check checks)
          (dolist (object *qt-objects*)
            (write-char (if (funcall (car check) object) #\1 #\0) out)
            (write-char (if (funcall (cdr check) object) #\1 #\0) out))
          (write-char #\/ out)))))
  (dotimes (i 30) (qt-row))
  (defun qt-hot ()
    (let ((before *qt-expansions*) (hits 0))
      (dotimes (i 5000)
        (when (typep i 'qt-outer) (incf hits))
        (when (egcl::%declaration-typep i '(or symbol qt-outer)) (incf hits)))
      (format t "HOT ~D ~D~%" hits (- *qt-expansions* before))))
  (qt-hot)
  (format t "MATRIX ~A " (qt-row))
  ;; Redefining a type, and defining a class under a DEFTYPE's name, change
  ;; the answers from then on.
  (deftype qt-index () '(integer 0 5))
  (defstruct qt-later y)
  (push (make-qt-later) *qt-objects*)
  (dotimes (i 30) (qt-row))
  (format t "~A~%" (qt-row))
  (format t "EXPANSIONS ~D~%" *qt-expansions*))
"#;

/// DEFTYPE names answer as the general path does at every tier, before and
/// after a type is redefined and a class takes a DEFTYPE's name, and the quick
/// check really answers them: 10000 checks of fixnums against a DEFTYPE whose
/// expansion names another run the inner, counting expander at most once
/// instead of on every check.
#[test]
fn deftype_names_answer_as_the_general_path_and_skip_their_expanders() {
    let general = run(DEFTYPE_PROGRAM, &[("EGCL_DISABLE_QUICK_TYPEP", "1")]);
    let oracle = line(&general, "MATRIX ");
    assert_eq!(line(&general, "HOT "), "2002 10000", "general path");
    for tier in TIERS {
        let quick = run(DEFTYPE_PROGRAM, &tier_env(tier));
        assert_eq!(line(&quick, "MATRIX "), oracle, "tier {tier:?}");
        let hot = line(&quick, "HOT ");
        assert!(
            hot == "2002 0" || hot == "2002 1",
            "tier {tier:?}: hits and expansions {hot}"
        );
    }
}
