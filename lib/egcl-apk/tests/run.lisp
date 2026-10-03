;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(load (merge-pathnames "../load.lisp" *load-truename*))
(load (merge-pathnames "check.lisp" *load-truename*))
(let* ((key-path (merge-pathnames "test.identity" *apk-test-root*))
       (identity (progn (format t "Creating identity~%") (egcl-apk:create-identity key-path)))
       (restored (progn (format t "Reloading identity~%") (egcl-apk:load-identity key-path)))
       (entries (list (cons "AndroidManifest.xml" (egcl-apk:manifest :package "org.egcl.apktest" :label "Lisp é😀"))
                      ;; Cross the v2 1 MiB chunk boundary, then start another ZIP section.
                      (cons "assets/payload.bin" (make-array (+ 1048576 23) :element-type '(unsigned-byte 8) :initial-element 42))
                      (cons "lib/arm64-v8a/test.so" #(1 2 3)))))
  (assert (equalp (egcl-apk::signing-identity-certificate identity)
                 (egcl-apk::signing-identity-certificate restored)))
  (assert (handler-case (progn (egcl-apk:create-identity key-path) nil) (error () t)))
  (format t "Signing chunk-boundary APK~%")
  (egcl-apk:write-apk (merge-pathnames "signed.apk" *apk-test-root*) entries :identity identity)
  (egcl-apk:write-apk (merge-pathnames "signed-again.apk" *apk-test-root*) entries :identity restored))
(format t "APK-SIGNING-OK~%")

;; An APK is described by an ASDF system definition; that is the only way.
(asdf:load-asd (merge-pathnames "../egcl-apk-asdf.asd" *load-truename*))
(asdf:load-system :egcl-apk-asdf)
;; The integration against a synthesised runtime, so it needs no
;; egcl-target-android install.
(load (merge-pathnames "asdf.lisp" *load-truename*))

;; The real demo project, when a real runtime is available.
(when (uiop:getenv "EGCL_APK_RUNTIME")
  (egcl-apk-asdf:build-project (uiop:getenv "EGCL_APK_TEST_PROJECT"))
  (format t "APK-DEMO-OK~%"))
