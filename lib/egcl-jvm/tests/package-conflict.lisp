;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(require :asdf)
(defpackage :java (:use :cl) (:export :untouched))
(defparameter java:untouched :original)
(asdf:load-asd (truename "lib/egcl-jvm/egcl-jvm.asd"))
(let ((existing (find-package :java)))
  (unless (handler-case (progn (asdf:load-system :egcl-jvm) nil) (error () t))
    (error "Existing JAVA package must be diagnosed"))
  (unless (and (eq existing (find-package :java)) (eq java:untouched :original))
    (error "Existing JAVA package was modified")))
(format t "JAVA-PACKAGE-CONFLICT-PASS~%")
