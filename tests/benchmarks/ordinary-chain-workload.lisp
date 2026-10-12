;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Both loop targets are forwarding entries, so their outer adapter matches.
;;; The retained pre-tail CI artifacts must show CALL, not JMP, for both edges.
(declaim (notinline ordinary-chain-leaf ordinary-chain-one ordinary-chain-two))
(defun ordinary-chain-leaf (a b c d)
  (declare (ignore a b c))
  d)

(defun ordinary-chain-one (a b c d)
  (ordinary-chain-leaf a b c d))

(defun ordinary-chain-two (a b c d)
  (ordinary-chain-one a b c d))

(defun ordinary-chain-one-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum (ordinary-chain-one 0 1 2 i))))))

(defun ordinary-chain-two-loop (n)
  (let ((sum 0))
    (dotimes (i n sum)
      (setq sum (+ sum (ordinary-chain-two 0 1 2 i))))))

(defun check-ordinary-bridge-result (result n)
  (assert (= result (/ (* n (1- n)) 2)))
  result)

(dotimes (i 1024)
  (check-ordinary-bridge-result (ordinary-chain-one-loop 8) 8)
  (check-ordinary-bridge-result (ordinary-chain-two-loop 8) 8))
