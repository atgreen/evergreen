;;;; boot.lisp — Bliss bootstrap prelude.
;;;;
;;;; This file is loaded by the CLI when invoked with --bootstrap. It is the
;;;; first slice of the standard library written in Lisp rather than Rust: the
;;;; goal is to push everything that can be expressed as a macro or ordinary
;;;; function out of the `eval_form` interpreter and into this file.
;;;;
;;;; Constraints of the current bootstrap evaluator (see crates/bliss):
;;;;   * macro lambda lists are flat — &optional and &rest work, but nested
;;;;     destructuring does NOT yet. Keep parameter lists simple.
;;;;   * user macros are expanded before builtins, so nothing here should
;;;;     redefine a form the interpreter already special-cases.
;;;;   * `setq` on an unbound symbol creates a persistent global binding, which
;;;;     is what the defining macros below rely on.

;;; ---------------------------------------------------------------------------
;;; Global variable definitions
;;; ---------------------------------------------------------------------------

(defmacro defvar (name &rest value)
  `(progn
     (if (boundp ',name)
         ,name
         (setq ,name ,(if value (car value) nil)))
     ',name))

(defmacro defparameter (name &rest value)
  `(setq ,name ,(if value (car value) nil)))

;;; ---------------------------------------------------------------------------
;;; Sequencing
;;; ---------------------------------------------------------------------------

(defmacro prog1 (first &rest body)
  (let ((result (gensym)))
    `(let ((,result ,first))
       ,@body
       ,result)))

(defmacro prog2 (first second &rest body)
  `(progn ,first (prog1 ,second ,@body)))

;;; ---------------------------------------------------------------------------
;;; Stack / place mutation
;;; ---------------------------------------------------------------------------

(defmacro push (item place)
  `(setf ,place (cons ,item ,place)))

(defmacro pop (place)
  `(prog1 (car ,place)
     (setf ,place (cdr ,place))))

(defmacro incf (place &rest delta)
  `(setf ,place (+ ,place ,(if delta (car delta) 1))))

(defmacro decf (place &rest delta)
  `(setf ,place (- ,place ,(if delta (car delta) 1))))

;;; ---------------------------------------------------------------------------
;;; Declarations, type aliases, and condition definitions used by the shipped
;;; bootstrap evaluator.
;;; ---------------------------------------------------------------------------

;; declaim: declarations have no bearing on the tree-walking interpreter.
(defmacro declaim (&rest ignore) nil)

;; Track bootstrap type aliases so TYPEP/CHECK-TYPE can consult them.
(defvar *type-definitions* nil)

(defmacro deftype (name lambda-list &rest body)
  (declare (ignore lambda-list))
  `(progn
     (setq *type-definitions*
           (cons (list ',name ',(if body (car body) t))
                 *type-definitions*))
     ',name))

;; Track condition definitions so MAKE-CONDITION/SIGNAL can create and match
;; real condition instances through the evaluator.
(defvar *condition-types* nil)
(defvar *condition-definitions* nil)

(defun %define-condition-option-reader-defs (slot-name opts)
  (if opts
      (let ((key (car opts))
            (value (car (cdr opts))))
        (if (or (eq key :reader) (eq key :accessor))
            (cons `(defun ,value (instance)
                     (slot-value instance ',slot-name))
                  (%define-condition-option-reader-defs slot-name (cdr (cdr opts))))
            (%define-condition-option-reader-defs slot-name (cdr (cdr opts)))))
      nil))

(defun %define-condition-slot-reader-defs (slot)
  (if (consp slot)
      (%define-condition-option-reader-defs (car slot) (cdr slot))
      nil))

(defun %define-condition-reader-defs (slots)
  (if slots
      (append (%define-condition-slot-reader-defs (car slots))
              (%define-condition-reader-defs (cdr slots)))
      nil))

(defmacro define-condition (name parents slots &rest options)
  (let ((effective-parents (if parents parents '(condition)))
        (reader-defs (%define-condition-reader-defs slots)))
    `(progn
       (setq *condition-types*
             (cons (list ',name ',effective-parents)
                   *condition-types*))
       (setq *condition-definitions*
             (cons (list ',name ',effective-parents ',slots ',options)
                   *condition-definitions*))
       (defclass ,name ,effective-parents ,slots)
       ,@reader-defs
       ',name)))

(defmacro check-type (place typespec &rest ignore)
  (declare (ignore ignore))
  `(if (typep ,place ',typespec)
       ,place
       (error (format nil "CHECK-TYPE failed: ~S is not of type ~S" ,place ',typespec))))

(defmacro assert (test-form &rest ignore)
  (declare (ignore ignore))
  `(if ,test-form
       t
       (error (format nil "ASSERT failed: ~S" ',test-form))))

;;; ---------------------------------------------------------------------------
;;; CLOS convenience macros and standard condition accessors.
;;;
;;; WITH-SLOTS / WITH-ACCESSORS expand into SYMBOL-MACROLET so the bound names
;;; are places: reading goes through SLOT-VALUE / the accessor, and SETF on them
;;; works too. The standard condition readers are ordinary functions over the
;;; condition instance's slots — conditions are CLOS objects, so SLOT-VALUE is
;;; all that is needed.
;;; ---------------------------------------------------------------------------

(defmacro with-slots (slots instance &rest body)
  (let ((obj (gensym)))
    `(let ((,obj ,instance))
       (symbol-macrolet
           ,(mapcar (lambda (s)
                      (let ((var (if (consp s) (car s) s))
                            (slot (if (consp s) (car (cdr s)) s)))
                        (list var (list 'slot-value obj (list 'quote slot)))))
                    slots)
         ,@body))))

(defmacro with-accessors (bindings instance &rest body)
  (let ((obj (gensym)))
    `(let ((,obj ,instance))
       (symbol-macrolet
           ,(mapcar (lambda (b)
                      (list (car b) (list (car (cdr b)) obj)))
                    bindings)
         ,@body))))

(defun type-error-datum (c) (slot-value c 'datum))
(defun type-error-expected-type (c) (slot-value c 'expected-type))
(defun simple-condition-format-control (c) (slot-value c 'format-control))
(defun simple-condition-format-arguments (c) (slot-value c 'format-arguments))
(defun cell-error-name (c) (slot-value c 'name))
(defun unbound-slot-instance (c) (slot-value c 'instance))

;; String-producing printers, built on FORMAT now that ~A/~S print lists.
(defun princ-to-string (x) (format nil "~a" x))
(defun prin1-to-string (x) (format nil "~s" x))
(defun write-to-string (x &rest ignore)
  (declare (ignore ignore))
  (format nil "~s" x))

;;; ---------------------------------------------------------------------------
;;; Control-flow macros still needed during the Stage 2 bootstrap.
;;; ---------------------------------------------------------------------------

(defmacro case (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((keys (car clause))
            (body (cdr clause)))
        (push
          (cond
            ((or (eq keys 'otherwise) (eq keys t))
             `(t ,@body))
            ((consp keys)
             `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys))
               ,@body))
            (t
             `((eql ,value ',keys) ,@body)))
          expanded)))
    `(let ((,value ,keyform))
       (cond ,@expanded))))

(defmacro typecase (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((type (car clause))
            (body (cdr clause)))
        (push
          (if (or (eq type 'otherwise) (eq type t))
              `(t ,@body)
              `((typep ,value ',type) ,@body))
          expanded)))
    `(let ((,value ,keyform))
       (cond ,@expanded))))

(defmacro etypecase (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((type (car clause))
            (body (cdr clause)))
        (push `((typep ,value ',type) ,@body) expanded)))
    `(let ((,value ,keyform))
       (cond ,@expanded
             (t (error (format nil "ETYPECASE: no clause matched ~s" ,value)))))))

(defmacro ecase (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((keys (car clause))
            (body (cdr clause)))
        (push
          (if (consp keys)
              `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys)) ,@body)
              `((eql ,value ',keys) ,@body))
          expanded)))
    `(let ((,value ,keyform))
       (cond ,@expanded
             (t (error (format nil "ECASE: ~s is not one of the expected keys" ,value)))))))

(defmacro ignore-errors (&rest body)
  `(handler-case (progn ,@body)
     (error (c) (values nil c))))

;;; ---------------------------------------------------------------------------
;;; Sequence / list helpers (Common Lisp, now that lambda lists bind properly)
;;; ---------------------------------------------------------------------------

(defun identity (x) x)

;; remove-duplicates: keeps the first occurrence and preserves order. The
;; keyword arguments (:test/:key/:from-end/...) are accepted but ignored for
;; now — the default EQL-style comparison via MEMBER covers the bootstrap uses
;; (deduplicating symbol/keyword lists in the package machinery).
(defun remove-duplicates (seq &rest keys)
  (declare (ignore keys))
  (let ((result nil))
    (dolist (x seq)
      (unless (member x result)
        (push x result)))
    (reverse result)))

(defun remove (item seq &rest keys)
  (declare (ignore keys))
  (let ((result nil))
    (dolist (x seq)
      (unless (eql x item)
        (push x result)))
    (reverse result)))

(defun remove-if (pred seq &rest keys)
  (declare (ignore keys))
  (let ((result nil))
    (dolist (x seq)
      (unless (funcall pred x)
        (push x result)))
    (reverse result)))

(defun remove-if-not (pred seq &rest keys)
  (declare (ignore keys))
  (let ((result nil))
    (dolist (x seq)
      (when (funcall pred x)
        (push x result)))
    (reverse result)))

(defun set-difference (a b &rest keys)
  (declare (ignore keys))
  (let ((result nil))
    (dolist (x a)
      (unless (member x b)
        (push x result)))
    (reverse result)))

;;; ---------------------------------------------------------------------------
;;; Lenient package layer
;;;
;;; Bliss's evaluator provides package primitives from the Rust CLI/runtime.
;;; Keep only thin symbol helpers here; package functions themselves should
;;; resolve to the real builtins so bundled ASDF can exercise actual package
;;; state instead of bootstrap stubs.
;;; ---------------------------------------------------------------------------

(defun symbol-name (s) (string s))

;; SYMBOL-PACKAGE is provided as a builtin that inspects the symbol's real
;; package prefix; the previous bootstrap definition parsed (string s), which
;; no longer carries a package prefix now that STRING returns the bare name.

(defmacro do-external-symbols (binding &rest body)
  (let ((var (car binding))
        (package (if (cdr binding) (car (cdr binding)) '*package*))
        (result (if (cdr (cdr binding)) (car (cdr (cdr binding))) nil)))
    `(dolist (,var (bliss-internal::package-symbols ,package nil) ,result)
       ,@body)))

(defmacro do-symbols (binding &rest body)
  (let ((var (car binding))
        (package (if (cdr binding) (car (cdr binding)) '*package*))
        (result (if (cdr (cdr binding)) (car (cdr (cdr binding))) nil)))
    `(dolist (,var (bliss-internal::package-symbols ,package t) ,result)
       ,@body)))

(defmacro do-all-symbols (binding &rest body)
  (let ((var (car binding))
        (result (if (cdr binding) (car (cdr binding)) nil))
        (pkg (gensym)))
    `(progn
       (dolist (,pkg (list-all-packages) ,result)
         (dolist (,var (bliss-internal::package-symbols ,pkg t))
           ,@body)))))
