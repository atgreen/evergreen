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

;; No boundp check yet, so defvar behaves like defparameter for now: it always
;; assigns. That is close enough for bootstrapping; a conforming defvar can
;; replace this once boundp is available.
(defmacro defvar (name &rest value)
  `(setq ,name ,(if value (car value) nil)))

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

;; deftype: type definitions are not consulted by the interpreter yet.
(defmacro deftype (&rest ignore) nil)

;; define-condition: no condition class is created yet. Code that signals such
;; a condition will fail at signal time, not load time — acceptable for now.
(defmacro define-condition (&rest ignore) nil)

;; check-type / assert / declaim-like checks: no type/assertion enforcement in
;; the bootstrap evaluator yet, so these are no-ops.
(defmacro check-type (&rest ignore) nil)
(defmacro assert (&rest ignore) nil)

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
;;; Bliss's evaluator resolves package-qualified symbols by their bare name, so
;;; real package objects, use-lists, and external-symbol tables are not needed
;;; for code to run. These stubs exist only so ASDF's own package machinery
;;; (define-package -> ensure-package) executes harmlessly. A package is
;;; represented by its name string.
;;; ---------------------------------------------------------------------------

(defun symbol-name (s) (string s))

(defun symbol-package (s)
  (let ((name (string s)))
    (cond
      ((find #\: name)
       (let ((pkg-end (position #\: name)))
         (and pkg-end (subseq name 0 pkg-end))))
      (t nil))))

(defun make-package (name &rest keys) (declare (ignore keys)) (string name))
(defun find-package (name) (if name (string name) nil))
(defun package-name (pkg) (if pkg (string pkg) nil))
(defun package-names (pkg) (list (package-name pkg)))
(defun package-nicknames (pkg) (declare (ignore pkg)) nil)
(defun package-use-list (pkg) (declare (ignore pkg)) nil)
(defun package-used-by-list (pkg) (declare (ignore pkg)) nil)
(defun package-shadowing-symbols (pkg) (declare (ignore pkg)) nil)
(defun use-package (pkgs &rest r) (declare (ignore pkgs r)) t)
(defun unuse-package (pkgs &rest r) (declare (ignore pkgs r)) t)
(defun rename-package (pkg name &rest nicknames) (declare (ignore name nicknames)) pkg)
(defun delete-package (pkg) (declare (ignore pkg)) t)
(defun find-symbol (name &rest pkg) (declare (ignore name pkg)) (values nil nil))
(defun import (symbols &rest pkg) (declare (ignore symbols pkg)) t)
(defun export (symbols &rest pkg) (declare (ignore symbols pkg)) t)
(defun unexport (symbols &rest pkg) (declare (ignore symbols pkg)) t)
(defun shadow (symbols &rest pkg) (declare (ignore symbols pkg)) t)
(defun shadowing-import (symbols &rest pkg) (declare (ignore symbols pkg)) t)
(defun unintern (symbol &rest pkg) (declare (ignore symbol pkg)) t)

;; Symbol iteration macros: no external/present symbols are tracked, so the
;; body never runs; the optional result form is not evaluated (defaults to nil).
(defmacro do-external-symbols (&rest args) (declare (ignore args)) nil)
(defmacro do-symbols (&rest args) (declare (ignore args)) nil)
(defmacro do-all-symbols (&rest args) (declare (ignore args)) nil)
