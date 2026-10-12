;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; Deliberately untyped: shared source for EGCL LOAD and SBCL COMPILE-FILE.
(declaim (optimize (speed 3) (safety 1) (debug 1)))
(defun fib (n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))
