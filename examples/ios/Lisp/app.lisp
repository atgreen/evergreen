;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(in-package #:cl-user)
(format t "EGCL iOS source application: ~D~%" (+ 20 22))
(with-open-file (out (merge-pathnames "result.txt" egcl-ios:*writable-directory*)
                     :direction :output :if-exists :supersede
                     :if-does-not-exist :create)
  (format out "answer=~D~%" (+ 20 22)))
;; Ends this Lisp world; the Swift application keeps running and displays status.
(egcl-ext:quit 0)
