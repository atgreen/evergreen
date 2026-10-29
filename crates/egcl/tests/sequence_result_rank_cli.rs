// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(defun rank-error-p (f) (handler-case (progn (funcall f) nil) (type-error () t)))
(defun check-rank-element (element)
  (dolist (head '(array simple-array))
    (dolist (dimensions '(nil (0 0) 0 2))
      (let ((type (list head element dimensions)))
        (assert (rank-error-p (lambda () (make-sequence type 0))))
        (assert (rank-error-p (lambda () (coerce nil type))))))
    (dolist (dimensions '(1 (*) (0)))
      (let* ((type (list head element dimensions))
             (made (make-sequence type 0))
             (converted (coerce nil type)))
        (assert (= (array-rank made) 1))
        (assert (= (length made) 0))
        (assert (typep made type))
        (assert (typep converted type))))
    (let ((type (list head element '(1))))
      (assert (rank-error-p (lambda () (make-sequence type 0))))
      (assert (rank-error-p (lambda () (coerce nil type)))))))
(dolist (element '(t character bit (unsigned-byte 8)))
  (check-rank-element element))
;; COERCE must return an already matching object even when it is not a sequence.
(let ((matrix (make-array '(0 0))))
  (assert (eq matrix (coerce matrix '(array t (0 0))))))
(assert (= (coerce 3 'complex) 3))
;; String shortcuts must respect fixed lengths, not merely their element type.
(assert (rank-error-p (lambda () (coerce nil '(simple-array character (1))))))
(assert (rank-error-p (lambda () (coerce nil '(string 1)))))
(assert (typep (coerce '(#\a) '(simple-array character (1))) '(simple-array character (1))))
(assert (typep (make-sequence '(vector (unsigned-byte 8) 1) 1 :initial-element 7)
               '(vector (unsigned-byte 8) 1)))
(defvar *rank-effects* nil)
(assert (rank-error-p
          (lambda () (make-sequence (progn (push :type *rank-effects*) '(array t (0 0)))
                                    (progn (push :size *rank-effects*) 0)
                                    :initial-element (progn (push :initial *rank-effects*) nil)))))
(assert (equal (reverse *rank-effects*) '(:type :size :initial)))
(setq *rank-effects* nil)
(assert (rank-error-p
          (lambda () (coerce (progn (push :object *rank-effects*) nil)
                             (progn (push :type *rank-effects*) '(array character (0 0)))))))
(assert (equal (reverse *rank-effects*) '(:object :type)))
(declaim (notinline rank-native-make rank-native-coerce))
(defun rank-native-make (type size) (make-sequence type size))
(defun rank-native-coerce (object type) (coerce object type))
(dotimes (i 120)
  (assert (typep (rank-native-make '(array character (0)) 0) '(array character (0))))
  (assert (typep (rank-native-coerce nil '(array character (0))) '(array character (0)))))
(when *rank-required-tier*
  (dolist (name '(rank-native-make rank-native-coerce))
    (loop repeat 100 until (= (egcl-ext:function-tier name) *rank-required-tier*)
          do (rank-native-make '(array character (0)) 0)
             (rank-native-coerce nil '(array character (0))) (sleep 0.01))
    (assert (= (egcl-ext:function-tier name) *rank-required-tier*))))
(assert (rank-error-p (lambda () (rank-native-make '(array character (0 0)) 0))))
(assert (rank-error-p (lambda () (rank-native-coerce nil '(array character (0 0))))))
(write-line "SEQUENCE-RESULT-RANK-OK")
"#;

#[test]
fn sequence_construction_and_coercion_preserve_rank_and_type() {
    let mut baseline = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let source = format!("(defparameter *rank-required-tier* {required})\n{PROGRAM}");
        command.args(["--no-init", "--eval", &source]);
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(key);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("SEQUENCE-RESULT-RANK-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
