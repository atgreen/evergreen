;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; DEFSTRUCT accessor reads and writes through an :INCLUDEd parent's
;;; accessors from a warmed caller: the pointer-chasing shape that dominates
;;; EGCL-hosted SBCL compilation (bliss-gg3i1.7).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defstruct bnode (prev nil) (next nil) (flags 0 :type fixnum))
(defstruct (bcombination (:include bnode)) (fun nil) (args nil))
(defstruct (bref (:include bnode)) (leaf nil))

(defun struct-access-loop (x n)
  (let ((acc 0))
    (dotimes (i n acc)
      (when (bnode-prev x) (incf acc))
      (setf (bnode-next x) i)
      (incf acc (bnode-flags x)))))

(defvar *struct-access-node* (make-bcombination :prev t :flags 1))

(run-benchmark "struct-access"
               (lambda () (dotimes (i 10000) (struct-access-loop *struct-access-node* 100))))
