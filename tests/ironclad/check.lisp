;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil ocicl-runtime:*local-only* t)
(asdf:load-system :ironclad)
;; The original failure: a nested local macro in PRODUCE-DIGEST interns the
;; register helper. It must use IRONCLAD's compilation package, not CL-USER.
(assert
 (string-equal
  (ironclad:byte-array-to-hex-string
   (ironclad:digest-sequence :whirlpool
                            (make-array 0 :element-type '(unsigned-byte 8))))
  "19FA61D75522A4669B44E39C1D2E1726C530232130D407F89AFEE0964997F7A73E83BE698B288FEBCF88E3E03C4F0757EA8964E59B63D93708B138CC42A66EB3"))
(format t "IRONCLAD-WHIRLPOOL-OK~%")
(asdf:load-system :ironclad/tests)
;; Stop at the first failure and retain its diagnostic instead of reducing
;; every condition to an unreadable object in RT's aggregate report.
(setf rtest::*catch-errors* nil)
(if (equal (uiop:getenv "EGCL_IRONCLAD_FULL_TESTS") "1")
    (progn
      (asdf:test-system :ironclad/tests)
      (format t "IRONCLAD-FULL-TESTS-OK~%"))
    ;; These complete upstream vector groups exercise the runtime regressions
    ;; fixed here: local macros, method initialization, typed registers,
    ;; generated SETF writers, reader callbacks and array result types.
    (dolist (test '(:eax :etm :gcm
                   ironclad-tests::eax/incremental
                   ironclad-tests::etm/incremental
                   ironclad-tests::gcm/incremental))
      (assert (eq (rtest:do-test test) test))
      (format t "IRONCLAD-~A-OK~%" test)))
(format t "IRONCLAD-REGRESSIONS-OK~%")
