;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defpackage :egcl-jvm
  (:use :cl)
  (:export :start-jvm :stop-jvm :jvm-running-p :jvm-error :java-error
           :error-message :new :call :call-static :find-java-class :implement
           :release :retain :with-java-objects :java-object-p :same-object-p
           :array-length :array-ref :array-set :+null+ :weak-reference :promote
           :drain-output :draining))
