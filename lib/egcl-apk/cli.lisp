;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; Driven by the `egcl-apk' launcher, which sets umask 077 before starting
;;; us. That umask is why this entry point -- and only this one -- may create
;;; a missing signing identity: the key lands 0600 rather than under whatever
;;; umask an interactive session carries.
(load (merge-pathnames "load.lisp" *load-truename*))
(asdf:load-asd (merge-pathnames "egcl-apk-asdf.asd" *load-truename*))
(asdf:load-system :egcl-apk-asdf)
(let ((egcl-apk-asdf:*allow-identity-creation* t)
      (egcl-apk-asdf:*runtime-directory* (uiop:getenv "EGCL_APK_RUNTIME")))
  (egcl-apk-asdf:build-project (uiop:getenv "EGCL_APK_PROJECT")))
