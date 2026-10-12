;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; This public shape is the currently admitted closed raw-F64 graph. Keep
;;; parameter declarations absent: the initial EGCL pass uses runtime guards.
(declaim (notinline public-packed-bench))
(defun public-packed-bench (src dst n scale)
  (dotimes (i n)
    (let ((x (aref src i)))
      (setf (aref dst i) (+ (* x scale) x))))
  dst)

(defun packed-bench-source (n)
  (let ((src (make-array n :element-type 'double-float :initial-element 0d0)))
    (dotimes (i n src)
      (setf (aref src i) (coerce (1+ (mod i 32)) 'double-float)))))

(defun packed-bench-check (dst n)
  (let ((sum 0d0))
    (dotimes (i n)
      (let ((expected (* 3 (1+ (mod i 32))))
            (actual (aref dst i)))
        (assert (= actual expected))
        (setq sum (+ sum actual))))
    (truncate sum)))
