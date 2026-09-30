;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;; Cross-architecture CLI regression: interpreter, bytecode, default tiering,
;; and moving-GC stress must produce identical observable output.
(assert (= (+ (expt 2 90) 17) 1237940039285380274899124241))
(assert (= (/ 10 3) (+ 3 1/3)))
(assert (= (+ 1.25d0 2.5d0) 3.75d0))
(assert (= (+ 1.25 2.5) 3.75))
(dolist (bits '(8 16 32 64))
  (let* ((largest (1- (expt 2 bits)))
         (array (make-array 20 :element-type (list 'unsigned-byte bits)
                              :initial-element largest)))
    (assert (= (aref array 19) largest))
    (setf (aref array 0) 17)
    (assert (= (aref array 0) 17))))
(defun portable-sum (n)
  (let ((sum 0)) (dotimes (i n sum) (incf sum i))))
;; Cross the invocation promotion threshold and the anonymous OSR threshold.
(dotimes (i 12) (assert (= (portable-sum 25) 300)))
(assert (= (loop repeat 210 count t) 210))
(let ((items (loop for i below 12 collect (list i (format nil "item-~D" i)))))
  (assert (= (length items) 12))
  (assert (equal (car (last items)) '(11 "item-11"))))
(let ((table (make-hash-table :test 'equal)))
  (setf (gethash (copy-seq "key") table) '(a b c))
  (assert (equal (gethash "key" table) '(a b c))))
(defclass portable-box () ((value :initarg :value :accessor portable-value)))
(assert (= (portable-value (make-instance 'portable-box :value 42)) 42))
(assert (= (handler-case (error "expected") (error () 73)) 73))
(assert (string= (with-output-to-string (s) (format s "~A:~D" "hello" 42))
                 "hello:42"))
(format t "PORTABILITY-OK~%")
