;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil ocicl-runtime:*local-only* t)
(if (equal (uiop:getenv "EGCL_PPCRE_FIRST") "1")
    (progn (asdf:load-system :cl-ppcre) (asdf:load-system :ironclad))
    (progn (asdf:load-system :ironclad) (asdf:load-system :cl-ppcre)))
;; Ironclad's GROUP reader must not turn the unrelated parser function into
;; an optimized slot read when CL-PPCRE is compiled later (bliss-el2mg).
(assert (not (eq 'cl-ppcre::group 'ironclad::group)))
(assert (equal (cl-ppcre:parse-string "(abc)") '(:register "abc")))
(dolist (regex '("abc" "(abc)" "a+" "(?:ab|cd)" "[a-z]+"))
  (assert (eql 0 (cl-ppcre:scan regex "abc"))))
(let* ((group (make-instance 'ironclad::discrete-logarithm-group :p 23 :q 11 :g 2))
       (key (make-instance 'ironclad::dsa-public-key :group group :y 4)))
  (assert (eq group (ironclad::group key))))
(format t "LOAD-ORDER-OK~%")
