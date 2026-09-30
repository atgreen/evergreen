;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defmacro expand-local (form &environment env)
  (macroexpand form env))
(defmacro sum-pairs (pairs)
  `(let ((total 0))
     (dolist (pair ,pairs)
       (destructuring-bind (a . b) pair
         (when t
           (incf total (+ a b)))))
     total))
(print
  (list
    (macrolet ((local-answer () 41))
      (+ 1 (expand-local (local-answer))))
    (sum-pairs '((1 . 2) (3 . 4)))))
