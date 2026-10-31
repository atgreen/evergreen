;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; DEFSTRUCT predicates on an :INCLUDE subtype from a warmed caller
;;; (bliss-gg3i1.7).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defstruct bnode (prev nil) (next nil) (flags 0 :type fixnum))
(defstruct (bcombination (:include bnode)) (fun nil) (args nil))
(defstruct (bref (:include bnode)) (leaf nil))

(defun struct-predicate-loop (x n)
  (let ((c 0))
    (dotimes (i n c)
      (when (bnode-p x) (incf c))
      (when (bref-p x) (incf c)))))

(defvar *struct-predicate-node* (make-bcombination))

(run-benchmark "struct-predicate"
               (lambda () (dotimes (i 5000) (struct-predicate-loop *struct-predicate-node* 100))))
