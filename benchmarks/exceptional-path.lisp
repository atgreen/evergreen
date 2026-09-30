;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defun exceptional-kernel (x)
  (if (zerop (mod x 97))
      (let ((box (vector x)))
        (funcall #'car (aref box 0)))
      x))

(defun bench-validate ()
  (assert (= (exceptional-kernel 1) 1))
  (assert (eq :caught
             (handler-case (exceptional-kernel 0)
               (type-error () :caught)))))

(defun exceptional-call (x)
  (handler-case (exceptional-kernel x)
    (type-error () 0)))

(defun bench-train ()
  (dotimes (i 4096)
    (bench-workload)))

(defun bench-workload ()
  (let ((sum 0))
    (dotimes (i 2000 sum)
      (incf sum (exceptional-call i)))))
