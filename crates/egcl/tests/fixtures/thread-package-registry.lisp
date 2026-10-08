;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(defparameter *thread-test-package*
  (make-package "THREAD-DIGEST-REGISTRY" :nicknames '("TDR") :use '("CL")))
(defparameter *thread-test-digest* (intern "SHA256" *thread-test-package*))
(defparameter *thread-test-length* (intern "%DIGEST-LENGTH" *thread-test-package*))
(setf (get *thread-test-digest* *thread-test-length*) 32)
(export *thread-test-digest* *thread-test-package*)
(defun thread-test-lookup ()
  (assert (eq *thread-test-package* (find-package "TDR")))
  (multiple-value-bind (symbol status) (find-symbol "SHA256" "TDR")
    (assert (eq symbol *thread-test-digest*))
    (assert (eq status :external))
    (assert (= (get symbol (find-symbol "%DIGEST-LENGTH" "TDR")) 32)))
  t)
(assert (thread-test-lookup))
(assert
 (egcl-thread:join-thread
  (egcl-thread:make-thread
   (lambda ()
     (thread-test-lookup)
     (assert (egcl-thread:join-thread
              (egcl-thread:make-thread #'thread-test-lookup)))
     (intern "WORKER-ADDED" "TDR")
     (make-package "THREAD-CREATED-PACKAGE" :use nil)
     t))))
(assert (find-symbol "WORKER-ADDED" "TDR"))
(assert (find-package "THREAD-CREATED-PACKAGE"))
(dolist (worker
          (egcl-fiber:run-fibers
           (list (egcl-fiber:make-fiber
                  (lambda () (egcl-thread:make-thread #'thread-test-lookup))))
           :carrier-count 1))
  (assert (egcl-thread:join-thread worker)))
(format t "THREAD-PACKAGE-REGISTRY-PASS~%")
