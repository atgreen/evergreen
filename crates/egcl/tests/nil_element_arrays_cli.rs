// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const PROGRAM: &str = r##"
(defun nil-error-p (thunk) (handler-case (progn (funcall thunk) nil) (type-error () t)))
(defun check-nil-array (array dimensions)
  (assert (arrayp array)) (assert (null (array-element-type array)))
  (assert (equal (array-dimensions array) dimensions))
  (assert (typep array (type-of array)))
  (assert (not (typep array '(array character *))))
  (assert (not (typep array '(array bit *)))))
(assert (null (upgraded-array-element-type nil)))
(deftype no-array-element () nil)
(check-nil-array (make-array 0 :element-type 'no-array-element) '(0))
(check-nil-array (make-string 0 :element-type 'no-array-element) '(0))
(check-nil-array (get-output-stream-string (make-string-output-stream :element-type 'no-array-element)) '(0))
(assert (eq (upgraded-array-element-type 'bit) 'bit))
(assert (eq (upgraded-array-element-type 'character) 'character))
(dolist (dimensions '(0 3 nil (2 0) (2 3)))
  (let ((array (make-array dimensions :element-type nil)))
    (check-nil-array array (if (listp dimensions) dimensions (list dimensions)))
    (when (= (array-rank array) 1)
      (assert (stringp array)) (assert (typep array 'simple-string))
      (assert (eq (coerce array '(vector nil)) array))
      (assert (not (typep array 'simple-vector)))
      (assert (= (length array) (array-dimension array 0))))
    (when (> (array-total-size array) 0)
      (assert (nil-error-p (lambda () (row-major-aref array 0))))
      (assert (nil-error-p (lambda () (setf (row-major-aref array 0) #\a)))))
  (assert (nil-error-p (lambda () (make-array dimensions :element-type nil :initial-element nil))))
  (assert (nil-error-p (lambda () (make-array dimensions :element-type nil :initial-element #\a))))))
(check-nil-array (make-array 0 :element-type nil :initial-contents nil) '(0))
(check-nil-array (make-array '(2 0) :element-type nil :initial-contents '(nil nil)) '(2 0))
(assert (nil-error-p (lambda () (make-array 1 :element-type nil :initial-contents '(nil)))))
(dolist (array (list (make-string 0 :element-type nil)
                    (make-sequence '(vector nil) 0)
                    (coerce nil '(vector nil))
                    (concatenate '(vector nil) nil)))
  (check-nil-array array '(0))
  (assert (string= array "")) (assert (equal array ""))
  (assert (string= (princ-to-string array) ""))
  (assert (string= (namestring (pathname array)) ""))
  (assert (= (sxhash array) (sxhash "")))
  (check-nil-array (copy-seq array) '(0))
  (check-nil-array (subseq array 0) '(0))
  (check-nil-array (reverse array) '(0)))
(assert (nil-error-p (lambda () (make-string 0 :element-type nil :initial-element #\a))))
(assert (nil-error-p (lambda () (make-sequence '(vector nil) 0 :initial-element nil))))
(assert (nil-error-p (lambda () (coerce '(#\a) '(vector nil)))))
(assert (nil-error-p (lambda () (concatenate '(vector nil) "a"))))
;; MAP and CONCATENATE must validate rank and declared length before the
;; empty NIL representation shortcut. No callback runs for empty input.
(dolist (type '((array nil ()) (simple-array nil (0 0))))
  (assert (nil-error-p (lambda () (concatenate type nil))))
  (assert (nil-error-p (lambda () (map type #'identity nil)))))
(dolist (type '((vector nil 0) (array nil (0)) (simple-array nil (0))))
  (check-nil-array (concatenate type nil) '(0))
  (check-nil-array (map type #'identity nil) '(0)))
(dolist (type '((vector nil 1) (array nil (1)) (simple-array nil (1))))
  (assert (nil-error-p (lambda () (concatenate type nil))))
  (assert (nil-error-p (lambda () (map type #'identity nil))))
  (assert (nil-error-p (lambda () (map type #'identity '(nil))))))
;; A missing element-type argument remains T, not the explicit NIL type.
(assert (equal (array-element-type (make-sequence '(vector) 0)) t))
(let* ((base (make-array 4 :element-type nil :adjustable t :fill-pointer 0))
       (view (make-array 2 :element-type nil :displaced-to base :displaced-index-offset 1)))
  (check-nil-array base '(4)) (check-nil-array view '(2))
  (assert (zerop (length base)))
  (assert (not (typep base 'simple-string)))
  (assert (eq (nth-value 0 (array-displacement view)) base))
  (assert (= (nth-value 1 (array-displacement view)) 1))
  (assert (nil-error-p (lambda () (vector-push-extend #\a base))))
  (assert (zerop (fill-pointer base)))
  (assert (eq (adjust-array base 6) base))
  (assert (= (array-total-size base) 6))
  (assert (eq (nth-value 0 (array-displacement view)) base))
  (assert (nil-error-p (lambda () (adjust-array base 7 :initial-element nil))))
  (assert (= (array-total-size base) 6)))
(dolist (dimensions '(0 3 nil (2 0) (2 3)))
  (let* ((array (make-array dimensions :element-type nil)) (*print-readably* t)
         (*print-base* 29) (*read-base* 29)
         (copy (read-from-string (write-to-string array))))
    (check-nil-array copy (array-dimensions array))))
(let* ((array (make-array 0 :element-type nil)) (*print-circle* t) (*print-readably* t)
       (copy (read-from-string (format nil "~S" (vector array array)))))
  (assert (eq (aref copy 0) (aref copy 1))) (check-nil-array (aref copy 0) '(0)))
(let* ((s (make-string-output-stream :element-type nil))
       (a (get-output-stream-string s)) (b (get-output-stream-string s)))
  (assert (null (stream-element-type s))) (assert (output-stream-p s))
  (assert (not (eq a b))) (check-nil-array a '(0)) (check-nil-array b '(0))
  (write-string "" s) (write-sequence a s)
  (assert (nil-error-p (lambda () (write-char #\a s))))
  (assert (nil-error-p (lambda () (write-string "abc" s))))
  (assert (nil-error-p (lambda () (write-sequence "abc" s))))
  (assert (nil-error-p (lambda () (terpri s))))
  (check-nil-array (get-output-stream-string s) '(0)))
(defvar *nil-stream-target*)
(let* ((*nil-stream-target* (make-string-output-stream :element-type nil))
       (s (make-synonym-stream '*nil-stream-target*)))
  (assert (null (stream-element-type s)))
  (assert (nil-error-p (lambda () (write-char #\a s))))
  (let ((*nil-stream-target* (make-string-output-stream)))
    (assert (eq (stream-element-type s) 'character))
    (write-char #\b s)
    (assert (equal (get-output-stream-string *nil-stream-target*) "b"))))
;; NIL vectors are strings even when ordinary arrays are printed concisely.
(let* ((base (make-array 4 :element-type nil :adjustable t :fill-pointer 0))
       (displaced (make-array 0 :element-type nil :displaced-to base))
       (simple (make-array 0 :element-type nil))
       (*print-array* nil) (*print-readably* nil))
  (dolist (array (list simple base displaced))
    (assert (stringp array))
    (assert (equal (write-to-string array) "\"\""))
    (assert (equal (princ-to-string array) "")))
  (let ((matrix (make-array '(2 0) :element-type nil)))
    (assert (not (stringp matrix)))
    (assert (not (typep matrix 'string)))
    ;; This is an ordinary non-string array under PRINT-ARRAY NIL; the concise
    ;; printer must not try to load its nonexistent element slots.
    (assert (search "ARRAY" (write-to-string matrix)))))
;; Ordinary string streams and MAKE-STRING remain unchanged.
(assert (equal (with-output-to-string (s) (write-string "abc" s)) "abc"))
(assert (equal (make-string 2 :initial-element #\a) "aa"))
(declaim (notinline nil-native-info nil-native-access nil-native-string))
(defun nil-native-info (n)
  (let ((a (make-array n :element-type nil)))
    (list (length a) (array-element-type a) (stringp a))))
(defun nil-native-string (n) (make-string n :element-type nil))
(defun nil-native-access (a flag)
  (if (zerop flag) (array-total-size a) (char a 0)))
(let ((a (make-array 3 :element-type nil)))
  (dotimes (i 100)
    (assert (equal (nil-native-info 3) '(3 nil t)))
    (assert (= (nil-native-access a 0) 3))
    (check-nil-array (nil-native-string 3) '(3)))
  (when *nil-required-tier*
    (dolist (name '(nil-native-info nil-native-access nil-native-string))
      (loop repeat 100 until (= *nil-required-tier* (egcl-ext:function-tier name))
            do (nil-native-info 3) (nil-native-access a 0) (nil-native-string 3) (sleep 0.01))
      (assert (= *nil-required-tier* (egcl-ext:function-tier name)))))
  (let ((before (egcl-ext:deopt-count)))
    (assert (equal (nil-native-info 3) '(3 nil t)))
    (check-nil-array (nil-native-string 3) '(3))
    (assert (= (nil-native-access a 0) 3))
    (when *nil-required-tier* (assert (= before (egcl-ext:deopt-count)))))
  (assert (nil-error-p (lambda () (nil-native-access a 1)))))
(let ((large (ash 1 40)))
  (assert (equal (nil-native-info large) (list large nil t))))
(assert (nil-error-p (lambda () (make-array array-dimension-limit :element-type nil))))
(assert (nil-error-p (lambda () (make-array (ash 1 70) :element-type nil))))
(assert (handler-case (progn (make-array (list array-dimension-limit 2) :element-type nil) nil) (error () t)))
(write-line "NIL-ELEMENT-ARRAYS-OK")
"##;

fn invoke(program: &str, tier: &str, stress: bool, image: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command
        .args(["--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_LAZY", "0")
        .env("EGCL_T2_THREADS", "1");
    for key in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(key);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    command.output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn nil_elements_preserve_shape_string_semantics_and_native_type_errors() {
    let mut expected = None;
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
        let output = invoke(
            &format!("(defparameter *nil-required-tier* {required}){PROGRAM}"),
            tier,
            stress,
            None,
        );
        success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("NIL-ELEMENT-ARRAYS-OK"));
        if let Some(expected) = &expected {
            assert_eq!(&output.stdout, expected);
        } else {
            expected = Some(output.stdout);
        }
    }
}
#[test]
fn nil_array_literals_share_storage_after_source_free_load_and_core_restore() {
    let dir = std::env::temp_dir().join(format!("egcl-nil-array-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("arrays.lisp");
    let fasl = dir.join("arrays.bfasl");
    let image = dir.join("arrays.core");
    fs::write(&source, r#"(defparameter *nil-graph* '#.(let* ((base (make-array 3 :element-type nil :adjustable t :fill-pointer 0)) (view (make-array 2 :element-type nil :displaced-to base :displaced-index-offset 1))) (vector base base view (make-array nil :element-type nil))))"#).unwrap();
    success(&invoke(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "interp",
        false,
        None,
    ));
    fs::remove_file(source).unwrap();
    let checks = r#"(let ((base (aref *nil-graph* 0)) (view (aref *nil-graph* 2)))
(assert (eq base (aref *nil-graph* 1))) (assert (null (array-element-type base)))
(assert (eq base (nth-value 0 (array-displacement view))))
(assert (= (array-total-size view) 2)) (assert (zerop (length base)))
(assert (= (array-total-size (aref *nil-graph* 3)) 1))
(assert (null (array-element-type (aref *nil-graph* 3)))))"#;
    for stress in [false, true] {
        success(&invoke(
            &format!("(load {fasl:?}){checks}"),
            "t2",
            stress,
            None,
        ));
    }
    success(&invoke(
        &format!("(load {fasl:?})(egcl-ext:save-lisp-and-die {image:?})"),
        "interp",
        false,
        None,
    ));
    for stress in [false, true] {
        success(&invoke(checks, "t2", stress, Some(&image)));
    }
    fs::remove_dir_all(dir).unwrap();
}
