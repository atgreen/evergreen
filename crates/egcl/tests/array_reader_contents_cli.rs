// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r##"
(defun read-array-contents (rank form)
  (read-from-string (format nil "#~dA#.~s" rank form)))
(defun check-array-contents (rank form expected)
  (let ((array (read-array-contents rank form)))
    (assert (= (array-rank array) rank))
    (assert (equalp array expected))))
(dotimes (i 80) (read-array-contents 1 '(vector 1 2)))
(when *array-required-tier*
  (loop repeat 100
        until (= *array-required-tier* (egcl-ext:function-tier 'read-array-contents))
        do (read-array-contents 1 '(vector 1 2)) (sleep 0.01))
  (assert (= *array-required-tier* (egcl-ext:function-tier 'read-array-contents))))
(check-array-contents 1 '(make-array 8 :fill-pointer 3 :initial-element 17) #(17 17 17))
(check-array-contents 1 '(make-array 3 :adjustable t :initial-element 19) #(19 19 19))
(check-array-contents 1 '(make-array '(3) :adjustable t :initial-contents '(21 22 23))
  #(21 22 23))
(check-array-contents 1
  '(make-array 3 :displaced-to (vector 0 1 2 3 4) :displaced-index-offset 1) #(1 2 3))
(check-array-contents 1
  '(make-array 3 :element-type '(unsigned-byte 8)
    :displaced-to (make-array 6 :element-type '(unsigned-byte 8)
                  :initial-contents '(0 11 22 33 44 55))
    :displaced-index-offset 2) #(22 33 44))
(check-array-contents 1 '(make-array 4 :element-type 'character :fill-pointer 2
                         :initial-contents "abcd") #(#\a #\b))
(check-array-contents 1 '(make-array 4 :element-type 'bit :fill-pointer 3
                         :initial-contents '(1 0 1 0)) #(1 0 1))
(check-array-contents 2
  '(vector (make-array 4 :fill-pointer 2 :initial-contents '(1 2 90 91))
           (make-array 2 :displaced-to (vector 80 3 4 81) :displaced-index-offset 1))
  #2A((1 2) (3 4)))
(check-array-contents 2
  '(make-array 2 :adjustable t
    :initial-contents (list (make-array 3 :fill-pointer 0)
                           (make-array 4 :fill-pointer 0)))
  #2A(() ()))
(let ((array (read-array-contents 2
               '(vector (make-array 16 :element-type 'double-float :adjustable t
                         :initial-element 1.25d0)
                        (make-array 16 :element-type 'double-float :fill-pointer 16
                         :initial-element 2.5d0)))))
  (assert (equal (array-dimensions array) '(2 16)))
  (dotimes (i 16)
    (assert (typep (aref array 0 i) 'double-float))
    (assert (= (aref array 0 i) 1.25d0))
    (assert (= (aref array 1 i) 2.5d0))))
(defvar *rank-zero-contents* (make-array 3 :adjustable t :initial-element 'retained))
(let ((array (read-from-string "#0A#.*rank-zero-contents*")))
  (assert (null (array-dimensions array)))
  (assert (eq (row-major-aref array 0) *rank-zero-contents*)))
;; A multidimensional array is not a sequence; higher-rank contents still
;; require nested sequences, rather than flattening arbitrary array storage.
(dolist (source '("#1A17" "#1A#.(make-array '(2 2))"
                  "#2A#.(make-array '(2 2) :adjustable t)"
                  "#2A#.(vector (make-array 2) (make-array 3))"))
  (assert (handler-case (progn (read-from-string source) nil)
            (stream-error () t))))
(write-line "ARRAY-CONTENTS-OK")
"##;

#[test]
fn array_reader_accepts_non_simple_vectors_at_each_contents_level() {
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.env("EGCL_FORCE_TIER", tier);
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        command.env("EGCL_T2_LAZY", "0").env("EGCL_T2_THREADS", "1");
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let program = format!("(defparameter *array-required-tier* {required}){PROGRAM}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .expect("run array contents regression");
        assert!(
            output.status.success(),
            "{tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "ARRAY-CONTENTS-OK")
                .count(),
            1
        );
    }
}
