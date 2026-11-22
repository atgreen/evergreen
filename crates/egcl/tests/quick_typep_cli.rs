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

fn run(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", PROGRAM]);
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
        .lines()
        .find(|line| line.starts_with("MATRIX "))
        .unwrap_or_else(|| panic!("{envs:?}: no MATRIX line in:\n{stdout}"))
        .to_owned()
}

#[test]
fn constant_type_checks_answer_as_the_general_path_at_every_tier() {
    let oracle = run(&[("EGCL_DISABLE_QUICK_TYPEP", "1")]);
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let envs: Vec<(&str, &str)> = tier.map(|t| ("EGCL_FORCE_TIER", t)).into_iter().collect();
        assert_eq!(run(&envs), oracle, "tier {tier:?}");
    }
}
