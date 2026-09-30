;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defvar *xl2* nil)
(defun trans2 (type)
  (or (cdr (assoc type *xl2*))
      (lambda (form) `(documentation ',(if (listp form) (first form) form) ',type))))
(defun fmt2 (type var doc) (declare (ignore type var)) doc)
(defmacro define-docs2 (&body expressions)
  `(progn
     ,@(loop for expr in expressions
             for length = (length expr)
             for type = (if (< 2 length) (first expr) 'function)
             for var = (if (< 2 length) (rest (butlast expr)) (butlast expr))
             for doc = (car (last expr))
             collect `(setf ,(funcall (trans2 type) var)
                            ,(fmt2 type var doc)))))
