;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; No type declarations: identical general-integer recursion on both runtimes.
(defun fibonacci (n)
  (if (< n 2) n (+ (fibonacci (- n 1)) (fibonacci (- n 2)))))
(defparameter *fib-input* 30)
(defparameter *fib-repetitions* 10)
(defun bench-validate ()
  (assert (= (fibonacci 0) 0))
  (assert (= (fibonacci 1) 1))
  (assert (= (fibonacci 10) 55))
  (assert (= (fibonacci *fib-input*) 832040)))
(defun bench-workload ()
  (let ((sum 0))
    (dotimes (i *fib-repetitions*) (incf sum (fibonacci *fib-input*)))
    sum))

;;; Short calls heat both the driver and recursive kernel without timing warmup.
(defun bench-train ()
  (let ((*fib-input* 10) (*fib-repetitions* 1))
    (dotimes (i 10000) (bench-workload))))
