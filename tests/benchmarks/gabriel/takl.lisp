;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Gabriel TAKL — TAK over unary list numbers; cons-heavy compares (bliss-jpd0).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defun listn (n)
  (if (not (= 0 n))
      (cons n (listn (1- n)))
      nil))

(defun shorterp (x y)
  (and y (or (null x)
             (shorterp (cdr x) (cdr y)))))

(defun mas (x y z)
  (if (not (shorterp y x))
      z
      (mas (mas (cdr x) y z)
           (mas (cdr y) z x)
           (mas (cdr z) x y))))

(let ((l18 (listn 18))
      (l12 (listn 12))
      (l6 (listn 6)))
  (run-benchmark "takl" (lambda () (mas l18 l12 l6))))
