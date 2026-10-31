;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; TYPECASE over DEFSTRUCT types, the dispatch shape of SBCL's IR
;;; walkers (bliss-gg3i1.7).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defstruct bnode (prev nil) (next nil) (flags 0 :type fixnum))
(defstruct (bcombination (:include bnode)) (fun nil) (args nil))
(defstruct (bref (:include bnode)) (leaf nil))

(defun struct-typecase-1 (x)
  (typecase x (bref 1) (bcombination 2) (bnode 3) (t 4)))

(defun struct-typecase-loop (nodes n)
  (let ((acc 0))
    (dotimes (i n acc)
      (dolist (x nodes) (incf acc (struct-typecase-1 x))))))

(defvar *struct-typecase-nodes* (list (make-bref) (make-bcombination) (make-bnode) 7))

(run-benchmark "struct-typecase"
               (lambda () (dotimes (i 2500) (struct-typecase-loop *struct-typecase-nodes* 100))))
