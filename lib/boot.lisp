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

;; defconstant: this interpreter has no separate constant cell; model it as a
;; global binding, like defparameter.
(defmacro defconstant (name value &rest doc)
  (declare (ignore doc))
  `(setq ,name ,value))

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

;; pushnew: add ITEM to the list in PLACE only if not already a MEMBER.
;; Keyword args (:test/:key) are accepted but only the default EQL test is
;; honored, which covers the prelude/UIOP uses (e.g. (pushnew :x *features*)).
(defmacro pushnew (item place &rest keys)
  (declare (ignore keys))
  (let ((v (gensym)))
    `(let ((,v ,item))
       (if (member ,v ,place)
           ,place
           (setf ,place (cons ,v ,place))))))

(defmacro incf (place &rest delta)
  `(setf ,place (+ ,place ,(if delta (car delta) 1))))

(defmacro decf (place &rest delta)
  `(setf ,place (- ,place ,(if delta (car delta) 1))))

;; define-modify-macro: define NAME so that (NAME place args...) expands to
;; (setf place (FUNCTION place args...)). Supports required and &rest args in
;; LAMBDA-LIST, which covers the standard uses (appendf, etc.).
(defmacro define-modify-macro (name lambda-list function &rest doc)
  (declare (ignore doc))
  (let ((vars '()) (rest-var nil) (mode :req) (place (gensym)))
    (dolist (item lambda-list)
      (cond ((eq item '&rest) (setq mode :rest))
            ((eq item '&optional) (setq mode :opt))
            ((eq mode :rest) (setq rest-var item))
            (t (push (if (consp item) (car item) item) vars))))
    (setq vars (reverse vars))
    `(defmacro ,name (,place ,@lambda-list)
       (list 'setf ,place
             (cons ',function
                   (cons ,place
                         (append (list ,@vars) ,(or rest-var 'nil))))))))


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
  ;; ANSI: signal a correctable TYPE-ERROR with a STORE-VALUE restart that
  ;; supplies a new value for PLACE. Returns NIL when PLACE already conforms.
  `(unless (typep ,place ',typespec)
     (restart-case
         (error 'type-error :datum ,place :expected-type ',typespec)
       (store-value (value) (setf ,place value)))))

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

;;; ---------------------------------------------------------------------------
;;; Standard reader/printer control variables and WITH-STANDARD-IO-SYNTAX.
;;;
;;; The control variables are bound globally to their ANSI defaults so bare
;;; references (and ASDF's save/restore idiom `(*readtable* *readtable*)`) work.
;;; The interpreter's printer does not yet consult most of them, so the defaults
;;; are currently inert on output; they exist so library code that binds and
;;; reads them behaves correctly. *readtable* is an opaque placeholder — ASDF
;;; only saves and restores it. See issue bliss-2pt.1.
;;; ---------------------------------------------------------------------------

(defvar *readtable* :standard-readtable)
(defvar *print-array* t)
(defvar *print-base* 10)
(defvar *print-case* :upcase)
(defvar *print-circle* nil)
(defvar *print-escape* t)
(defvar *print-gensym* t)
(defvar *print-length* nil)
(defvar *print-level* nil)
(defvar *print-lines* nil)
(defvar *print-miser-width* nil)
(defvar *print-pretty* nil)
(defvar *print-radix* nil)
(defvar *print-readably* nil)
(defvar *print-right-margin* nil)
(defvar *read-base* 10)
(defvar *read-default-float-format* 'single-float)
(defvar *read-eval* t)
(defvar *read-suppress* nil)

;;; WITH-STANDARD-IO-SYNTAX: evaluate BODY with the standard reader/printer
;;; variables bound to their ANSI-standard values. An empty body yields NIL.
(defmacro with-standard-io-syntax (&rest body)
  `(let ((*readtable* :standard-readtable)
         (*package* "COMMON-LISP-USER")
         (*print-array* t)
         (*print-base* 10)
         (*print-case* :upcase)
         (*print-circle* nil)
         (*print-escape* t)
         (*print-gensym* t)
         (*print-length* nil)
         (*print-level* nil)
         (*print-lines* nil)
         (*print-miser-width* nil)
         (*print-pretty* nil)
         (*print-radix* nil)
         (*print-readably* t)
         (*print-right-margin* nil)
         (*read-base* 10)
         (*read-default-float-format* 'single-float)
         (*read-eval* t)
         (*read-suppress* nil))
     ,@body))

;;; ---------------------------------------------------------------------------
;;; Standard restart-invoking functions (CLHS 9.1.4.2.2). Each finds the named
;;; restart (optionally associated with CONDITION) and invokes it; STORE-VALUE
;;; and USE-VALUE pass their argument to the restart function. See spec §5.4.
;;; ---------------------------------------------------------------------------

(defun continue (&optional condition)
  "Invoke the most recent CONTINUE restart, or return NIL if none is active."
  (let ((r (find-restart 'continue condition)))
    (when r (invoke-restart r))))

(defun abort (&optional condition)
  "Invoke the most recent ABORT restart; signal an error if none is active."
  (let ((r (find-restart 'abort condition)))
    (if r (invoke-restart r) (error "no ABORT restart is active"))))

(defun muffle-warning (&optional condition)
  "Invoke the most recent MUFFLE-WARNING restart; error if none is active."
  (let ((r (find-restart 'muffle-warning condition)))
    (if r (invoke-restart r) (error "no MUFFLE-WARNING restart is active"))))

(defun store-value (value &optional condition)
  "Invoke the most recent STORE-VALUE restart with VALUE, or NIL if none."
  (let ((r (find-restart 'store-value condition)))
    (when r (invoke-restart r value))))

(defun use-value (value &optional condition)
  "Invoke the most recent USE-VALUE restart with VALUE, or NIL if none."
  (let ((r (find-restart 'use-value condition)))
    (when r (invoke-restart r value))))

;;; ---------------------------------------------------------------------------
;;; REDUCE and the sequence -IF / -IF-NOT predicate family. Defined in Lisp over
;;; ELT/LENGTH (general sequences) and FUNCALL (any function), since the stdlib's
;;; internal apply helper can't invoke interpreter builtins. See spec §5.6.
;;; ---------------------------------------------------------------------------

(defun reduce (fn seq &key key from-end (start 0) end initial-value)
  (let ((items (coerce seq 'list)))
    (when (or (> start 0) end)
      (setq items (subseq items start (or end (length items)))))
    (when key (setq items (mapcar key items)))
    (when from-end (setq items (reverse items)))
    (if (null items)
        (if initial-value initial-value (funcall fn))
        (let ((acc (if initial-value initial-value (pop items))))
          (dolist (x items acc)
            (setq acc (if from-end (funcall fn x acc) (funcall fn acc x))))))))

(defun find-if (pred seq &key key)
  (dotimes (i (length seq) nil)
    (let ((e (elt seq i)))
      (when (funcall pred (if key (funcall key e) e)) (return e)))))

(defun find-if-not (pred seq &key key)
  (dotimes (i (length seq) nil)
    (let ((e (elt seq i)))
      (unless (funcall pred (if key (funcall key e) e)) (return e)))))

(defun position-if (pred seq &key key)
  (dotimes (i (length seq) nil)
    (let ((e (elt seq i)))
      (when (funcall pred (if key (funcall key e) e)) (return i)))))

(defun position-if-not (pred seq &key key)
  (dotimes (i (length seq) nil)
    (let ((e (elt seq i)))
      (unless (funcall pred (if key (funcall key e) e)) (return i)))))

(defun count-if (pred seq &key key)
  (let ((n 0))
    (dotimes (i (length seq) n)
      (let ((e (elt seq i)))
        (when (funcall pred (if key (funcall key e) e)) (incf n))))))

(defun count-if-not (pred seq &key key)
  (let ((n 0))
    (dotimes (i (length seq) n)
      (let ((e (elt seq i)))
        (unless (funcall pred (if key (funcall key e) e)) (incf n))))))

(defun member-if (pred list &key key)
  (loop for l on list
        when (funcall pred (if key (funcall key (car l)) (car l)))
          return l))

(defun member-if-not (pred list &key key)
  (loop for l on list
        unless (funcall pred (if key (funcall key (car l)) (car l)))
          return l))

(defun assoc-if (pred alist &key key)
  (dolist (pair alist nil)
    (when (and (consp pair)
               (funcall pred (if key (funcall key (car pair)) (car pair))))
      (return pair))))

(defun assoc-if-not (pred alist &key key)
  (dolist (pair alist nil)
    (when (and (consp pair)
               (not (funcall pred (if key (funcall key (car pair)) (car pair)))))
      (return pair))))

(defun delete-if (pred seq &rest keys) (apply #'remove-if pred seq keys))
(defun delete-if-not (pred seq &rest keys) (apply #'remove-if-not pred seq keys))

;;; ---------------------------------------------------------------------------
;;; CxR list accessors (all compositions of CAR/CDR two to four deep).
;;; ---------------------------------------------------------------------------

(defun caar (x) (car (car x)))
(defun cadr (x) (car (cdr x)))
(defun cdar (x) (cdr (car x)))
(defun cddr (x) (cdr (cdr x)))
(defun caaar (x) (car (caar x)))
(defun caadr (x) (car (cadr x)))
(defun cadar (x) (car (cdar x)))
(defun caddr (x) (car (cddr x)))
(defun cdaar (x) (cdr (caar x)))
(defun cdadr (x) (cdr (cadr x)))
(defun cddar (x) (cdr (cdar x)))
(defun cdddr (x) (cdr (cddr x)))
(defun caaaar (x) (car (caaar x)))
(defun caaadr (x) (car (caadr x)))
(defun caadar (x) (car (cadar x)))
(defun caaddr (x) (car (caddr x)))
(defun cadaar (x) (car (cdaar x)))
(defun cadadr (x) (car (cdadr x)))
(defun caddar (x) (car (cddar x)))
(defun cadddr (x) (car (cdddr x)))
(defun cdaaar (x) (cdr (caaar x)))
(defun cdaadr (x) (cdr (caadr x)))
(defun cdadar (x) (cdr (cadar x)))
(defun cdaddr (x) (cdr (caddr x)))
(defun cddaar (x) (cdr (cdaar x)))
(defun cddadr (x) (cdr (cdadr x)))
(defun cdddar (x) (cdr (cddar x)))
(defun cddddr (x) (cdr (cdddr x)))

;;; ---------------------------------------------------------------------------
;;; PSETQ and the DO / DO* iteration macros.
;;; ---------------------------------------------------------------------------

;; Parallel assignment: evaluate every value form, then assign (via temporaries).
(defmacro psetq (&rest pairs)
  (let ((bindings nil) (assigns nil) (p pairs))
    (loop while (consp (cdr p)) do
      (let ((var (car p)) (tmp (gensym)))
        (setq bindings (cons (list tmp (cadr p)) bindings))
        (setq assigns (cons (list 'setq var tmp) assigns))
        (setq p (cddr p))))
    `(let ,(reverse bindings) ,@(reverse assigns) nil)))

;; Interleave two lists: (a b) (x y) => (a x b y). Helper for DO's step forms.
(defun %zip-pairs (a b)
  (if (or (null a) (null b))
      nil
      (cons (car a) (cons (car b) (%zip-pairs (cdr a) (cdr b))))))

(defun %do-var (b) (if (consp b) (car b) b))
(defun %do-init (b) (if (consp b) (cadr b) nil))
(defun %do-step (b)
  (if (and (consp b) (cddr b)) (caddr b) (%do-var b)))

(defmacro do (bindings end-test &rest body)
  (let ((vars (mapcar (function %do-var) bindings))
        (inits (mapcar (function %do-init) bindings))
        (steps (mapcar (function %do-step) bindings))
        (top (gensym)))
    `(block nil
       (let ,(mapcar (function list) vars inits)
         (tagbody
            ,top
            (when ,(car end-test) (return (progn ,@(cdr end-test))))
            ,@body
            (psetq ,@(%zip-pairs vars steps))
            (go ,top))))))

(defmacro do* (bindings end-test &rest body)
  (let ((vars (mapcar (function %do-var) bindings))
        (steps (mapcar (function %do-step) bindings))
        (top (gensym)))
    `(block nil
       (let* ,(mapcar (function list) vars (mapcar (function %do-init) bindings))
         (tagbody
            ,top
            (when ,(car end-test) (return (progn ,@(cdr end-test))))
            ,@body
            ,@(mapcar (lambda (v s) (list 'setq v s)) vars steps)
            (go ,top))))))
