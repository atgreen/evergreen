// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(defun array-access-sum (v n)
  (let ((sum 0)) (dotimes (i n sum) (incf sum (aref v (logand i 7))))))
(defun array-access-double-sum (v n)
  (let ((sum 0d0)) (dotimes (i n sum) (incf sum (the double-float (svref v 0))))))
(defun array-access-packed-store (v n)
  (dotimes (i n (aref v 0)) (setf (aref v 0) (* i 0.125d0))))
(defun array-access-alias (a b)
  (let ((before (aref a 0)))
    (setf (aref b 0) (1+ before))
    (- (aref a 0) before)))
(defun array-access-read (v i) (aref v i))
(defun array-access-discard (v i) (aref v i) :ok)
(let ((fix (make-array 8 :element-type 'fixnum :initial-contents '(0 1 2 3 4 5 6 7)))
      (boxed (vector 3d0))
      (packed (make-array 1 :element-type 'double-float :initial-element 0d0))
      (alias (vector 0)))
  (dotimes (i 500)
    (assert (= (array-access-sum fix 8) 28))
    (assert (= (array-access-double-sum boxed 8) 24d0))
    (assert (= (array-access-packed-store packed 8) 0.875d0))
    (assert (= (array-access-alias alias alias) 1))
    (assert (= (array-access-read fix 7) 7))
    (assert (eq (array-access-discard packed 0) :ok)))
  (assert (= (array-access-sum fix 1000000) 3500000))
  (assert (= (array-access-double-sum boxed 1000000) 3000000d0))
  (assert (= (array-access-packed-store packed 1000000) 124999.875d0))
  (assert (= (egcl-ext:function-tier 'array-access-packed-store) 2))
  (assert (= (array-access-alias alias alias) 1))
  (let* ((fill (make-array 8 :fill-pointer 1 :initial-contents '(0 1 2 3 4 5 6 7)))
         (displaced (make-array 3 :displaced-to fill :displaced-index-offset 2))
         (large (make-array 70000 :initial-element 19)))
    (assert (= (array-access-read fill 7) 7))
    (assert (= (array-access-read displaced 2) 4))
    (assert (= (array-access-read large 69999) 19)))
  (dolist (bad '(-1 8 0.5))
    (assert (eq :error (handler-case (progn (array-access-read fix bad) :bad)
                        (error () :error)))))
  (assert (eq :error (handler-case (array-access-discard packed 1)
                      (error () :error))))
  (format t "ARRAY-ACCESS-OK~%"))
(egcl::%exit 0)
"#;

#[test]
fn array_access_values_aliasing_bounds_and_non_simple_fallbacks() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ARRAY-ACCESS-OK"));
}
