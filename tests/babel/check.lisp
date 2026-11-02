;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*local-only* t ocicl-runtime:*download* nil)
(asdf:load-system :babel-tests)

;;; The pinned Babel has one defect that fails on every implementation
;;; (bliss-tutll): the :KSC_5601 encoder calls HANDLE-ERROR, a macro that only
;;; DEFINE-UNIBYTE-ENCODER defines, when a character has no KSC 5601 code. Its
;;; loop also declares that code OF-TYPE CODE-POINT, so SBCL signals a
;;; TYPE-ERROR for the NIL first, while an implementation that does not check
;;; the declaration reaches the call: UNDEFINED-FUNCTION HANDLE-ERROR. Only
;;; these two errors, in the two tests that encode with :KSC_5601, are accepted.
;;; Every other failure, and any change in their number, still fails the run.
(defparameter *known-upstream-tests*
  '(babel-tests::rw-equiv.1 babel-tests::encoder/decoder-retvals))

(defun known-upstream-failure-p (failure)
  (let ((condition (and (typep failure 'hu.dwim.stefil::unexpected-error)
                        (hu.dwim.stefil::condition-of failure)))
        (tests (mapcar (lambda (context)
                         (hu.dwim.stefil::name-of (hu.dwim.stefil::test-of context)))
                       (hu.dwim.stefil::test-context-backtrace-of failure))))
    (and condition
         (intersection tests *known-upstream-tests*)
         (or (and (typep condition 'undefined-function)
                  (string= (symbol-name (cell-error-name condition)) "HANDLE-ERROR")
                  (string= (package-name (symbol-package (cell-error-name condition)))
                           "BABEL-ENCODINGS"))
             (and (typep condition 'type-error)
                  (null (type-error-datum condition)))))))

(multiple-value-bind (passed result) (babel-tests:run)
  (declare (ignore passed))
  (format t "~%BABEL-STATS ~S~%" (hu.dwim.stefil::extract-test-run-statistics result))
  (let* ((failures (coerce (hu.dwim.stefil::failure-descriptions-of result) 'list))
         (known (remove-if-not #'known-upstream-failure-p failures))
         (unexpected (set-difference failures known)))
    (dolist (failure known)
      (format t "KNOWN-UPSTREAM-FAILURE (bliss-tutll) ~A~%"
              (hu.dwim.stefil::condition-of failure)))
    (unless (and (null unexpected)
                 (<= (length known) (length *known-upstream-tests*))
                 (plusp (hu.dwim.stefil::assertion-count-of result)))
      (error "Babel upstream suite failed or executed no assertions"))))
(format t "BABEL-SUITE-PASS~%")
(uiop:quit 0)
