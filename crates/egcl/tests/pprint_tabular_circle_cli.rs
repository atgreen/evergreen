// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline tabular-output tabular-format))
(defun tabular-output (object)
  (with-output-to-string (stream)
    (pprint-tabular stream object t nil 1)))
(defun tabular-format (object)
  (format nil "~:/pprint-tabular/" object))
(defun check-tabular (object expected)
  (assert (equal (tabular-output object) expected))
  (assert (equal (with-output-to-string (stream) (pprint-fill stream object t)) expected))
  (assert (equal (with-output-to-string (stream) (pprint-linear stream object t)) expected)))
(with-standard-io-syntax
  (let ((*print-pretty* t) (*print-readably* nil) (*print-circle* t)
        (*print-right-margin* 1000))
    (dotimes (i 80)
      (tabular-output '(1 2 3))
      (tabular-format '(1 2 3)))
    (when *tabular-required-tier*
      (assert (= (egcl-ext:function-tier 'tabular-output) *tabular-required-tier*))
      (assert (= (egcl-ext:function-tier 'tabular-format) *tabular-required-tier*)))
    (let* ((shared (list 'a)) (object (list shared shared)))
      (check-tabular object "(#1=(A) #1#)")
      ;; FORMAT's user callback has the same queued graph lifetime.
      (let ((copy (read-from-string (tabular-format object))))
        (assert (= (length copy) 2))
        (assert (eq (first copy) (second copy))))
      (let ((copy (read-from-string (tabular-output object))))
        (assert (= (length copy) 2))
        (assert (eq (first copy) (second copy))))
      (assert (equal (with-output-to-string (stream)
                       (pprint-tabular stream object nil t 1)) "#1=(A) #1#")))
    (let ((object (list 'a)))
      (setf (cdr object) object)
      (check-tabular object "#1=(A . #1#)")
      (let ((copy (read-from-string (tabular-output object))))
        (assert (eq copy (cdr copy)))))
    (let* ((tail (list 'b)) (object (cons tail tail)))
      (check-tabular object "(#1=(B) . #1#)"))
    (let ((object (vector nil)))
      (setf (aref object 0) object)
      (check-tabular (list object object) "(#1=#(#1#) #1#)"))
    (check-tabular '(a . b) "(A . B)")
    (check-tabular nil "()")
    (check-tabular 7 "7")
    (assert (equal (multiple-value-list
                    (pprint-tabular (make-string-output-stream) '(a))) '(nil)))
    (let ((*print-length* 1))
      (check-tabular '(a b c) "(A ...)")
      (let ((*print-readably* t))
        (check-tabular '(a b c) "(A B C)")))
    (let ((*print-level* 1))
      (check-tabular '((a) (b)) "(# #)"))
    (let ((*print-pretty* nil))
      (let ((x (list 'a)))
        (check-tabular (list x x) "(#1=(A) #1#)")))
    ;; Native queued tabs preserve existing column and fill-break behavior.
    (assert (equal (with-output-to-string (stream)
                     (write-string "X " stream)
                     (pprint-tabular stream '(a b c) t nil 4)) "X (A   B   C)"))
    (let ((*print-right-margin* 7))
      (assert (equal (with-output-to-string (stream)
                       (pprint-tabular stream '(aa bb cc) t nil 4))
                     (format nil "(AA~% BB~% CC)"))))))
(defvar *tabular-custom-count* 0)
(defclass tabular-visible () ())
(defmethod print-object ((object tabular-visible) stream)
  (incf *tabular-custom-count*)
  ;; Allocate inside the actual callback; hidden elements must not run it.
  (write-string (copy-seq "VISIBLE") stream)
  object)
(let ((*print-readably* nil) (*print-pretty* t) (*print-circle* t)
      (*print-length* 1) (*tabular-custom-count* 0))
  (assert (equal (tabular-output (list (make-instance 'tabular-visible)
                                     (make-instance 'tabular-visible)))
                 "(VISIBLE ...)"))
  (assert (= *tabular-custom-count* 1)))
(defclass tabular-transfer () ())
(defmethod print-object ((object tabular-transfer) stream)
  (declare (ignore stream))
  (throw 'tabular-exit :escaped))
(assert (eq (catch 'tabular-exit
              (tabular-output (list (make-instance 'tabular-transfer)))) :escaped))
;; A nonlocal callback exit must not leave a circle table or logical block active.
(let ((*print-readably* nil) (*print-circle* t) (*print-pretty* t)
      (*print-right-margin* 1000) (x (list 'a)))
  (assert (equal (tabular-output (list x x)) "(#1=(A) #1#)")))
(write-line "PPRINT-TABULAR-CIRCLE-OK")
"##;

#[test]
fn tabular_preserves_shared_graphs_limits_and_callback_lifetimes() {
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"));
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_T2_LOG",
            "EGCL_T1_LOG",
        ] {
            child.env_remove(key);
        }
        child
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        if stress {
            child
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let program = format!(
            "(defparameter *tabular-required-tier* {}){PROGRAM}",
            match tier {
                "t1" => "1",
                "t2" => "2",
                _ => "nil",
            }
        );
        let output = child
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}/{stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "PPRINT-TABULAR-CIRCLE-OK")
                .count(),
            1
        );
    }
}
