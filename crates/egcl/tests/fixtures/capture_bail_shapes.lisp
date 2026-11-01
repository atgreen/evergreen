;;;; Capture shapes that used to make the lowerer bail with
;;;; `flet:closure-captures-slot` (bliss-gg3i1.7.17), modelled on SBCL's
;;;; QQ-TEMPLATE-1 (backq.lisp) and MAP-COMBINATION-ARGS-AND-TYPES
;;;; (ir1util.lisp): LABELS/FLET closures over MULTIPLE-VALUE-BIND variables,
;;;; over &OPTIONAL/&AUX parameters, and over each other.

(defun cb-report (key value)
  (format t "~A=~S~%" key value))

(defun cb-map-template (input)
  (values (mapcar (lambda (x) (cons x (if (symbolp x) 'quote 'eval))) input)
          (and (consp input) (cdr (last input)) t)))

;;; QQ-TEMPLATE-1: MVB variables captured by mutually calling LABELS
;;; functions, one with an &AUX lambda list, passed as #'FN to SOME/EVERY/
;;; MAPCAR.
(defun cb-qq-template (input depth compiler-p)
  (declare (type fixnum depth))
  (multiple-value-bind (subforms dot-p) (cb-map-template input)
    (labels ((const-p (subform)
               (or (eq (cdr subform) 'quote)
                   (let ((exp (car subform)))
                     (if (atom exp) (atom-const-p exp) nil))))
             (atom-const-p (atom)
               (typep atom '(or (not symbol) (member t nil) keyword)))
             (const-val (subform)
               (car subform))
             (render (subform)
               (let ((exp (car subform)))
                 (if (and (eq (cdr subform) 'quote)
                          (not (and (atom exp) (atom-const-p exp))))
                     (list 'quote exp)
                     exp)))
             (normalize-fn (fn-name)
               (if (or compiler-p (eq fn-name 'nconc))
                   fn-name
                   (ecase fn-name
                     (append 'append)
                     (list 'list)
                     (list* 'list*))))
             (recurse (list &aux (elt (car list)) (rest (cdr list)))
               (if (null rest)
                   (cond (dot-p (render elt))
                         ((const-p elt) (list 'quote (list (const-val elt))))
                         (t (list (normalize-fn 'list) (render elt))))
                   (let ((fn (normalize-fn 'list*))
                         (head (render elt))
                         (tail (recurse rest)))
                     (if (and (listp tail) (eq (car tail) fn))
                         (list* fn head (cdr tail))
                         (list fn head tail))))))
      (cond ((> depth 0) (recurse subforms))
            ((every #'const-p subforms)
             (values (mapcar #'const-val subforms) 'quote))
            (t (values (cons (normalize-fn 'list) (mapcar #'render subforms))
                       'eval))))))

;;; MAP-COMBINATION-ARGS-AND-TYPES: local functions over &OPTIONAL
;;; parameters and an assigned accumulator, one calling another, with a
;;; closure handed to a mapping function.
(defun cb-map-args (function args &optional info (unknown-keys-fun nil))
  (let ((position 0) (seen '()))
    (flet ((positional-annotation (pos)
             (if info (list :info info pos) pos)))
      (labels ((add (arg type annotation)
                 (push (list arg type annotation) seen)
                 (funcall function arg type annotation))
               (call (arg)
                 (add arg (if (numberp arg) 'number 't)
                      (positional-annotation (prog1 position (incf position)))))
               (body ()
                 (mapc #'call args)
                 (when unknown-keys-fun (funcall unknown-keys-fun position))))
        (body)
        (list position (length seen))))))

;;; MAP-COMBINATION-ARGS-AND-TYPES as BINDING* expands it: FLET functions
;;; over MULTIPLE-VALUE-BIND variables and an assigned counter, with
;;; RETURN-FROM out of DOLISTs that call them.
(defun cb-resolve (args) (values (reverse args) (length args)))
(defun cb-map-combination (fun args &key annotation)
  (let ((type (list :required 2 :optional 1)))
    (multiple-value-bind (arg-lvars unknown-keys) (cb-resolve args)
      (let ((i -1))
        (flet ((positional-annotation ()
                 (and annotation (list annotation (incf i))))
               (call (arg type &optional note)
                 (funcall fun arg type arg-lvars note)))
          (dolist (req '(a b))
            (when (null args) (return-from cb-map-combination (list :short i)))
            (let ((arg (pop args)))
              (call arg req (positional-annotation))))
          (dolist (opt '(c))
            (when (null args) (return-from cb-map-combination (list :opt i)))
            (call (pop args) opt (positional-annotation)))
          (list (getf type :required) unknown-keys i))))))

;;; A closure over a HANDLER-CASE clause variable.
(defun cb-handler-closure (x)
  (handler-case (if (numberp x) (error "bad ~A" x) x)
    (error (condition)
      (let ((report (lambda () (princ-to-string condition))))
        (funcall report)))))

(defun cb-run ()
  (cb-report "qq-depth" (cb-qq-template '(a 1 :k) 1 nil))
  (cb-report "qq-const" (multiple-value-list (cb-qq-template '(1 :k nil) 0 nil)))
  (cb-report "qq-eval" (multiple-value-list (cb-qq-template '(a 1) 0 t)))
  
  (let ((out '()))
    (cb-report "map-args"
               (cb-map-args (lambda (a ty an) (push (list a ty an) out))
                            '(1 x 2) :i (lambda (n) (push n out))))
    (cb-report "map-args-out" out))
  (cb-report "map-args-plain" (cb-map-args (lambda (&rest r) r) '(a b)))
  (let ((seen '()))
    (cb-report "combination"
               (cb-map-combination (lambda (arg type lvars note)
                                     (push (list arg type (length lvars) note) seen))
                                   '(1 2 3) :annotation :pos))
    (cb-report "combination-seen" seen))
  (cb-report "combination-short"
             (cb-map-combination (lambda (&rest r) r) '(1)))
  (cb-report "handler" (cb-handler-closure 5)))

(defvar *cb-warm* 30)
(dotimes (i *cb-warm*) (let ((*standard-output* (make-broadcast-stream))) (cb-run)))
(cb-run)
