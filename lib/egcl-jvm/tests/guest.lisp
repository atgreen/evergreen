;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(require :asdf)
(asdf:load-asd (truename "lib/egcl-jvm/egcl-jvm.asd"))
(asdf:load-system :egcl-jvm)
(let ((vm (egcl-jvm:start-jvm :attach t)))
  (assert (= 42 (egcl-jvm:call-static "java.lang.Math" "abs" "(I)I" -42)))
  (assert (egcl-jvm:stop-jvm vm)))
;; The embedding host still owns a usable JVM after the guest bridge stops.
(let ((library (egcl-ffi:load-foreign-library
                 (concatenate 'string (egcl-ext:getenv "EGCL_JVM_PROBE_DIR") "/libprobe.so"))))
  (assert (= 42 (egcl-ffi:foreign-call
                 (egcl-ffi:foreign-symbol-pointer "probe_call" library)
                 :int '(:int :int) '(0 21))))
  (assert (zerop (egcl-ffi:foreign-call
                  (egcl-ffi:foreign-symbol-pointer "probe_stop" library) :int nil nil))))
(format t "JVM-GUEST-PASS~%")
