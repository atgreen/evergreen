;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Naive Fibonacci — call-heavy fixnum recursion (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun fib (n)
  (if (< n 2)
      n
      (+ (fib (- n 1)) (fib (- n 2)))))

(run-benchmark "fib" (lambda () (fib 30)))
