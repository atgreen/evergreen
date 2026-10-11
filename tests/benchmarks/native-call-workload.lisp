;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Four arguments exercise the proposed ABI's first overflow stack argument.
;;; NOTINLINE keeps the cross-function call in both implementations. The leaf
;;; does no arithmetic or allocation; the matched baseline retains loop work.
(declaim (notinline native-call4-leaf))
(defun native-call4-leaf (a b c d)
  (declare (ignore a b c))
  d)

(defun native-call4-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum (native-call4-leaf 0 1 2 i))))))

(defun native-call-base-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum i)))))

(defun check-native-call-result (result n)
  (assert (= result (/ (* n (1- n)) 2)))
  result)

;;; Repeated short invocations warm both caller and callee. The benchmark
;;; protocol also runs an untimed full workload before taking its timestamp.
;;; This encourages promotion; only a per-function tier trace proves it.
(dotimes (i 1024)
  (check-native-call-result (native-call4-loop 8) 8)
  (check-native-call-result (native-call-base-loop 8) 8))
