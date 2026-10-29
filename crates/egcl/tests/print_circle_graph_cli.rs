// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defun graph-outputs (object)
  (list (write-to-string object)
        (prin1-to-string object)
        (format nil "~S" object)
        (with-output-to-string (stream) (prin1 object stream))))
(defun graph-check (object expected)
  (dolist (pretty '(nil t))
    (let ((*print-circle* t) (*print-pretty* pretty) (*print-readably* nil))
      (dolist (text (graph-outputs object))
        (assert (equal text expected))))))
(let ((v (vector 25 26 27)))
  (setf (aref v 2) v)
  (graph-check v "#1=#(25 26 #1#)"))
(let ((a (make-array '(2 2) :initial-contents '((5 6) (7 8)))))
  (setf (aref a 1 1) a)
  (graph-check a "#1=#2A((5 6) (7 #1#))"))
(let ((v (vector 1 2))) (graph-check (list v v) "(#1=#(1 2) #1#)"))
(let ((v (list 1 2))) (graph-check (vector v v) "#(#1=(1 2) #1#)"))
(let ((s (make-symbol "X"))) (graph-check (cons s s) "(#1=#:X . #1#)"))
(let ((a (make-symbol "X")) (b (make-symbol "X")))
  (graph-check (list a b a b) "(#1=#:X #2=#:X #1# #2#)"))
(let* ((v (make-array 1 :adjustable t)) (cell (list v)))
  (setf (aref v 0) cell)
  (graph-check v "#1=#((#1#))"))
(let* ((base (vector nil nil nil))
       (view (make-array 1 :displaced-to base :displaced-index-offset 1)))
  (setf (aref base 1) view)
  (graph-check view "#1=#(#1#)"))
(let ((v (make-array 2 :fill-pointer 1 :initial-element 1)))
  (setf (aref v 1) v)
  (graph-check v "#(1)"))
(let ((a (make-array nil)))
  (setf (row-major-aref a 0) a)
  (graph-check a "#1=#0A#1#"))
(let* ((s (make-symbol "X")) (v (vector s s)))
  (let ((*print-circle* t) (*print-pretty* nil))
    (dolist (text (graph-outputs v))
      (let ((copy (read-from-string text)))
        (assert (eq (aref copy 0) (aref copy 1)))
        (assert (not (eq s (aref copy 0))))))))
(let* ((package (make-package "CIRCLE-GRAPH-HOME" :use nil))
       (uninterned (intern "X" package)) (deleted (intern "Y" package)))
  (unintern uninterned package)
  (graph-check (list uninterned uninterned) "(#1=#:X #1#)")
  (delete-package package)
  (graph-check (list deleted deleted) "(#1=#:Y #1#)"))
(dolist (text (list "abc" (copy-seq "abc")))
  (graph-check (list text text) "(#1=\"abc\" #1#)"))
(let ((v (make-array 2 :element-type '(unsigned-byte 8) :initial-contents '(1 2))))
  (let ((*print-circle* t) (*print-pretty* nil) (*print-readably* t))
    (dolist (text (graph-outputs (list v v)))
      (let ((copy (read-from-string text)))
        (assert (eq (first copy) (second copy)))
        (assert (= 2 (aref (first copy) 1)))))))
(let* ((v (vector 1 2)) (*print-circle* t) (*print-pretty* nil) (*print-level* 0))
  (dolist (text (graph-outputs (vector v v))) (assert (equal text "#"))))
(graph-check (list nil t nil t) "(NIL T NIL T)")
(format t "PRINT-CIRCLE-GRAPH-OK~%")
"##;

#[test]
fn circular_arrays_and_shared_symbols_print_with_eq_labels() {
    let mut baseline = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", PROGRAM]);
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(key);
        }
        command.env("EGCL_FORCE_TIER", tier);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("PRINT-CIRCLE-GRAPH-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
