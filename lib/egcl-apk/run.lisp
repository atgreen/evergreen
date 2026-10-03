;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
;;; The driver for the saved image: everything is already loaded, so this only
;;; builds. The `egcl-apk' launcher sets EGCL_APK_PROJECT and the umask that
;;; makes creating a signing identity safe (see asdf-integration.lisp).
(let ((egcl-apk-asdf:*allow-identity-creation* t)
      (egcl-apk-asdf:*runtime-directory* (uiop:getenv "EGCL_APK_RUNTIME")))
  (egcl-apk-asdf:build-project (uiop:getenv "EGCL_APK_PROJECT")))
