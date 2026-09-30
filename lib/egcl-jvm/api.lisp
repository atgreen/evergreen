;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;; JAVA is the ordinary entry point; EGCL-JVM remains the descriptor-level API.
(eval-when (:compile-toplevel :load-toplevel :execute)
  (let ((existing (find-package "JAVA")))
    (when (and existing (not (eq existing (find-package "EGCL-JAVA"))))
      (error "Package JAVA already exists; rename it before loading egcl-jvm"))))
(defpackage :egcl-java
  (:nicknames :java)
  (:use :cl)
  (:shadow :lambda :find-class)
  (:import-from :egcl-jvm
    :start-jvm :stop-jvm :jvm-running-p :jvm-error :java-error :error-message :ambiguous-call
    :+null+ :java-object-p :release :retain :same-object-p :drain-output :draining)
  (:export :start-jvm :stop-jvm :jvm-running-p :jvm-error :java-error :error-message
    :+null+ :java-object-p :release :retain :same-object-p :ambiguous-call
    :with-scope :as :define-class :new :call :static :define-call
    :lambda :implement :to-list :find-class :new-array :array-ref :array-length
    :field :static-field :with-resource :verify :describe-class :flush :draining))
(in-package :egcl-java)
(defvar *classes* (make-hash-table :test 'eq))
(defvar *bindings* (make-hash-table :test 'eq))
(defvar *scope* nil)
(defstruct scope references)
(defstruct (typed-argument (:constructor as (type value))) type value)
(defun %track (value)
  (when (and *scope* (java-object-p value)) (push value (scope-references *scope*)))
  value)
(defun %cleanup (scope)
  ;; Finish all releases even if one reference cannot yet be revoked.
  (let ((failure nil))
    (dolist (object (scope-references scope))
      (handler-case (release object) (error (e) (unless failure (setf failure e)))))
    (when failure (error failure))))
(defmacro with-scope (() &body body)
  "Release references created by JAVA calls in BODY, including on nonlocal exit.
RETAIN returns an independent reference that escapes this scope."
  `(let ((*scope* (make-scope)))
     (unwind-protect (progn ,@body) (%cleanup *scope*))))
(defun %class (designator)
  (if (and (symbolp designator) (not (keywordp designator)))
      (or (gethash designator *classes*) (egcl-jvm::%fail "Unknown Java class alias ~S" designator))
      designator))
(defmacro define-class (name class)
  "Define a lazy class alias. Java names are case-sensitive strings."
  `(setf (gethash ',name *classes*) ,class))
(defun %type-name (type)
  (if (keywordp type)
      (or (cdr (assoc type '((:boolean . "boolean") (:byte . "byte") (:short . "short")
                             (:int . "int") (:long . "long") (:float . "float")
                             (:double . "double") (:char . "char") (:void . "void"))))
          (egcl-jvm::%fail "Unknown Java primitive ~S" type))
      (%class type)))
(defun %descriptor (type)
  (if (keywordp type)
      (or (cdr (assoc type '((:boolean . "Z") (:byte . "B") (:short . "S") (:int . "I")
                             (:long . "J") (:float . "F") (:double . "D") (:char . "C") (:void . "V"))))
          (egcl-jvm::%fail "Unknown Java type ~S" type))
      (let ((name (%class type)))
        (unless (stringp name) (egcl-jvm::%fail "Exact selectors require type names"))
        (if (and (plusp (length name)) (char= (char name 0) #\[))
            (substitute #\/ #\. name)
            (concatenate 'string "L" (substitute #\/ #\. name) ";")))))
(defun %signature (parameters &optional returns)
  (format nil "(~{~A~})~A" (mapcar #'%descriptor parameters) (if returns (%descriptor returns) "")))
(defun %selector (method)
  (cond ((stringp method) (values method nil))
        ((and (consp method) (stringp (car method))) (values (car method) (%signature (cdr method))))
        (t (egcl-jvm::%fail "Expected a method name or (name parameter-types...), got ~S" method))))
(defun %invoke (op target name signature arguments)
  (let ((temporary nil))
    (labels ((annotation (type value)
               (let ((object (egcl-jvm::%invoke 11 (%type-name type) nil nil (list value))))
                 (push object temporary) object))
             (prepare (value)
               (cond ((typed-argument-p value)
                      (annotation (typed-argument-type value) (prepare (typed-argument-value value))))
                     ((integerp value) (annotation (if (<= -2147483648 value 2147483647) :int :long) value))
                     ((floatp value) (annotation (if (typep value 'single-float) :float :double) value))
                     ((or (eq value t) (null value)) (annotation :boolean value))
                     ((characterp value) (annotation :char value))
                     (t value))))
      (unwind-protect
          (%track (egcl-jvm::%invoke op target name signature (mapcar #'prepare arguments)))
        (dolist (object temporary) (release object))))))
(defun new (class &rest arguments) (%invoke 8 (%class class) nil nil arguments))
(defun call (object method &rest arguments)
  (multiple-value-bind (name signature) (%selector method)
    (%invoke 9 object name signature arguments)))
(defun static (class method &rest arguments)
  (multiple-value-bind (name signature) (%selector method)
    (%invoke 10 (%class class) name signature arguments)))
(defmacro define-call (name (class method) &key static (parameters nil parameters-p) returns)
  "Define an ordinary Lisp function with lazy Java member resolution."
  (when (and returns (not parameters-p)) (error ":returns requires :parameters"))
  (let ((args (gensym "ARGS")) (receiver (gensym "RECEIVER")))
    `(progn
       (defun ,name ,(if static `(&rest ,args) `(,receiver &rest ,args))
         ,@(when parameters-p `((unless (= (length ,args) ,(length parameters))
                                  (egcl-jvm::%fail "Wrong argument count for ~S" ',name))))
         ,@(unless static `((egcl-jvm:with-java-objects ((expected (egcl-jvm:find-java-class (%class ,class))))
                              (unless (call expected "isInstance" ,receiver)
                                (egcl-jvm::%fail "Receiver is not an instance of ~A" ,class)))))
         (%invoke ,(if static 10 9) ,(if static `(%class ,class) receiver) ,method
                  ,(if parameters-p `(%signature ',parameters ',returns) nil) ,args))
       (setf (gethash ',name *bindings*)
             (list ,class ,method ,static ',parameters ',returns ,parameters-p))
       ',name)))
;; Java's output is drained into *STANDARD-OUTPUT* / *ERROR-OUTPUT* at every
;; crossing back into Lisp, so it is normally already there. FLUSH is for pulling
;; it mid-computation -- from inside a callback, or from another thread watching a
;; long-running Java call that has not returned yet.
(defun flush () (egcl-jvm:drain-output))

(defun verify (binding)
  "Resolve a DEFINE-CALL binding with explicit parameters without invoking it."
  (let ((definition (gethash binding *bindings*)))
    (unless definition (egcl-jvm::%fail "Unknown Java binding ~S" binding))
    (destructuring-bind (class name static parameters returns explicit) definition
      (unless explicit (egcl-jvm::%fail "VERIFY requires a binding with :parameters"))
      (%invoke 21 (%class class) name (%signature parameters returns) (list static)))))
(defun describe-class (class)
  "Return sorted descriptions of a class's public constructors, methods and fields."
  (let ((members (egcl-jvm::%invoke 22 (%class class) nil nil nil)))
    (unwind-protect
        (loop for i below (array-length members) collect (egcl-jvm:array-ref members i))
      (release members))))
(defun find-class (name &optional loader)
  (%track (egcl-jvm:find-java-class (%class name) loader)))
(defun array-length (array) (egcl-jvm:array-length array))
(defun array-ref (array index) (%track (egcl-jvm:array-ref array index)))
(defun (setf array-ref) (value array index)
  (%invoke 20 array nil nil (list index value)) value)
(defun new-array (component length) (%invoke 13 (%type-name component) nil nil (list length)))
(defun field (object name) (%invoke 14 object name nil nil))
(defun (setf field) (value object name) (%invoke 15 object name nil (list value)) value)
(defun static-field (class name) (%invoke 16 (%class class) name nil nil))
(defun (setf static-field) (value class name) (%invoke 17 (%class class) name nil (list value)) value)
(defun to-list (iterable)
  "Copy an Iterable. Object-valued elements belong to the current scope or caller."
  (let ((iterator (call iterable "iterator")))
    (unwind-protect
        (loop while (call iterator "hasNext") collect (call iterator "next"))
      (release iterator))))
(defmacro with-resource ((name expression) &body body)
  "Close and release a resource on every exit, preserving a pending nonlocal exit."
  (let ((completed (gensym "COMPLETED")))
    `(let ((,name ,expression) (,completed nil))
       (flet ((cleanup () (unwind-protect (call ,name "close") (release ,name))))
         (unwind-protect
             (multiple-value-prog1 (progn ,@body) (setf ,completed t))
           (if ,completed (cleanup)
               (handler-case (cleanup) (error () nil))))))))
(defun %method-keys (interface)
  (let ((array (egcl-jvm::%invoke 18 (%class interface) nil nil nil)))
    (unwind-protect
        (loop for i below (array-length array) collect (egcl-jvm:array-ref array i))
      (release array))))
(defun %callback-context (thunk)
  ;; Low-level dispatch copies the callback result before this scope releases it.
  (with-scope () (funcall thunk)))
(defun %implement (interface clauses)
  (let ((keys (%method-keys interface)) (handlers nil))
    (dolist (clause clauses)
      (destructuring-bind (selector arity function) clause
        (multiple-value-bind (name signature) (%selector selector)
          (let ((matches (remove-if-not
                          (cl:lambda (key)
                            (let ((split (position #\Newline key)))
                              (and (string= name (subseq key 0 split))
                                   (or (null signature)
                                       (string= signature (subseq key (1+ split) (1+ (position #\) key)))))))) keys)))
            (unless (= 1 (length matches)) (egcl-jvm::%fail "Callback selector ~S is missing or ambiguous" selector))
            (let* ((key (car matches)) (start (+ 2 (position #\Newline key)))
                   (end (position #\) key)) (count 0) (i start))
              ;; Count descriptor parameters, treating arrays/references as one.
              (loop while (< i end) do
                (loop while (char= (char key i) #\[) do (incf i))
                (if (char= (char key i) #\L) (setf i (1+ (position #\; key :start i))) (incf i))
                (incf count))
              (unless (= arity count) (egcl-jvm::%fail "Callback ~A expects ~D arguments" name count))
              (when (assoc key handlers :test #'equal) (egcl-jvm::%fail "Duplicate callback ~A" name))
              (push (cons key function) handlers))))))
    (unless (= (length handlers) (length keys)) (egcl-jvm::%fail "Implement every abstract method of ~A" interface))
    (%track (egcl-jvm::%implement (%class interface)
              (cl:lambda (key &rest args)
                (let ((function (cdr (assoc key handlers :test #'equal))))
                  (unless function (egcl-jvm::%fail "Unbound callback ~A" key))
                  (let ((result (apply function args)))
                    (if (java-object-p result) result
                        (%invoke 23 +null+ nil nil (list result))))))
              :signed t :context #'%callback-context))))
(defmacro implement (interface &body methods)
  "Implement all abstract methods with (selector (arguments) body...) clauses."
  `( %implement ,interface
     (list ,@(mapcar (cl:lambda (method)
                       (destructuring-bind (selector args &body body) method
                         (unless (every (cl:lambda (x) (and (symbolp x) (not (member x lambda-list-keywords)))) args)
                           (error "Java callbacks require fixed argument lists"))
                         `(list ',selector ,(length args) (cl:lambda ,args ,@body)))) methods))))
(defmacro lambda (interface args &body body)
  "Implement a functional interface with a Lisp closure."
  (let ((type (gensym "INTERFACE")) (keys (gensym "METHODS")))
    `(let* ((,type ,interface) (,keys (%method-keys ,type)))
       (unless (= 1 (length ,keys)) (egcl-jvm::%fail "Expected a single-abstract-method interface"))
       (%implement ,type (list (list (subseq (car ,keys) 0 (position #\Newline (car ,keys)))
                                    ,(length args) (cl:lambda ,args ,@body)))))))
