;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(asdf:defsystem "egcl-apk"
  :description "NativeActivity APK packaging in Common Lisp"
  :version "0.0.1"
  :depends-on ("ironclad/digest/sha256" "ironclad/public-key/secp256r1" "cl-json")
  :serial t
  :components ((:file "binary") (:file "manifest") (:file "signing") (:file "apk")))
