;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; The initial ordinary ABI admits the identity leaf and the single-call
;;; bridge. Both accumulation loops retain the same outer runtime call path.
(declaim (notinline ordinary-bridge-leaf ordinary-bridge-forward))
(defun ordinary-bridge-leaf (a b c d)
  (declare (ignore a b c))
  d)

(defun ordinary-bridge-forward (a b c d)
  (ordinary-bridge-leaf a b c d))

(defun ordinary-bridge-direct-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum (ordinary-bridge-leaf 0 1 2 i))))))

(defun ordinary-bridge-forward-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum (ordinary-bridge-forward 0 1 2 i))))))

(defun check-ordinary-bridge-result (result n)
  (assert (= result (/ (* n (1- n)) 2)))
  result)

;;; Load identical definitions and warm both paths in each benchmark process.
;;; Warmup is an opportunity to promote; retained code listings prove the ABI.
(dotimes (i 1024)
  (check-ordinary-bridge-result (ordinary-bridge-direct-loop 8) 8)
  (check-ordinary-bridge-result (ordinary-bridge-forward-loop 8) 8))
