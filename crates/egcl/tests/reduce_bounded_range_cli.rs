// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! REDUCE walks only its bounded range, in place (bliss-kn680). It used to
//! coerce the whole sequence to a list, copy the range with SUBSEQ and map KEY
//! into a third list before reducing. The results must not change.
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defun probe ()
  (list (reduce #'+ #(1 2 3 4)) (reduce #'+ '(1 2 3 4))
        (reduce #'list '(1 2 3 4) :from-end t) (reduce #'list #(1 2 3 4) :from-end t)
        (reduce #'list '(1 2 3 4)) (reduce #'+ '()) (reduce #'+ #() :initial-value 5)
        (reduce #'list '(1 2 3) :initial-value 0)
        (reduce #'list '(1 2 3) :initial-value 0 :from-end t)
        (reduce #'+ '((1) (2) (3)) :key #'car)
        (reduce #'+ #(1 2 3 4 5) :start 1 :end 3) (reduce #'+ '(1 2 3 4 5) :start 1 :end 3)
        (reduce #'list '(a)) (reduce #'list '(a) :key #'list)
        (reduce #'+ "" :initial-value 0) (reduce #'+ #(1 2 3) :end nil)
        (handler-case (reduce #'+ '(1 2) :end 5) (type-error () :type-error))
        (handler-case (reduce #'+ #(1 2) :start 3) (type-error () :type-error))
        (handler-case (reduce 'cons 'a) (type-error () :type-error))
        (handler-case (reduce 'cons #'car) (type-error () :type-error))
        (reduce #'list '(1 2 3 4) :from-end t :start 1 :end 3 :initial-value 'z)
        (let ((v (make-array 5 :fill-pointer 3 :initial-contents '(1 2 3 4 5))))
          (reduce #'+ v))))
(dotimes (i 20) (probe))
(format t "~&RESULT ~s~%" (probe)))"#;

#[test]
fn reduce_results_over_bounded_ranges() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains(
                "RESULT (10 10 (1 (2 (3 4))) (1 (2 (3 4))) (((1 2) 3) 4) 0 5 (((0 1) 2) 3) \
                 (1 (2 (3 0))) 6 5 5 A (A) 0 6 :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR :TYPE-ERROR \
                 (2 (3 Z)) 6)"
            ),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
