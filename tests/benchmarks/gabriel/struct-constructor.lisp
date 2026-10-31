;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;; Keyword constructors of an :INCLUDE subtype from a warmed caller
;;; (bliss-gg3i1.7).
(load (merge-pathnames "prelude.lisp" *load-truename*))

(defstruct bnode (prev nil) (next nil) (flags 0 :type fixnum))
(defstruct (bcombination (:include bnode)) (fun nil) (args nil))
(defstruct (bref (:include bnode)) (leaf nil))

(defun struct-constructor-loop (n)
  (let (r)
    (dotimes (i n r)
      (setq r (make-bcombination :fun i :flags i)))))

(run-benchmark "struct-constructor"
               (lambda () (dotimes (i 200) (struct-constructor-loop 100))))
