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
