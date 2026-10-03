;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;;
;;; The ASDF integration: an APK described entirely in a .asd. Builds against a
;;; synthesised runtime, so it needs no egcl-target-android install -- what it
;;; checks is the ASDF half (slots -> config, components -> assets, the guards),
;;; not the native payload, which runtime-entry validates by ELF and SHA-256
;;; anyway.
(let* ((root (merge-pathnames "asdf-fixture/" *apk-test-root*))
       (runtime (merge-pathnames "runtime/" root))
       (key (merge-pathnames ".egcl-apk-key" root)))
  (ensure-directories-exist (merge-pathnames "templates/" runtime))
  (ensure-directories-exist (merge-pathnames "aarch64-linux-android/" runtime))
  ;; A minimal ELF the builder will accept: \x7fELF, 64-bit, little-endian,
  ;; e_machine 183 (AArch64) at offset 18.
  (let ((so (make-array 64 :element-type '(unsigned-byte 8) :initial-element 0)))
    (replace so #(127 69 76 70 2 1))
    (setf (aref so 18) 183 (aref so 19) 0)
    (egcl-apk::write-bytes (merge-pathnames "aarch64-linux-android/libegcl_android.so" runtime) so)
    (egcl-apk::write-bytes
     (merge-pathnames "runtime.json" runtime)
     (egcl-apk::utf8
      (format nil "{\"api\":4,\"version\":\"0.0.1\",\"hosts\":{\"aarch64-linux-android\":~
                   {\"abi\":\"arm64-v8a\",\"sha256\":\"~(~A~)\"}}}"
              (ironclad:byte-array-to-hex-string (egcl-apk::sha256 so))))))
  (egcl-apk::write-bytes (merge-pathnames "templates/android.lisp" runtime)
                         (egcl-apk::utf8 ";; runtime android.lisp"))
  (egcl-apk::write-bytes (merge-pathnames "app.lisp" root) (egcl-apk::utf8 "(values)"))
  (egcl-apk::write-bytes (merge-pathnames "helper.lisp" root) (egcl-apk::utf8 "(values)"))
  (egcl-apk::write-bytes
   (merge-pathnames "fixture.asd" root)
   (egcl-apk::utf8 "
;; The primary system must exist for ASDF to accept the secondary name.
(asdf:defsystem \"fixture\" :components ())
(asdf:defsystem \"fixture/apk\"
  :class \"egcl-apk-asdf:android-apk\"
  :build-operation \"egcl-apk-asdf:apk-op\"
  :version \"3.1\"
  :apk-package \"org.egcl.fixture\"
  :apk-label \"Fixture\"
  :apk-version-code 9
  :apk-permissions (\"android.permission.INTERNET\")
  :components ((:static-file \"app.lisp\") (:static-file \"helper.lisp\")))"))
  (asdf:load-asd (merge-pathnames "fixture.asd" root))
  (let ((system (asdf:find-system "fixture/apk"))
        (egcl-apk-asdf:*runtime-directory* runtime))
    ;; Slots become the builder's configuration plist.
    (let ((config (egcl-apk-asdf::apk-config system)))
      (assert (equal (getf config :package) "org.egcl.fixture"))
      (assert (equal (getf config :label) "Fixture"))
      (assert (eql (getf config :version-code) 9))
      ;; ASDF's own :version supplies the version name.
      (assert (equal (getf config :version-name) "3.1"))
      (assert (equal (getf config :permissions) '("android.permission.INTERNET")))
      ;; An unset optional is omitted, not passed as NIL.
      (assert (not (member :debuggable config))))
    ;; Components become the assets, in declaration order.
    (assert (equal (mapcar #'file-namestring (egcl-apk-asdf::system-asset-files system))
                   '("app.lisp" "helper.lisp")))
    ;; A build must not mint a signing key under an inherited umask.
    (assert (handler-case (progn (asdf:make "fixture/apk") nil)
              (error (e) (search "no signing identity" (princ-to-string e)))))
    (egcl-apk:create-identity key)
    ;; An :apk-entry that is not a component is rejected rather than producing
    ;; an APK the runtime cannot start.
    (setf (slot-value system 'egcl-apk-asdf::entry) "absent.lisp")
    (assert (handler-case (progn (asdf:make "fixture/apk") nil)
              (error (e) (search "is not one of this system's components"
                                 (princ-to-string e)))))
    (setf (slot-value system 'egcl-apk-asdf::entry) "app.lisp")
    (asdf:make "fixture/apk")
    (let* ((apk (merge-pathnames "build/fixture.apk" root)))
      ;; Named for the PRIMARY system, not "fixture/apk" -> build/fixture/apk.apk.
      (assert (probe-file apk))
      ;; Entry names appear literally in each ZIP local header, so searching the
      ;; bytes checks them without needing a reader this library does not have.
      (let ((bytes (egcl-apk::read-bytes apk)))
        (dolist (want '("AndroidManifest.xml" "lib/arm64-v8a/libegcl_android.so"
                        "assets/app.lisp" "assets/helper.lisp"
                        "assets/android.lisp" "assets/egcl-assets.txt"
                        ;; and the configuration really reached the manifest
                        "org.egcl.fixture" "Fixture" "3.1"
                        "android.permission.INTERNET"))
          (assert (search (egcl-apk::utf8 want) bytes) ()
                  "missing from the APK: ~A" want))))))
(format t "APK-ASDF-OK~%")
