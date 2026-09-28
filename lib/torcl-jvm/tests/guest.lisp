(require :asdf)
(asdf:load-asd (truename "lib/torcl-jvm/torcl-jvm.asd"))
(asdf:load-system :torcl-jvm)
(let ((vm (torcl-jvm:start-jvm :attach t)))
  (assert (= 42 (torcl-jvm:call-static "java.lang.Math" "abs" "(I)I" -42)))
  (assert (torcl-jvm:stop-jvm vm)))
;; The embedding host still owns a usable JVM after the guest bridge stops.
(let ((library (torcl-ffi:load-foreign-library
                 (concatenate 'string (torcl-ext:getenv "TORCL_JVM_PROBE_DIR") "/libprobe.so"))))
  (assert (= 42 (torcl-ffi:foreign-call
                 (torcl-ffi:foreign-symbol-pointer "probe_call" library)
                 :int '(:int :int) '(0 21))))
  (assert (zerop (torcl-ffi:foreign-call
                  (torcl-ffi:foreign-symbol-pointer "probe_stop" library) :int nil nil))))
(format t "JVM-GUEST-PASS~%")
