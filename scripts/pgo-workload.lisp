;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; Dependency-free PGO inputs, not performance benchmarks.
;;;; Run each phase in a fresh --no-init process from the checkout root.
;;;; EGCL_PGO_WORK names a private directory; phases: prepare, load, runtime.
;;;; Preparation profiles must NOT be merged into the cached-load profile.
(require :asdf)
(defpackage :egcl-pgo-driver (:use :cl))
(defpackage :egcl-pgo-training (:use :cl))
(in-package :egcl-pgo-driver)
(defvar egcl-pgo-training::*loaded*)

(defparameter *phase* (uiop:getenv "EGCL_PGO_PHASE"))
(unless (member *phase* '("prepare" "load" "runtime") :test #'equal)
  (error "Unknown PGO phase: ~S" *phase*))
(defparameter *work*
  (let ((path (uiop:getenv "EGCL_PGO_WORK")))
    (unless (and path (plusp (length path)) (uiop:absolute-pathname-p path))
      (error "PGO work directory must be absolute"))
    (uiop:ensure-directory-pathname path)))
(defparameter *source* (merge-pathnames "source/" *work*))
(defparameter *cache* (merge-pathnames "cache/" *work*))
(defparameter *ready* (merge-pathnames "prepared" *work*))

;; A partial or stale cache is an error during training, not an invitation to
;; measure compilation. This method exists only in the disposable process.
(defmethod asdf:perform :before ((operation asdf:compile-op)
                               (component asdf:cl-source-file))
  (declare (ignore operation component))
  (when (equal *phase* "load")
    (error "PGO cached load attempted compilation")))

(defun write-forms (name forms)
  (with-open-file (out (merge-pathnames name *source*) :direction :output
                       :if-exists :supersede)
    (let ((*package* (find-package :cl)) (*print-pretty* nil)
          (*print-readably* t))
      (dolist (form forms) (print form out)))))

(defun unit-symbol (prefix index)
  (intern (format nil "~A-~D" prefix index) :egcl-pgo-training))

(defun prepare-sources ()
  (ensure-directories-exist (merge-pathnames "placeholder" *source*))
  (ensure-directories-exist (merge-pathnames "placeholder" *cache*))
  (write-forms "egcl-pgo-training.asd"
    `((asdf:defsystem "egcl-pgo-training" :serial t
        :components ((:file "package")
                     ,@(loop for index from 1 to 24
                             collect `(:file ,(format nil "unit-~D" index)))))))
  (write-forms "package.lisp"
    '((defpackage :egcl-pgo-training (:use :cl))
      (defvar egcl-pgo-training::*loaded* nil)
      (defgeneric egcl-pgo-training::score (object))))
  (loop for index from 1 to 24 do
    (let ((class (unit-symbol "BOX" index))
          (reader (unit-symbol "VALUE" index))
          (sum (unit-symbol "SUM" index))
          (table (unit-symbol "TABLE" index)))
      (write-forms (format nil "unit-~D.lisp" index)
        `((defclass ,class () ((value :initarg :value :reader ,reader)))
          (defmethod egcl-pgo-training::score ((object ,class))
            (+ ,index (,reader object)))
          (defun ,sum (items &key (initial 0))
            (reduce #'+ items :initial-value initial))
          (defvar ,table nil)
          ;; Explicit load-time effects avoid relying on the known ordinary
          ;; top-level macro compilation defect (bliss-t4qs).
          (eval-when (:load-toplevel :execute)
            (setf ,table (make-hash-table :test #'equal))
            (dotimes (key 20)
              (setf (gethash (format nil "key-~D" key) ,table) key))
            (assert (= 19 (gethash "key-19" ,table)))
            (assert (= (egcl-pgo-training::score
                         (make-instance ',class :value 10)) ,(+ 10 index)))
            (assert (= (,sum '(1 2 3) :initial 4) 10))
            (push ,index egcl-pgo-training::*loaded*)))))))

(defun load-training-system ()
  (asdf:initialize-source-registry
   `(:source-registry (:directory ,(namestring *source*)) :ignore-inherited-configuration))
  (asdf:initialize-output-translations
   `(:output-translations (t ,(namestring *cache*)) :ignore-inherited-configuration))
  (asdf:load-system :egcl-pgo-training)
  (assert (= 24 (length egcl-pgo-training::*loaded*)))
  (assert (equal (sort (copy-list egcl-pgo-training::*loaded*) #'<)
                 (loop for index from 1 to 24 collect index))))

(defclass counter () ((value :initform 0 :accessor counter-value)))
(defgeneric advance (counter))
(defmethod advance ((object counter)) (incf (counter-value object)))
(defun train-runtime ()
  (let ((table (make-hash-table :test #'equal))
        (counter (make-instance 'counter)))
    (dotimes (index 1000)
      (setf (gethash (format nil "item-~D" index) table)
            (list index (* index index)))
      (advance counter))
    (assert (= 1000 (hash-table-count table) (counter-value counter)))
    (assert (equal '(999 998001) (gethash "item-999" table)))
    (assert (= 499500 (reduce #'+ (loop for index below 1000 collect index))))
    (assert (equal '(2 4 6) (remove-if #'oddp '(1 2 3 4 5 6))))
    (assert (equal '(3 2 1) (reverse '(1 2 3))))
    (assert (string= "bc" (subseq "abcd" 1 3)))
    (assert (= 17 (handler-case (error "training") (error () 17))))
    (format t "~&PGO-RUNTIME ~D 499500~%" (counter-value counter))))

(cond
  ((equal *phase* "prepare")
   (prepare-sources)
   (load-training-system)
   (with-open-file (out *ready* :direction :output :if-exists :supersede)
     (write-line "Prepared; training must run in a fresh process." out))
   (format t "~&PGO-PREPARED 24~%"))
  ((equal *phase* "load")
   (unless (probe-file *ready*) (error "PGO workload is not prepared"))
   (load-training-system)
   (format t "~&PGO-LOAD ~D ~D~%" (length egcl-pgo-training::*loaded*)
           (reduce #'+ egcl-pgo-training::*loaded*)))
  (t (train-runtime)))
