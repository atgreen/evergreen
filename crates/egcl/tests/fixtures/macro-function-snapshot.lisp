;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defmacro snapshot-target (x) (list 'list :old x))
(defparameter *held-expander* (macro-function 'snapshot-target))
(defmacro snapshot-target (x) (list 'list :new x))
(assert (equal '(list :old 7) (funcall *held-expander* '(snapshot-target 7) nil)))
(assert (equal '(list :new 7)
               (funcall (macro-function 'snapshot-target) '(snapshot-target 7) nil)))
(setf (macro-function 'copied-target) *held-expander*)
(assert (equal '(list :old 8) (macroexpand-1 '(copied-target 8))))
(setf (macro-function 'snapshot-target)
      (lambda (form environment) (funcall *held-expander* form environment)))
(assert (equal '(list :old 9) (macroexpand-1 '(snapshot-target 9))))

(defmacro whole-target (&whole form &rest arguments)
  (declare (ignore arguments))
  form)
(let ((form (list 'another-operator 10)))
  (assert (eq form (funcall (macro-function 'whole-target) form nil))))

(defmacro environment-target (&environment environment) environment)
(defmacro environment-matches (&environment environment)
  (eq environment
      (funcall (macro-function 'environment-target) '(environment-target) environment)))
(assert (environment-matches))

(defmacro package-target () (package-name *package*))
(defparameter *package-expander* (macro-function 'package-target))
(defpackage :expander-client (:use :cl))
(let ((*package* (find-package :expander-client)))
  (assert (string= "EXPANDER-CLIENT"
                   (funcall *package-expander* '(package-target) nil))))

(let ((captured 31))
  (defmacro captured-target (x) (list '+ captured x)))
(defparameter *captured-expander* (macro-function 'captured-target))
(defmacro captured-target (x) (list '+ 99 x))
(assert (equal '(+ 31 2) (funcall *captured-expander* '(captured-target 2) nil)))
(format t "MACRO-SNAPSHOTS-OK~%")
