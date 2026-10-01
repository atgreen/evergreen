;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(load (merge-pathnames "load.lisp" *load-truename*))
(egcl-apk:build-apk (uiop:getenv "EGCL_APK_PROJECT")
                    (or (uiop:getenv "EGCL_APK_RUNTIME") "/usr/libexec/egcl/android/")
                    :output (uiop:getenv "EGCL_APK_OUTPUT")
                    :identity-path (uiop:getenv "EGCL_APK_IDENTITY"))
