;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Gabriel TAK — deep non-tail recursion, fixnum compares (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun tak (x y z)
  (if (not (< y x))
      z
      (tak (tak (1- x) y z)
           (tak (1- y) z x)
           (tak (1- z) x y))))

(run-benchmark "tak" (lambda () (tak 24 16 8)))
