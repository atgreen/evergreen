(format t "JVM-API-LOAD~%")
(finish-output)
(require :asdf)
(format t "JVM-API-ASDF-LOADED~%")
(finish-output)
(asdf:load-asd (truename "lib/torcl-jvm/torcl-jvm.asd"))
(asdf:load-system :torcl-jvm)
(defun check (name expected actual)
  (format t "~A: ~S~%" name actual)
  (unless (equal expected actual) (error "~A: expected ~S" name expected)))
(check "missing-guest" :caught
  (handler-case (torcl-jvm:start-jvm :attach t) (torcl-jvm:jvm-error () :caught)))
(let ((vm (torcl-jvm:start-jvm :classpath (list (namestring (truename "lib/torcl-jvm/build/tests/"))) :options '("-Xcheck:jni" "-Xmx128m"))))
  (check "duplicate-start" :caught
    (handler-case (torcl-jvm:start-jvm) (torcl-jvm:jvm-error () :caught)))
  (torcl-jvm:with-java-objects ((text (torcl-jvm:new "java.lang.String" "(Ljava/lang/String;)V" "hello")))
    (check "constructor-proxy" t (torcl-jvm:java-object-p text))
    (check "string-instance" 5 (torcl-jvm:call text "length" "()I")))
  (torcl-jvm:with-java-objects ((empty (torcl-jvm:call-static "java.util.Collections" "emptyList" "()Ljava/util/List;")))
    (check "public-interface-method" 0 (torcl-jvm:call empty "size" "()I")))
  (check "primitive" 42 (torcl-jvm:call-static "java.lang.Integer" "parseInt" "(Ljava/lang/String;)I" "42"))
  (check "unicode" "λ😀" (torcl-jvm:call-static "java.lang.String" "valueOf" "(Ljava/lang/Object;)Ljava/lang/String;" "λ😀"))
  (torcl-jvm:with-java-objects ((list (torcl-jvm:new "java.util.ArrayList" "()V")))
    (check "add" t (torcl-jvm:call list "add" "(Ljava/lang/Object;)Z" "hello"))
    (check "size" 1 (torcl-jvm:call list "size" "()I"))
    (check "get" "hello" (torcl-jvm:call list "get" "(I)Ljava/lang/Object;" 0))
    (check "wrong-signature" :caught
      (handler-case (torcl-jvm:call list "size" "()J") (torcl-jvm:java-error () :caught)))
    (check "bad-value" :caught
      (handler-case (torcl-jvm:call list "get" "(I)Ljava/lang/Object;" "oops")
        (torcl-jvm:java-error () :caught))))
  (let ((object (torcl-jvm:new "java.lang.Object" "()V")))
    (torcl-jvm:release object)
    (torcl-jvm:release object)
    (check "released" :caught
      (handler-case (torcl-jvm:call object "toString" "()Ljava/lang/String;")
        (torcl-jvm:jvm-error () :caught))))
  (let ((callback (torcl-jvm:implement "java.util.function.IntUnaryOperator"
                    (lambda (method value)
                      (check "callback-method" "applyAsInt" method)
                      (torcl-ext:gc)
                      (torcl-jvm:call-static "java.lang.Math" "addExact" "(II)I" value 1)))))
    (check "java-lisp-java" 42 (torcl-jvm:call callback "applyAsInt" "(I)I" 41))
    (torcl-jvm:release callback))
  (check "java-exception" :caught
    (handler-case (torcl-jvm:call-static "java.lang.Integer" "parseInt" "(Ljava/lang/String;)I" "not-an-integer")
      (torcl-jvm:java-error () :caught)))
  (check "range-check" :caught
    (handler-case (torcl-jvm:call-static "java.lang.Math" "abs" "(I)I" 2147483648)
      (torcl-jvm:java-error () :caught)))
  (check "unpaired-surrogate" :caught
    (handler-case (torcl-jvm:call-static "JvmFixture" "unpaired" "()Ljava/lang/String;")
      (torcl-jvm:jvm-error () :caught)))
  (torcl-jvm:with-java-objects ((array (torcl-jvm:call-static "JvmFixture" "numbers" "()[I")))
    (check "array-length" 3 (torcl-jvm:array-length array))
    (check "array-ref" 20 (torcl-jvm:array-ref array 1))
    (torcl-jvm:array-set array 1 42)
    (check "array-set" 42 (torcl-jvm:array-ref array 1))
    (check "bounds" :caught (handler-case (torcl-jvm:array-ref array 99) (torcl-jvm:java-error () :caught))))
  (torcl-jvm:with-java-objects ((object (torcl-jvm:new "java.lang.Object" "()V"))
                               (weak (torcl-jvm:weak-reference object))
                               (copy (torcl-jvm:promote weak)))
    (check "weak-promote" t (torcl-jvm:same-object-p object copy))
    (check "stop-with-live-reference" :caught
      (handler-case (torcl-jvm:stop-jvm vm) (torcl-jvm:jvm-error () :caught))))
  (let ((callback (torcl-jvm:implement "java.util.function.IntUnaryOperator"
                    (lambda (method value) (declare (ignore method)) (torcl-ext:gc) (+ value 1)))))
    (check "java-created-thread" 42 (torcl-jvm:call-static "JvmFixture" "onThread" "(Ljava/util/function/IntUnaryOperator;I)I" callback 41))
    (torcl-jvm:call-static "JvmFixture" "remember" "(Ljava/util/function/IntUnaryOperator;)V" callback)
    (torcl-jvm:release callback)
    (check "revoked-callback" :caught
      (handler-case (torcl-jvm:call-static "JvmFixture" "remembered" "(I)I" 41) (torcl-jvm:java-error () :caught)))
    (torcl-jvm:call-static "JvmFixture" "forget" "()V"))
  (torcl-jvm:with-java-objects ((callback (torcl-jvm:implement "java.util.function.IntUnaryOperator"
                                           (lambda (method value) (declare (ignore method value)) (error "callback test λ")))))
    (check "lisp-error-to-java" t
      (handler-case (progn (torcl-jvm:call callback "applyAsInt" "(I)I" 1) nil)
        (torcl-jvm:java-error (e) (not (null (search "callback test λ" (torcl-jvm:error-message e))))))))
  (let ((captured nil))
    (torcl-jvm:with-java-objects ((object (torcl-jvm:new "java.lang.Object" "()V"))
                                 (callback (torcl-jvm:implement "java.util.function.UnaryOperator"
                                   (lambda (method value) (declare (ignore method))
                                     (setf captured (torcl-jvm:retain value)) (torcl-ext:gc) value)))
                                 (result (torcl-jvm:call-static "JvmFixture" "echo" "(Ljava/util/function/UnaryOperator;Ljava/lang/Object;)Ljava/lang/Object;" callback object)))
      (check "object-callback" t (torcl-jvm:same-object-p object result))
      (check "retained-callback-argument" t (torcl-jvm:same-object-p captured object))
      (torcl-jvm:release captured)))
  (torcl-jvm:with-java-objects
      ((first-loader (torcl-jvm:call-static "JvmFixture" "isolatedLoader" "()Ljava/lang/ClassLoader;"))
       (second-loader (torcl-jvm:call-static "JvmFixture" "isolatedLoader" "()Ljava/lang/ClassLoader;"))
       (first-class (torcl-jvm:find-java-class "JvmFixture" first-loader))
       (second-class (torcl-jvm:find-java-class "JvmFixture" second-loader)))
    (check "class-loader-identity" nil (torcl-jvm:same-object-p first-class second-class))
    (check "class-proxy-call" 17 (torcl-jvm:call-static first-class "pause" "(I)I" 0))
    (torcl-jvm:call first-loader "close" "()V")
    (torcl-jvm:call second-loader "close" "()V"))
  (let ((callback nil))
    (unwind-protect
        (progn
          (setf callback (torcl-jvm:implement "java.util.function.IntUnaryOperator"
                          (lambda (method value) (declare (ignore method))
                            (check "active-revoke-rejected" :caught
                              (handler-case (torcl-jvm:release callback) (torcl-jvm:jvm-error () :caught)))
                            value)))
          (check "active-callback-survives" 42 (torcl-jvm:call callback "applyAsInt" "(I)I" 42)))
      (torcl-jvm:release callback)))
  (check "image-rejected" :caught
    (handler-case (save-lisp-and-die "/tmp/torcl-jvm-must-not-save.bimg")
      (error (e) (if (search "foreign runtime" (princ-to-string e)) :caught (error e)))))
  (torcl-jvm:call-static "JvmFixture" "linger" "(I)V" 1000)
  (check "shutdown-pending" :caught
    (handler-case (torcl-jvm:stop-jvm vm :timeout 0) (torcl-jvm:jvm-error () :caught)))
  (check "stop" t (torcl-jvm:stop-jvm vm))
  (check "stop-again" t (torcl-jvm:stop-jvm vm))
  (check "restart-rejected" :caught
    (handler-case (torcl-jvm:start-jvm) (torcl-jvm:jvm-error () :caught))))
(format t "JVM-API-PASS~%")
