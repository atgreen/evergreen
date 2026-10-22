(require :asdf)
(asdf:initialize-source-registry '(:source-registry :ignore-inherited-configuration))
(asdf:initialize-output-translations
 (list :output-translations (list t (uiop:getenv "EGCL_PORT_CACHE"))
       :ignore-inherited-configuration))
(load (uiop:getenv "EGCL_PORT_RUNTIME"))
(setf ocicl-runtime:*download* nil ocicl-runtime:*local-only* t)
(asdf:load-system :closer-mop)

;;;; Slot inspection and callable instances required by cl-iparse.
;;;; Run after (asdf:load-system :closer-mop); also portable to SBCL.

(defclass mop-parent () ((a :initarg :a)))
(defclass mop-child (mop-parent) ((b :initarg :b)))
(make-instance 'mop-child)
(assert (equal (sort (mapcar #'c2mop:slot-definition-name (c2mop:class-slots (find-class 'mop-child))) #'string< :key #'symbol-name) '(a b)))
(defclass mop-callable () ((value :initarg :value)) (:metaclass c2mop:funcallable-standard-class))
(let ((instance (make-instance 'mop-callable :value 7)))
  (c2mop:set-funcallable-instance-function instance (lambda (x) (+ x (slot-value instance 'value))))
  (assert (= 12 (funcall instance 5))))
(format t "CLOSER-MOP-OK~%")

;;;; Generic function and method introspection (GitHub #508): find every
;;;; generic function with a method on a class, and what a congruent method on
;;;; each needs -- its name, lambda list and the method's specializers.
(defclass mop-kernel () ())
(defgeneric mop-probe (x &key tol))
(defmethod mop-probe ((x mop-kernel) &key tol) tol)
(let ((method (first (c2mop:generic-function-methods #'mop-probe))))
  (assert (member #'mop-probe (c2mop:specializer-direct-generic-functions (find-class 'mop-kernel))))
  (assert (member method (c2mop:specializer-direct-methods (find-class 'mop-kernel))))
  (assert (equal (c2mop:method-specializers method) (list (find-class 'mop-kernel))))
  (assert (eq (c2mop:method-generic-function method) #'mop-probe))
  (assert (equal (c2mop:generic-function-lambda-list #'mop-probe) '(x &key tol)))
  (assert (eq (c2mop:generic-function-name #'mop-probe) 'mop-probe))
  (multiple-value-bind (methods definitive)
      (c2mop:compute-applicable-methods-using-classes #'mop-probe (list (find-class 'mop-kernel)))
    (assert definitive)
    (assert (equal methods (list method))))
  (remove-method #'mop-probe method)
  (assert (not (member #'mop-probe (c2mop:specializer-direct-generic-functions (find-class 'mop-kernel))))))
(format t "CLOSER-MOP-GF-OK~%")

;;;; METHOD-FUNCTION and the remaining generic-function readers (GitHub #508).
(defgeneric mop-options (x)
  (declare (optimize (speed 1))))
(defmethod mop-options ((x mop-kernel)) (list :kernel (next-method-p)))
(let ((method (first (c2mop:generic-function-methods #'mop-options))))
  (assert (equal (funcall (c2mop:method-function method) (list (make-instance 'mop-kernel)) '())
                 '(:kernel nil)))
  (assert (eq (c2mop:generic-function-method-combination #'mop-options)
              (c2mop:find-method-combination #'mop-options 'standard nil)))
  (assert (eq (c2mop:generic-function-method-class #'mop-options) (find-class 'standard-method)))
  (assert (equal (c2mop:generic-function-declarations #'mop-options) '((optimize (speed 1)))))
  (assert (eql 0 (search "#<STANDARD-GENERIC-FUNCTION " (prin1-to-string #'mop-options)))))
(format t "CLOSER-MOP-GF-REST-OK~%")
