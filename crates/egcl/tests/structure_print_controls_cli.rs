// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defstruct s-empty)
(defstruct s-pair a b)
(defun structure-render (object mode)
  (case mode
    (:write (write-to-string object))
    (:stream (with-output-to-string (stream) (write object :stream stream)))
    (:format-s (format nil "~S" object))
    (:format-a (format nil "~A" object))))
(dolist (pretty '(nil t))
  (dolist (mode '(:write :stream :format-s :format-a))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-level* 0))
      (assert (equal (structure-render (make-s-empty) mode) "#"))
      (assert (equal (structure-render (make-s-pair :a 1 :b 2) mode) "#")))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-level* 1))
      (assert (equal (structure-render (list (make-s-empty)) mode) "(#)"))
      (assert (equal (structure-render (make-s-pair :a (make-s-empty) :b '(7 8)) mode)
                     "#S(S-PAIR :A # :B #)")))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-level* 2))
      (assert (equal (structure-render (list (make-s-pair :a 1 :b 2) '(7 8)) mode)
                     "(#S(S-PAIR :A 1 :B 2) (7 8))")))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-length* 0))
      (assert (equal (structure-render (make-s-empty) mode) "#S(S-EMPTY)"))
      (assert (equal (structure-render (make-s-pair :a 1 :b 2) mode) "#S(S-PAIR ...)")))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-length* 1))
      (assert (equal (structure-render (make-s-pair :a 1 :b 2) mode) "#S(S-PAIR :A 1 ...)")))
    (let ((*print-pretty* pretty) (*print-readably* nil) (*print-length* 2))
      (assert (equal (structure-render (make-s-pair :a 1 :b 2) mode) "#S(S-PAIR :A 1 :B 2)")))))
;; Readable output overrides both truncation controls and round-trips slots.
(dolist (mode '(:write :stream :format-s))
  (let* ((*print-readably* t) (*print-level* 0) (*print-length* 0)
         (text (structure-render (make-s-pair :a '(1 2) :b "value") mode))
         (copy (read-from-string text)))
    (assert (typep copy 's-pair))
    (assert (equal (s-pair-a copy) '(1 2)))
    (assert (equal (s-pair-b copy) "value"))))
;; The fallback policy must not replace an explicit user PRINT-OBJECT method.
(defvar *structure-custom-calls* 0)
(defun structure-custom-printer (object stream depth)
  (declare (ignore object depth))
  (incf *structure-custom-calls*) (write-string "CUSTOM" stream))
(defstruct s-custom value)
(defmethod print-object ((object s-custom) stream)
  (structure-custom-printer object stream 0))
(defstruct s-method value)
(defmethod print-object ((object s-method) stream)
  (declare (ignore object))
  (incf *structure-custom-calls*) (write-string "METHOD" stream))
(dolist (mode '(:write :stream :format-s :format-a))
  (let ((*print-length* 0) (*print-level* nil) (*print-readably* nil))
    (let ((before *structure-custom-calls*))
      (assert (equal (structure-render (make-s-custom) mode) "CUSTOM"))
      (assert (= *structure-custom-calls* (1+ before))))
    (let ((before *structure-custom-calls*))
      (assert (equal (structure-render (make-s-method) mode) "METHOD"))
      (assert (= *structure-custom-calls* (1+ before))))))
(let ((*print-pprint-dispatch* (copy-pprint-dispatch nil))
      (*print-pretty* t) (*print-length* 0) (*print-level* nil))
  (set-pprint-dispatch 's-pair (lambda (stream object) (declare (ignore object))
                               (write-string "PPRINT" stream)) 10)
  (assert (equal (write-to-string (make-s-pair)) "PPRINT")))
;; Preserve ordinary circle printing; structure-slot discovery is separate debt.
(let* ((tail (list 17)) (data (list tail tail)) (*print-circle* t))
  (assert (search "#1=" (write-to-string data))))
;; A depth-limited fallback never enters an imaginary/circular slot traversal.
(let ((object (make-s-pair :b 2)) (*print-level* 1) (*print-circle* t))
  (setf (s-pair-a object) object)
  (assert (equal (write-to-string object) "#S(S-PAIR :A # :B 2)")))
(declaim (notinline native-structure-write native-structure-format))
(defun native-structure-write (object) (write-to-string object))
(defun native-structure-format (object) (format nil "~S" object))
(let ((object (make-s-pair :a 1 :b 2)))
  (dotimes (i 120) (native-structure-write object) (native-structure-format object))
  (when *structure-required-tier*
    (dolist (name '(native-structure-write native-structure-format))
      (loop repeat 100 until (= (egcl-ext:function-tier name) *structure-required-tier*)
            do (native-structure-write object) (native-structure-format object) (sleep 0.01))
      (assert (= (egcl-ext:function-tier name) *structure-required-tier*))))
  (let ((*print-level* 0))
    (assert (equal (native-structure-write object) "#"))
    (assert (equal (native-structure-format object) "#")))
  (let ((*print-length* 1))
    (assert (equal (native-structure-write object) "#S(S-PAIR :A 1 ...)"))
    (assert (equal (native-structure-format object) "#S(S-PAIR :A 1 ...)"))))
;; Sibling/default controls also prove the per-structure depth was restored.
(assert (equal (write-to-string '(1 2) :level 0) "#"))
(assert (equal (write-to-string #(1 2) :length 1) "#(1 ...)"))
(assert (equal (write-to-string (make-s-pair :a 1 :b 2)) "#S(S-PAIR :A 1 :B 2)"))
(write-line "STRUCTURE-PRINT-CONTROLS-OK")
"##;

#[test]
fn default_structure_printers_share_depth_and_length_policy() {
    let mut baseline = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let source = format!("(defparameter *structure-required-tier* {required})\n{PROGRAM}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
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
        assert!(String::from_utf8_lossy(&output.stdout).contains("STRUCTURE-PRINT-CONTROLS-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
