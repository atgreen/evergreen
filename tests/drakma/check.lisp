;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil ocicl-runtime:*local-only* t)
;; This scenario exercises plain HTTP only.
(pushnew :drakma-no-ssl *features*)
(asdf:load-system :drakma)
(assert (fboundp 'usocket::socket-connect-internal))
(defvar *endpoint*
  (format nil "http://127.0.0.1:~A/probe" (uiop:getenv "DRAKMA_TEST_PORT")))
(multiple-value-bind (body status)
    (drakma:http-request *endpoint* :proxy nil)
  (assert (= status 200))
  (assert (string= body "hello from loopback")))
(multiple-value-bind (body status)
    (drakma:http-request *endpoint* :proxy nil :method :post
                         :content "octet alias request" :content-type "text/plain")
  (assert (= status 201))
  (assert (string= body "received octet alias request")))
(format t "DRAKMA-HTTP-OK~%")
