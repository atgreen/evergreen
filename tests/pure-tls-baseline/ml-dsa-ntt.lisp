;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations (list :output-translations (list t (or (uiop:getenv "TLS_COMPARE_CACHE")
                     (error "TLS_COMPARE_CACHE must name an isolated output cache"))) :ignore-inherited-configuration))
(load (or (uiop:getenv "TLS_COMPARE_RUNTIME")
          (error "TLS_COMPARE_RUNTIME must name the pinned ocicl runtime")))
(setf ocicl-runtime:*download* nil ocicl-runtime:*local-only* t)
(asdf:load-system :pure-tls)
(defun ntt-pairs (n a)
  (dotimes (i n a) (pure-tls::ml-dsa-ntt! a) (pure-tls::ml-dsa-intt! a)))
(defun poly-additions (n a b)
  (dotimes (i n a) (pure-tls::ml-dsa-poly-add! a b)))
(defparameter *a* (pure-tls::make-ml-dsa-poly))
(defparameter *b* (pure-tls::make-ml-dsa-poly))
(dotimes (i 256) (setf (aref *a* i) (+ 11 (* i 17)) (aref *b* i) (+ 7 (* i 31))))
(defparameter *original* (copy-seq *a*))
(defparameter *add-a* (copy-seq *a*))
;; Warm actual transform/store paths first, then the caller's invocation heat.
(dotimes (i 20) (ntt-pairs 5 *a*) (poly-additions 5 *add-a* *b*))
(dotimes (i 4100) (ntt-pairs 0 *a*) (poly-additions 0 *add-a* *b*))
#+egcl
(loop repeat 100
      until (every (lambda (fn) (= 2 (egcl-ext:function-tier fn)))
                   '(ntt-pairs poly-additions pure-tls::ml-dsa-ntt!
                     pure-tls::ml-dsa-intt! pure-tls::ml-dsa-poly-add!))
      do (ntt-pairs 1 *a*) (poly-additions 1 *add-a* *b*))
(assert (equalp *a* *original*))
(format t "COMPONENT-IMPL ~S ~S ARRAY ~S~%" (lisp-implementation-type) (lisp-implementation-version) (array-element-type *a*))
#+egcl
(progn
 (format t "COMPONENT-DEOPTS-BEGIN ~S~%" (egcl-ext:deopt-count))
 (dolist (fn '(ntt-pairs poly-additions pure-tls::ml-dsa-ntt! pure-tls::ml-dsa-intt! pure-tls::ml-dsa-poly-add!))
   (let ((tier (egcl-ext:function-tier fn)))
     (assert (= tier 2))
     (format t "COMPONENT-TIER ~S ~S~%" fn tier))))
(defparameter *n* (parse-integer (or (uiop:getenv "TLS_COMPARE_N") "1000")))
(dotimes (round 3)
 (let ((wall (get-internal-real-time)) (cpu (get-internal-run-time)))
  (ntt-pairs *n* *a*)
  (let ((wall-end (get-internal-real-time)) (cpu-end (get-internal-run-time)))
   (assert (equalp *a* *original*))
   (format t "COMPONENT-NTT ~D ~D WALL ~,6F CPU ~,6F SUM ~S~%" round *n*
     (/ (- wall-end wall) (float internal-time-units-per-second 1d0))
     (/ (- cpu-end cpu) (float internal-time-units-per-second 1d0)) (reduce #'+ *a*))))
 (let ((before (copy-seq *add-a*)) (wall (get-internal-real-time)) (cpu (get-internal-run-time)))
  (poly-additions *n* *add-a* *b*)
  (let ((wall-end (get-internal-real-time)) (cpu-end (get-internal-run-time)))
   (dotimes (i 256) (assert (= (aref *add-a* i) (mod (+ (aref before i) (* *n* (aref *b* i))) pure-tls::+ml-dsa-q+))))
   (format t "COMPONENT-ADD ~D ~D WALL ~,6F CPU ~,6F SUM ~S~%" round *n*
     (/ (- wall-end wall) (float internal-time-units-per-second 1d0))
     (/ (- cpu-end cpu) (float internal-time-units-per-second 1d0)) (reduce #'+ *add-a*)))))
#+egcl (format t "COMPONENT-DEOPTS-END ~S~%" (egcl-ext:deopt-count))
(when (uiop:getenv "TLS_COMPARE_DISASSEMBLE") (disassemble 'pure-tls::ml-dsa-ntt!) (disassemble 'pure-tls::ml-dsa-poly-add!))
(finish-output) (uiop:quit 0)
