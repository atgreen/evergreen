;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(format t "JVM-API-LOAD~%")
(finish-output)
(require :asdf)
(format t "JVM-API-ASDF-LOADED~%")
(finish-output)
(asdf:load-asd (truename "lib/egcl-jvm/egcl-jvm.asd"))
(asdf:load-system :egcl-jvm)
(defun check (name expected actual)
  (format t "~A: ~S~%" name actual)
  (unless (equal expected actual) (error "~A: expected ~S" name expected)))
(check "missing-guest" :caught
  (handler-case (egcl-jvm:start-jvm :attach t) (egcl-jvm:jvm-error () :caught)))
(let ((vm (egcl-jvm:start-jvm :classpath (list (namestring (truename "lib/egcl-jvm/build/tests/"))) :options '("-Xcheck:jni" "-Xmx128m"))))
  (check "duplicate-start" :caught
    (handler-case (egcl-jvm:start-jvm) (egcl-jvm:jvm-error () :caught)))
  (egcl-jvm:with-java-objects ((text (egcl-jvm:new "java.lang.String" "(Ljava/lang/String;)V" "hello")))
    (check "constructor-proxy" t (egcl-jvm:java-object-p text))
    (check "string-instance" 5 (egcl-jvm:call text "length" "()I")))
  (egcl-jvm:with-java-objects ((empty (egcl-jvm:call-static "java.util.Collections" "emptyList" "()Ljava/util/List;")))
    (check "public-interface-method" 0 (egcl-jvm:call empty "size" "()I")))
  (check "primitive" 42 (egcl-jvm:call-static "java.lang.Integer" "parseInt" "(Ljava/lang/String;)I" "42"))
  (check "unicode" "λ😀" (egcl-jvm:call-static "java.lang.String" "valueOf" "(Ljava/lang/Object;)Ljava/lang/String;" "λ😀"))
  (egcl-jvm:with-java-objects ((list (egcl-jvm:new "java.util.ArrayList" "()V")))
    (check "add" t (egcl-jvm:call list "add" "(Ljava/lang/Object;)Z" "hello"))
    (check "size" 1 (egcl-jvm:call list "size" "()I"))
    (check "get" "hello" (egcl-jvm:call list "get" "(I)Ljava/lang/Object;" 0))
    (check "wrong-signature" :caught
      (handler-case (egcl-jvm:call list "size" "()J") (egcl-jvm:java-error () :caught)))
    (check "bad-value" :caught
      (handler-case (egcl-jvm:call list "get" "(I)Ljava/lang/Object;" "oops")
        (egcl-jvm:java-error () :caught))))
  (let ((object (egcl-jvm:new "java.lang.Object" "()V")))
    (egcl-jvm:release object)
    (egcl-jvm:release object)
    (check "released" :caught
      (handler-case (egcl-jvm:call object "toString" "()Ljava/lang/String;")
        (egcl-jvm:jvm-error () :caught))))
  (let ((callback (egcl-jvm:implement "java.util.function.IntUnaryOperator"
                    (lambda (method value)
                      (check "callback-method" "applyAsInt" method)
                      (egcl-ext:gc)
                      (egcl-jvm:call-static "java.lang.Math" "addExact" "(II)I" value 1)))))
    (check "java-lisp-java" 42 (egcl-jvm:call callback "applyAsInt" "(I)I" 41))
    (egcl-jvm:release callback))
  (check "java-exception" :caught
    (handler-case (egcl-jvm:call-static "java.lang.Integer" "parseInt" "(Ljava/lang/String;)I" "not-an-integer")
      (egcl-jvm:java-error () :caught)))
  (check "range-check" :caught
    (handler-case (egcl-jvm:call-static "java.lang.Math" "abs" "(I)I" 2147483648)
      (egcl-jvm:java-error () :caught)))
  (check "unpaired-surrogate" :caught
    (handler-case (egcl-jvm:call-static "JvmFixture" "unpaired" "()Ljava/lang/String;")
      (egcl-jvm:jvm-error () :caught)))
  (egcl-jvm:with-java-objects ((array (egcl-jvm:call-static "JvmFixture" "numbers" "()[I")))
    (check "array-length" 3 (egcl-jvm:array-length array))
    (check "array-ref" 20 (egcl-jvm:array-ref array 1))
    (egcl-jvm:array-set array 1 42)
    (check "array-set" 42 (egcl-jvm:array-ref array 1))
    (check "bounds" :caught (handler-case (egcl-jvm:array-ref array 99) (egcl-jvm:java-error () :caught))))
  (egcl-jvm:with-java-objects ((object (egcl-jvm:new "java.lang.Object" "()V"))
                               (weak (egcl-jvm:weak-reference object))
                               (copy (egcl-jvm:promote weak)))
    (check "weak-promote" t (egcl-jvm:same-object-p object copy))
    (check "stop-with-live-reference" :caught
      (handler-case (egcl-jvm:stop-jvm vm) (egcl-jvm:jvm-error () :caught))))
  (let ((callback (egcl-jvm:implement "java.util.function.IntUnaryOperator"
                    (lambda (method value) (declare (ignore method)) (egcl-ext:gc) (+ value 1)))))
    (check "java-created-thread" 42 (egcl-jvm:call-static "JvmFixture" "onThread" "(Ljava/util/function/IntUnaryOperator;I)I" callback 41))
    (egcl-jvm:call-static "JvmFixture" "remember" "(Ljava/util/function/IntUnaryOperator;)V" callback)
    (egcl-jvm:release callback)
    (check "revoked-callback" :caught
      (handler-case (egcl-jvm:call-static "JvmFixture" "remembered" "(I)I" 41) (egcl-jvm:java-error () :caught)))
    (egcl-jvm:call-static "JvmFixture" "forget" "()V"))
  (egcl-jvm:with-java-objects ((callback (egcl-jvm:implement "java.util.function.IntUnaryOperator"
                                           (lambda (method value) (declare (ignore method value)) (error "callback test λ")))))
    (check "lisp-error-to-java" t
      (handler-case (progn (egcl-jvm:call callback "applyAsInt" "(I)I" 1) nil)
        (egcl-jvm:java-error (e) (not (null (search "callback test λ" (egcl-jvm:error-message e))))))))
  (let ((captured nil))
    (egcl-jvm:with-java-objects ((object (egcl-jvm:new "java.lang.Object" "()V"))
                                 (callback (egcl-jvm:implement "java.util.function.UnaryOperator"
                                   (lambda (method value) (declare (ignore method))
                                     (setf captured (egcl-jvm:retain value)) (egcl-ext:gc) value)))
                                 (result (egcl-jvm:call-static "JvmFixture" "echo" "(Ljava/util/function/UnaryOperator;Ljava/lang/Object;)Ljava/lang/Object;" callback object)))
      (check "object-callback" t (egcl-jvm:same-object-p object result))
      (check "retained-callback-argument" t (egcl-jvm:same-object-p captured object))
      (egcl-jvm:release captured)))
  (egcl-jvm:with-java-objects
      ((first-loader (egcl-jvm:call-static "JvmFixture" "isolatedLoader" "()Ljava/lang/ClassLoader;"))
       (second-loader (egcl-jvm:call-static "JvmFixture" "isolatedLoader" "()Ljava/lang/ClassLoader;"))
       (first-class (egcl-jvm:find-java-class "JvmFixture" first-loader))
       (second-class (egcl-jvm:find-java-class "JvmFixture" second-loader)))
    (check "class-loader-identity" nil (egcl-jvm:same-object-p first-class second-class))
    (check "class-proxy-call" 17 (egcl-jvm:call-static first-class "pause" "(I)I" 0))
    (egcl-jvm:call first-loader "close" "()V")
    (egcl-jvm:call second-loader "close" "()V"))
  (let ((callback nil))
    (unwind-protect
        (progn
          (setf callback (egcl-jvm:implement "java.util.function.IntUnaryOperator"
                          (lambda (method value) (declare (ignore method))
                            (check "active-revoke-rejected" :caught
                              (handler-case (egcl-jvm:release callback) (egcl-jvm:jvm-error () :caught)))
                            value)))
          (check "active-callback-survives" 42 (egcl-jvm:call callback "applyAsInt" "(I)I" 42)))
      (egcl-jvm:release callback)))
  (check "image-rejected" :caught
    (handler-case (save-lisp-and-die "/tmp/egcl-jvm-must-not-save.bimg")
      (error (e) (if (search "foreign runtime" (princ-to-string e)) :caught (error e)))))
  (egcl-jvm:call-static "JvmFixture" "linger" "(I)V" 1000)
  (check "shutdown-pending" :caught
    (handler-case (egcl-jvm:stop-jvm vm :timeout 0) (egcl-jvm:jvm-error () :caught)))
  (check "stop" t (egcl-jvm:stop-jvm vm))
  (check "stop-again" t (egcl-jvm:stop-jvm vm))
  (check "restart-rejected" :caught
    (handler-case (egcl-jvm:start-jvm) (egcl-jvm:jvm-error () :caught))))
(format t "JVM-API-PASS~%")
