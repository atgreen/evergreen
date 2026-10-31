// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

/// A typed structure's writers store through the dispatch fast path once the
/// value satisfies the slot's type in the instance's own class. Anything else
/// still runs the writer body: a correctable TYPE-ERROR with STORE-VALUE. As in
/// SBCL, a parent's writer checks only the parent's declared slot type, not a
/// type the :INCLUDE narrowed.
#[test]
fn typed_structure_writers_check_the_instance_class_type() {
    let program = r#"
      (progn
        (defstruct st (a 0 :type fixnum) (b nil))
        (defstruct (sub (:include st (b nil :type (or null string)))))
        (defun try (thunk)
          (handler-case (progn (funcall thunk) :stored)
            (type-error (e) (list :type-error (type-error-datum e)
                                  (type-error-expected-type e)))))
        (let ((p (make-st)) (q (make-sub)))
          (dotimes (i 3) (setf (st-a p) i (st-b p) i))
          (format t "RESULT:~S~%"
                  (list (try (lambda () (setf (st-a p) 5)))
                        (st-a p)
                        (try (lambda () (setf (st-a p) "x")))
                        (st-a p)
                        (try (lambda () (setf (st-b p) 'anything)))
                        (try (lambda () (setf (st-b q) "ok")))
                        (try (lambda () (setf (st-b q) 'not-a-string)))
                        (st-b q)
                        (handler-bind ((type-error
                                         (lambda (c)
                                           (declare (ignore c))
                                           (invoke-restart 'store-value 42))))
                          (setf (st-a p) :bad)
                          (st-a p))))))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains(
            r#"RESULT:(:STORED 5 (:TYPE-ERROR "x" FIXNUM) 5 :STORED :STORED :STORED NOT-A-STRING 42)"#
        ),
        "{stdout}"
    );
}
