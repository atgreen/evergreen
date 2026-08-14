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

;; with-hash-table-iterator: (with-hash-table-iterator (name table) . body)
;; Within BODY, calling (name) returns (values more-p key value), advancing over
;; a snapshot of TABLE's entries, and (values nil) once exhausted (bliss-jtc.8).
(defmacro with-hash-table-iterator (spec &rest body)
  (let ((name (car spec)) (table (car (cdr spec)))
        (rest (gensym)) (pair (gensym)))
    `(let ((,rest (hash-table-entries ,table)))
       (flet ((,name ()
                (if ,rest
                    (let ((,pair (car ,rest)))
                      (setq ,rest (cdr ,rest))
                      (values t (car ,pair) (cdr ,pair)))
                    (values nil))))
         ,@body))))

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

(defun remove (item seq &key key test test-not (start 0) end count from-end)
  ;; Honour :key/:test/:test-not/:start/:end/:count/:from-end.  Operates over a
  ;; list view and returns a fresh sequence of the same type as SEQ.  When
  ;; :count limits removals, :from-end selects the trailing matches rather than
  ;; the leading ones (CLHS 17.3).  See bliss-0l1.
  (let* ((items (coerce seq 'list))
         (testfn (or test test-not #'eql))
         (neg (if test-not t nil))
         (chosen (%match-positions
                  (lambda (x) (%seq-match item x key testfn neg))
                  items start end count from-end))
         (result nil)
         (i 0))
    (dolist (x items)
      (unless (member i chosen) (push x result))
      (incf i))
    (%coerce-like (reverse result) seq)))

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

(defun find-if (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (flet ((matchp (e) (funcall pred (if key (funcall key e) e))))
      (if from-end
          (loop for i from (1- stop) downto start
                for e = (elt seq i)
                when (matchp e) return e)
          (loop for i from start below stop
                for e = (elt seq i)
                when (matchp e) return e)))))

(defun find-if-not (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (flet ((matchp (e) (not (funcall pred (if key (funcall key e) e)))))
      (if from-end
          (loop for i from (1- stop) downto start
                for e = (elt seq i)
                when (matchp e) return e)
          (loop for i from start below stop
                for e = (elt seq i)
                when (matchp e) return e)))))

(defun position-if (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (flet ((matchp (e) (funcall pred (if key (funcall key e) e))))
      (if from-end
          (loop for i from (1- stop) downto start
                when (matchp (elt seq i)) return i)
          (loop for i from start below stop
                when (matchp (elt seq i)) return i)))))

(defun position-if-not (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (flet ((matchp (e) (not (funcall pred (if key (funcall key e) e)))))
      (if from-end
          (loop for i from (1- stop) downto start
                when (matchp (elt seq i)) return i)
          (loop for i from start below stop
                when (matchp (elt seq i)) return i)))))

(defun count-if (pred seq &key key (start 0) end)
  (let ((stop (or end (length seq))))
    (loop for i from start below stop
          count (funcall pred (let ((e (elt seq i))) (if key (funcall key e) e))))))

(defun count-if-not (pred seq &key key (start 0) end)
  (let ((stop (or end (length seq))))
    (loop for i from start below stop
          count (not (funcall pred (let ((e (elt seq i))) (if key (funcall key e) e)))))))

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

;;; Item-based FIND / POSITION / COUNT. Defined in Lisp over ELT/LENGTH/FUNCALL
;;; (like the -IF family and REDUCE) because the stdlib's internal apply helper
;;; cannot invoke an interpreter :key/:test — a real function used to reach it
;;; and panic the whole process. See bliss-0l1 and spec §5.6.

(defun find (item seq &key key test test-not (start 0) end from-end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (flet ((matchp (e)
             (let ((r (funcall testfn item (if key (funcall key e) e))))
               (if neg (not r) r))))
      (if from-end
          (loop for i from (1- stop) downto start
                for e = (elt seq i)
                when (matchp e) return e)
          (loop for i from start below stop
                for e = (elt seq i)
                when (matchp e) return e)))))

(defun position (item seq &key key test test-not (start 0) end from-end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (flet ((matchp (e)
             (let ((r (funcall testfn item (if key (funcall key e) e))))
               (if neg (not r) r))))
      (if from-end
          (loop for i from (1- stop) downto start
                when (matchp (elt seq i)) return i)
          (loop for i from start below stop
                when (matchp (elt seq i)) return i)))))

(defun count (item seq &key key test test-not (start 0) end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (flet ((matchp (e)
             (let ((r (funcall testfn item (if key (funcall key e) e))))
               (if neg (not r) r))))
      (loop for i from start below stop
            count (matchp (elt seq i))))))

(defun delete-if (pred seq &rest keys) (apply #'remove-if pred seq keys))
(defun delete-if-not (pred seq &rest keys) (apply #'remove-if-not pred seq keys))
;; DELETE / DELETE-DUPLICATES are permitted to modify their argument but a
;; conforming program must not depend on it; delegate to the non-destructive
;; versions, which is a legal implementation.
(defun delete (item seq &rest keys) (apply #'remove item seq keys))
(defun delete-duplicates (seq &rest keys) (apply #'remove-duplicates seq keys))

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

;;; ---------------------------------------------------------------------------
;;; Character functions (over CHAR-CODE / CODE-CHAR; ASCII case mapping).
;;; ---------------------------------------------------------------------------

(defun char= (a b) (= (char-code a) (char-code b)))
(defun char/= (a b) (/= (char-code a) (char-code b)))
(defun char< (a b) (< (char-code a) (char-code b)))
(defun char> (a b) (> (char-code a) (char-code b)))
(defun char<= (a b) (<= (char-code a) (char-code b)))
(defun char>= (a b) (>= (char-code a) (char-code b)))

(defun upper-case-p (c) (and (>= (char-code c) 65) (<= (char-code c) 90)))
(defun lower-case-p (c) (and (>= (char-code c) 97) (<= (char-code c) 122)))
(defun char-upcase (c) (if (lower-case-p c) (code-char (- (char-code c) 32)) c))
(defun char-downcase (c) (if (upper-case-p c) (code-char (+ (char-code c) 32)) c))
(defun alpha-char-p (c) (or (upper-case-p c) (lower-case-p c)))
(defun digit-char-p (c &optional (radix 10))
  (let ((code (char-code c)))
    (if (and (>= code 48) (<= code 57))
        (let ((d (- code 48))) (if (< d radix) d nil))
        nil)))
(defun alphanumericp (c)
  (or (alpha-char-p c) (and (>= (char-code c) 48) (<= (char-code c) 57))))

;; STRING-UPCASE / STRING-DOWNCASE honour the bounding indices :START/:END,
;; transforming only characters in that half-open range and copying the rest.
(defun string-upcase (s &key (start 0) end)
  (let* ((str (string s)) (stop (or end (length str))) (i 0) (res nil))
    (dolist (c (coerce str 'list) (coerce (reverse res) 'string))
      (push (if (and (>= i start) (< i stop)) (char-upcase c) c) res)
      (incf i))))
(defun string-downcase (s &key (start 0) end)
  (let* ((str (string s)) (stop (or end (length str))) (i 0) (res nil))
    (dolist (c (coerce str 'list) (coerce (reverse res) 'string))
      (push (if (and (>= i start) (< i stop)) (char-downcase c) c) res)
      (incf i))))

;;; ---------------------------------------------------------------------------
;;; Additional list functions.
;;; ---------------------------------------------------------------------------

(defun nthcdr (n list)
  (if (or (<= n 0) (null list)) list (nthcdr (- n 1) (cdr list))))
(defun last (list &optional (n 1))
  (nthcdr (max 0 (- (length list) n)) list))
(defun butlast (list &optional (n 1))
  (subseq list 0 (max 0 (- (length list) n))))
(defun mapc (fn &rest lists)
  (apply (function mapcar) fn lists)
  (car lists))
(defun mapcan (fn &rest lists)
  (apply (function append) (apply (function mapcar) fn lists)))
(defun getf (plist key &optional default)
  (do ((p plist (cddr p)))
      ((null p) default)
    (when (eq (car p) key) (return (cadr p)))))
(defun nreverse (seq) (reverse seq))

;;; ---------------------------------------------------------------------------
;;; GCD / LCM and the STRING-TRIM family.
;;; ---------------------------------------------------------------------------

(defun %gcd2 (a b) (if (= b 0) a (%gcd2 b (mod a b))))

(defun gcd (&rest integers)
  (if (null integers)
      0
      (reduce (lambda (a b) (%gcd2 (abs a) (abs b))) integers)))

(defun lcm (&rest integers)
  (if (null integers)
      1
      (reduce (lambda (a b)
                (if (or (= a 0) (= b 0))
                    0
                    (/ (abs (* a b)) (%gcd2 (abs a) (abs b)))))
              integers)))

(defun string-left-trim (bag s)
  (let ((str (string s)) (n (length (string s))) (i 0))
    (loop while (and (< i n) (find (elt str i) bag)) do (incf i))
    (subseq str i)))

(defun string-right-trim (bag s)
  (let ((str (string s)) (i (length (string s))))
    (loop while (and (> i 0) (find (elt str (- i 1)) bag)) do (decf i))
    (subseq str 0 i)))

(defun string-trim (bag s)
  (string-left-trim bag (string-right-trim bag s)))

;;; ---------------------------------------------------------------------------
;;; More list/tree/string functions.
;;; ---------------------------------------------------------------------------

(defun char (s i) (elt s i))
(defun acons (key datum alist) (cons (cons key datum) alist))
(defun list-length (list) (length list))
(defun nconc (&rest lists) (apply (function append) lists))
(defun revappend (x y) (append (reverse x) y))
(defun make-list (n &key initial-element) (loop repeat n collect initial-element))
(defun string-equal (a b) (string= (string-downcase a) (string-downcase b)))

(defun subst (new old tree)
  (cond ((eql tree old) new)
        ((consp tree) (cons (subst new old (car tree))
                            (subst new old (cdr tree))))
        (t tree)))

;;; ===========================================================================
;;; Conformance layer: sequence/list/string/number/control functions that were
;;; missing from the bootstrap prelude.  Everything here is pure Lisp on top of
;;; the existing primitives (ELT, LENGTH, COERCE, FLOOR, REM, EXPT, ...).  The
;;; builtin MOD is unreliable for negative arguments and MEMBER's :KEY is
;;; broken, so these definitions avoid both (see CONFORMANCE-TODO.md).
;;; ===========================================================================

;;; --- shared helpers --------------------------------------------------------

;; Return a fresh sequence of the same type as ORIG holding the elements of the
;; list LIST.  Used to keep REMOVE/SUBSTITUTE/FILL/... type-preserving.
(defun %coerce-like (list orig)
  (cond ((stringp orig) (coerce list 'string))
        ((listp orig) list)
        (t (coerce list 'vector))))

;; Does ITEM match ELT under TESTFN, with KEY applied to ELT and NEG inverting?
(defun %seq-match (item elt key testfn neg)
  (let ((r (funcall testfn item (if key (funcall key elt) elt))))
    (if neg (not r) r)))

;; Indices (ascending) in ITEMS where PREDFN holds, restricted to [START,STOP);
;; when COUNT is supplied keep only COUNT of them, trailing ones if FROM-END.
(defun %match-positions (predfn items start end count from-end)
  (let* ((len (length items)) (stop (or end len))
         (positions nil) (i 0))
    (dolist (x items)
      (when (and (>= i start) (< i stop) (funcall predfn x))
        (push i positions))
      (incf i))
    (setq positions (reverse positions))
    (if count
        (if from-end
            (last positions count)
            (subseq positions 0 (min count (length positions))))
        positions)))

;; Membership test honouring KEY/TESTFN/NEG (KEY is applied to each element of
;; LIST; ITEM is assumed already keyed by the caller).
(defun %seq-find (item list key testfn neg)
  (dolist (x list nil)
    (when (%seq-match item x key testfn neg) (return t))))

;;; --- list constructors / accessors -----------------------------------------

(defun copy-list (list)
  (if (consp list)
      (cons (car list) (copy-list (cdr list)))
      list))

(defun copy-tree (tree)
  (if (consp tree)
      (cons (copy-tree (car tree)) (copy-tree (cdr tree)))
      tree))

(defun copy-seq (seq) (subseq seq 0))

(defun list* (&rest args)
  (if (null (cdr args))
      (car args)
      (cons (car args) (apply (function list*) (cdr args)))))

(defun nbutlast (list &optional (n 1)) (butlast list n))

(defun ldiff (list object)
  (if (or (null list) (eql list object) (not (consp list)))
      nil
      (cons (car list) (ldiff (cdr list) object))))

(defun tailp (object list)
  (block nil
    (loop
      (when (eql object list) (return t))
      (if (consp list) (setq list (cdr list)) (return (eql object list))))))

(defun nreconc (list tail) (append (reverse list) tail))

;;; --- set operations (KEY/TEST honoured via %seq-find) -----------------------

(defun union (a b &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil))
        (result (copy-list b)))
    (dolist (x a result)
      (let ((kx (if key (funcall key x) x)))
        (unless (%seq-find kx b key testfn neg)
          (push x result))))))

(defun intersection (a b &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil)) (result nil))
    (dolist (x a (reverse result))
      (let ((kx (if key (funcall key x) x)))
        (when (%seq-find kx b key testfn neg)
          (push x result))))))

(defun set-difference (a b &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil)) (result nil))
    (dolist (x a (reverse result))
      (let ((kx (if key (funcall key x) x)))
        (unless (%seq-find kx b key testfn neg)
          (push x result))))))

(defun set-exclusive-or (a b &rest keys)
  (append (apply (function set-difference) a b keys)
          (apply (function set-difference) b a keys)))

(defun subsetp (a b &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil)))
    (dolist (x a t)
      (let ((kx (if key (funcall key x) x)))
        (unless (%seq-find kx b key testfn neg)
          (return nil))))))

;; Destructive variants are permitted to reuse structure; delegating to the
;; non-destructive forms is a conforming implementation.
(defun nunion (a b &rest keys) (apply (function union) a b keys))
(defun nintersection (a b &rest keys) (apply (function intersection) a b keys))
(defun nset-difference (a b &rest keys) (apply (function set-difference) a b keys))
(defun nset-exclusive-or (a b &rest keys) (apply (function set-exclusive-or) a b keys))

(defun adjoin (item list &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil)))
    (if (%seq-find (if key (funcall key item) item) list key testfn neg)
        list
        (cons item list))))

;;; PUSHNEW now delegates to ADJOIN so :TEST/:KEY are honoured.
(defmacro pushnew (item place &rest keys)
  `(setf ,place (adjoin ,item ,place ,@keys)))

;;; --- reverse-association and tree equality ---------------------------------

(defun rassoc (item alist &key key (test (function eql)) test-not)
  (let ((testfn (or test-not test)) (neg (if test-not t nil)))
    (dolist (pair alist nil)
      (when (and (consp pair) (%seq-match item (cdr pair) key testfn neg))
        (return pair)))))

(defun rassoc-if (pred alist &key key)
  (dolist (pair alist nil)
    (when (and (consp pair)
               (funcall pred (if key (funcall key (cdr pair)) (cdr pair))))
      (return pair))))

(defun rassoc-if-not (pred alist &key key)
  (dolist (pair alist nil)
    (when (and (consp pair)
               (not (funcall pred (if key (funcall key (cdr pair)) (cdr pair)))))
      (return pair))))

(defun %tree-equal (a b testfn neg)
  (if (and (consp a) (consp b))
      (and (%tree-equal (car a) (car b) testfn neg)
           (%tree-equal (cdr a) (cdr b) testfn neg))
      (if (or (consp a) (consp b))
          nil
          (let ((r (funcall testfn a b))) (if neg (not r) r)))))

(defun tree-equal (a b &key (test (function eql)) test-not)
  (%tree-equal a b (or test-not test) (if test-not t nil)))

;;; --- plist removal ----------------------------------------------------------

;; Returns (values NEW-PLIST FOUND-P); removes only the first matching pair.
(defun %remf (plist indicator)
  (let ((result nil) (found nil) (p plist))
    (loop while (consp p) do
      (if (and (not found) (consp (cdr p)) (eq (car p) indicator))
          (progn (setq found t) (setq p (cddr p)))
          (progn (push (car p) result) (setq p (cdr p)))))
    (values (reverse result) found)))

(defmacro remf (place indicator)
  (let ((np (gensym)) (fp (gensym)))
    `(multiple-value-bind (,np ,fp) (%remf ,place ,indicator)
       (setf ,place ,np)
       ,fp)))

;;; --- list mapping variants --------------------------------------------------

(defun maplist (fn &rest lists)
  (let ((result nil))
    (block nil
      (loop
        (when (some (function null) lists) (return))
        (push (apply fn lists) result)
        (setq lists (mapcar (function cdr) lists))))
    (reverse result)))

(defun mapl (fn &rest lists)
  (let ((first (car lists)))
    (block nil
      (loop
        (when (some (function null) lists) (return))
        (apply fn lists)
        (setq lists (mapcar (function cdr) lists))))
    first))

(defun mapcon (fn &rest lists)
  (apply (function append) (apply (function maplist) fn lists)))

;;; --- SUBST / SUBSTITUTE families -------------------------------------------

(defun subst-if (new pred tree &key key)
  (cond ((funcall pred (if key (funcall key tree) tree)) new)
        ((consp tree) (cons (subst-if new pred (car tree) :key key)
                            (subst-if new pred (cdr tree) :key key)))
        (t tree)))

(defun subst-if-not (new pred tree &key key)
  (subst-if new (lambda (x) (not (funcall pred x))) tree :key key))

(defun nsubst (new old tree &rest keys)
  (declare (ignore keys))
  (subst new old tree))
(defun nsubst-if (new pred tree &rest keys) (apply (function subst-if) new pred tree keys))
(defun nsubst-if-not (new pred tree &rest keys) (apply (function subst-if-not) new pred tree keys))

;; Build a new sequence replacing chosen positions with NEW.
(defun %substitute-list (new items chosen)
  (let ((res nil) (i 0))
    (dolist (x items (reverse res))
      (push (if (member i chosen) new x) res)
      (incf i))))

(defun substitute (new old seq &key key (test (function eql)) test-not
                                    (start 0) end count from-end)
  (let* ((items (coerce seq 'list))
         (testfn (or test-not test)) (neg (if test-not t nil))
         (chosen (%match-positions
                  (lambda (x) (%seq-match old x key testfn neg))
                  items start end count from-end)))
    (%coerce-like (%substitute-list new items chosen) seq)))

(defun substitute-if (new pred seq &key key (start 0) end count from-end)
  (let* ((items (coerce seq 'list))
         (chosen (%match-positions
                  (lambda (x) (funcall pred (if key (funcall key x) x)))
                  items start end count from-end)))
    (%coerce-like (%substitute-list new items chosen) seq)))

(defun substitute-if-not (new pred seq &rest keys)
  (apply (function substitute-if) new (lambda (x) (not (funcall pred x))) seq keys))

(defun nsubstitute (new old seq &rest keys) (apply (function substitute) new old seq keys))
(defun nsubstitute-if (new pred seq &rest keys) (apply (function substitute-if) new pred seq keys))
(defun nsubstitute-if-not (new pred seq &rest keys) (apply (function substitute-if-not) new pred seq keys))

;;; --- REMOVE-DUPLICATES (spec-faithful: default keeps last occurrence) ------

(defun remove-duplicates (seq &key key (test (function eql)) test-not
                                    from-end (start 0) end)
  (let* ((items (coerce seq 'list)) (len (length items)) (stop (or end len))
         (testfn (or test-not test)) (neg (if test-not t nil))
         (res nil) (i 0))
    (dolist (x items)
      (let ((keep t))
        (when (and (>= i start) (< i stop))
          (let ((j 0) (kx (if key (funcall key x) x)))
            (dolist (y items)
              (when (and (/= i j) (>= j start) (< j stop)
                         (%seq-match kx y key testfn neg))
                (if from-end
                    (when (< j i) (setq keep nil))
                    (when (> j i) (setq keep nil))))
              (incf j))))
        (when keep (push x res)))
      (incf i))
    (%coerce-like (reverse res) seq)))

(defun delete-duplicates (seq &rest keys)
  (apply (function remove-duplicates) seq keys))

;;; --- FILL / REPLACE / SEARCH / MISMATCH / MERGE ----------------------------

(defun fill (seq item &key (start 0) end)
  (let* ((items (coerce seq 'list)) (len (length items)) (stop (or end len))
         (i 0) (res nil))
    (dolist (x items)
      (push (if (and (>= i start) (< i stop)) item x) res)
      (incf i))
    (%coerce-like (reverse res) seq)))

(defun replace (seq1 seq2 &key (start1 0) end1 (start2 0) end2)
  (let* ((l1 (coerce seq1 'list)) (l2 (coerce seq2 'list))
         (e1 (or end1 (length l1))) (e2 (or end2 (length l2)))
         (n (min (- e1 start1) (- e2 start2)))
         (res nil) (i 0))
    (dolist (x l1)
      (if (and (>= i start1) (< i (+ start1 n)))
          (push (nth (+ start2 (- i start1)) l2) res)
          (push x res))
      (incf i))
    (%coerce-like (reverse res) seq1)))

(defun %match-at (pat list start key testfn neg)
  (let ((ok t) (i start))
    (block nil
      (dolist (p pat ok)
        (let ((x (nth i list)))
          (unless (%seq-match (if key (funcall key p) p) x key testfn neg)
            (setq ok nil) (return)))
        (incf i)))))

(defun search (seq1 seq2 &key key (test (function eql)) test-not
                              (start1 0) end1 (start2 0) end2 from-end)
  (let* ((l1 (coerce seq1 'list)) (l2 (coerce seq2 'list))
         (e1 (or end1 (length l1))) (e2 (or end2 (length l2)))
         (pat (subseq l1 start1 e1)) (plen (length pat))
         (testfn (or test-not test)) (neg (if test-not t nil))
         (matches nil))
    (if (= plen 0)
        (if from-end e2 start2)
        (progn
          (loop for i from start2 to (- e2 plen) do
            (when (%match-at pat l2 i key testfn neg) (push i matches)))
          (cond ((null matches) nil)
                (from-end (car matches))          ; largest index (pushed last)
                (t (car (last matches))))))))      ; smallest index

(defun mismatch (seq1 seq2 &key key (test (function eql)) test-not
                                (start1 0) end1 (start2 0) end2 from-end)
  (let* ((l1 (coerce seq1 'list)) (l2 (coerce seq2 'list))
         (e1 (or end1 (length l1))) (e2 (or end2 (length l2)))
         (s1 (subseq l1 start1 e1)) (s2 (subseq l2 start2 e2))
         (n1 (length s1)) (n2 (length s2))
         (testfn (or test-not test)) (neg (if test-not t nil)))
    (flet ((eqp (a b)
             (%seq-match (if key (funcall key a) a) b key testfn neg)))
      (if from-end
          (let ((j 0))
            (block nil
              (loop
                (when (or (>= j n1) (>= j n2)) (return))
                (unless (eqp (nth (- n1 1 j) s1) (nth (- n2 1 j) s2)) (return))
                (incf j)))
            (if (and (= j n1) (= j n2)) nil (+ start1 (- n1 j))))
          (let ((i 0))
            (block nil
              (loop
                (when (or (>= i n1) (>= i n2)) (return))
                (unless (eqp (nth i s1) (nth i s2)) (return))
                (incf i)))
            (if (and (= i n1) (= i n2)) nil (+ start1 i)))))))

(defun merge (result-type seq1 seq2 predicate &key key)
  (let ((l1 (coerce seq1 'list)) (l2 (coerce seq2 'list)) (res nil))
    (block nil
      (loop
        (cond ((null l1) (setq res (append (reverse res) l2)) (return))
              ((null l2) (setq res (append (reverse res) l1)) (return))
              ((funcall predicate
                        (if key (funcall key (car l2)) (car l2))
                        (if key (funcall key (car l1)) (car l1)))
               (push (car l2) res) (setq l2 (cdr l2)))
              (t (push (car l1) res) (setq l1 (cdr l1))))))
    (coerce res result-type)))

;;; --- character predicates and naming ---------------------------------------

(defun char-int (c) (char-code c))
(defun both-case-p (c) (or (upper-case-p c) (lower-case-p c)))
(defun standard-char-p (c)
  (let ((code (char-code c)))
    (or (= code 10) (and (>= code 32) (< code 127)))))
(defun graphic-char-p (c)
  (let ((code (char-code c)))
    (or (= code 32) (and (> code 32) (< code 127)) (>= code 160))))

(defun %char-key (c) (char-code (char-upcase c)))

;; Case-insensitive character comparisons.  EQUAL/NOT-EQUAL require all/none of
;; the arguments equal; the ordered comparisons require a monotonic chain.
(defun %char-chain (fn cs)
  (if (or (null cs) (null (cdr cs)))
      t
      (and (funcall fn (car cs) (cadr cs)) (%char-chain fn (cdr cs)))))

(defun char-equal (&rest cs)
  (%char-chain (lambda (a b) (= (%char-key a) (%char-key b))) cs))
(defun char-lessp (&rest cs)
  (%char-chain (lambda (a b) (< (%char-key a) (%char-key b))) cs))
(defun char-greaterp (&rest cs)
  (%char-chain (lambda (a b) (> (%char-key a) (%char-key b))) cs))
(defun char-not-greaterp (&rest cs)
  (%char-chain (lambda (a b) (<= (%char-key a) (%char-key b))) cs))
(defun char-not-lessp (&rest cs)
  (%char-chain (lambda (a b) (>= (%char-key a) (%char-key b))) cs))
(defun char-not-equal (&rest cs)
  ;; every pair must differ (case-insensitively).  RETURN-FROM (not RETURN)
  ;; because the inner DOLIST establishes its own BLOCK NIL.
  (block done
    (loop for tail on cs do
      (dolist (o (cdr tail))
        (when (= (%char-key (car tail)) (%char-key o)) (return-from done nil))))
    t))

(defun digit-char (weight &optional (radix 10))
  (if (and (integerp weight) (>= weight 0) (< weight radix) (< weight 36))
      (if (< weight 10)
          (code-char (+ 48 weight))
          (code-char (+ 55 weight)))
      nil))

(defun char-name (c)
  (let ((code (char-code c)))
    (cond ((= code 32) "Space")
          ((= code 10) "Newline")
          ((= code 9) "Tab")
          ((= code 13) "Return")
          ((= code 12) "Page")
          ((= code 8) "Backspace")
          ((= code 127) "Rubout")
          ((= code 0) "Null")
          ((= code 7) "Bell")
          ((= code 27) "Escape")
          ((= code 65533) "Rubout")
          (t nil))))

(defun name-char (name)
  (let ((n (string name)))
    (cond ((string-equal n "Space") #\Space)
          ((string-equal n "Newline") #\Newline)
          ((string-equal n "Linefeed") #\Newline)
          ((string-equal n "Tab") (code-char 9))
          ((string-equal n "Return") (code-char 13))
          ((string-equal n "Page") (code-char 12))
          ((string-equal n "Backspace") (code-char 8))
          ((string-equal n "Rubout") (code-char 127))
          ((string-equal n "Delete") (code-char 127))
          ((string-equal n "Null") (code-char 0))
          ((string-equal n "Nul") (code-char 0))
          ((string-equal n "Bell") (code-char 7))
          ((string-equal n "Escape") (code-char 27))
          (t nil))))

;;; --- string builders and STRING-CAPITALIZE / N-string ops ------------------

(defun make-string (n &key (initial-element #\Space) element-type)
  (declare (ignore element-type))
  (coerce (make-list n :initial-element initial-element) 'string))

(defun string-capitalize (s &key (start 0) end)
  (let* ((str (string s)) (stop (or end (length str)))
         (res nil) (i 0) (in-word nil))
    (dolist (c (coerce str 'list))
      (if (and (>= i start) (< i stop))
          (if (alphanumericp c)
              (progn
                (push (if in-word (char-downcase c) (char-upcase c)) res)
                (setq in-word t))
              (progn (push c res) (setq in-word nil)))
          (push c res))
      (incf i))
    (coerce (reverse res) 'string)))

;; The N-string operators cannot mutate in place here (no settable string
;; elements), so they return a freshly transformed string.
(defun nstring-upcase (s &rest keys) (apply (function string-upcase) s keys))
(defun nstring-downcase (s &rest keys) (apply (function string-downcase) s keys))
(defun nstring-capitalize (s &rest keys) (apply (function string-capitalize) s keys))

;;; --- string comparison family (return mismatch index or NIL) ---------------

;; Compare substrings A[sa,ea) and B[sb,eb).  Returns (values REL IDX) where REL
;; is one of '<, '>, '= and IDX is the absolute index in A at the decision
;; point.  FOLD requests a case-insensitive comparison.
(defun %str-cmp (a b sa ea sb eb fold)
  (let ((i sa) (j sb))
    (block nil
      (loop
        (cond ((and (>= i ea) (>= j eb)) (return (values '= i)))
              ((>= i ea) (return (values '< i)))
              ((>= j eb) (return (values '> i)))
              (t (let ((ca (char a i)) (cb (char b j)))
                   (when fold (setq ca (char-upcase ca) cb (char-upcase cb)))
                   (cond ((char< ca cb) (return (values '< i)))
                         ((char< cb ca) (return (values '> i)))
                         (t (incf i) (incf j))))))))))

(defmacro %defstringcmp (name accept fold)
  `(defun ,name (str1 str2 &key (start1 0) end1 (start2 0) end2)
     (let ((a (string str1)) (b (string str2)))
       (multiple-value-bind (rel idx)
           (%str-cmp a b start1 (or end1 (length a))
                     start2 (or end2 (length b)) ,fold)
         (if (member rel ,accept) idx nil)))))

;; Case-sensitive (STRING< / STRING> / STRING= already exist as builtins; add
;; the remaining relational operators).
(%defstringcmp string<= '(< =) nil)
(%defstringcmp string>= '(> =) nil)
(%defstringcmp string/= '(< >) nil)
;; Case-insensitive family.
(%defstringcmp string-lessp '(<) t)
(%defstringcmp string-greaterp '(>) t)
(%defstringcmp string-not-greaterp '(< =) t)
(%defstringcmp string-not-lessp '(> =) t)
(%defstringcmp string-not-equal '(< >) t)

;;; --- integer bit operations (non-negative; see CONFORMANCE-TODO.md) --------

(defun floatp (x) (typep x 'float))
(defun integerp (x) (typep x 'integer))
(defun rationalp (x) (or (integerp x) (typep x 'ratio)))
(defun realp (x) (or (rationalp x) (floatp x)))
(defun complexp (x) (typep x 'complex))
(defun characterp (x) (typep x 'character))
(defun functionp (x) (typep x 'function))

(defun ash (n count)
  (if (>= count 0)
      (* n (expt 2 count))
      (values (floor n (expt 2 (- count))))))

(defun lognot (n) (- (- n) 1))

;; Low bit of N, computed via floor so it is correct for negative (two's
;; complement) operands: N - 2*floor(N/2) is 0 or 1 for any integer.
(defun %lowbit (n) (- n (* 2 (floor n 2))))

;; The two-argument bitwise kernels recurse on floor(N/2) — an arithmetic shift
;; that realises two's-complement semantics for negatives — with base cases at 0
;; (all zero bits above) and -1 (all one bits above).
(defun %logand2 (a b)
  (cond ((= a 0) 0) ((= b 0) 0)
        ((= a -1) b) ((= b -1) a)
        (t (+ (* 2 (%logand2 (floor a 2) (floor b 2)))
              (if (and (= (%lowbit a) 1) (= (%lowbit b) 1)) 1 0)))))
(defun %logior2 (a b)
  (cond ((= a 0) b) ((= b 0) a)
        ((= a -1) -1) ((= b -1) -1)
        (t (+ (* 2 (%logior2 (floor a 2) (floor b 2)))
              (if (or (= (%lowbit a) 1) (= (%lowbit b) 1)) 1 0)))))
(defun %logxor2 (a b)
  (cond ((= a 0) b) ((= b 0) a)
        ((= a -1) (lognot b)) ((= b -1) (lognot a))
        (t (+ (* 2 (%logxor2 (floor a 2) (floor b 2)))
              (if (= (%lowbit a) (%lowbit b)) 0 1)))))

(defun logand (&rest ints)
  (if (null ints) -1 (reduce (function %logand2) ints)))
(defun logior (&rest ints)
  (if (null ints) 0 (reduce (function %logior2) ints)))
(defun logxor (&rest ints)
  (if (null ints) 0 (reduce (function %logxor2) ints)))
(defun logeqv (&rest ints)
  (if (null ints) -1 (lognot (apply (function logxor) ints))))
(defun lognand (a b) (lognot (logand a b)))
(defun lognor (a b) (lognot (logior a b)))
(defun logandc1 (a b) (logand (lognot a) b))
(defun logandc2 (a b) (logand a (lognot b)))
(defun logorc1 (a b) (logior (lognot a) b))
(defun logorc2 (a b) (logior a (lognot b)))

(defun logtest (a b) (not (zerop (logand a b))))
(defun logbitp (index n)
  (= 1 (%lowbit (floor n (expt 2 index)))))

(defun integer-length (n)
  (cond ((< n 0) (integer-length (lognot n)))
        ((= n 0) 0)
        (t (1+ (integer-length (floor n 2))))))

(defun logcount (n)
  (cond ((< n 0) (logcount (lognot n)))
        ((= n 0) 0)
        (t (+ (rem n 2) (logcount (floor n 2))))))

;;; --- byte specifiers: LDB / DPB / ... --------------------------------------

(defun byte (size position) (cons size position))
(defun byte-size (bytespec) (car bytespec))
(defun byte-position (bytespec) (cdr bytespec))

(defun ldb (bytespec integer)
  (logand (ash integer (- (byte-position bytespec)))
          (1- (expt 2 (byte-size bytespec)))))

(defun ldb-test (bytespec integer) (not (zerop (ldb bytespec integer))))

(defun mask-field (bytespec integer)
  (* (ldb bytespec integer) (expt 2 (byte-position bytespec))))

(defun dpb (newbyte bytespec integer)
  (let* ((size (byte-size bytespec)) (pos (byte-position bytespec))
         (mask (1- (expt 2 size))) (scale (expt 2 pos)))
    (+ (- integer (* (ldb bytespec integer) scale))
       (* (logand newbyte mask) scale))))

(defun deposit-field (newbyte bytespec integer)
  ;; Replace the BYTESPEC field of INTEGER with the same-position bits of
  ;; NEWBYTE (both taken in place via MASK-FIELD).
  (+ (- integer (mask-field bytespec integer))
     (mask-field bytespec newbyte)))

;;; --- misc numeric functions -------------------------------------------------

(defun signum (n)
  (cond ((zerop n) n)
        ((> n 0) (if (floatp n) 1.0 1))
        (t (if (floatp n) -1.0 -1))))

(defun isqrt (n)
  (cond ((< n 0) (error "ISQRT of a negative integer"))
        ((< n 2) n)
        (t (let ((x (ash 1 (ceiling (integer-length n) 2))))
             (block nil
               (loop
                 (let ((y (floor (+ x (floor n x)) 2)))
                   (if (< y x) (setq x y) (return x)))))))))

;;; --- functional combinators -------------------------------------------------

(defun complement (fn)
  (lambda (&rest args) (not (apply fn args))))

(defun constantly (value)
  (lambda (&rest args) (declare (ignore args)) value))

;;; --- place-mutating and control macros -------------------------------------

;; PSETF: evaluate all value forms, then assign to all places (parallel).
(defmacro psetf (&rest pairs)
  (let ((places nil) (temps nil) (vals nil) (p pairs))
    (loop while (consp (cdr p)) do
      (push (car p) places)
      (push (gensym) temps)
      (push (cadr p) vals)
      (setq p (cddr p)))
    (setq places (reverse places) temps (reverse temps) vals (reverse vals))
    `(let ,(mapcar (function list) temps vals)
       ,@(mapcar (lambda (pl tp) (list 'setf pl tp)) places temps)
       nil)))

;; ROTATEF: each place receives the (old) value of the next; last gets first.
(defmacro rotatef (&rest places)
  (if (or (null places) (null (cdr places)))
      nil
      (let ((temps (mapcar (lambda (p) (declare (ignore p)) (gensym)) places)))
        `(let ,(mapcar (function list) temps places)
           ,@(mapcar (lambda (pl tp) (list 'setf pl tp))
                     places (append (cdr temps) (list (car temps))))
           nil))))

;; SHIFTF: return the old value of the first place; shift the rest leftward and
;; store NEWVALUE (the final argument) into the last place.
(defmacro shiftf (&rest args)
  (let* ((places (butlast args))
         (newval (car (last args)))
         (temps (mapcar (lambda (p) (declare (ignore p)) (gensym)) places)))
    `(let ,(mapcar (function list) temps places)
       (setf ,@(%zip-pairs places (append (cdr temps) (list newval))))
       ,(car temps))))

;;; PROG / PROG*: LET (or LET*) plus an implicit BLOCK NIL and TAGBODY.
(defmacro prog (bindings &rest body)
  `(block nil (let ,bindings (tagbody ,@body))))
(defmacro prog* (bindings &rest body)
  `(block nil (let* ,bindings (tagbody ,@body))))

;;; CCASE / CTYPECASE: like ECASE / ETYPECASE but the key is a place and a
;;; correctable STORE-VALUE restart lets the handler supply a fresh value.
(defmacro ccase (keyplace &rest clauses)
  (let ((value (gensym)) (top (gensym)))
    `(block nil
       (tagbody
          ,top
          (return
            (let ((,value ,keyplace))
              (cond
                ,@(mapcar (lambda (clause)
                            (let ((keys (car clause)) (body (cdr clause)))
                              (if (consp keys)
                                  `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys))
                                    ,@body)
                                  `((eql ,value ',keys) ,@body))))
                          clauses)
                (t (restart-case
                       (error 'type-error :datum ,value :expected-type t)
                     (store-value (v) (setf ,keyplace v) (go ,top)))))))))))

(defmacro ctypecase (keyplace &rest clauses)
  (let ((value (gensym)) (top (gensym)))
    `(block nil
       (tagbody
          ,top
          (return
            (let ((,value ,keyplace))
              (cond
                ,@(mapcar (lambda (clause)
                            `((typep ,value ',(car clause)) ,@(cdr clause)))
                          clauses)
                (t (restart-case
                       (error 'type-error :datum ,value :expected-type t)
                     (store-value (v) (setf ,keyplace v) (go ,top)))))))))))

;;; ---------------------------------------------------------------------------
;;; Gray streams: CLOS class hierarchy and generic-function protocol (spec
;;; §5.5.2, bliss-jtc.7b).
;;;
;;; Built-in streams stay Rust-backed for speed; these classes and generics let
;;; user code define its own stream types. The standard stream functions
;;; (read-char, write-char, …) dispatch to these generics when their argument is
;;; a FUNDAMENTAL-STREAM instance, and to the fast Rust path otherwise. The Gray
;;; generics use fixed arities — the standard functions fill in optional
;;; start/end/eof arguments before dispatching.
;;; ---------------------------------------------------------------------------

;;; 5.5.2.1  Base classes.
(defclass fundamental-stream (standard-object) ())
(defclass fundamental-input-stream (fundamental-stream) ())
(defclass fundamental-output-stream (fundamental-stream) ())
(defclass fundamental-character-stream (fundamental-stream) ())
(defclass fundamental-binary-stream (fundamental-stream) ())
(defclass fundamental-character-input-stream
    (fundamental-input-stream fundamental-character-stream) ())
(defclass fundamental-character-output-stream
    (fundamental-output-stream fundamental-character-stream) ())
(defclass fundamental-binary-input-stream
    (fundamental-input-stream fundamental-binary-stream) ())
(defclass fundamental-binary-output-stream
    (fundamental-output-stream fundamental-binary-stream) ())

;;; 5.5.2.2 / 5.5.2.3 / 5.5.2.4  Generic functions.
(defgeneric stream-read-char (stream))
(defgeneric stream-unread-char (stream character))
(defgeneric stream-read-char-no-hang (stream))
(defgeneric stream-peek-char (stream))
(defgeneric stream-listen (stream))
(defgeneric stream-read-line (stream))
(defgeneric stream-clear-input (stream))
(defgeneric stream-read-byte (stream))

(defgeneric stream-write-char (stream character))
(defgeneric stream-line-column (stream))
(defgeneric stream-start-line-p (stream))
(defgeneric stream-write-string (stream string start end))
(defgeneric stream-terpri (stream))
(defgeneric stream-fresh-line (stream))
(defgeneric stream-finish-output (stream))
(defgeneric stream-force-output (stream))
(defgeneric stream-clear-output (stream))
(defgeneric stream-write-byte (stream integer))

(defgeneric gray-stream-element-type (stream))
(defgeneric gray-close (stream))

;;; Required-to-implement operations: a subclass that does not provide a method
;;; gets a clear error rather than a mysterious no-applicable-method.
(defmethod stream-read-char ((stream fundamental-input-stream))
  (error "stream-read-char must be implemented by ~a" (class-of stream)))
(defmethod stream-write-char ((stream fundamental-output-stream) character)
  (error "stream-write-char must be implemented by ~a" (class-of stream)))

;;; Input defaults (§5.5.2.2).
(defmethod stream-peek-char ((stream fundamental-character-input-stream))
  (let ((c (stream-read-char stream)))
    (unless (eq c :eof)
      (stream-unread-char stream c))
    c))

(defmethod stream-read-char-no-hang ((stream fundamental-character-input-stream))
  (stream-read-char stream))

(defmethod stream-listen ((stream fundamental-character-input-stream))
  (let ((c (stream-read-char-no-hang stream)))
    (cond ((eq c :eof) nil)
          ((null c) nil)
          (t (stream-unread-char stream c) t))))

(defmethod stream-clear-input ((stream fundamental-input-stream)) nil)

(defmethod stream-read-line ((stream fundamental-character-input-stream))
  (let ((chars nil))
    (block done
      (loop
        (let ((c (stream-read-char stream)))
          (cond ((eq c :eof)
                 (return-from done (values (coerce (nreverse chars) 'string) t)))
                ((eql c #\Newline)
                 (return-from done (values (coerce (nreverse chars) 'string) nil)))
                (t (push c chars))))))))

;;; Output defaults (§5.5.2.3).
(defmethod stream-line-column ((stream fundamental-character-output-stream)) nil)

(defmethod stream-start-line-p ((stream fundamental-character-output-stream))
  (eql (stream-line-column stream) 0))

(defmethod stream-write-string ((stream fundamental-character-output-stream) string start end)
  (let ((end (or end (length string))))
    (do ((i start (1+ i)))
        ((>= i end) string)
      (stream-write-char stream (char string i)))))

(defmethod stream-terpri ((stream fundamental-character-output-stream))
  (stream-write-char stream #\Newline)
  nil)

(defmethod stream-fresh-line ((stream fundamental-character-output-stream))
  (if (stream-start-line-p stream)
      nil
      (progn (stream-terpri stream) t)))

(defmethod stream-finish-output ((stream fundamental-output-stream)) nil)
(defmethod stream-force-output ((stream fundamental-output-stream)) nil)
(defmethod stream-clear-output ((stream fundamental-output-stream)) nil)

;;; Query / lifecycle defaults (§5.5.2.4).
(defmethod gray-stream-element-type ((stream fundamental-character-stream)) 'character)
(defmethod gray-stream-element-type ((stream fundamental-binary-stream)) '(unsigned-byte 8))
(defmethod gray-close ((stream fundamental-stream)) t)
