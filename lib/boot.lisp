;;;; boot.lisp — Bliss bootstrap prelude.
;;;;
;;;; This file is loaded by the CLI when invoked with --bootstrap. It is the
;;;; first slice of the standard library written in Lisp rather than Rust: the
;;;; goal is to push everything that can be expressed as a macro or ordinary
;;;; function out of the `eval_form` interpreter and into this file.
;;;;
;;;; Constraints of the current bootstrap evaluator (see crates/bliss-cli):
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
;;; Stack / place mutation (symbol places only, for now)
;;; ---------------------------------------------------------------------------

(defmacro push (item place)
  `(setq ,place (cons ,item ,place)))

(defmacro pop (place)
  `(prog1 (car ,place)
     (setq ,place (cdr ,place))))

(defmacro incf (place &rest delta)
  `(setq ,place (+ ,place ,(if delta (car delta) 1))))

(defmacro decf (place &rest delta)
  `(setq ,place (- ,place ,(if delta (car delta) 1))))

;;; ---------------------------------------------------------------------------
;;; Bootstrap stubs for declaring / type / condition forms
;;;
;;; These are LENIENT no-ops: they let a file load past forms the bootstrap
;;; evaluator does not yet model, without giving them real semantics. That is
;;; sufficient because the forms below either carry no runtime obligation
;;; (declaim, deftype used only for declaration) or their effect is only
;;; needed when a defining form's body actually runs. Replace with conforming
;;; implementations as the type and condition systems come online.
;;; ---------------------------------------------------------------------------

;; declaim: declarations have no bearing on the tree-walking interpreter.
(defmacro declaim (&rest ignore) nil)

;; Track bootstrap type aliases so TYPEP/CHECK-TYPE can consult them.
(defvar *type-definitions* nil)

(defmacro deftype (name lambda-list &rest body)
  (declare (ignore lambda-list))
  `(progn
     (setq *type-definitions*
           (cons (cons ',name ',(if body (car body) t))
                 *type-definitions*))
     ',name))

;; Track condition supertypes so SIGNAL/HANDLER-BIND can do real type matching.
(defvar *condition-types* nil)

(defmacro define-condition (name parents slots &rest options)
  (declare (ignore slots options))
  `(progn
     (setq *condition-types*
           (cons (cons ',name ',(if parents parents '(condition)))
                 *condition-types*))
     ',name))

(defmacro check-type (place typespec &rest ignore)
  (declare (ignore ignore))
  `(if (typep ,place ',typespec)
       ,place
       (error "CHECK-TYPE failed")))

(defmacro assert (test-form &rest ignore)
  (declare (ignore ignore))
  `(if ,test-form
       t
       (error "ASSERT failed")))

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

(defun symbol-package (s)
  (let ((name (string s)))
    (cond
      ((find #\: name)
       (let ((pkg-end (position #\: name)))
         (and pkg-end (subseq name 0 pkg-end))))
      (t nil))))

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
