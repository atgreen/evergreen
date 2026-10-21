// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! GH #507: a generic function created implicitly by DEFMETHOD has a lambda
//! list with &KEY but no keyword names (CLHS 7.6.1), so later methods may
//! accept different keywords. An explicit DEFGENERIC's keyword names are
//! still enforced (CLHS 7.6.4), and call-time keyword checking still uses the
//! union of the applicable methods' keywords (CLHS 7.6.5).

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defclass gh507-a () ()) (defclass gh507-b () ())
  (defmethod gh507-f ((x gh507-a) &key p) p)
  (defmethod gh507-f ((x gh507-b) &key q) q)
  (defgeneric gh507-g (x &key k))
  (defmethod gh507-g ((x gh507-a) &key k) k)
  (defmethod gh507-h ((x gh507-a) &optional (y 3) &rest r &key m &allow-other-keys &aux (z 1))
    (list y r m z))
  (defmethod gh507-h ((x gh507-b) &optional y &key n) (list y n))
  (flet ((rejects (form) (handler-case (progn (eval form) :accepted)
                           (program-error () :rejected))))
    (format t "RESULT ~S~%"
      (list (gh507-f (make-instance 'gh507-a) :p 1)
            (gh507-f (make-instance 'gh507-b) :q 2)
            (rejects '(gh507-f (make-instance 'gh507-a) :zz 1))
            (rejects '(gh507-f (make-instance 'gh507-a) :q 1))
            (gh507-f (make-instance 'gh507-a) :zz 1 :allow-other-keys t)
            (rejects '(defmethod gh507-g ((x gh507-b) &key q) q))
            (rejects '(defmethod gh507-f ((x gh507-b) y) y))
            (rejects '(defmethod gh507-f ((x gh507-b)) x))
            (gh507-h (make-instance 'gh507-a))
            (gh507-h (make-instance 'gh507-b) 1 :n 2)
            (get 'gh507-f 'egcl::%generic-lambda-list)
            (get 'gh507-h 'egcl::%generic-lambda-list))))
"#;

const EXPECTED: &str = "RESULT (1 2 :REJECTED :REJECTED NIL :REJECTED :REJECTED :REJECTED \
(3 NIL NIL 1) (1 2) (X &KEY) (X &OPTIONAL Y &REST R &KEY))";

#[test]
fn implicit_generic_function_accepts_methods_with_different_keywords() {
    for stress in [false, true] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--eval", PROGRAM]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "stress={stress}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains(EXPECTED), "stress={stress}: {stdout}");
    }
}
