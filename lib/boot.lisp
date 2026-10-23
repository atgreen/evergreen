;;; SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
;;; SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

;;;; boot.lisp — EGCL bootstrap prelude.
;;;;
;;;; This file is loaded by the CLI when invoked with --bootstrap. It is the
;;;; first slice of the standard library written in Lisp rather than Rust: the
;;;; goal is to push everything that can be expressed as a macro or ordinary
;;;; function out of the `eval_form` interpreter and into this file.
;;;;
;;;; Constraints of the current bootstrap evaluator (see crates/egcl):
;;;;   * macro lambda lists are flat — &optional and &rest work, but nested
;;;;     destructuring does NOT yet. Keep parameter lists simple.
;;;;   * user macros are expanded before builtins. Standard control macros
;;;;     are installed last, after definitions that bootstrap through builtins.
;;;;   * `setq` on an unbound symbol creates a persistent global binding, which
;;;;     is what the defining macros below rely on.

;;; ---------------------------------------------------------------------------
;;; Global variable definitions
;;; ---------------------------------------------------------------------------

;; DEFVAR/DEFPARAMETER proclaim NAME globally SPECIAL (ANSI 3.8), so a later LET
;; on it binds dynamically on BOTH backends. Without this the tree-walker treated
;; a globally-bound var as dynamic while the compiler bound it lexically — the
;; same function then returned different values cold vs hot, a tier inconsistency
;; (bliss-av5). The %proclaim-special registry (bliss-7na) is consulted by
;; is_special_var (tree-walker) and is_special_name (compiler).
;;; Documentation strings (CLHS 4.4). One EQUAL-keyed table over (name . doc-type)
;;; so every doc-type shares a single store. Defined BEFORE DEFVAR because
;;; DEFVAR's expansion records its docstring through it — and set up with plain
;;; SETQ rather than DEFVAR for the same reason (bliss-61u1).
(egcl-internal::%proclaim-special (list 'egcl-internal::*documentation*))
(setq egcl-internal::*documentation* (make-hash-table :test 'equal))

(defun documentation (object &optional doc-type)
  ;; A function object documented under T is documented as a FUNCTION
  ;; (ansi defgeneric.2). GETHASH returns two values; DOCUMENTATION returns one.
  (when (and (eq doc-type t) (functionp object))
    (setq doc-type 'function))
  (values (gethash (cons object doc-type) egcl-internal::*documentation*)))

(defun (setf documentation) (new object &optional doc-type)
  (when (and (eq doc-type t) (functionp object))
    (setq doc-type 'function))
  (setf (gethash (cons object doc-type) egcl-internal::*documentation*) new)
  new)

;;; Record DOC for NAME under DOC-TYPE, ignoring a NIL docstring so the definers
;;; can pass their optional one unconditionally.
(defun egcl-internal::%set-documentation (name doc-type doc)
  (when doc
    (setf (documentation name doc-type) doc))
  name)

(defmacro defvar (name &rest value)
  ;; (defvar name) with no initial value only proclaims NAME special; it must
  ;; NOT assign a value (NAME stays unbound if it was unbound). Only
  ;; (defvar name value) initialises it, and only when currently unbound (CLHS).
  `(progn
     (egcl-internal::%proclaim-special (list ',name))
     ,@(when value
         `((unless (boundp ',name)
             (setq ,name ,(car value)))))
     ,@(when (cdr value)
         `((egcl-internal::%set-documentation ',name 'variable ,(cadr value))))
     ',name))

(defmacro defparameter (name &rest value)
  `(progn
     (egcl-internal::%proclaim-special (list ',name))
     (setq ,name ,(if value (car value) nil))
     ,@(when (cdr value)
         `((egcl-internal::%set-documentation ',name 'variable ,(cadr value))))
     ',name))

;; defconstant: this interpreter has no separate constant cell; model it as a
;; global binding, like defparameter.
(defmacro defconstant (name value &rest doc)
  ;; No separate constant cell: model as a global binding, but record the name
  ;; so CONSTANTP recognises it (alexandria's DEFINE-CONSTANT, used by babel,
  ;; asks CONSTANTP whether a re-defined constant is already constant).
  ;; NOT `(setq ,name ,value)`: re-evaluating a DEFCONSTANT form is normal
  ;; (COMPILE-FILE evaluates it, then the fasl load evaluates it again), and by
  ;; then the name is marked, so a SETQ is an illegal assignment to a constant.
  ;; %defconstant assigns and marks as one operation (bliss-sci0).
  `(progn (%defconstant ',name ,value)
          ,@(when doc
              `((egcl-internal::%set-documentation ',name 'variable ,(car doc))))
          ',name))

;; Fixnums are 61-bit signed (EgclVal tags the low 3 bits): the value is
;; stored as n<<3, so the representable range is [-2^60, 2^60-1].
(defconstant most-positive-fixnum 1152921504606846975)
(defconstant most-negative-fixnum -1152921504606846976)
;; Standard array/character limit constants. The interpreter pre-seeds their
;; values into the symbol value cell (seed_standard_constant), so both
;; interpreted and compiled reads see them (bliss-1i3q). DEFCONSTANT here marks
;; them CONSTANTP and exports them as genuine external CL symbols (bliss-9m5c);
;; the literal values MUST match the cli.rs seeds. CHAR-CODE-LIMIT = #x110000
;; (Unicode scalar upper bound); flexi-streams' .asd guards with
;; (<= char-code-limit 65533) and errored → ASDF re-loaded the .asd in a tight
;; loop to OOM without this.
(defconstant char-code-limit 1114112)
(defconstant array-rank-limit 8)
(defconstant array-dimension-limit 1152921504606846975)
(defconstant array-total-size-limit 1152921504606846975)
;; PI (a float approximation of π; egcl floats are single-precision, so the
;; long-float literal rounds to 3.1415927) and the single-float magnitude
;; extremes, plus the least-positive / normalized / epsilon family for both
;; float formats (bliss-pzz4; values match SBCL / IEEE-754 binary32 & binary64).
(defconstant pi 3.141592653589793d0)
(defconstant most-positive-single-float 3.4028235e38)
(defconstant most-negative-single-float -3.4028235e38)
(defconstant least-positive-single-float 1.4012985e-45)
(defconstant least-positive-normalized-single-float 1.1754944e-38)
(defconstant least-negative-single-float -1.4012985e-45)
(defconstant least-negative-normalized-single-float -1.1754944e-38)
(defconstant single-float-epsilon 5.960465e-8)
(defconstant single-float-negative-epsilon 2.9802326e-8)
(defconstant most-positive-double-float 1.7976931348623157d308)
(defconstant most-negative-double-float -1.7976931348623157d308)
(defconstant least-positive-double-float 4.9406564584124654d-324)
(defconstant least-positive-normalized-double-float 2.2250738585072014d-308)
(defconstant least-negative-double-float -4.9406564584124654d-324)
(defconstant least-negative-normalized-double-float -2.2250738585072014d-308)
(defconstant double-float-epsilon 1.1102230246251568d-16)
(defconstant double-float-negative-epsilon 5.551115123125784d-17)
;; egcl has two float formats: SHORT-FLOAT ≡ SINGLE-FLOAT and LONG-FLOAT ≡
;; DOUBLE-FLOAT. The corresponding limit constants are aliases of the single/
;; double values so that code referencing the short/long family (e.g. ansi-test
;; make-hash-table.26/.29) resolves them (egcl hash-tables chapter).
(defconstant most-positive-short-float most-positive-single-float)
(defconstant most-negative-short-float most-negative-single-float)
(defconstant least-positive-short-float least-positive-single-float)
(defconstant least-positive-normalized-short-float least-positive-normalized-single-float)
(defconstant least-negative-short-float least-negative-single-float)
(defconstant least-negative-normalized-short-float least-negative-normalized-single-float)
(defconstant short-float-epsilon single-float-epsilon)
(defconstant short-float-negative-epsilon single-float-negative-epsilon)
(defconstant most-positive-long-float most-positive-double-float)
(defconstant most-negative-long-float most-negative-double-float)
(defconstant least-positive-long-float least-positive-double-float)
(defconstant least-positive-normalized-long-float least-positive-normalized-double-float)
(defconstant least-negative-long-float least-negative-double-float)
(defconstant least-negative-normalized-long-float least-negative-normalized-double-float)
(defconstant long-float-epsilon double-float-epsilon)
(defconstant long-float-negative-epsilon double-float-negative-epsilon)
(defconstant lambda-list-keywords
  '(&optional &rest &key &allow-other-keys &aux &body &whole &environment))
;; Implementation limits: egcl caps these at MOST-POSITIVE-FIXNUM so they are
;; themselves fixnums (a larger literal like 2^62 would be a bignum and is not a
;; meaningful arg-count ceiling here). Must match the seed_standard_constant
;; values in cli.rs (bliss-1i3q).
(defconstant call-arguments-limit 1152921504606846975)
(defconstant lambda-parameters-limit 1152921504606846975)
;; Seeded in the value cell by cli.rs (seed_standard_constant) like the two
;; above, but never marked CONSTANTP -- so they were BOUNDP yet not CONSTANTP,
;; which is exactly what ansi CL-CONSTANT-SYMBOLS.1 collects. The literals MUST
;; match the cli.rs seeds: MULTIPLE-VALUES-LIMIT is (1<<60)-1 and
;; INTERNAL-TIME-UNITS-PER-SECOND is 1000.
(defconstant multiple-values-limit 1152921504606846975)
(defconstant internal-time-units-per-second 1000)

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

;; PUSH: evaluate ITEM first, then the PLACE subforms once each (left to right),
;; read/store the place exactly once (CLHS 5.1.2 / push.order.*). Uses the
;; place's setf-expansion so the subforms are lifted into temporaries and the
;; returned writer handles the store, including custom places whose reader is
;; not itself assignable. A naive (setf place (cons item place))
;; double-evaluates the place subforms and gets the order wrong.
(defmacro push (item place &environment env)
  ;; Expand lexical macro/symbol-macro places in ENV, stopping at an explicit
  ;; SETF writer even when its accessor is also a reader macro. Ordinary
  ;; MACROEXPAND would discard that writer (bliss-frd2v).
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((g (gensym)) (result (gensym)))
        `(let* ((,g ,item)
                ,@(mapcar (function list) dummies vals)
                (,result (cons ,g ,getter)))
           (multiple-value-bind ,newval ,result
             ,setter)
           ,result)))))

;; POP: read the PLACE's list once (its subforms evaluated once, left to right),
;; return its CAR, and store its CDR back into the place (CLHS 5.1.2 /
;; pop.order.*). The naive (prog1 (car place) (setf place (cdr place)))
;; double-evaluates the place subforms — e.g. (pop (aref a (progn (incf i) 0)))
;; must increment I exactly once. Use the setf-expansion so the subforms are
;; lifted into temporaries and the getter is read/stored once.
(defmacro pop (place &environment env)
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((g (gensym)))
        `(let* (,@(mapcar (function list) dummies vals)
                (,g ,getter))
           (prog1 (car ,g)
             (multiple-value-bind ,newval (cdr ,g)
               ,setter)))))))

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

;; INCF/DECF: read and write the PLACE with its subforms evaluated exactly ONCE,
;; left to right (CLHS 5.1.3). The naive `(setf place (+ place delta))` mentions
;; PLACE twice, so a place with a side-effecting subform ran it twice — e.g.
;; (incf (car (progn (incf n) v))) incremented N twice where SBCL increments it
;; once (bliss-pbp8). Use the setf-expansion, like PUSH/POP above, so the
;; subforms are lifted into temporaries and the getter is read once.
;; DELTA is bound after the place temporaries to preserve left-to-right order.
(defmacro incf (place &rest delta &environment env)
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((d (gensym)))
        `(let* (,@(mapcar (function list) dummies vals)
                (,d ,(if delta (car delta) 1)))
           (multiple-value-bind ,newval (+ ,getter ,d)
             ,setter))))))

(defmacro decf (place &rest delta &environment env)
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((d (gensym)))
        `(let* (,@(mapcar (function list) dummies vals)
                (,d ,(if delta (car delta) 1)))
           (multiple-value-bind ,newval (- ,getter ,d)
             ,setter))))))

;; with-hash-table-iterator: (with-hash-table-iterator (name table) . body)
;; Within BODY, calling (name) returns (values more-p key value), advancing over
;; a snapshot of TABLE's entries, and (values nil) once exhausted (bliss-jtc.8).
(defmacro with-hash-table-iterator (spec &rest body)
  ;; NAME is established as a local MACRO (macrolet), per CLHS — so within BODY
  ;; `(macroexpand '(name))` expands it (ansi with-hash-table-iterator.9). Each
  ;; `(name)` expands to code that pops the next entry off a snapshot list held
  ;; in the lexical variable REST, returning (values more-p key value), and
  ;; (values nil) once exhausted.
  (let ((name (car spec)) (table (car (cdr spec)))
        (rest (gensym)) (pair (gensym)))
    `(let ((,rest (hash-table-entries ,table)))
       (macrolet ((,name ()
                    (list 'if ',rest
                          (list 'let (list (list ',pair (list 'car ',rest)))
                                (list 'setq ',rest (list 'cdr ',rest))
                                (list 'values t
                                      (list 'car ',pair)
                                      (list 'cdr ',pair)))
                          (list 'values nil))))
         ,@body))))

;; define-modify-macro: define NAME so that (NAME place args...) expands to
;; (setf place (FUNCTION place args...)). Supports required and &rest args in
;; LAMBDA-LIST, which covers the standard uses (appendf, etc.).
(defmacro define-modify-macro (name lambda-list function &rest doc)
  (declare (ignore doc))
  ;; The generated macro must expand through GET-SETF-EXPANSION, not simply put
  ;; the place in twice as `(setf PLACE (fn PLACE args...))`. That older form
  ;; evaluated the place's SUBFORMS twice -- once for the read and once for the
  ;; write -- so any subform with a side effect ran twice:
  ;;   (new-incf (aref a (incf i)))  incremented I twice (ansi
  ;;   DEFINE-MODIFY-MACRO.3/4; bliss-v8f3). The stored value was right, so it
  ;;   was a silent wrong answer rather than an error.
  ;; CLHS 5.1.1.1: the subforms are evaluated once, left to right, before the
  ;; argument forms. Binding the expansion's temporaries first gives exactly
  ;; that order.
  (let ((vars '()) (rest-var nil) (mode :req)
        (place (gensym "PLACE")) (env (gensym "ENV"))
        ;; Gensyms, not plain symbols: these are bound inside the GENERATED
        ;; macro's body, where a user lambda-list variable of the same name
        ;; would otherwise shadow them.
        (temps (gensym "TEMPS")) (vals (gensym "VALS")) (stores (gensym "STORES"))
        (store-form (gensym "STORE")) (access (gensym "ACCESS")))
    (dolist (item lambda-list)
      (cond ((eq item '&rest) (setq mode :rest))
            ((eq item '&optional) (setq mode :opt))
            ((eq mode :rest) (setq rest-var item))
            (t (push (if (consp item) (car item) item) vars))))
    (setq vars (reverse vars))
    `(defmacro ,name (,place ,@lambda-list &environment ,env)
       (multiple-value-bind (,temps ,vals ,stores ,store-form ,access)
           (get-setf-expansion ,place ,env)
         (list 'let*
               (append (mapcar #'list ,temps ,vals)
                       (list (list (car ,stores)
                                   (cons ',function
                                         (cons ,access
                                               (append (list ,@vars)
                                                       ,(or rest-var 'nil)))))))
               ,store-form)))))


;;; ---------------------------------------------------------------------------
;;; Declarations, type aliases, and condition definitions used by the shipped
;;; bootstrap evaluator.
;;; ---------------------------------------------------------------------------

;; DECLAIM records global declarations. Inline policy and function types take
;; effect while compiling subsequent definitions as well as when loading or
;; evaluating the form; ordinary PROCLAIM calls alone have no compile-time effect.
(defmacro declaim (&rest specs)
  (let ((forms nil))
    (dolist (spec specs)
      (when (consp spec)
        (case (car spec)
          ((inline notinline ftype)
           (push (list 'eval-when '(:compile-toplevel :load-toplevel :execute)
                       (list 'proclaim (list 'quote spec)))
                 forms))
          (special
           (push (list 'egcl-internal::%proclaim-special (list 'quote (cdr spec)))
                 forms))
          ;; (declaration name …) names declarations the implementation must
          ;; accept; the environment then records them for
          ;; EGCL-CLTL2:DECLARATION-INFORMATION.
          (declaration
           (push (list 'egcl-internal::%proclaim-declaration (list 'quote (cdr spec)))
                 forms))
          ;; The qualities are advisory to this compiler, but the global policy
          ;; is reported by EGCL-CLTL2:DECLARATION-INFORMATION.
          (optimize
           (push (list 'egcl-internal::%proclaim-optimize (list 'quote (cdr spec)))
                 forms)))))
    (if forms (cons 'progn (nreverse forms)) nil)))

;; proclaim: the run-time counterpart (ANSI 3.8). Non-special declarations are
;; stored where needed; (special x …) registers the variables as special.
;; The parameter is deliberately NOT named DECLARATION-SPECIFIER: boot.lisp is
;; read into COMMON-LISP, so every name it mentions mints a bare, home-package-
;; less symbol identity, and a qualified read of the same name (e.g.
;; EGCL-EXT:DECLARATION-SPECIFIER) then resolves to that bare identity instead
;; of the extension package's symbol.
(defun proclaim (decl-spec)
  ;; A declaration specifier is a proper list headed by a symbol (CLHS 3.3.2;
  ;; ansi proclaim.error.3-11 feed dotted lists).
  (unless (and (consp decl-spec)
               (symbolp (car decl-spec))
               (loop for tail = decl-spec then (cdr tail)
                     while (consp tail)
                     finally (return (null tail))))
    (error 'type-error :datum decl-spec :expected-type '(cons symbol list)))
  (when (consp decl-spec)
    (case (car decl-spec)
      ((inline notinline)
        (dolist (name (cdr decl-spec))
          ;; The setter and getter are distinct function names. Keep their
          ;; policies separate rather than applying GET to a SETF name list.
          (if (and (consp name) (eq (car name) 'setf)
                   (consp (cdr name)) (symbolp (cadr name)) (null (cddr name)))
              (setf (get (cadr name) 'egcl-internal::setf-inline-policy)
                    (car decl-spec))
              (setf (get name 'egcl-internal::inline-policy) (car decl-spec)))))
      (ftype
        ;; Retain the raw type, including VALUES cardinality syntax. This is
        ;; declaration metadata, not yet a proof about a compiled call result.
        ;; SETF functions and ordinary functions have distinct names even when
        ;; their accessor symbol is the same; symbol properties retain package
        ;; identity and are traced with the symbol.
        (dolist (name (cddr decl-spec))
          (cond
            ((symbolp name)
             (setf (get name 'egcl-internal::function-type) (cadr decl-spec)))
            ((and (consp name) (eq (car name) 'setf)
                  (consp (cdr name)) (symbolp (cadr name)) (null (cddr name)))
             (setf (get (cadr name) 'egcl-internal::setf-function-type)
                   (cadr decl-spec))))))
      (special (egcl-internal::%proclaim-special (cdr decl-spec)))
      (declaration (egcl-internal::%proclaim-declaration (cdr decl-spec)))
      (optimize (egcl-internal::%proclaim-optimize (cdr decl-spec)))))
  nil)

;; Track bootstrap type aliases so TYPEP/CHECK-TYPE can consult them.
(defvar *type-definitions* nil)

(defvar *type-expanders* nil)

(defun egcl-internal::%deftype-defaults (lambda-list)
  ;; Preserve explicit initforms, including NIL. Only an omitted optional/key
  ;; initform differs from an ordinary lambda list (CLHS 3.4.8).
  (let ((mode nil))
    (mapcar
      (lambda (parameter)
        (cond
          ((member parameter '(&optional &key &rest &body &aux &whole &environment))
           (setq mode parameter)
           parameter)
          ((eq parameter '&allow-other-keys) parameter)
          ((or (eq mode '&optional) (eq mode '&key))
           (cond ((symbolp parameter) (list parameter ''*))
                 ((null (cdr parameter)) (list (car parameter) ''*))
                 (t parameter)))
          (t parameter)))
      lambda-list)))

(defmacro deftype (name lambda-list &rest body)
  ;; Keep declarations at the lambda's body head and put only executable forms
  ;; inside the named block. Parameter initforms remain outside that block.
  (let ((declarations nil) (documentation nil))
    (tagbody
      scan
      (cond
        ((and (consp body) (consp (car body)) (eq (caar body) 'declare))
         (push (car body) declarations)
         (setq body (cdr body))
         (go scan))
        ((and (consp body) (stringp (car body)) (cdr body) (null documentation))
         (setq documentation (car body) body (cdr body))
         (go scan))))
    ;; Even a zero-argument alias is a use-time expansion. The closure captures
    ;; the definition's lexical bindings without executing its body now.
    `(progn
       (setq *type-expanders*
             (cons (list ',name
                         (function (lambda ,(egcl-internal::%deftype-defaults lambda-list)
                                     ,@(nreverse declarations)
                                     (block ,name ,@body))))
                   *type-expanders*))
       ,@(when documentation
           `((egcl-internal::%set-documentation ',name 'type ,documentation)))
       (egcl-internal::%home-symbol ',name)
       ',name)))

;; Track condition definitions so MAKE-CONDITION/SIGNAL can create and match
;; real condition instances through the evaluator.
(defvar *condition-types* nil)
(defvar *condition-definitions* nil)

;; NOTE: slot :reader/:accessor options are installed as real generic METHODS
;; by the DEFCLASS expansion below (install_slot_accessor_method), exactly like
;; any defclass. They were previously ALSO emitted here as plain DEFUNs, which
;; clobbered the whole generic function: asdf's (define-condition bad-system-name
;; … (source-file :reader system-source-file)) overwrote ASDF:SYSTEM-SOURCE-FILE's
;; designator methods, so (system-source-file :quri) returned NIL and
;; system-relative-pathname yielded relative paths (bliss-d0b, quri).
(defmacro define-condition (name parents slots &rest options)
  (let ((effective-parents (if parents parents '(condition))))
    `(progn
       (setq *condition-types*
             (cons (list ',name ',effective-parents)
                   *condition-types*))
       (setq *condition-definitions*
             (cons (list ',name ',effective-parents ',slots ',options)
                   *condition-definitions*))
       (defclass ,name ,effective-parents ,slots)
       ',name)))

;; Out-of-line failure handler for CHECK-TYPE (bliss-gq5). Establishing the
;; STORE-VALUE restart-case (and its enclosing loop/block/tagbody) INLINE in
;; every CHECK-TYPE cost a per-call restart-case setup even on the passing path,
;; and — because PushRestartCase/PushBlock/PushTag opcodes make the T1 native
;; compiler decline (native_would_lose_captured_control) — kept EVERY function
;; that uses CHECK-TYPE pinned at T0. UIOP's ensure-inherited/ensure-symbol call
;; CHECK-TYPE 6-8 times per package symbol, so this dominated ASDF/package load.
;; Keeping the restart-case out of line makes CHECK-TYPE's fast path a bare TYPEP
;; and lets its callers promote to native.
(defun %check-type-fail (value typespec)
  ;; Signal a correctable TYPE-ERROR with a STORE-VALUE restart; loop until the
  ;; supplied value conforms; return the conforming value (the CHECK-TYPE
  ;; expansion stores it back into PLACE).
  (loop
    (restart-case
        (error 'type-error :datum value :expected-type typespec)
      (store-value (v) (setf value v)))
    (when (typep value typespec) (return value))))

(defmacro check-type (place typespec &rest ignore)
  (declare (ignore ignore))
  ;; ANSI (CLHS 9.1): signal a correctable TYPE-ERROR with a STORE-VALUE restart
  ;; that supplies a new value for PLACE; re-test and re-signal until PLACE
  ;; conforms. Always returns NIL. The fast (passing) path is a bare TYPEP; the
  ;; restart-case machinery lives in %check-type-fail, off the hot path.
  `(progn
     (unless (typep ,place ',typespec)
       (setf ,place (%check-type-fail ,place ',typespec)))
     nil))

(defun egcl::%checked-structure-slot-value (value object slot)
  ;; An inherited writer must honor the actual structure's narrowed type, so
  ;; the type comes from OBJECT's own class, looked up natively.
  (let ((typespec (egcl::%struct-slot-typespec object slot)))
    (if (or (eq typespec t) (typep value typespec))
        value
        (%check-type-fail value typespec))))

(defmacro assert (test-form &rest more)
  ;; (assert test [(place*) [datum arg*]]) — CLHS 9.2. Signal a correctable
  ;; error with a CONTINUE restart; when CONTINUE is invoked, re-evaluate
  ;; TEST-FORM and, if still false, re-signal. Always returns NIL. A DATUM (a
  ;; condition type, a condition, or a format control) selects the condition
  ;; type; otherwise a SIMPLE-ERROR is signalled. The optional PLACES list and
  ;; the interactive restart values are still not implemented.
  (let ((datum (second more))
        (args (cddr more)))
    `(loop until ,test-form
           do (restart-case
                  ,(if datum
                       `(error ,datum ,@args)
                       `(error 'simple-error
                               :format-control "Assertion failed: ~S"
                               :format-arguments (list ',test-form)))
                (continue () :report "Retry assertion." nil)))))

;;; ---------------------------------------------------------------------------
;;; CLOS convenience macros and standard condition accessors.
;;;
;;; WITH-SLOTS / WITH-ACCESSORS expand into SYMBOL-MACROLET so the bound names
;;; are places: reading goes through SLOT-VALUE / the accessor, and SETF on them
;;; works too. The standard condition readers are ordinary functions over the
;;; condition instance's slots — conditions are CLOS objects, so SLOT-VALUE is
;;; all that is needed.
;;; ---------------------------------------------------------------------------

;; DEFINE-METHOD-COMBINATION (CLHS 7.7) — STUB. Accepts the short and long forms
;; and returns NAME, but does not yet register a usable custom combination:
;; invoking a generic function declared with a user-defined :method-combination
;; is unsupported (only the built-in combinations — standard/+/and/or/list/
;; append/nconc/min/max/progn — dispatch). This lets support code that merely
;; DEFINES a combination load (ansi-test random-aux.lsp defines `randomized` but
;; never uses it in the CONS/SYMBOLS/… chapters). Full custom-combination
;; dispatch is tracked separately (bliss-cpm9).
(defmacro define-method-combination (name &rest args)
  (declare (ignore args))
  `(quote ,name))

;; Keep initialization protocol dispatch in Lisp so library methods participate
;; in ordinary :around/:before/:after combination. NIL selects no initforms
;; during reinitialization; explicit initargs still update their slots.
(defmethod shared-initialize ((instance standard-object) slot-names &rest initargs)
  (egcl-internal::%standard-shared-initialize instance slot-names initargs))

(defmethod slot-unbound (class instance slot-name)
  (declare (ignore class))
  (error 'unbound-slot :name slot-name :instance instance))

;; CLHS SLOT-MISSING: the default method signals an error; SLOT-VALUE, (SETF
;; SLOT-VALUE), SLOT-BOUNDP and SLOT-MAKUNBOUND call it for a slot the class
;; does not have.
(defmethod slot-missing (class instance slot-name operation &optional new-value)
  (declare (ignore class new-value))
  (error "~S: the slot ~S is missing from ~S" operation slot-name instance))

;; CLHS MAKE-LOAD-FORM: the default methods for standard objects, structures
;; and conditions signal an error; a program defines its own methods, usually
;; through MAKE-LOAD-FORM-SAVING-SLOTS.
(defgeneric make-load-form (object &optional environment))
(defmethod make-load-form ((object standard-object) &optional environment)
  (declare (ignore environment))
  (error "MAKE-LOAD-FORM: no method has been defined for ~S" object))
(defmethod make-load-form ((object structure-object) &optional environment)
  (declare (ignore environment))
  (error "MAKE-LOAD-FORM: no method has been defined for ~S" object))
(defmethod make-load-form ((object condition) &optional environment)
  (declare (ignore environment))
  (error "MAKE-LOAD-FORM: no method has been defined for ~S" object))

;; CLHS FUNCTION-KEYWORDS: the keyword parameter names a method's lambda list
;; declares, as keywords, and whether it has &ALLOW-OTHER-KEYS.
(defgeneric function-keywords (method))
(defmethod function-keywords ((method standard-method))
  (let ((keys '()) (allow-other-keys nil) (in-keys nil))
    (dolist (item (method-lambda-list method))
      (cond ((eq item '&key) (setq in-keys t))
            ((eq item '&allow-other-keys) (setq allow-other-keys t))
            ((member item lambda-list-keywords) (setq in-keys nil))
            (in-keys
             (push (cond ((symbolp item) (intern (symbol-name item) :keyword))
                         ((consp (car item)) (caar item))
                         (t (intern (symbol-name (car item)) :keyword)))
                   keys))))
    (values (nreverse keys) allow-other-keys)))

;; CHANGE-CLASS is a generic function so :before/:after/:around methods take
;; part; the standard method is the built-in class change.
(defmethod change-class ((instance standard-object) new-class &rest initargs)
  (apply #'egcl-internal::%standard-change-class instance new-class initargs))

;; MAKE-INSTANCES-OBSOLETE returns the class; instances keep their slots and
;; are brought up to date lazily by a class redefinition.
(defgeneric make-instances-obsolete (class))
(defmethod make-instances-obsolete ((class t))
  (if (symbolp class) (find-class class) class))

(defgeneric update-instance-for-redefined-class
    (instance added-slots discarded-slots property-list &rest initargs))
(defmethod update-instance-for-redefined-class
    ((instance standard-object) added-slots discarded-slots property-list &rest initargs)
  (declare (ignore discarded-slots property-list))
  (apply #'shared-initialize instance added-slots initargs))

;; ED has no editor to invoke; it accepts its argument and does nothing.
(defun ed (&optional x)
  (declare (ignore x))
  nil)

(defmethod reinitialize-instance ((instance standard-object) &rest initargs)
  (egcl-internal::%check-initargs instance initargs)
  (apply #'shared-initialize instance nil initargs))

;; CLHS INITIALIZE-INSTANCE: the default method calls SHARED-INITIALIZE with
;; slot-names T.  MAKE-INSTANCE's built-in slot initialization stands in for it
;; and runs the user's :after methods itself, so this method is reached by a
;; direct call and by a user :around method's CALL-NEXT-METHOD.
(defmethod initialize-instance ((instance standard-object) &rest initargs)
  (apply #'shared-initialize instance t initargs))

;; CLHS NO-APPLICABLE-METHOD and NO-NEXT-METHOD: the default methods signal an
;; error.  The dispatcher signals its own conditions in those situations; these
;; definitions make the two standard generic functions reachable as function
;; objects (ansi documentation.function.t.12) and callable by user code.
(defmethod no-applicable-method ((generic-function t) &rest function-arguments)
  (error "no applicable method for generic function ~S with arguments ~S"
         generic-function function-arguments))

(defmethod no-next-method ((generic-function t) (method t) &rest args)
  (declare (ignore args))
  (error "no next method for ~S in generic function ~S" method generic-function))

(defmethod update-instance-for-different-class
    ((previous standard-object) (current standard-object) &rest initargs)
  (apply #'shared-initialize current
         (egcl-internal::%class-change-added-slots previous current)
         initargs))

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
;; ARITHMETIC-ERROR's OPERATION and OPERANDS slots already exist (see
;; cli.rs's condition table); only these two readers were missing, so
;; (arithmetic-error-operation c) was an UNDEFINED-FUNCTION even for a
;; condition built with :operation and :operands supplied (ansi
;; ARITHMETIC-ERROR.3 constructs one and reads both back).
;; The slots carry no initform, so reading one the signaller never supplied
;; would raise "slot OPERATION is unbound" rather than answering. egcl's
;; internal arithmetic signallers do not record the operation or operands yet
;; (a separate gap, filed), so guard the read: an arithmetic error with nothing
;; recorded reports NIL rather than erroring inside a handler.
(defun arithmetic-error-operation (c)
  (if (slot-boundp c 'operation) (slot-value c 'operation) nil))
(defun arithmetic-error-operands (c)
  (if (slot-boundp c 'operands) (slot-value c 'operands) nil))
(defun cell-error-name (c) (slot-value c 'name))
(defun unbound-slot-instance (c) (slot-value c 'instance))
(defun package-error-package (c) (slot-value c 'package))
(defun stream-error-stream (c) (slot-value c 'stream))

;; FORMATTER (CLHS 22.3.9.3) — return a function equivalent to the control
;; string, callable as (fn stream &rest args). FORMAT accepts such a function as
;; its control string. The function returns the arguments the control string
;; did not consume; the directive interpreter records that count for the last
;; completed FORMAT call.
(defmacro formatter (control-string)
  (check-type control-string string)
  `(lambda (%formatter-stream &rest %formatter-args)
     (apply #'format %formatter-stream ,control-string %formatter-args)
     (nthcdr (egcl::%format-consumed) %formatter-args)))

;; INVOKE-DEBUGGER (CLHS 9.1). *DEBUGGER-HOOK*, when bound to a function, is
;; called with the condition and the hook function itself, with *DEBUGGER-HOOK*
;; rebound to NIL for the duration. If the hook returns normally (or there is
;; none) we have no interactive debugger in batch mode, so the condition is
;; re-signalled. The single required parameter makes (invoke-debugger) and
;; (invoke-debugger c nil) PROGRAM-ERRORs (invoke-debugger.error.1/2), and
;; funcalling a wrong-arity hook is a PROGRAM-ERROR too (error.3-5).
(defvar *debugger-hook* nil)
(defun invoke-debugger (condition)
  (let ((hook *debugger-hook*))
    (when hook
      (let ((*debugger-hook* nil))
        (funcall hook condition hook))))
  (error condition))

;; String-producing printers, built on FORMAT now that ~A/~S print lists.
(defun princ-to-string (x) (format nil "~a" x))
(defun prin1-to-string (x) (format nil "~s" x))
(defun write-to-string (x &rest keys)
  ;; WRITE to a string stream: WRITE binds every printer-control keyword
  ;; (:base :circle :escape :pretty :length ...) around the printing, so this
  ;; honours them all (CLHS 22.4; ansi printer print.cons.5-7).
  (with-output-to-string (s)
    (apply #'write x :stream s keys)))

;; Minimal PPRINT: a fresh newline, then the escaped printed representation of
;; OBJECT, with no trailing space, returning no values (CLHS). Full XP pretty-
;; printing (spec R5.166) is not yet implemented, so this is the degenerate
;; *print-pretty* NIL case — equivalent to (terpri) followed by WRITE.
(defun pprint (object &optional stream)
  (terpri stream)
  (write object :stream stream :escape t)
  (values))

;;; ---------------------------------------------------------------------------
;;; Control-flow macros still needed during the Stage 2 bootstrap.
;;; ---------------------------------------------------------------------------

(defmacro case (keyform &rest clauses)
  ;; CLHS 5.3 CASE: the keys of a normal clause are a *designator for a list of
  ;; keys*, so an atom key NIL means the empty key list (matches nothing), not
  ;; the object NIL (ansi CASE.6). T / OTHERWISE introduce the default clause. A
  ;; clause with no forms yields NIL, not the test value (ansi CASE.32/.33/.34).
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((keys (car clause))
            (body (cdr clause)))
        (push
          (cond
            ((or (eq keys 'otherwise) (eq keys t))
             `(t ,@(or body '(nil))))
            ((null keys)
             nil) ; empty key list — matches nothing
            ((consp keys)
             `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys))
               ,@(or body '(nil))))
            (t
             `((eql ,value ',keys) ,@(or body '(nil)))))
          expanded)))
    `(let ((,value ,keyform))
       (cond ,@(remove nil expanded)))))

(defmacro typecase (keyform &rest clauses)
  ;; A clause with no forms yields NIL, not the test value (ansi TYPECASE.12-14).
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((type (car clause))
            (body (cdr clause)))
        (push
          (if (or (eq type 'otherwise) (eq type t))
              `(t ,@(or body '(nil)))
              `((typep ,value ',type) ,@(or body '(nil))))
          expanded)))
    `(let ((,value ,keyform))
       (cond ,@expanded))))

(defmacro etypecase (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil))
    (dolist (clause (reverse clauses))
      (let ((type (car clause))
            (body (cdr clause)))
        (push `((typep ,value ',type) ,@(or body '(nil))) expanded)))
    ;; ETYPECASE signals a TYPE-ERROR whose expected type is the disjunction of
    ;; the clause types (ansi ETYPECASE.ERROR.*).
    `(let ((,value ,keyform))
       (cond ,@expanded
             (t (error 'type-error :datum ,value
                       :expected-type '(or ,@(mapcar #'car clauses))))))))

(defmacro ecase (keyform &rest clauses)
  (let ((value (gensym))
        (expanded nil)
        (all-keys nil))
    (dolist (clause (reverse clauses))
      (let ((keys (car clause))
            (body (cdr clause)))
        (cond
          ;; A NIL keys designator is the EMPTY key list (CLHS): the clause can
          ;; never match, and contributes no keys to the expected type. Treating
          ;; it as the single key NIL made `(ecase nil (nil …))` match instead of
          ;; signalling (ansi ECASE.9; bliss-gm4h). `(nil)` still matches NIL.
          ((null keys))
          ((consp keys)
           (dolist (k keys) (push k all-keys))
           (push `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys)) ,@(or body '(nil)))
                 expanded))
          (t
           (push keys all-keys)
           (push `((eql ,value ',keys) ,@(or body '(nil))) expanded)))))
    ;; ECASE signals a (non-correctable) TYPE-ERROR whose datum is the value and
    ;; whose expected type is the set of keys (ansi ECASE.ERROR.*/ECASE.4/.5).
    `(let ((,value ,keyform))
       (cond ,@expanded
             (t (error 'type-error :datum ,value
                       :expected-type '(member ,@all-keys)))))))

(defmacro ignore-errors (&rest body)
  `(handler-case (progn ,@body)
     (error (c) (values nil c))))

;; WITH-SIMPLE-RESTART (CLHS 9.1) — run BODY with a single named restart that,
;; when invoked, aborts BODY and returns (values NIL T). Used pervasively by
;; UIOP/ASDF around operations. Expands over RESTART-CASE with a :report that
;; formats the given control string and arguments.
(defmacro with-simple-restart ((name format-control &rest format-arguments) &rest body)
  `(restart-case (progn ,@body)
     (,name ()
       :report (lambda (%wsr-stream) (format %wsr-stream ,format-control ,@format-arguments))
       (values nil t))))

;;; ---------------------------------------------------------------------------
;;; Sequence / list helpers (Common Lisp, now that lambda lists bind properly)
;;; ---------------------------------------------------------------------------

(defun identity (x) x)

;; GET-DECODED-TIME (CLHS 25.1.4.1) — the current time as nine decoded values.
(defun get-decoded-time ()
  (decode-universal-time (get-universal-time)))

;; FFLOOR/FCEILING/FTRUNCATE/FROUND (CLHS 12.2) — like FLOOR/&c but the quotient
;; is a float. Second value is the (exact) remainder. The quotient's float
;; format follows the argument: if NUMBER (or DIVISOR) is a float, the quotient
;; is a float of that format; otherwise a single-float (the default).
(defun %float-quotient-proto (number divisor)
  (cond ((floatp number) number) ((floatp divisor) divisor) (t 1.0)))
(defun ffloor (number &optional (divisor 1))
  (multiple-value-bind (q r) (floor number divisor)
    (values (float q (%float-quotient-proto number divisor)) r)))
(defun fceiling (number &optional (divisor 1))
  (multiple-value-bind (q r) (ceiling number divisor)
    (values (float q (%float-quotient-proto number divisor)) r)))
(defun ftruncate (number &optional (divisor 1))
  (multiple-value-bind (q r) (truncate number divisor)
    (values (float q (%float-quotient-proto number divisor)) r)))
(defun fround (number &optional (divisor 1))
  (multiple-value-bind (q r) (round number divisor)
    (values (float q (%float-quotient-proto number divisor)) r)))

;; UPGRADED-ARRAY-ELEMENT-TYPE (CLHS 15.1.1): the element type the implementation
;; actually stores. BIT, BASE-CHAR (8-bit strings), CHARACTER (32-bit strings)
;; and DOUBLE-FLOAT have specialized storage; other element types upgrade to T.
;; BASE-CHAR used to upgrade to CHARACTER while MAKE-ARRAY and TYPEP kept the two
;; widths apart (spec §1.6.3), so (upgraded-array-element-type 'base-char)
;; disagreed with (array-element-type (make-array 1 :element-type 'base-char)).
(defun upgraded-array-element-type (type &optional environment)
  (declare (ignore environment))
  (cond ((member (if (consp type) (car type) type) '(double-float long-float)) 'double-float)
        ((eq type 'bit) 'bit)
        ((member type '(base-char standard-char)) 'base-char)
        ((member type '(character extended-char)) 'character)
        (t t)))

;; UPGRADED-COMPLEX-PART-TYPE (CLHS 12.2.6): the part type used for a complex of
;; the given part type. egcl stores complex parts unspecialised, so a float part
;; keeps its float type and everything else upgrades to RATIONAL (CL default).
(defun upgraded-complex-part-type (type &optional environment)
  (declare (ignore environment))
  ;; A class metaobject stands for its name. Rationals share one part type,
  ;; each float format is its own, FLOAT and REAL stay as they are
  ;; (ansi numbers upgraded-complex-part-type.*).
  (let ((type (if (typep type 'class) (class-name type) type)))
    (cond ((member type '(single-float short-float)) 'single-float)
          ((member type '(double-float long-float)) 'double-float)
          ((member type '(float real)) type)
          ((subtypep type 'rational) 'rational)
          ((subtypep type 'single-float) 'single-float)
          ((subtypep type 'double-float) 'double-float)
          ((subtypep type 'float) 'float)
          (t 'real))))

;; Early bootstrap REMOVE-DUPLICATES: EQL only, keeps first occurrence. Used by
;; the package machinery loaded before the full definition (further below, after
;; GETF/FIND/%COERCE-LIKE) supersedes it. Deliberately simple so it compiles here
;; with no forward references.
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

;; REMOVE-IF / REMOVE-IF-NOT honour the full keyword set. They used to take
;; `&rest keys` and `(declare (ignore keys))` — so :KEY, :COUNT, :START, :END and
;; :FROM-END were all silently dropped and every match was removed. The old
;; comment here admitted it ("a separate gap"); this closes it (ansi
;; REMOVE-IF.ORDER.1 and the DELETE-IF pair that delegates here).
;;
;; Built on the same %match-positions picker REMOVE uses, so the keyword
;; semantics — including that :FROM-END selects the TRAILING matches and applies
;; the predicate back-to-front — cannot drift between the two.
(defun remove-if (pred seq &key key (start 0) end count from-end)
  (let* ((items (coerce seq 'list))
         (chosen (%match-positions
                  (lambda (x) (funcall pred (if key (funcall key x) x)))
                  items start end count from-end))
         (result nil)
         (i 0))
    (dolist (x items)
      (unless (member i chosen) (push x result))
      (incf i))
    (%coerce-like (reverse result) seq)))

(defun remove-if-not (pred seq &key key (start 0) end count from-end)
  (remove-if (lambda (x) (not (funcall pred x))) seq
             :key key :start start :end end :count count :from-end from-end))

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
;;; EGCL's evaluator provides package primitives from the Rust CLI/runtime.
;;; Keep only thin symbol helpers here; package functions themselves should
;;; resolve to the real builtins so bundled ASDF can exercise actual package
;;; state instead of bootstrap stubs.
;;; ---------------------------------------------------------------------------

;; SYMBOL-NAME requires a symbol; a non-symbol is a TYPE-ERROR whose datum is the
;; offending object (ansi-test symbol-name.error.3). STRING would otherwise coerce
;; a string/character silently.
(defun symbol-name (s)
  (if (symbolp s)
      (string s)
      (error 'type-error :datum s :expected-type 'symbol)))

;; SYMBOL-PACKAGE is provided as a builtin that inspects the symbol's real
;; package prefix; the previous bootstrap definition parsed (string s), which
;; no longer carries a package prefix now that STRING returns the bare name.

(defmacro do-external-symbols (binding &rest body)
  (let ((var (car binding))
        (package (if (cdr binding) (car (cdr binding)) '*package*))
        (result (if (cdr (cdr binding)) (car (cdr (cdr binding))) nil)))
    ;; :external — only the package's exported symbols (CLHS); passing NIL here
    ;; enumerated every PRESENT symbol, which broke UIOP's ensure-package
    ;; export bookkeeping (bliss-jnzb).
    `(dolist (,var (egcl-internal::package-symbols ,package :external) ,result)
       ,@body)))

(defmacro do-symbols (binding &rest body)
  (let ((var (car binding))
        (package (if (cdr binding) (car (cdr binding)) '*package*))
        (result (if (cdr (cdr binding)) (car (cdr (cdr binding))) nil)))
    `(dolist (,var (egcl-internal::package-symbols ,package t) ,result)
       ,@body)))

(defmacro do-all-symbols (binding &rest body)
  (let ((var (car binding))
        (result (if (cdr binding) (car (cdr binding)) nil))
        (pkg (gensym))
        (all (gensym))
        (s (gensym)))
    ;; ONE flat DOLIST over the concatenated per-package symbol lists, so a
    ;; RETURN in the body targets the implicit BLOCK NIL of this form (CLHS
    ;; do-all-symbols) — the old nested per-package DOLIST captured it and
    ;; merely advanced to the next package (ansi DO-ALL-SYMBOLS.5/6/12).
    ;; DOLIST also evaluates the result-form with VAR bound to NIL (CLHS).
    `(dolist (,var (let ((,all nil))
                     (dolist (,pkg (list-all-packages))
                       (dolist (,s (egcl-internal::package-symbols ,pkg t))
                         (push ,s ,all)))
                     (nreverse ,all))
             ,result)
       ,@body)))

;;; WITH-PACKAGE-ITERATOR / FIND-ALL-SYMBOLS
;;;
;;; Built on the EGCL-INTERNAL::PACKAGE-SYMBOLS primitive:
;;;   (… pkg nil)       → present symbols (internal + external)
;;;   (… pkg :external)  → external symbols only
;;;   (… pkg t)          → accessible symbols (present + inherited)
;;; from which internal = present \ external and inherited = accessible \ present.

(defun egcl-internal::%package-iterator-tuples (packages symbol-types)
  ;; PACKAGES is a single package designator or a list of them. Returns a list
  ;; of (symbol access-type package) triples for the requested SYMBOL-TYPES.
  (let ((pkgs (if (listp packages) packages (list packages)))
        (result nil))
    (dolist (pd pkgs result)
      (let* ((p (find-package pd)))
        (when p
          (let ((present (egcl-internal::package-symbols p nil))
                (external (egcl-internal::package-symbols p :external))
                (accessible (egcl-internal::package-symbols p t)))
            (when (member :external symbol-types)
              (dolist (s external) (push (list s :external p) result)))
            (when (member :internal symbol-types)
              (dolist (s present)
                (unless (member s external) (push (list s :internal p) result))))
            (when (member :inherited symbol-types)
              (dolist (s accessible)
                (unless (member s present) (push (list s :inherited p) result))))))))))

(defmacro with-package-iterator ((name package-list-form &rest symbol-types) &body body)
  ;; CLHS 11.2: omitted or invalid symbol-types signal PROGRAM-ERROR.
  (unless symbol-types
    (error 'program-error))
  (dolist (st symbol-types)
    (unless (member st '(:internal :external :inherited))
      (error 'program-error)))
  (let ((tuples (gensym "TUPLES"))
        (tup (gensym "TUP")))
    `(let ((,tuples (egcl-internal::%package-iterator-tuples
                     ,package-list-form ',symbol-types)))
       (macrolet ((,name ()
                    '(if ,tuples
                         (let ((,tup (pop ,tuples)))
                           (values t (first ,tup) (second ,tup) (third ,tup)))
                         (values nil nil nil nil))))
         ,@body))))

(defun find-all-symbols (string-designator)
  (let ((name (string string-designator))
        (result nil))
    (dolist (p (list-all-packages) result)
      (multiple-value-bind (sym access) (find-symbol name p)
        (when (and access (not (eq access :inherited)))
          (pushnew sym result))))))

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
(defvar *macroexpand-hook* #'funcall)
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
;; The suffix counter GENSYM appends to its default "G" prefix. GENSYM reads it,
;; makes the symbol, then increments it; it must be a non-negative integer
;; (CLHS *GENSYM-COUNTER*). Declared special here so `(let ((*gensym-counter* n))
;; …)` rebinds it dynamically and GENSYM (a builtin) sees the bound value.
(defvar *gensym-counter* 0)
(defvar *read-default-float-format* 'single-float)
(defvar *read-eval* t)
(defvar *read-suppress* nil)
;; Bound to the pathname/truename of the file being LOADed, or NIL when not in a
;; load (e.g. under --eval). ASDF's DEFSYSTEM reads these to record a system's
;; source location.
(defvar *load-pathname* nil)
(defvar *load-truename* nil)
(defvar *load-verbose* nil)
(defvar *load-print* nil)
(defvar *compile-file-pathname* nil)
(defvar *compile-file-truename* nil)
;; COMPILE-FILE prints per-file progress when true (default T, as in SBCL); a
;; per-form printer flag also exists but is quiet by default.
(defvar *compile-verbose* t)
(defvar *compile-print* nil)
;; Pretty-print dispatch tables (CLHS 22.2.1.4).  A table is a cons whose car
;; is the tag EGCL::%PPRINT-DISPATCH-TABLE and whose cdr is the entry list;
;; each entry is (TYPE-SPECIFIER PRIORITY FUNCTION).  When *PRINT-PRETTY* is
;; true the printer asks EGCL::%PPRINT-DISPATCH-STRING for the text of the
;; highest-priority entry whose type the object satisfies, and prints normally
;; when there is none.  The standard table has no entries.
(defun egcl::%make-pprint-dispatch-table (&optional entries)
  (cons 'egcl::%pprint-dispatch-table entries))
(defun egcl::%pprint-dispatch-table-p (object)
  (and (consp object) (eq (car object) 'egcl::%pprint-dispatch-table)))
(defun egcl::%check-pprint-dispatch-table (table)
  (unless (egcl::%pprint-dispatch-table-p table)
    (error 'type-error :datum table :expected-type 'egcl::pprint-dispatch-table))
  table)
(defvar egcl::*standard-pprint-dispatch-table* (egcl::%make-pprint-dispatch-table))
(defvar *print-pprint-dispatch* egcl::*standard-pprint-dispatch-table*)
(defun copy-pprint-dispatch (&optional (table *print-pprint-dispatch*))
  (let ((source (if (null table)
                    egcl::*standard-pprint-dispatch-table*
                    (egcl::%check-pprint-dispatch-table table))))
    (egcl::%make-pprint-dispatch-table (mapcar #'copy-list (cdr source)))))
(defun egcl::%pprint-dispatch-entry (table object)
  (let ((best nil))
    (dolist (entry (cdr table) best)
      (when (and (typep object (first entry))
                 (or (null best) (> (second entry) (second best))))
        (setq best entry)))))
(defun set-pprint-dispatch (type-specifier function
                            &optional (priority 0) (table *print-pprint-dispatch*))
  (check-type priority real)
  (egcl::%check-pprint-dispatch-table table)
  (setf (cdr table)
        (remove type-specifier (cdr table) :key #'first :test #'equal))
  (when function
    (push (list type-specifier priority function) (cdr table)))
  nil)
(defun egcl::%pprint-default-printer (stream object)
  (let ((*print-pprint-dispatch* (egcl::%make-pprint-dispatch-table)))
    (write object :stream stream)))
(defun pprint-dispatch (object &optional (table *print-pprint-dispatch*))
  (let ((entry (egcl::%pprint-dispatch-entry
                (egcl::%check-pprint-dispatch-table
                 (or table egcl::*standard-pprint-dispatch-table*))
                object)))
    (if entry
        (values (third entry) t)
        (values #'egcl::%pprint-default-printer nil))))
(defun egcl::%pprint-dispatch-string (table object)
  (let ((entry (egcl::%pprint-dispatch-entry table object)))
    (when entry
      (with-output-to-string (stream)
        (funcall (third entry) stream object)))))
;; Random-state and readtable copiers. EGCL's RANDOM uses a single global PRNG
;; and its readtable is the immutable :STANDARD-READTABLE, so these return
;; lightweight placeholders — enough for portable code (e.g. bordeaux-threads'
;; +STANDARD-IO-BINDINGS+) that rebinds *RANDOM-STATE* / *READTABLE* to fresh
;; copies around a computation.
;; The REPL history variables and *BREAK-ON-SIGNALS* are standard variables
;; (CLHS 25.1.1, 9.2) and must be bound even when no REPL maintains them
;; (ansi symbols cl-variable-symbols.1).
(defvar * nil)
(defvar ** nil)
(defvar *** nil)
(defvar + nil)
(defvar ++ nil)
(defvar +++ nil)
(defvar / nil)
(defvar // nil)
(defvar /// nil)
(defvar - nil)
(defvar *break-on-signals* nil)

;; A random state is (:RANDOM-STATE . seed); RANDOM advances the seed in place
;; and seeds a NIL one from OS entropy on first use. Copying the cons copies
;; the sequence, which is what MAKE-RANDOM-STATE promises (CLHS 12.2).
(defvar *random-state* (cons :random-state nil))
(defun make-random-state (&optional state)
  ;; CLHS: STATE is a RANDOM-STATE, T (a fresh, randomly seeded state), or
  ;; NIL (a copy of the current state).
  (unless (or (null state) (eq state t) (random-state-p state))
    (error 'type-error
           :datum state
           :expected-type '(or (member nil t) random-state)))
  (if (eq state t)
      (cons :random-state nil)
      (let ((source (or state *random-state*)))
        ;; A state never drawn from has no seed yet; one draw fixes it so the
        ;; copy and the original continue identically.
        (unless (cdr source) (random 2 source))
        (cons :random-state (cdr source)))))
(defun random-state-p (object)
  (and (consp object) (eq (car object) :random-state)))
(defun copy-readtable (&optional from-readtable to-readtable)
  ;; Real readtable objects with copied macro/dispatch registrations
  ;; (bliss-r4mk). (copy-readtable nil) per CLHS restores standard syntax —
  ;; the builtin copies from the CURRENT readtable when from is nil, which
  ;; still yields a fresh table without user registrations at boot time.
  (egcl::%copy-readtable from-readtable to-readtable))
(defun readtablep (object)
  (typep object 'readtable))
;; EGCL has a single immutable standard readtable; its case mode is :UPCASE
;; (CLHS 23.1.2 default). Portable code (e.g. chunga) reads READTABLE-CASE to
;; decide how to case-fold tokens; supporting the reader — and a SETF that
;; accepts the one mode we implement — is enough to load such systems.
;; The reader-macro function GET-MACRO-CHARACTER reports for a standard macro
;; character.  EGCL's built-in character readers are in Rust, so this stands in
;; for them: it reads the object the character introduces by pushing the
;; character back in front of the stream and reading with the standard
;; readtable.  That makes the function callable, as CLHS 2.1.4.4 allows
;; ((funcall (get-macro-character #\() stream #\() reads a list), and lets a
;; library that copies syntax between readtables with GET-MACRO-CHARACTER and
;; SET-MACRO-CHARACTER -- named-readtables' (:merge :standard) -- produce a
;; readtable that still reads.  A semicolon consumes the comment and returns no
;; values; a close paren is the reader error it always is.
(defvar egcl::*standard-readtable-for-macro-functions* nil)
(defun egcl::%standard-reader-macro (stream char)
  (cond ((char= char #\;) (read-line stream nil nil) (values))
        ((char= char #\)) (error 'reader-error :stream stream))
        ((char= char #\#)
         ;; The dispatching character: an optional decimal argument, the
         ;; sub-character, then that sub-character's standard reader below.
         (let ((arg nil) (sub nil))
           (loop (let ((c (read-char stream t nil t)))
                   (if (digit-char-p c)
                       (setq arg (+ (* 10 (or arg 0)) (digit-char-p c)))
                       (progn (setq sub c) (return)))))
           (egcl::%standard-dispatch-reader-macro stream sub arg)))
        (t (let ((*readtable* (or egcl::*standard-readtable-for-macro-functions*
                                  (setq egcl::*standard-readtable-for-macro-functions*
                                        (copy-readtable nil))))
                 (in (make-concatenated-stream (make-string-input-stream (string char))
                                               stream)))
             (read in t nil t)))))

;; The reader-macro function GET-DISPATCH-MACRO-CHARACTER reports for a
;; standard # sub-character (CLHS 2.4.8), so that a library which copies or
;; wraps the standard syntax -- zacl's reader hands a rewritten feature
;; expression back to the standard #+ function, named-readtables merges
;; dispatch tables -- gets a callable function rather than NIL.  #+ and #-
;; read the feature expression in the KEYWORD package and skip the next
;; object under *READ-SUPPRESS* when it is not wanted, returning no values
;; as the standard reader does, so they work inside a list; #| skips the
;; balanced comment and returns no values; every other sub-character is
;; read by pushing the dispatch sequence back in front of the stream and
;; reading with the standard readtable.  #\ keeps its own reader.
(defun egcl::%featurep (expression)
  (cond ((consp expression)
         (case (car expression)
           ((:and and) (every #'egcl::%featurep (cdr expression)))
           ((:or or) (some #'egcl::%featurep (cdr expression)))
           ((:not not) (not (egcl::%featurep (cadr expression))))
           (t (error "Unknown feature expression operator ~S" (car expression)))))
        ((symbolp expression)
         (member expression *features* :test #'string=))
        (t (error "~S is not a feature expression" expression))))
(defun egcl::%standard-dispatch-reader-macro (stream sub-char arg)
  (case sub-char
    ((#\+ #\-)
     (let* ((feature (let ((*package* (find-package :keyword))
                           (*read-suppress* nil))
                       (read stream t nil t)))
            (wanted (if (egcl::%featurep feature)
                        (char= sub-char #\+)
                        (char= sub-char #\-))))
       (if wanted
           (values (read stream t nil t))
           (let ((*read-suppress* t))
             (read stream t nil t)
             (values)))))
    (#\|
     (let ((depth 1))
       (loop
         (let ((c (read-char stream t nil t)))
           (cond ((and (char= c #\|) (char= (peek-char nil stream t nil t) #\#))
                  (read-char stream t nil t)
                  (when (zerop (decf depth)) (return)))
                 ((and (char= c #\#) (char= (peek-char nil stream t nil t) #\|))
                  (read-char stream t nil t)
                  (incf depth))))))
     (values))
    (t
     (let ((*readtable* (or egcl::*standard-readtable-for-macro-functions*
                            (setq egcl::*standard-readtable-for-macro-functions*
                                  (copy-readtable nil))))
           (in (make-concatenated-stream
                (make-string-input-stream
                 (if arg (format nil "#~d~c" arg sub-char) (format nil "#~c" sub-char)))
                stream)))
       (read in t nil t)))))
(defun readtable-case (readtable)
  ;; A non-readtable argument is a TYPE-ERROR (ansi readtable-case.error.3).
  (check-type readtable readtable)
  (egcl::%readtable-case readtable))
(defun (setf readtable-case) (mode readtable)
  (check-type readtable readtable)
  (unless (member mode '(:upcase :downcase :preserve :invert))
    (error 'type-error :datum mode
                       :expected-type '(member :upcase :downcase :preserve :invert)))
  (egcl::%set-readtable-case mode readtable)
  mode)

;;; SBCL's base-char preference. Under :STRINGS or :BOTH the reader stores a
;;; string literal of BASE-CHARs as an 8-bit SIMPLE-BASE-STRING; otherwise, as
;;; with the standard readtable's :SYMBOLS, every literal is a CHARACTER string.
;;; Symbol names are always stored compactly, whatever the readtable says.
(defun egcl-ext::readtable-base-char-preference (readtable)
  "How READTABLE stores names and string literals made only of BASE-CHARs:
:SYMBOLS (the standard readtable's), :STRINGS, :BOTH or NIL, as in SBCL."
  (check-type readtable readtable)
  (egcl::%readtable-base-char-preference readtable))
(defun (setf egcl-ext::readtable-base-char-preference) (preference readtable)
  (check-type readtable readtable)
  (unless (member preference '(:symbols :strings :both nil))
    (error 'type-error :datum preference
                       :expected-type '(member :symbols :strings :both nil)))
  (egcl::%set-readtable-base-char-preference preference readtable)
  preference)
(export '(egcl-ext::readtable-base-char-preference) :egcl-ext)

;;; The reader-macro function GET-MACRO-CHARACTER reports for a dispatching
;;; macro character made by MAKE-DISPATCH-MACRO-CHARACTER, whose dispatching
;;; reader is in Rust: an optional decimal argument, the sub-character, then
;;; that sub-character's function in the current readtable.
(defun egcl::%dispatch-macro-reader (stream char)
  (let ((arg nil) (sub nil))
    (loop (let ((c (read-char stream t nil t)))
            (if (digit-char-p c)
                (setq arg (+ (* 10 (or arg 0)) (digit-char-p c)))
                (progn (setq sub c) (return)))))
    (let ((function (get-dispatch-macro-character char sub)))
      (if function
          (funcall function stream sub arg)
          (error 'reader-error :stream stream)))))

;;; Enumerate a readtable's macro characters without probing every one of the
;;; CHAR-CODE-LIMIT codes (GitHub #473). The Rust side lists a small superset
;;; of candidates; each is reported only when GET-MACRO-CHARACTER (and, for a
;;; sub-character, GET-DISPATCH-MACRO-CHARACTER) gives it a function, so the
;;; result always agrees with those functions.
(defun egcl-ext::map-readtable-macro-characters (function readtable)
  "Call FUNCTION with (CHAR READER-FUNCTION NON-TERMINATING-P DISPATCH-ALIST)
once for each character with a reader-macro function in READTABLE (NIL means
the standard readtable), in order of character code. READER-FUNCTION and
NON-TERMINATING-P are what GET-MACRO-CHARACTER returns. DISPATCH-ALIST is NIL
unless CHAR is a dispatching macro character, when it is a fresh list of
(SUB-CHAR . FUNCTION) for every sub-character GET-DISPATCH-MACRO-CHARACTER
gives a function, listing an ASCII letter in upper case only.
Returns NIL."
  (let ((readtable (if (null readtable) (copy-readtable nil) readtable)))
    (check-type readtable readtable)
    (dolist (entry (egcl::%readtable-macro-character-candidates readtable) nil)
      (let ((char (if (consp entry) (car entry) entry)))
        (multiple-value-bind (reader-function non-terminating-p)
            (get-macro-character char readtable)
          (when reader-function
            (let ((alist nil))
              (when (consp entry)
                (dolist (sub (cdr entry))
                  (let ((sub-function (get-dispatch-macro-character char sub readtable)))
                    (when sub-function
                      (push (cons sub sub-function) alist)))))
              (funcall function char reader-function non-terminating-p
                       (nreverse alist)))))))))
(export '(egcl-ext::map-readtable-macro-characters) :egcl-ext)
;; The default pathname merged against by MERGE-PATHNAMES and friends; ANSI
;; requires it to be bound to a pathname. Initialize to the startup directory.
(defvar *default-pathname-defaults* (truename "."))

;;; WITH-STANDARD-IO-SYNTAX: evaluate BODY with the standard reader/printer
;;; variables bound to their ANSI-standard values. An empty body yields NIL.
;; WITH-COMPILATION-UNIT batches compiler warnings; this interpreter has no such
;; batching, so run the body directly and ignore the options (bliss-lb6.14: ASDF
;; compile-op wraps perform in it).
(defmacro with-compilation-unit (options &rest body)
  (declare (ignore options))
  (cons 'progn body))

;;; WITH-OUTPUT-TO-STRING: bind VAR to a fresh string-output-stream, run BODY,
;;; and return the accumulated string. (A supplied target string with a fill
;;; pointer is not supported; the stream form is used regardless.)
(defun egcl::%split-body-declarations (body)
  "BODY as (DECLARATIONS . FORMS): the leading DECLARE forms and the rest."
  (let ((declarations '()))
    (loop while (and (consp body) (consp (car body)) (eq (caar body) 'declare))
          do (push (pop body) declarations))
    (cons (nreverse declarations) body)))

;; WITH-OUTPUT-TO-STRING (CLHS 22.4): with a STRING-FORM the output is appended
;; to that string (which must have a fill pointer) when the body finishes, and
;; the body's values are returned; otherwise the collected output string is the
;; value.  An :ELEMENT-TYPE form is evaluated, before the body, for effect.
(defmacro with-output-to-string ((var &optional string-form &rest keys) &rest body)
  (let* ((split (egcl::%split-body-declarations body))
         (declarations (car split))
         (forms (cdr split))
         (element-type-form (getf keys :element-type))
         (element-type-forms (when (member :element-type keys) (list element-type-form))))
    (if string-form
        (let ((target (gensym "STRING")) (out (gensym "OUT")) (ch (gensym "CHAR")))
          `(let ((,target ,string-form))
             ,@element-type-forms
             (let ((,var (make-string-output-stream)))
               ,@declarations
               (multiple-value-prog1 (progn ,@forms)
                 (let ((,out (get-output-stream-string ,var)))
                   (loop for ,ch across ,out do (vector-push-extend ,ch ,target)))))))
        `(progn
           (let ((,var (make-string-output-stream
                        ,@(when element-type-forms
                            `(:element-type ,element-type-form)))))
             ,@declarations
             ,@forms
             (get-output-stream-string ,var))))))

(defmacro print-unreadable-object ((object stream &key type identity) &body body)
  (let ((obj (gensym "OBJECT"))
        (out (gensym "STREAM"))
        (show-type (gensym "TYPE"))
        (show-identity (gensym "IDENTITY")))
    `(let* ((,obj ,object)
            (,out ,stream)
            (,show-type ,type)
            (,show-identity ,identity))
       (when *print-readably*
         (error 'print-not-readable :object ,obj))
       (write-string "#<" ,out)
       (when ,show-type
         (princ (type-of ,obj) ,out)
         (write-char #\Space ,out))
       ,@body
       (when ,show-identity
         (when (or (not ,show-type) ,(not (null body)))
           (write-char #\Space ,out))
         (let ((*print-base* 16) (*print-radix* nil))
           (princ (egcl-internal::%object-address ,obj) ,out)))
       (write-char #\> ,out)
       nil)))

;;; READ-DELIMITED-LIST: read objects from STREAM up to the terminating CHAR,
;;; which is consumed, and return them as a list (CLHS 23.2).
(defun read-delimited-list (char &optional (stream *standard-input*) recursive-p)
  (declare (ignore recursive-p))
  (let ((stream (cond ((eq stream t) *terminal-io*)
                      ((null stream) *standard-input*)
                      (t stream)))
        (result '()))
    ;; Walked with TAGBODY rather than DO, like PSETQ below and for the same
    ;; reason: DO is a macro defined further down this file, so a function up
    ;; here that uses it is lowered before the expander exists. The bytecode
    ;; compiler then refuses the form (special:DO), and because that verdict
    ;; belongs to the body it was built for, the function stays in the
    ;; tree-walker for the life of the image -- no later call can lift it.
    ;; MEMBER-IF, measured at 32x its COUNT-IF equivalent, is what found this.
    (tagbody
      scan
        (when (char= (peek-char t stream t nil t) char)
          (read-char stream t nil t)
          (go done))
        (push (read stream t nil t) result)
        (go scan)
      done)
    (nreverse result)))

;;; READ-CHAR-NO-HANG is a builtin wired to the stdlib stream protocol, which
;;; knows how to poll a pipe or socket without blocking; it cannot be written
;;; here in terms of READ-CHAR, which blocks.

;;; INTERACTIVE-STREAM-P: true for a stream connected to the terminal (CLHS
;;; 21.2): *TERMINAL-IO* itself, a synonym stream resolving to an interactive
;;; stream, or a two-way/echo stream whose input side is interactive.
(defun interactive-stream-p (stream)
  (unless (streamp stream)
    (error 'type-error :datum stream :expected-type 'stream))
  ;; The process's standard streams are interactive; composite streams are
  ;; as interactive as the stream they read from (so a *TERMINAL-IO* rebound
  ;; to a two-way stream over a string is not; ansi streams listen.8).
  (cond ((egcl::%native-interactive-stream-p stream) t)
        ((typep stream 'synonym-stream)
         (let ((target (symbol-value (synonym-stream-symbol stream))))
           (and (streamp target) (not (eq target stream))
                (interactive-stream-p target))))
        ((typep stream 'two-way-stream)
         (interactive-stream-p (two-way-stream-input-stream stream)))
        ((typep stream 'echo-stream)
         (interactive-stream-p (echo-stream-input-stream stream)))
        (t nil)))

;;; LISTEN: true when a character is immediately available on STREAM, and false
;;; both when none is available yet and at end of file (CLHS 21.2).
;;;
;;; READ-CHAR-NO-HANG draws exactly that distinction -- it answers NIL for
;;; either case and a character only when one is really there -- so ask it and
;;; push the character back. PEEK-CHAR cannot serve here: on a pipe, socket or
;;; terminal with nothing buffered it blocks, which is the one thing LISTEN must
;;; never do. Nor can an interactive stream simply answer NIL: that reports no
;;; input while a key is waiting.
(defun listen (&optional (stream *standard-input*))
  (let ((stream (cond ((eq stream t) *terminal-io*)
                      ((null stream) *standard-input*)
                      (t stream))))
    (unless (streamp stream)
      (error 'type-error :datum stream :expected-type 'stream))
    ;; An octet stream (a binary file, a socket) has no characters to push
    ;; back: READ-CHAR-NO-HANG would take the pending byte as a character
    ;; and UNREAD-CHAR would park it where READ-BYTE never looks, so LISTEN
    ;; answered T and the READ-BYTE after it hung.  Ask the stream itself,
    ;; which probes the handle without consuming anything.
    (if (subtypep (stream-element-type stream) 'character)
        (let ((c (read-char-no-hang stream nil nil)))
          (when c
            (unread-char c stream)
            t))
        (egcl::%stream-listen stream))))

;;; CLEAR-INPUT: discard unread buffered input; nothing is buffered beyond what
;;; the stream itself holds, so this only checks the designator and returns NIL.
(defun clear-input (&optional (stream *standard-input*))
  (let ((stream (cond ((eq stream t) *terminal-io*)
                      ((null stream) *standard-input*)
                      (t stream))))
    (unless (streamp stream)
      (error 'type-error :datum stream :expected-type 'stream))
    nil))

;;; FILE-AUTHOR: the file's author is not recorded in a portable way; NIL is
;;; the permitted answer when it cannot be determined (CLHS 20.2). The file
;;; must exist, else a FILE-ERROR.
(defun file-author (pathspec)
  (let ((truename (probe-file pathspec)))
    (unless truename
      (error 'file-error :pathname (pathname pathspec)))
    nil))

;;; WITH-INPUT-FROM-STRING: bind VAR to a string-input-stream over STRING.
(defmacro with-input-from-string ((var string &key (start 0) end index) &rest body)
  (let* ((split (egcl::%split-body-declarations body))
         (declarations (car split))
         (forms (cdr split)))
    `(let ((,var (make-string-input-stream ,string ,start ,end)))
       ,@declarations
       ,(if index
            ;; :INDEX names a place that receives the index of the first
            ;; unread character when the body finishes normally.
            `(multiple-value-prog1 (progn ,@forms)
               (setf ,index (file-position ,var)))
            `(progn ,@forms)))))

;;; WITH-OPEN-STREAM: bind VAR to STREAM for BODY, closing it on exit.
(defmacro with-open-stream ((var stream) &rest body)
  `(let ((,var ,stream))
     (unwind-protect (progn ,@body)
       (close ,var))))

(defmacro with-standard-io-syntax (&rest body)
  `(let ((*readtable* :standard-readtable)
         ;; The package object itself, not its name: the binding must be EQ
         ;; to (find-package "CL-USER") (ansi with-standard-io-syntax.1).
         (*package* (find-package "COMMON-LISP-USER"))
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
         (*print-pprint-dispatch* egcl::*standard-pprint-dispatch-table*)
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
  "Invoke the most recent ABORT restart; signal CONTROL-ERROR if none is active."
  (let ((r (find-restart 'abort condition)))
    (if r (invoke-restart r) (error 'control-error))))

(defun muffle-warning (&optional condition)
  "Invoke the most recent MUFFLE-WARNING restart; CONTROL-ERROR if none active."
  (let ((r (find-restart 'muffle-warning condition)))
    (if r (invoke-restart r) (error 'control-error))))

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

(defun reduce (fn seq &key key from-end (start 0) end (initial-value nil ivp))
  ;; A FUNCTION is not a sequence. REDUCE reaches its elements through
  ;; (coerce seq 'list), and COERCE hands a function straight back rather than
  ;; rejecting it, so the closure — structurally a cons — was then walked and
  ;; the error blamed its cdr instead of the function (ansi REDUCE.ERROR.1).
  ;; The COERCE gap itself is wider than this one caller; see bliss-hvfe.
  (when (functionp seq)
    (error 'type-error :datum seq :expected-type 'sequence))
  ;; NB: distinguish an explicit `:initial-value nil` from an omitted one via the
  ;; supplied-p flag IVP — otherwise (reduce f seq :initial-value nil) on an empty
  ;; sequence wrongly calls (f) with zero args (bliss-lb6.14: UIOP timestamps).
  (let ((items (coerce seq 'list)))
    (when (or (> start 0) end)
      (setq items (subseq items start (or end (length items)))))
    (when key (setq items (mapcar key items)))
    (when from-end (setq items (reverse items)))
    (if (null items)
        (if ivp initial-value (funcall fn))
        (let ((acc (if ivp initial-value (pop items))))
          (dolist (x items acc)
            (setq acc (if from-end (funcall fn x acc) (funcall fn acc x))))))))

;; The FIND and COUNT families test inline, like POSITION, rather than through
;; a local FLET: a FLET that refers to PRED, KEY or SEQ is a closure, and a
;; function that creates a capturing closure cannot leave the interpreter
;; (bliss-15gms). The COUNT family iterates with LOOP rather than DO: DO is
;; defined further down this file, so the COUNT family written with it never
;; compiled to bytecode and stayed tree-walked however hot it ran.
(defun find-if (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              when (funcall pred (if key (funcall key e) e)) return e)
        (loop for i from start below stop
              for e = (elt seq i)
              when (funcall pred (if key (funcall key e) e)) return e))))

(defun find-if-not (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              unless (funcall pred (if key (funcall key e) e)) return e)
        (loop for i from start below stop
              for e = (elt seq i)
              unless (funcall pred (if key (funcall key e) e)) return e))))

(defun position-if (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              when (funcall pred (if key (funcall key e) e)) return i)
        (loop for i from start below stop
              for e = (elt seq i)
              when (funcall pred (if key (funcall key e) e)) return i))))

(defun position-if-not (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              unless (funcall pred (if key (funcall key e) e)) return i)
        (loop for i from start below stop
              for e = (elt seq i)
              unless (funcall pred (if key (funcall key e) e)) return i))))

(defun count-if (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              count (funcall pred (if key (funcall key e) e)))
        (loop for i from start below stop
              for e = (elt seq i)
              count (funcall pred (if key (funcall key e) e))))))

(defun count-if-not (pred seq &key key (start 0) end from-end)
  (let ((stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              count (not (funcall pred (if key (funcall key e) e))))
        (loop for i from start below stop
              for e = (elt seq i)
              count (not (funcall pred (if key (funcall key e) e)))))))

;; Walked with ENDP, not `loop for l on list` (bliss-b9dr). LOOP's `on` driver
;; terminates via ATOM, which is right for LOOP but wrong here: MEMBER-IF
;; requires a LIST, so a non-list — or an improper tail actually reached
;; because no element matched — is a TYPE-ERROR. ENDP signals in exactly those
;; cases and stays quiet when a match is found before the bad tail, matching
;; SBCL: (member-if #'identity '(1 . 2)) => (1 . 2) but
;; (member-if #'null '(1 . 2)) and (member-if #'identity 7) both signal.
;; TAGBODY rather than DO, for the reason READ-DELIMITED-LIST explains above:
;; DO's expander is defined further down this file, so using it here pinned
;; both functions to the tree-walker permanently. MEMBER-IF then cost 51 s
;; where the equivalent COUNT-IF cost 1.6 s on the same 2000-element list.
(defun member-if (pred list &key key)
  (let ((l list) (found nil))
    (tagbody
      scan
        (when (endp l) (go done))
        (when (funcall pred (if key (funcall key (car l)) (car l)))
          (setq found l)
          (go done))
        (setq l (cdr l))
        (go scan)
      done)
    found))

(defun member-if-not (pred list &key key)
  (let ((l list) (found nil))
    (tagbody
      scan
        (when (endp l) (go done))
        (unless (funcall pred (if key (funcall key (car l)) (car l)))
          (setq found l)
          (go done))
        (setq l (cdr l))
        (go scan)
      done)
    found))

;; Each alist entry must be a cons or NIL; NIL entries are skipped, but a
;; non-NIL atom is a type-error (ansi-test assoc-if.error.12). The DOLIST
;; walk also signals on an improper alist tail via ENDP.
(defun assoc-if (pred alist &key key)
  (dolist (pair alist nil)
    (cond ((null pair))
          ((consp pair)
           (when (funcall pred (if key (funcall key (car pair)) (car pair)))
             (return pair)))
          (t (error 'type-error :datum pair :expected-type 'list)))))

(defun assoc-if-not (pred alist &key key)
  (dolist (pair alist nil)
    (cond ((null pair))
          ((consp pair)
           (when (not (funcall pred (if key (funcall key (car pair)) (car pair))))
             (return pair)))
          (t (error 'type-error :datum pair :expected-type 'list)))))

;;; Item-based FIND / POSITION / COUNT. Defined in Lisp over ELT/LENGTH/FUNCALL
;;; (like the -IF family and REDUCE) because the stdlib's internal apply helper
;;; cannot invoke an interpreter :key/:test — a real function used to reach it
;;; and panic the whole process. See bliss-0l1 and spec §5.6.

(defun find (item seq &key key test test-not (start 0) end from-end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              when (if neg (not matched) matched) return e)
        (loop for i from start below stop
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              when (if neg (not matched) matched) return e))))

(defun position (item seq &key key test test-not (start 0) end from-end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              when (if neg (not matched) matched) return i)
        (loop for i from start below stop
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              when (if neg (not matched) matched) return i))))

;; COUNT and friends accept :FROM-END. The COUNT itself cannot depend on
;; direction, but the ORDER in which :KEY and the test are applied does, and
;; ansi checks it (COUNT-*.7/.9/.12, COUNT*.ORDER.1 and the COUNT-IF* .16/.17
;; family). Omitting the keyword made every one of those a PROGRAM-ERROR for an
;; unrecognized keyword argument — 32 tests from one missing parameter.
(defun count (item seq &key key test test-not (start 0) end from-end)
  (let ((testfn (or test test-not #'eql))
        (neg (if test-not t nil))
        (stop (or end (length seq))))
    (if from-end
        (loop for i from (1- stop) downto start
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              count (if neg (not matched) matched))
        (loop for i from start below stop
              for e = (elt seq i)
              for matched = (funcall testfn item (if key (funcall key e) e))
              count (if neg (not matched) matched)))))

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
(defmacro psetq (&rest pairs &environment env)
  ;; CLHS: if any var refers to a SYMBOL MACRO, PSETQ behaves as PSETF. The plain
  ;; expansion below -- (let ((tmp val)...) (setq var tmp)...) -- assigns without
  ;; lifting the PLACE's subforms first, so with a symbol-macro var such as
  ;; (aref a (incf i)) the subform ran at assignment time and the parallel
  ;; semantics came out wrong (ansi PSETQ.7; bliss-t00q).
  ;;
  ;; Only that case delegates to PSETF, and deliberately so: PSETF's own
  ;; expansion uses DOLIST, which expands through DO to PSETQ, so delegating
  ;; UNCONDITIONALLY makes the two macros expand each other forever -- it blew
  ;; the control stack on the first test that used one.
  (let ((uses-symbol-macro nil) (p pairs))
    (tagbody
      scan
      (unless (consp p) (go done))
      (unless (symbolp (car p))
        (error "PSETQ: ~S is not a variable name" (car p)))
      (multiple-value-bind (expansion expanded) (macroexpand-1 (car p) env)
        (declare (ignore expansion))
        (when expanded (setq uses-symbol-macro t)))
      (setq p (cddr p))
      (go scan)
      done)
    (if uses-symbol-macro
        `(psetf ,@pairs)
        (let ((bindings nil) (assigns nil) (q pairs))
          (tagbody
            scan
            (unless (consp (cdr q)) (go done))
            (let ((var (car q)) (tmp (gensym)))
              (setq bindings (cons (list tmp (cadr q)) bindings))
              (setq assigns (cons (list 'setq var tmp) assigns))
              (setq q (cddr q)))
            (go scan)
            done)
          `(let ,(reverse bindings) ,@(reverse assigns) nil)))))

;; Interleave two lists: (a b) (x y) => (a x b y). Helper for DO's step forms.
(defun %zip-pairs (a b)
  (if (or (null a) (null b))
      nil
      (cons (car a) (cons (car b) (%zip-pairs (cdr a) (cdr b))))))

(defun %do-var (b) (if (consp b) (car b) b))
(defun %do-init (b) (if (consp b) (cadr b) nil))
(defun %do-step (b)
  (if (and (consp b) (cddr b)) (caddr b) (%do-var b)))

;; Split leading (declare ...) forms off a DO/DO*/DOLIST/DOTIMES body: returns
;; (values declarations remaining-body). The declarations belong at the top of
;; the implicit variable-binding LET (CLHS 6.1.7 / 5.3.3), where a `(declare
;; (special x))` scopes the whole loop body — putting them inside the tagbody
;; would make them inert (DO.17-19, DO*.17-19).
(defun %split-declares (forms)
  (let ((decls nil))
    ;; DO is used by LOOP's expander, so this bootstrap helper must not
    ;; itself invoke LOOP (or DO) while finding declarations.
    (tagbody
      scan
      (unless (and (consp forms) (consp (car forms))
                   (eq (car (car forms)) 'declare))
        (go done))
      (setq decls (cons (car forms) decls) forms (cdr forms))
      (go scan)
      done)
    (values (reverse decls) forms)))

(defmacro do (bindings end-test &rest body)
  (let ((vars (mapcar (function %do-var) bindings))
        (inits (mapcar (function %do-init) bindings))
        (steps (mapcar (function %do-step) bindings))
        (top (gensym)))
    (multiple-value-bind (decls forms) (%split-declares body)
      `(block nil
         (let ,(mapcar (function list) vars inits)
           ,@decls
           (tagbody
              ,top
              (when ,(car end-test) (return (progn ,@(cdr end-test))))
              ,@forms
              (psetq ,@(%zip-pairs vars steps))
              (go ,top)))))))

(defmacro do* (bindings end-test &rest body)
  (let ((vars (mapcar (function %do-var) bindings))
        (steps (mapcar (function %do-step) bindings))
        (top (gensym)))
    (multiple-value-bind (decls forms) (%split-declares body)
      `(block nil
         (let* ,(mapcar (function list) vars (mapcar (function %do-init) bindings))
           ,@decls
           (tagbody
              ,top
              (when ,(car end-test) (return (progn ,@(cdr end-test))))
              ,@forms
              ,@(mapcar (lambda (v s) (list 'setq v s)) vars steps)
              (go ,top)))))))

;;; ---------------------------------------------------------------------------
;;; Character functions (over CHAR-CODE / CODE-CHAR; ASCII case mapping).
;;; ---------------------------------------------------------------------------

;; CHAR= and the ordered char comparisons are variadic in CL: CHAR< etc. test a
;; monotonic sequence, CHAR= that all args are equal, CHAR/= that all are
;; pairwise distinct. cl-ppcre's char-class matcher relies on (char<= lo c hi)
;; range tests (3 args) (bliss-omw).
;; CHAR=, CHAR/=, CHAR<, CHAR>, CHAR<= and CHAR>= are Rust builtins wired to
;; egcl-stdlib::characters (bliss-7oa5). They were defuns here, which made
;; every 2-argument call allocate a rest list, run an interpreted DOLIST,
;; dispatch CHAR-CODE twice and then generic `=` -- 9.8us against 0.27us for
;; EQ. A defun here would SHADOW the builtin (the function cell wins over the
;; operator-position arm), so this must stay a comment, not a definition.

;; CHAR-UPCASE / CHAR-DOWNCASE are Rust builtins (bliss-7oa5) using the
;; Unicode 1:1 case mapping in egcl-stdlib::characters. The case predicates are
;; DEFINED from those conversions rather than restating a rule of their own, so
;; the ansi CHAR-UPCASE.2 / CHAR-DOWNCASE.2 invariant -- a character the
;; conversion changes is one the predicate classifies -- holds by construction
;; for every code point (bliss-ne14). CHAR-DOWNCASE also type-checks C.
(defun upper-case-p (c) (char/= (char-downcase c) c))
(defun lower-case-p (c) (char/= (char-upcase c) c))
;; Every character with case is a letter, but not the reverse: uncased letters
;; (U+00DF, CJK) are still missing here, a known gap.
(defun alpha-char-p (c) (or (upper-case-p c) (lower-case-p c)))
(defun digit-char-p (c &optional (radix 10))
  ;; Weight of C as a digit in RADIX (0-9, then A-Z / a-z = 10-35), or NIL.
  (let* ((code (char-code c))
         (d (cond ((and (>= code 48) (<= code 57)) (- code 48))         ; 0-9
                  ((and (>= code 65) (<= code 90)) (+ 10 (- code 65)))  ; A-Z
                  ((and (>= code 97) (<= code 122)) (+ 10 (- code 97))) ; a-z
                  (t nil))))
    (if (and d (< d radix)) d nil)))
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
  ;; ANSI: N is a non-negative integer; a negative index is a TYPE-ERROR
  ;; (previously (<= n 0) silently returned the whole list).
  (unless (and (integerp n) (>= n 0))
    (error 'type-error :datum n :expected-type '(integer 0)))
  ;; Iterative walk so (last long-list) / (nthcdr big-n list) do not recurse
  ;; one frame per step (bliss-2r5).
  (do ((i n (- i 1))
       (l list (cdr l)))
      ((or (= i 0) (null l)) l)))
(defun last (list &optional (n 1))
  ;; ANSI: return the last N conses of LIST. N must be a non-negative
  ;; integer (a negative or non-integer N is a TYPE-ERROR). Works on dotted
  ;; lists and on huge (bignum) N without walking N times: a lead pointer L
  ;; advances up to N steps, then L and the trailing pointer R advance in
  ;; lockstep until L reaches the terminating atom.
  (unless (and (integerp n) (>= n 0))
    (error 'type-error :datum n :expected-type '(integer 0)))
  ;; LIST must actually be a list. Without this the loop below exits
  ;; immediately on a non-cons and RETURNS THE ARGUMENT ITSELF, so (last 5)
  ;; answered 5 and (last "abc") answered "abc" -- a non-list silently
  ;; reported as the last cons of itself (bliss-swi5). A dotted list is still
  ;; a list, so (last (cons 1 2)) => (1 . 2) is unaffected. SBCL gets this from
  ;; its (defknown last (list &optional unsigned-byte) ...) declaration.
  (unless (listp list)
    (error 'type-error :datum list :expected-type 'list))
  (let ((l list) (r list) (i 0))
    (do () ((or (>= i n) (not (consp l))))
      (setq l (cdr l)) (setq i (+ i 1)))
    (do () ((not (consp l)) r)
      (setq l (cdr l)) (setq r (cdr r)))))
(defun butlast (list &optional (n 1))
  ;; ANSI: fresh copy of LIST with the last N conses removed. Handles dotted
  ;; lists (LENGTH would error on them) and huge N. Non-destructive.
  (unless (and (integerp n) (>= n 0))
    (error 'type-error :datum n :expected-type '(integer 0)))
  (unless (listp list)
    (error 'type-error :datum list :expected-type 'list))
  (let ((len 0))
    (do ((l list (cdr l))) ((not (consp l))) (setq len (+ len 1)))
    (let ((keep (- len n)))
      (if (<= keep 0)
          nil
          (let* ((head (cons (car list) nil))
                 (tail head))
            (do ((rest (cdr list) (cdr rest))
                 (i 1 (+ i 1)))
                ((>= i keep) head)
              (let ((new (cons (car rest) nil)))
                (setf (cdr tail) new)
                (setq tail new))))))))
;; Ordinal list accessors. FIRST..THIRD have interpreter fast-paths, but the
;; higher ordinals (used by e.g. cl-ppcre's convert.lisp) need real function
;; cells so compiled code can call them (bliss-9q4).
(defun fourth (list) (nth 3 list))
(defun fifth (list) (nth 4 list))
(defun sixth (list) (nth 5 list))
(defun seventh (list) (nth 6 list))
(defun eighth (list) (nth 7 list))
(defun ninth (list) (nth 8 list))
(defun tenth (list) (nth 9 list))
(defun mapc (fn &rest lists)
  (apply (function mapcar) fn lists)
  (car lists))
(defun mapcan (fn &rest lists)
  (apply (function append) (apply (function mapcar) fn lists)))
;; MAP-INTO (CLHS 17.3): destructively store into RESULT-SEQUENCE the results of
;; applying FUNCTION to successive elements of the argument SEQUENCES, up to the
;; shortest length (or the whole result when there are no sequences). Returns
;; RESULT-SEQUENCE.
;; MAP-INTO ignores a result vector's FILL POINTER when deciding how many
;; elements to store — the limit is the vector's capacity — and sets the fill
;; pointer to the number actually stored (CLHS map-into). Both halves were
;; missing: LENGTH of a fill-pointer vector is its fill pointer, so a result with
;; fill-pointer 3 could never take more than 3 elements, and the pointer was left
;; wherever it started (ansi MAP-INTO-ARRAY.8/9/10).
;;
;; Elements go in through AREF for a vector rather than ELT, because ELT bounds
;; against the fill pointer and would refuse the very indices past it that this
;; is meant to fill.
(defun map-into (result-sequence function &rest sequences)
  (let* ((fp (and (vectorp result-sequence)
                  (array-has-fill-pointer-p result-sequence)))
         (capacity (if fp
                       (array-dimension result-sequence 0)
                       (length result-sequence)))
         (n (if sequences
                (apply (function min) capacity
                       (mapcar (function length) sequences))
                capacity)))
    (dotimes (i n)
      (let ((v (apply function (mapcar (lambda (s) (elt s i)) sequences))))
        (if (vectorp result-sequence)
            (setf (aref result-sequence i) v)
            (setf (elt result-sequence i) v))))
    (when fp (setf (fill-pointer result-sequence) n))
    result-sequence))
;;; GETF is a builtin (cli/evaluated_builtins.rs). It was a DEFUN here -- a DO
;;; loop plus an &optional -- and cost 7.31us against 0.035us for CAR, which
;;; matters because it is how every property list is read, including a UI
;;; framework's per-node properties. A DEFUN here would SHADOW the builtin.
;; GET-PROPERTIES (CLHS): scan PLIST for the first indicator that is EQ to one in
;; INDICATOR-LIST; return three values — that indicator, its value, and the PLIST
;; tail beginning at it — or (values NIL NIL NIL) if none is found.
(defun get-properties (plist indicator-list)
  (do ((tail plist (cddr tail)))
      ((null tail) (values nil nil nil))
    (when (member (car tail) indicator-list :test (function eq))
      (return (values (car tail) (cadr tail) tail)))))
;; A vector is reversed in place and returned as itself, so its element type,
;; fill pointer and adjustability survive (ansi nreverse-vector.5/6 compare
;; TYPE-OF of the argument and the result); a list goes through REVERSE.
(defun nreverse (seq)
  (if (listp seq)
      (reverse seq)
      (let ((i 0) (j (1- (length seq))))
        (do () ((>= i j) seq)
          (let ((tmp (aref seq i)))
            (setf (aref seq i) (aref seq j))
            (setf (aref seq j) tmp))
          (setq i (1+ i))
          (setq j (1- j))))))

;;; ---------------------------------------------------------------------------
;;; GCD / LCM and the STRING-TRIM family.
;;; ---------------------------------------------------------------------------

(defun %gcd2 (a b) (if (= b 0) a (%gcd2 b (mod a b))))

(defun %require-integers (integers)
  ;; ANSI: GCD/LCM accept only integers; a ratio/float/etc. is a TYPE-ERROR.
  (dolist (n integers)
    (unless (integerp n)
      (error 'type-error :datum n :expected-type 'integer))))

;; REDUCE hands back a lone element UNTOUCHED -- it never calls the function --
;; so the (abs a) below did not run for a ONE-argument call and (gcd -12)
;; answered -12 instead of 12 (ansi GCD.2-3; LCM.2-3 is the same shape).
;; Seeding with the identity (0 for GCD, since gcd(0,x) = |x|; 1 for LCM) puts
;; every element through the function, including the only one.
(defun gcd (&rest integers)
  (%require-integers integers)
  (reduce (lambda (a b) (%gcd2 (abs a) (abs b))) integers :initial-value 0))

(defun lcm (&rest integers)
  (%require-integers integers)
  (reduce (lambda (a b)
            (if (or (= a 0) (= b 0))
                0
                (/ (abs (* a b)) (%gcd2 (abs a) (abs b)))))
          integers :initial-value 1))

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
;; SCHAR is CHAR for simple strings; (setf schar) is handled as a place. babel's
;; string-get/string-set expand to (schar ...) / (setf (schar ...) ...).
(defun schar (s i) (elt s i))
(defun acons (key datum alist) (cons (cons key datum) alist))
(defun pairlis (keys data &optional alist)
  ;; CLHS PAIRLIS: prepend (key . datum) pairs onto ALIST (iterate uses it).
  (do ((k keys (cdr k))
       (d data (cdr d))
       (acc alist (acons (car k) (car d) acc)))
      ((or (endp k) (endp d)) acc)))
;; LIST-LENGTH (CLHS): the length of LIST, or NIL if it is circular. Floyd's
;; tortoise/hare cycle detection — a circular list yields NIL instead of looping
;; forever (ansi-test cons/list-length.lsp; the previous delegation to LENGTH
;; hung do-tests). ENDP signals a TYPE-ERROR on a dotted/improper tail, matching
;; the LIST-LENGTH.ERROR tests.
(defun list-length (list)
  (do ((n 0 (+ n 2))
       (fast list (cddr fast))
       (slow list (cdr slow)))
      (nil)
    (when (endp fast) (return n))
    (when (endp (cdr fast)) (return (+ n 1)))
    (when (and (eq fast slow) (> n 0)) (return nil))))
;; NCONC: destructively concatenate lists. Each argument except the last must be
;; a list (proper or dotted); its last cons is spliced onto the next non-empty
;; argument. NIL arguments are skipped; the LAST argument is used as the final
;; tail unchanged and may be any object (CLHS nconc). `(nconc)` => NIL and a lone
;; argument is returned unmodified. Unlike APPEND, structure is reused — so
;; `(nconc x y)` makes x's last cons point at y (nconc.4) and `(nconc x x)`
;; builds a circular list (nconc.5) rather than copying/looping.
(defun nconc (&rest lists)
  (let ((result nil) (tail nil) (p lists))
    (loop while p do
      (let ((l (car p)) (lastp (null (cdr p))))
        (cond
          ((null l))                        ; skip a NIL argument
          ((consp l)
           (if tail (setf (cdr tail) l) (setq result l))
           (unless lastp
             (let ((q l))
               (loop while (consp (cdr q)) do (setq q (cdr q)))
               (setq tail q))))
          (t
           ;; A non-list is legal only as the LAST argument, where it becomes
           ;; the final tail; anywhere else it is a type error.
           (if lastp
               (if tail (setf (cdr tail) l) (setq result l))
               (error 'type-error :datum l :expected-type 'list)))))
      (setq p (cdr p)))
    result))
;; Walk X; if it is not a proper list, signal a TYPE-ERROR whose datum is the
;; offending non-list tail (so it genuinely violates the expected type 'list —
;; ANSI's SIGNALS-ERROR rejects a type-error whose datum satisfies its
;; expected-type). Returns X when proper.
(defun %require-proper-list (x)
  (do ((p x (cdr p)))
      ((null p) x)
    (unless (consp p)
      (error 'type-error :datum p :expected-type 'list))))
(defun revappend (x y) (%require-proper-list x) (append (reverse x) y))
;; ANSI: a sequence/array size (and each array dimension) is a non-negative
;; integer; a negative or non-integer value is a TYPE-ERROR, not a silently
;; empty result. (make-list -1) used to return NIL; (make-array -1) => #().
(defun %check-nonneg-index (d)
  (unless (and (integerp d) (>= d 0))
    (error 'type-error :datum d :expected-type '(integer 0))))

(defun make-list (n &key initial-element)
  (%check-nonneg-index n)
  (loop repeat n collect initial-element))

;; Row-major flatten of nested :initial-contents matching DIMENSIONS: the
;; innermost axis contributes its elements; outer axes recurse and concatenate.
(defun %flatten-md-contents (contents dimensions)
  (if (null (cdr dimensions))
      (coerce contents 'list)
      (apply (function append)
             (mapcar (lambda (sub) (%flatten-md-contents sub (cdr dimensions)))
                     (coerce contents 'list)))))

;; Fill a freshly made multidimensional array from nested :initial-contents,
;; walking the array in row-major order.
(defun fill-md-array-from-contents (arr dimensions contents)
  (let ((i 0))
    (dolist (e (%flatten-md-contents contents dimensions))
      (setf (row-major-aref arr i) e)
      (setq i (+ i 1)))))

;; ARRAY-ROW-MAJOR-INDEX array &rest subscripts — the row-major (flat) index for
;; the given per-axis subscripts: fold (index*dim + subscript) across the axes.
;; A rank-0 array (no subscripts) yields 0. Called with no arguments the missing
;; required ARRAY signals a PROGRAM-ERROR (array-row-major-index.error.1).
(defun array-row-major-index (array &rest subscripts)
  (let ((dims (array-dimensions array))
        (index 0))
    (do ((s subscripts (cdr s))
         (d dims (cdr d)))
        ((null d) index)
      (setq index (+ (* index (car d)) (car s))))))

;; ARRAY-IN-BOUNDS-P array &rest subscripts — true iff there is one integer
;; subscript per axis and each lies in [0, dimension). Uses ARRAY-DIMENSIONS
;; (the backing capacity, ignoring any fill pointer, per CLHS). Non-integer,
;; negative, or out-of-range subscripts (including bignums) all give NIL rather
;; than an error.
(defun array-in-bounds-p (array &rest subscripts)
  (let ((dims (array-dimensions array)))
    (and (= (length subscripts) (length dims))
         (do ((s subscripts (cdr s))
              (d dims (cdr d)))
             ((null s) t)
           (let ((x (car s)))
             (unless (and (integerp x) (>= x 0) (< x (car d)))
               (return nil)))))))

;; MAKE-ARRAY dimensions &key initial-element initial-contents element-type.
;; A dimension list of rank ≥ 2 builds a real multidimensional array (row-major
;; storage); rank-0/1 build a string (character element-type) or simple/complex
;; vector as before. :adjustable / :fill-pointer apply to the rank-1 vector case.
;; A vector's :INITIAL-CONTENTS must have exactly as many elements as the
;; vector (CLHS MAKE-ARRAY); a shorter one used to be padded and a longer one
;; to fail on an index. SBCL's wording.
(defun %check-initial-contents-length (contents size)
  (unless (= (length contents) size)
    (error "There are ~D elements in the :INITIAL-CONTENTS, but the vector length is ~D."
           (length contents) size)))

(defun make-array (dimensions &rest keys)
  ;; A vector of fixnum length with at most :ELEMENT-TYPE and :INITIAL-ELEMENT,
  ;; whose element type upgrades to T, is built natively without the option
  ;; parsing below; %MAKE-SIMPLE-ARRAY returns NIL for every other request.
  (let ((vector (egcl::%make-simple-array dimensions keys)))
    (when vector
      (return-from make-array vector)))
  ;; Each dimension must be a non-negative integer (a list of them for rank ≥ 2,
  ;; or a bare integer for a vector). An empty list falls through to the
  ;; existing rank-0 path.
  (if (listp dimensions)
      (dolist (d dimensions) (%check-nonneg-index d))
      (%check-nonneg-index dimensions))
  (let* ((size (if (consp dimensions) (car dimensions) dimensions))
         (iel-cell (member :initial-element keys))
         (ic-cell (member :initial-contents keys))
         (et-cell (member :element-type keys))
         (et (%expand-type-spec (if et-cell (car (cdr et-cell)) t)))
         ;; DOUBLE-FLOAT uses packed storage. Other numeric requests currently
         ;; upgrade to T, but use numeric zero rather than NIL. This
         ;; lets read-modify-write users such as (SETF LDB) work without an
         ;; explicit initializer. ANSI leaves uninitialized reads undefined;
         ;; this zero default is an EGCL convenience.
         (et-kind (if (consp et) (car et) et))
         (iel (cond (iel-cell (car (cdr iel-cell)))
                    ((member et-kind '(short-float single-float float)) 0.0f0)
                    ((member et-kind '(double-float long-float)) 0.0d0)
                    ((member et-kind '(bit integer fixnum signed-byte unsigned-byte
                                      mod rational real number)) 0)
                    (t nil)))
         ;; A (VECTOR NIL) is a STRING subtype (CLHS 15.1.2.2): an
         ;; :element-type of NIL holds no elements, so a length-0 one is
         ;; SXHASH-similar to "" (ansi sxhash.8). Classify it as a string so
         ;; MAKE-ARRAY builds string storage, not a general NIL-filled vector.
         ;; EXTENDED-CHAR upgrades to CHARACTER, so it too builds a string.
         (stringp (member et '(character base-char standard-char extended-char nil)))
         (bitp (eq et 'bit))
         (doublep (member et-kind '(double-float long-float)))
         (fp-cell (member :fill-pointer keys))
         (adj-cell (member :adjustable keys))
         (fp (and fp-cell (car (cdr fp-cell))))
         (adjustable (and adj-cell (car (cdr adj-cell))))
         (dt-cell (member :displaced-to keys))
         (dio-cell (member :displaced-index-offset keys))
         ;; A dimension LIST of rank ≥ 2 ⇒ a real multidimensional array
         ;; (row-major storage). Rank-0/1 fall through to the vector paths.
         (mdp (and (consp dimensions) (consp (cdr dimensions)))))
    (cond
      ;; :displaced-to (rank-1): a complex vector whose element i reads and
      ;; writes BASE's row-major element (+ offset i) (bliss-7o4y). The
      ;; element-type tag mirrors the request so a character displaced array
      ;; answers STRINGP.
      ((and dt-cell (car (cdr dt-cell)) (not mdp))
       (let* ((base (car (cdr dt-cell)))
              (offset (if dio-cell (car (cdr dio-cell)) 0))
              (fpn (cond ((eq fp t) size)
                         ((integerp fp) fp)
                         (t size))))
         (%make-displaced-array base offset size fpn (and adjustable t)
                                (and stringp (not bitp) t) (and fp t) (and bitp t) (and doublep t))))
      ;; :displaced-to (rank >= 2): the row-major storage is itself a vector
      ;; displaced onto BASE, so AREF, ROW-MAJOR-AREF and the printer read
      ;; and write BASE's elements (ansi print.array.2.*).
      ((and dt-cell (car (cdr dt-cell)) mdp)
       (let* ((base (car (cdr dt-cell)))
              (offset (if dio-cell (car (cdr dio-cell)) 0))
              (total 1))
         (dolist (d dimensions) (setq total (* total d)))
         (egcl-internal::%make-md-array-with-storage
          dimensions
          (%make-displaced-array base offset total total nil
                                 (and stringp (not bitp) t) nil (and bitp t) (and doublep t)))))
      (mdp
       (let ((arr (%make-md-array dimensions iel (and doublep t))))
         (when ic-cell
           (fill-md-array-from-contents arr dimensions (car (cdr ic-cell))))
         arr))
      ;; Rank-0 array (dimensions = NIL): a single-element MD array. Its lone
      ;; element is the :initial-contents object itself (not a sequence) when
      ;; given, else the :initial-element or default (bliss-30be: ansi-test
      ;; universe.lsp builds (make-array nil)).
      ((null dimensions)
       (%make-md-array nil (cond (ic-cell (car (cdr ic-cell)))
                                 (t iel)) (and doublep t)))
      ;; A :fill-pointer or :adjustable request ⇒ a complex (fill-pointer /
      ;; adjustable) vector. The fill pointer is the given value, SIZE for
      ;; :fill-pointer t, or SIZE when only :adjustable is supplied.
      ((or fp adjustable)
       (when ic-cell (%check-initial-contents-length (car (cdr ic-cell)) size))
       (let* ((fpn (cond ((eq fp t) size)
                         ((integerp fp) fp)
                         (t size)))
              ;; A fill pointer exists only if the user passed a non-NIL
              ;; :fill-pointer; :adjustable alone gives a plain adjustable
              ;; array with no fill pointer (bliss-0x9y). A BIT element-type
              ;; tags the complex vector as a bit vector (bliss-65nx).
              (v (%make-complex-vector size fpn (and adjustable t) iel
                                       (and stringp (not bitp) t) (and fp t)
                                       (and bitp t) (and doublep t)
                                       ;; BASE-CHAR asks for 8-bit storage.
                                       (and (member et '(base-char standard-char)) t))))
         (when ic-cell
           (let ((i 0))
             (dolist (e (coerce (car (cdr ic-cell)) 'list))
               (setf (aref v i) e)
               (setq i (+ i 1)))))
         v))
      (doublep
       (let ((v (%make-double-float-vector size iel)))
         (when ic-cell
           (let ((contents (car (cdr ic-cell))))
             (unless (= (length contents) size)
               (error "Initial contents do not match array dimensions"))
             (dotimes (i size) (setf (aref v i) (elt contents i)))))
         v))
      (ic-cell
       (%check-initial-contents-length (car (cdr ic-cell)) size)
       ;; COERCE legitimately returns its ARGUMENT when it is already of the
       ;; target type (SBCL does the same), so coercing a string to 'string --
       ;; or a vector to 'vector -- would alias the :initial-contents. MAKE-ARRAY
       ;; must return a FRESH array (CLHS), and the source is often an immutable
       ;; literal, so a later (setf (aref v i) ...) died with "cannot modify an
       ;; interned string literal" (ansi EVERY/SOME/NOTANY/NOTEVERY.22;
       ;; bliss-4fbq). COPY-SEQ guarantees a new array. The bit-vector branch
       ;; already constructs a fresh one.
       ;; An element-type of T gets a general vector even when the contents
       ;; are a bit vector or string (ansi make-array.2d): COERCE to VECTOR
       ;; would hand those back as they are.
       (cond (bitp (%bit-vector-from-bits (coerce (car (cdr ic-cell)) 'list)))
             ;; A BASE-CHAR request copies into a fresh 8-bit string element by
             ;; element, so a wide character is the TYPE-ERROR the storage
             ;; signals rather than a silent promotion.
             ((and stringp (member et '(base-char standard-char)))
              (let ((v (make-string size :element-type 'base-char))
                    (i 0))
                (dolist (e (coerce (car (cdr ic-cell)) 'list))
                  (setf (aref v i) e)
                  (setq i (+ i 1)))
                v))
             (stringp (copy-seq (coerce (car (cdr ic-cell)) 'string)))
             (t (copy-seq (coerce (car (cdr ic-cell)) 'simple-vector)))))
      ;; BASE-CHAR (and STANDARD-CHAR) ask for the 8-bit SIMPLE-BASE-STRING;
      ;; CHARACTER and NIL get the 32-bit string (spec §1.6.3). MAKE-STRING
      ;; carries the width.
      (stringp
       (let ((width (if (member et '(base-char standard-char)) 'base-char 'character)))
         (if iel-cell
             (make-string size :initial-element (car (cdr iel-cell)) :element-type width)
             (make-string size :element-type width))))
      ;; A BIT array is a real SIMPLE-BIT-VECTOR (egcl builds these immutably,
      ;; so the whole content is supplied at construction). Default fill 0.
      (bitp
       (%bit-vector-from-bits
        (make-list size :initial-element iel)))
      ;; The plain simple-vector case — by far the most common MAKE-ARRAY —
      ;; allocates its storage in one step. It used to build a SIZE-element
      ;; list with MAKE-LIST and then `(apply #'vector …)` it, which cost a
      ;; Lisp-level CONS per element and spread the whole list as arguments:
      ;; (make-array 100000) took ~980ms, ~11x the 100k-iteration loop that
      ;; fills it (bliss-3o0r).
      (t
       (%make-simple-vector size iel)))))

;; ADJUST-ARRAY array new-dimensions &key fill-pointer initial-element — grow (or
;; shrink) a rank-1 fill-pointer/adjustable vector in place. cl-ppcre grows its
;; adjustable char collectors this way (bliss-omw). Only the fill-pointer/
;; adjustable rank-1 case is supported (delegated to %adjust-array); other cases
;; are uncommon in the libraries we load.
;; A fresh array of NEW-DIMENSIONS holding ARRAY's elements at the subscripts
;; both arrays share, the rest INITIAL-ELEMENT (or INITIAL-CONTENTS when given):
;; ADJUST-ARRAY's result for an array that is not actually adjustable, which
;; ANSI permits to be a new array (CLHS ADJUST-ARRAY; ansi adjust-array.*).
(defun %adjust-array-copy (array dims keys)
  (let* ((dims (if (listp dims) dims (list dims)))
         (iel-cell (member :initial-element keys))
         (ic-cell (member :initial-contents keys))
         (et-cell (member :element-type keys))
         (fp-cell (member :fill-pointer keys))
         (element-type (if et-cell (cadr et-cell) (array-element-type array)))
         (new (if ic-cell
                  (make-array dims :element-type element-type
                                   :initial-contents (cadr ic-cell))
                  (if iel-cell
                      (make-array dims :element-type element-type
                                       :initial-element (cadr iel-cell))
                      (make-array dims :element-type element-type))))
         (old-dims (array-dimensions array)))
    (unless (or ic-cell (/= (length dims) (length old-dims)))
      (dotimes (i (array-total-size new))
        ;; Decompose the row-major index into subscripts, innermost last.
        (let ((subs nil) (rest i) (inside t))
          (dolist (d (reverse dims))
            (push (mod rest d) subs)
            (setq rest (floor rest d)))
          (do ((ss subs (cdr ss)) (od old-dims (cdr od)))
              ((or (null ss) (not inside)))
            (when (>= (car ss) (car od)) (setq inside nil)))
          (when inside
            (setf (row-major-aref new i) (apply #'aref array subs))))))
    (when (and fp-cell (cadr fp-cell) (= (length dims) 1))
      (let ((fp (cadr fp-cell)))
        (setq new (make-array dims :element-type element-type
                                   :fill-pointer (if (eq fp t) (car dims) fp)
                                   :initial-contents (coerce new 'list)))))
    new))

(defun adjust-array (array new-dimensions &rest keys)
  (unless (and (adjustable-array-p array) (= (array-rank array) 1))
    (return-from adjust-array (%adjust-array-copy array new-dimensions keys)))
  (let* ((size (if (consp new-dimensions) (car new-dimensions) new-dimensions))
         (fp-cell (member :fill-pointer keys))
         (iel-cell (member :initial-element keys))
         (fp (and fp-cell (car (cdr fp-cell))))
         (iel (if iel-cell (car (cdr iel-cell))
                  (if (eq (array-element-type array) 'double-float) 0d0 nil)))
         ;; No :fill-pointer supplied. Per ANSI the array keeps its fill
         ;; pointer if it has one; a plain adjustable array has none, so its
         ;; length simply tracks the new SIZE. ARRAY-HAS-FILL-POINTER-P now
         ;; distinguishes the two exactly (bliss-0x9y), replacing the old
         ;; fp<total-size heuristic (bliss-6wng).
         (fpn (cond ((eq fp t) size)
                    ((integerp fp) fp)
                    ((array-has-fill-pointer-p array) (fill-pointer array))
                    (t size))))
    (%adjust-array array size fpn iel)))

;; Bit-vector boolean operations (CLHS 14.2.1). Each takes two simple-bit-vectors
;; of the same length and returns a fresh SIMPLE-BIT-VECTOR of the elementwise
;; result; BIT-NOT is unary. The optional OPT-RESULT arg (NIL/omitted, T, or a
;; bit-vector) selects the destination in ANSI, but egcl bit-vectors are built
;; immutable (bliss-27f5), so these always allocate a fresh result — value-correct
;; for the ubiquitous 2-arg use; a caller relying on in-place identity is not
;; served until bit-vectors become mutable. Each op is a bit of a boolean of two
;; single-bit inputs, so plain LOGAND/LOGIOR/LOGXOR and (- 1 bit) compute them.
;; The two operands may be bit arrays of any rank with the same dimensions
;; (CLHS 15.2: bit-and etc. take bit-arrays); the result goes into OPT-RESULT
;; when that is a bit array, into the first operand for T, else into a fresh
;; array of the same dimensions (ansi arrays bit-and.* on rank 0 and rank 2).
(defun %bit-op-2 (fn bv1 bv2 &optional opt-result)
  (let ((result (cond ((eq opt-result t) bv1)
                      ((arrayp opt-result) opt-result)
                      (t (make-array (array-dimensions bv1) :element-type 'bit)))))
    (dotimes (i (array-total-size bv1))
      (setf (row-major-aref result i)
            (funcall fn (row-major-aref bv1 i) (row-major-aref bv2 i))))
    result))
(defun bit-and (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logand a b)) bv1 bv2 opt-result))
(defun bit-ior (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logior a b)) bv1 bv2 opt-result))
(defun bit-xor (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logxor a b)) bv1 bv2 opt-result))
(defun bit-eqv (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (- 1 (logxor a b))) bv1 bv2 opt-result))
(defun bit-nand (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (- 1 (logand a b))) bv1 bv2 opt-result))
(defun bit-nor (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (- 1 (logior a b))) bv1 bv2 opt-result))
(defun bit-andc1 (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logand (- 1 a) b)) bv1 bv2 opt-result))
(defun bit-andc2 (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logand a (- 1 b))) bv1 bv2 opt-result))
(defun bit-orc1 (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logior (- 1 a) b)) bv1 bv2 opt-result))
(defun bit-orc2 (bv1 bv2 &optional opt-result)
  (%bit-op-2 (lambda (a b) (logior a (- 1 b))) bv1 bv2 opt-result))
(defun bit-not (bv &optional opt-result)
  (let ((result (cond ((eq opt-result t) bv)
                      ((arrayp opt-result) opt-result)
                      (t (make-array (array-dimensions bv) :element-type 'bit)))))
    (dotimes (i (array-total-size bv))
      (setf (row-major-aref result i) (- 1 (row-major-aref bv i))))
    result))

;; MAKE-SEQUENCE result-type size &key initial-element — a fresh sequence of the
;; given type. Dispatches on the type's head: list types build a list, string
;; types (or (vector character …)) a string, everything else a general vector.
;; The LENGTH a compound sequence type specifier declares, or NIL when it
;; leaves it unspecified. The length sits in a different position per head:
;; (VECTOR [element-type [size]]) puts it third, while (STRING [size]) and the
;; other one-parameter vector heads put it second. `*` means unspecified.
(defun %sequence-type-length (result-type)
  (and (consp result-type)
       (let* ((head (car result-type))
              (tail (cdr result-type))
              (slot (cond ((member head '(vector array simple-array))
                           (car (cdr tail)))
                          ((member head '(simple-vector string simple-string
                                          base-string simple-base-string
                                          bit-vector simple-bit-vector))
                           (car tail))
                          (t nil))))
         (and (integerp slot) slot))))

;; Signal unless RESULT-TYPE names a sequence type that can hold SIZE elements.
;; Shared by MAKE-SEQUENCE and MERGE, which have the same obligation.
;;
;; The DATUM is the SIZE, not the result type. ansi-test's SIGNALS-ERROR
;; additionally asserts that (typep datum expected-type) is FALSE — a TYPE-ERROR
;; whose datum satisfies its own expected-type is a bogus error — and a compound
;; specifier like (VECTOR * 4) is a LIST, hence itself a SEQUENCE, so reporting
;; it against SEQUENCE was exactly that bogus pairing (ansi MAKE-SEQUENCE.ERROR.3-6).
;; The size against the length the specifier demands is both truthful and
;; checkable.
(defun %check-sequence-result-type (rt head declared size original)
  (declare (ignore rt))
  ;; A DEFTYPE alias names a sequence type just as well as a built-in name
  ;; does (CLHS make-sequence takes any type specifier). Expand it before
  ;; matching, or e.g. babel's UNICODE-STRING is rejected as not a sequence.
  (let ((expanded (%expand-type-spec head)))
    (setq head (if (consp expanded) (car expanded) expanded)))
  (unless (member head '(list cons null sequence vector simple-vector
                         array simple-array string simple-string
                         base-string simple-base-string
                         bit-vector simple-bit-vector))
    (error 'type-error :datum original :expected-type 'sequence))
  (when (and declared (/= declared size))
    (error 'type-error :datum size :expected-type (list 'eql declared)))
  (when (and (eq head 'null) (/= size 0))
    (error 'type-error :datum size :expected-type '(eql 0)))
  (when (and (eq head 'cons) (= size 0))
    (error 'type-error :datum size :expected-type '(integer 1)))
  t)

;; MAKE-SEQUENCE's result-type must name a SEQUENCE type, and a length it
;; declares must agree with SIZE; otherwise the consequences are a TYPE-ERROR
;; (CLHS make-sequence). None of this was checked, so (make-sequence 'symbol 10)
;; happily built a vector and (make-sequence '(string 4) 3) built a 3-character
;; string (ansi MAKE-SEQUENCE.ERROR.1-16).
(defun make-sequence (result-type size &key (initial-element nil iel-cell)
                                            allow-other-keys)
  (declare (ignore allow-other-keys))
  (%check-nonneg-index size)
  (let* ((rt0 (if (and result-type (not (consp result-type)) (not (symbolp result-type)))
                  ;; A CLASS object designates its name (MAKE-SEQUENCE.57/58).
                  (or (ignore-errors (class-name result-type)) result-type)
                  result-type))
         ;; Expand a DEFTYPE alias BEFORE deriving HEAD and ELT, so the element
         ;; type it carries is seen: babel's
         ;; (deftype unicode-string () '(simple-array character (*)))
         ;; must build a STRING, not a general vector.
         (rt (%expand-type-spec rt0))
         (head (if (consp rt) (car rt) rt))
         (elt (if (consp rt) (car (cdr rt)) nil))
         (iel initial-element)
         (declared (%sequence-type-length rt))
         (stringp (or (member head '(string simple-string base-string simple-base-string))
                      (and (member head '(vector array simple-array simple-vector))
                           (member elt '(character base-char standard-char)))))
         (bitp (or (member head '(bit-vector simple-bit-vector))
                   (and (member head '(vector array simple-array simple-vector))
                        (eq elt 'bit)))))
    (%check-sequence-result-type rt head declared size result-type)
    (cond
      ((member head '(list cons null))
       (make-list size :initial-element iel))
      (stringp
       ;; BASE-STRING, or a vector of BASE-CHAR, is the 8-bit string.
       (let ((width (if (or (member head '(base-string simple-base-string))
                            (member elt '(base-char standard-char)))
                        'base-char
                        'character)))
         (if iel-cell
             (make-string size :initial-element iel :element-type width)
             (make-string size :element-type width))))
      (bitp
       (make-array size :element-type 'bit :initial-element (if iel-cell iel 0)))
      ((and (member head '(vector array simple-array))
            (member (if (consp elt) (car elt) elt) '(double-float long-float)))
       (make-array size :element-type elt :initial-element (if iel-cell iel 0d0)))
      (t
       (if iel-cell (make-array size :initial-element iel) (make-array size))))))
;; STRING-EQUAL is case-insensitive STRING=; forward the ANSI bounding keywords
;; (:start1/:end1/:start2/:end2) after case-folding (which preserves indices).
(defun string-equal (a b &rest keys)
  (apply #'string= (string-downcase (string a)) (string-downcase (string b)) keys))

;; SUBST new old tree &key key test test-not — substitute NEW for every subtree
;; of TREE that satisfies the test against OLD. Full CL lambda list: cl-ppcre's
;; INSERT-ADVANCE-FN calls (subst ... :test #'equalp) (bliss-9q4).
(defun subst (new old tree &key key test test-not)
  (let ((key (or key (function identity))))
    (labels ((match (x)
               (let ((k (funcall key x)))
                 (cond (test-not (not (funcall test-not old k)))
                       (test (funcall test old k))
                       (t (eql old k)))))
             (rec (tree)
               (cond ((match tree) new)
                     ((consp tree) (cons (rec (car tree)) (rec (cdr tree))))
                     (t tree))))
      (rec tree))))

;;; ===========================================================================
;;; Conformance layer: sequence/list/string/number/control functions that were
;;; missing from the bootstrap prelude.  Everything here is pure Lisp on top of
;;; the existing primitives (ELT, LENGTH, COERCE, FLOOR, REM, EXPT, ...).
;;;
;;; HISTORICAL: several definitions below route around builtin MOD (wrong sign
;;; for negative arguments) and builtin MEMBER's :KEY (ignored).  BOTH BUILTINS
;;; ARE NOW CORRECT -- measured: (mod -7 3) => 2 and (member 2 '((2)) :key #'car)
;;; => ((2)).  The workarounds are kept only because they work and removing them
;;; is a change to load-bearing bootstrap code; bliss-p3jme tracks unwinding
;;; them.  Do not add NEW workarounds for either builtin.
;;; ===========================================================================

;;; --- shared helpers --------------------------------------------------------

;; Return a fresh sequence of the same type as ORIG holding the elements of the
;; list LIST.  Used to keep REMOVE/SUBSTITUTE/FILL/... type-preserving.
;; Rebuild LIST in ORIG's representation. A BIT VECTOR must come back as one:
;; without this clause (substitute 1 0 #*0101010101 ...) answered the general
;; vector #(0 1 1 1 ...) instead of #*0101011111 (ansi SUBSTITUTE-BIT-VECTOR.24/25
;; and the NSUBSTITUTE pairs, which are built on this).
(defun %coerce-like (list orig)
  (cond ((stringp orig)
         ;; Same element type as ORIG, width included: an 8-bit base string
         ;; comes back as one (ansi REMOVE-STRING.4, DELETE-IF-STRING.1,
         ;; REMOVE-DUPLICATES.STRING.1 compare ARRAY-ELEMENT-TYPE of the
         ;; result with the input's).
         (coerce list (if (eq (array-element-type orig) 'base-char)
                          'simple-base-string
                          'string)))
        ((bit-vector-p orig) (coerce list 'bit-vector))
        ((listp orig) list)
        (t (coerce list 'vector))))

;; Does ITEM match ELT under TESTFN, with KEY applied to ELT and NEG inverting?
(defun %seq-match (item elt key testfn neg)
  (let ((r (funcall testfn item (if key (funcall key elt) elt))))
    (if neg (not r) r)))

;; Indices (ascending) in ITEMS where PREDFN holds, restricted to [START,STOP);
;; when COUNT is supplied keep only COUNT of them, trailing ones if FROM-END.
;;
;; FROM-END changes the ORDER PREDFN IS APPLIED IN, not just which matches are
;; kept. ansi tests this with a stateful :test — one that decrements a counter on
;; every call, so which element matches depends on the order it is called in
;; (SUBSTITUTE-*.21/.23, and the REMOVE/POSITION families built on this). Walking
;; forward and then taking the last COUNT gave the right SHAPE and the wrong
;; ELEMENT.
;;
;; The elements are indexed through a vector rather than NTH so a backward walk
;; does not re-traverse the list per step (the %match-at lesson, bliss-3o0r).
(defun %match-positions (predfn items start end count from-end)
  (let* ((v (coerce items 'vector))
         (len (length v))
         (stop (or end len))
         (positions nil))
    (if from-end
        ;; Pushing while descending leaves POSITIONS ascending.
        (do ((i (1- stop) (1- i))) ((< i start))
          (when (funcall predfn (aref v i)) (push i positions)))
        (do ((i start (1+ i))) ((>= i stop))
          (when (funcall predfn (aref v i)) (push i positions))))
    (unless from-end (setq positions (reverse positions)))
    (if count
        ;; A NEGATIVE :count means no matches at all (CLHS: as if count were 0).
        ;; Unclamped it reached LAST and SUBSEQ with a negative argument.
        (let ((n (max count 0)))
          (if from-end
              (last positions n)
              (subseq positions 0 (min n (length positions)))))
        positions)))

;; Membership test honouring KEY/TESTFN/NEG (KEY is applied to each element of
;; LIST; ITEM is assumed already keyed by the caller).
(defun %seq-find (item list key testfn neg)
  (dolist (x list nil)
    (when (%seq-match item x key testfn neg) (return t))))

;;; --- list constructors / accessors -----------------------------------------

;; RPLACA / RPLACD (CLHS 14.2): destructively set the car/cdr of a cons and
;; return THE CONS (unlike (setf (car ..)) which returns the stored value).
(defun rplaca (cons x)
  (unless (consp cons)
    (error 'type-error :datum cons :expected-type 'cons))
  (setf (car cons) x)
  cons)
(defun rplacd (cons x)
  (unless (consp cons)
    (error 'type-error :datum cons :expected-type 'cons))
  (setf (cdr cons) x)
  cons)

(defun copy-list (list)
  ;; Iterative (tail-pointer) copy so a long list does not recurse one stack
  ;; frame per element — deep lists (flexi-streams code-page tables, bliss-2r5)
  ;; overflowed the default EgclStack. A dotted tail is preserved.
  (if (consp list)
      (let* ((head (cons (car list) nil))
             (tail head))
        (do ((rest (cdr list) (cdr rest)))
            ((not (consp rest))
             (unless (null rest) (setf (cdr tail) rest))
             head)
          (let ((new (cons (car rest) nil)))
            (setf (cdr tail) new)
            (setq tail new))))
      list))

(defun copy-tree (tree)
  ;; Iterate down the cdr spine (the deep direction for list-shaped data such as
  ;; the flexi-streams tables, bliss-2r5); car recursion handles nested subtrees.
  (if (consp tree)
      (let* ((head (cons (copy-tree (car tree)) nil))
             (tail head))
        (do ((rest (cdr tree) (cdr rest)))
            ((not (consp rest))
             (unless (null rest) (setf (cdr tail) (copy-tree rest)))
             head)
          (let ((new (cons (copy-tree (car rest)) nil)))
            (setf (cdr tail) new)
            (setq tail new))))
      tree))

(defun copy-seq (seq) (subseq seq 0))

(defun list* (&rest args)
  ;; Iterative build so a very long argument list does not recurse (bliss-2r5).
  (if (null (cdr args))
      (car args)
      (let* ((head (cons (car args) nil))
             (tail head))
        (do ((rest (cdr args) (cdr rest)))
            ((null (cdr rest))
             (setf (cdr tail) (car rest))
             head)
          (let ((new (cons (car rest) nil)))
            (setf (cdr tail) new)
            (setq tail new))))))

(defun nbutlast (list &optional (n 1))
  ;; ANSI: destructive butlast — snip the CDR of the (len-n)th cons and return
  ;; the (possibly modified) original LIST, or NIL when nothing is kept.
  (unless (and (integerp n) (>= n 0))
    (error 'type-error :datum n :expected-type '(integer 0)))
  (unless (listp list)
    (error 'type-error :datum list :expected-type 'list))
  (let ((len 0))
    (do ((l list (cdr l))) ((not (consp l))) (setq len (+ len 1)))
    (let ((keep (- len n)))
      (if (<= keep 0)
          nil
          (progn
            (do ((l list (cdr l))
                 (i 1 (+ i 1)))
                ((>= i keep) (setf (cdr l) nil)))
            list)))))

(defun copy-alist (alist)
  ;; ANSI: copy the list structure of ALIST and, for each element that is a
  ;; cons, a fresh (car . cdr) cons; non-cons elements are shared. A dotted
  ;; spine (improper alist) is a TYPE-ERROR.
  (unless (listp alist)
    (error 'type-error :datum alist :expected-type 'list))
  (if (consp alist)
      (let* ((p (car alist))
             (head (cons (if (consp p) (cons (car p) (cdr p)) p) nil))
             (tail head))
        (do ((rest (cdr alist) (cdr rest)))
            ((not (consp rest))
             ;; The offending object is the improper tail REST (a non-list),
             ;; not the whole ALIST (which IS a list) — ANSI's SIGNALS-ERROR
             ;; rejects a type-error whose datum satisfies its expected-type
             ;; (copy-alist.error.3).
             (unless (null rest)
               (error 'type-error :datum rest :expected-type 'list))
             head)
          (let* ((q (car rest))
                 (new (cons (if (consp q) (cons (car q) (cdr q)) q) nil)))
            (setf (cdr tail) new)
            (setq tail new))))
      nil))

(defun ldiff (list object)
  ;; ANSI: fresh list of the part of LIST before the tail EQL to OBJECT. If no
  ;; tail matches, a full fresh copy is returned (preserving a dotted tail). A
  ;; non-list LIST is a TYPE-ERROR. Built with a tail pointer rather than
  ;; NRECONC/APPEND so the dotted terminating atom is preserved.
  (unless (listp list)
    (error 'type-error :datum list :expected-type 'list))
  (if (or (null list) (eql list object))
      nil
      (let* ((head (cons (car list) nil))
             (tail head))
        (do ((rest (cdr list) (cdr rest)))
            ((or (atom rest) (eql rest object))
             (when (and (not (eql rest object)) (not (null rest)))
               (setf (cdr tail) rest))
             head)
          (let ((new (cons (car rest) nil)))
            (setf (cdr tail) new)
            (setq tail new))))))

(defun tailp (object list)
  (block nil
    (loop
      (when (eql object list) (return t))
      (if (consp list) (setq list (cdr list)) (return (eql object list))))))

(defun nreconc (list tail) (%require-proper-list list) (append (reverse list) tail))

;;; --- set operations (KEY/TEST honoured via %seq-find) -----------------------

;; UNION / INTERSECTION / SET-DIFFERENCE / SUBSETP. The default EQL case builds
;; an EQL hash set of B (or A) for O(n+m) membership, instead of the O(n*m)
;; %SEQ-FIND scan — ansi-test cl-symbol-names.lsp runs these over ~1000-symbol
;; lists (`(reduce #'union …)`, `set-difference …`), where the quadratic scan
;; (also tree-walked, since a &key lambda list bails the compiler) hung
;; (bliss-cpm9). &rest+GETF and no inner LAMBDA keep the hot path compilable; a
;; custom :test/:test-not/:key takes the O(n*m) general helper.
(defun %eql-membership-set (list key)
  (let ((seen (make-hash-table :test 'eql)))
    (dolist (x list seen)
      (setf (gethash (if key (funcall key x) x) seen) t))))

(defun %in-hash-set-p (x seen)
  (multiple-value-bind (v p) (gethash x seen) (declare (ignore v)) p))

(defun %union-general (a b key testfn neg)
  (let ((result (copy-list b)))
    (dolist (x a result)
      (unless (%seq-find (if key (funcall key x) x) b key testfn neg)
        (push x result)))))

(defun %intersection-general (a b key testfn neg)
  (let ((result nil))
    (dolist (x a (reverse result))
      (when (%seq-find (if key (funcall key x) x) b key testfn neg)
        (push x result)))))

(defun %set-difference-general (a b key testfn neg)
  (let ((result nil))
    (dolist (x a (reverse result))
      (unless (%seq-find (if key (funcall key x) x) b key testfn neg)
        (push x result)))))

;; Validate a set-operation keyword plist (:test :test-not :key
;; :allow-other-keys). These functions parse KEYS with GETF rather than a strict
;; &key lambda list, so malformed keyword arguments were silently accepted; ANSI
;; requires a catchable PROGRAM-ERROR (CLHS 3.5.1.4/3.5.1.5/3.5.1.6). Signal it
;; when the plist has odd length, a non-symbol sits in a key position, or an
;; unrecognised keyword appears — the last unless :allow-other-keys is supplied
;; with a true value (ansi-test union/intersection/subsetp/... .ERROR.3-6).
(defun %check-set-op-keys (keys)
  (let ((allow (getf keys :allow-other-keys)))
    (do ((ks keys (cddr ks)))
        ((null ks))
      (unless (consp (cdr ks))
        (error 'program-error))
      (let ((k (car ks)))
        (unless (symbolp k)
          (error 'program-error))
        (unless (or allow
                    (member k '(:test :test-not :key :allow-other-keys)))
          (error 'program-error))))))

(defun union (a b &rest keys)
  (%check-set-op-keys keys)
  (let ((test (getf keys :test)) (test-not (getf keys :test-not)) (key (getf keys :key)))
    (if (and (null test) (null test-not))
        (let ((seen (%eql-membership-set b key)) (result (copy-list b)))
          (dolist (x a result)
            (let ((kx (if key (funcall key x) x)))
              (unless (%in-hash-set-p kx seen)
                (setf (gethash kx seen) t)
                (push x result)))))
        (%union-general a b key (or test-not test #'eql) (and test-not t)))))

(defun intersection (a b &rest keys)
  (%check-set-op-keys keys)
  (let ((test (getf keys :test)) (test-not (getf keys :test-not)) (key (getf keys :key)))
    (if (and (null test) (null test-not))
        (let ((seen (%eql-membership-set b key)) (result nil))
          (dolist (x a (reverse result))
            (when (%in-hash-set-p (if key (funcall key x) x) seen)
              (push x result))))
        (%intersection-general a b key (or test-not test #'eql) (and test-not t)))))

(defun set-difference (a b &rest keys)
  (%check-set-op-keys keys)
  (let ((test (getf keys :test)) (test-not (getf keys :test-not)) (key (getf keys :key)))
    (if (and (null test) (null test-not))
        (let ((seen (%eql-membership-set b key)) (result nil))
          (dolist (x a (reverse result))
            (unless (%in-hash-set-p (if key (funcall key x) x) seen)
              (push x result))))
        (%set-difference-general a b key (or test-not test #'eql) (and test-not t)))))

;; SET-EXCLUSIVE-OR: elements in exactly one of A/B. The test is always applied
;; as (test a-element b-element) — list1 element FIRST — for BOTH directions
;; (CLHS). Computing it as (set-difference b a) applied the test with swapped
;; arguments, which is wrong for a non-symmetric :test (ansi-test
;; set-exclusive-or with :test (lambda (x y) (= x (1- y))); bliss-30be). Apply the
;; matcher directly with the fixed order instead.
(defun %sxor-matches (kx ky testfn neg)
  (let ((r (funcall testfn kx ky))) (if neg (not r) r)))
(defun set-exclusive-or (a b &rest keys)
  (%check-set-op-keys keys)
  (let* ((test (getf keys :test))
         (test-not (getf keys :test-not))
         (key (getf keys :key))
         (keyfn (or key (function identity)))
         (testfn (or test-not test (function eql)))
         (neg (and test-not t))
         (result nil))
    (dolist (x a)
      (let ((kx (funcall keyfn x)))
        (unless (some (function (lambda (y) (%sxor-matches kx (funcall keyfn y) testfn neg))) b)
          (push x result))))
    (dolist (y b)
      (let ((ky (funcall keyfn y)))
        (unless (some (function (lambda (x) (%sxor-matches (funcall keyfn x) ky testfn neg))) a)
          (push y result))))
    (nreverse result)))

(defun subsetp (a b &rest keys)
  (%check-set-op-keys keys)
  (let ((test (getf keys :test)) (test-not (getf keys :test-not)) (key (getf keys :key)))
    (if (and (null test) (null test-not))
        (let ((seen (%eql-membership-set b key)))
          (dolist (x a t)
            (unless (%in-hash-set-p (if key (funcall key x) x) seen)
              (return nil))))
        (let ((testfn (or test-not test #'eql)) (neg (and test-not t)))
          (dolist (x a t)
            (unless (%seq-find (if key (funcall key x) x) b key testfn neg)
              (return nil)))))))

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

;; PUSHNEW: like PUSH, but only prepend ITEM when ADJOIN reports it absent.
;; ITEM is evaluated first, then the PLACE subforms, then the keyword forms in
;; source order (pushnew.order.* / pushnew.12-15). ADJOIN returns the list
;; unchanged (EQ) when the item is already present, so the place keeps its
;; identity (pushnew.2/3). The setf-expansion lifts place subforms so they are
;; evaluated once.
(defmacro pushnew (item place &rest keys &environment env)
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((g (gensym)) (result (gensym)))
        `(let* ((,g ,item)
                ,@(mapcar (function list) dummies vals)
                (,result (adjoin ,g ,getter ,@keys)))
           (multiple-value-bind ,newval ,result
             ,setter)
           ,result)))))

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
  ;; Iterate the cdr spine (the deep direction for list-shaped trees) and recurse
  ;; only into cars, so comparing long flat lists does not overflow the stack
  ;; (bliss-2r5 kin). An explicit flag drives the loop rather than RETURN.
  (let ((result t) (running t))
    (do () ((not running) result)
      (cond ((and (consp a) (consp b))
             (if (%tree-equal (car a) (car b) testfn neg)
                 (setq a (cdr a) b (cdr b))
                 (setq result nil running nil)))
            ((or (consp a) (consp b))
             (setq result nil running nil))
            (t (let ((r (funcall testfn a b)))
                 (setq result (if neg (not r) r) running nil)))))))

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

;; REMF: remove the INDICATOR/value pair from the plist in PLACE; return true iff
;; a pair was removed. The PLACE subforms are evaluated once, left to right,
;; BEFORE INDICATOR, and the place value is read only after that (CLHS 5.1.3 /
;; remf.order.*). Using the setf-expansion getter guarantees the place is read
;; after the indicator subform runs — remf.order.3 relies on that.
(defmacro remf (place indicator &environment env)
  (let ((place (egcl::%macroexpand-place place env)))
    (multiple-value-bind (dummies vals newval setter getter)
        (get-setf-expansion place env)
      (let ((ind (gensym)) (r (gensym)) (f (gensym)))
        `(let* (,@(mapcar (function list) dummies vals)
                (,ind ,indicator))
           (multiple-value-bind (,r ,f) (%remf ,getter ,ind)
             (multiple-value-bind ,newval ,r
               ,setter)
             ,f))))))

;;; --- list mapping variants --------------------------------------------------

;; %require-proper-lists — ANSI: the map* functions require proper lists.
;; A cursor that is a non-NIL atom (a dotted list, or a non-list argument)
;; is a TYPE-ERROR. Called before applying FN so the check happens even when
;; FN would accept the atom (e.g. IDENTITY).
(defun %require-proper-lists (lists)
  (dolist (l lists)
    (unless (listp l)
      (error 'type-error :datum l :expected-type 'list))))

;; MAPLIST/MAPL require at least one list (CLHS PROGRAM-ERROR): with zero lists
;; `(some #'null lists)` is NIL so the loop never terminates. Each cursor must
;; also be a proper list (%require-proper-lists) — bliss-x7aa/bliss-30be.
(defun maplist (fn &rest lists)
  (when (null lists) (error 'program-error))
  (let ((result nil))
    (block nil
      (loop
        (when (some (function null) lists) (return))
        (%require-proper-lists lists)
        (push (apply fn lists) result)
        (setq lists (mapcar (function cdr) lists))))
    (reverse result)))

(defun mapl (fn &rest lists)
  (when (null lists) (error 'program-error))
  (let ((first (car lists)))
    (block nil
      (loop
        (when (some (function null) lists) (return))
        (%require-proper-lists lists)
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

;; NSUBST may reuse structure; delegating to SUBST is conforming — but it must
;; forward :key/:test/:test-not (they were dropped, so nsubst ignored the test;
;; ansi-test cons/nsubst.lsp).
(defun nsubst (new old tree &rest keys)
  (apply (function subst) new old tree keys))
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

;; The N- variants are DESTRUCTIVE: they modify SEQ and return it. All three
;; simply delegated to the non-destructive version, so they answered a fresh
;; sequence and left the original untouched — (let ((x (copy-seq '(a b a c))))
;; (nsubstitute 'b 'a x) x) stayed (A B A C) instead of becoming (B B B C)
;; (ansi NSUBSTITUTE-LIST.2, NSUBSTITUTE-VECTOR.3 and the -IF / -IF-NOT pairs).
;;
;; Computing the answer with SUBSTITUTE and writing it back keeps one
;; implementation of the keyword semantics (:count, :from-end, :start/:end,
;; :key, :test/:test-not) rather than a second copy that can drift.
(defun %nsubstitute-into (seq result)
  (let ((n (min (length seq) (length result))))
    (dotimes (i n) (setf (elt seq i) (elt result i))))
  seq)
(defun nsubstitute (new old seq &rest keys)
  (%nsubstitute-into seq (apply (function substitute) new old seq keys)))
(defun nsubstitute-if (new pred seq &rest keys)
  (%nsubstitute-into seq (apply (function substitute-if) new pred seq keys)))
(defun nsubstitute-if-not (new pred seq &rest keys)
  (%nsubstitute-into seq (apply (function substitute-if-not) new pred seq keys)))

;;; --- REMOVE-DUPLICATES (spec-faithful: default keeps last occurrence) ------

;; General O(n^2) REMOVE-DUPLICATES honouring :key/:test/:test-not/:from-end/
;; :start/:end via %SEQ-MATCH. Compiled (loop body, no forward refs). Reached only
;; when the fast path below cannot serve the call.
(defun %remove-duplicates-general (seq key test test-not from-end start end)
  (let* ((items (coerce seq 'list)) (len (length items)) (stop (or end len))
         (testfn (or test-not test (function eql))) (neg (if test-not t nil))
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

;; REMOVE-DUPLICATES (CLHS 17.3). Default (FROM-END NIL) discards the EARLIER of
;; each matching pair, so the LAST occurrence is retained; FROM-END T keeps the
;; first. For the common case — default EQL test, whole sequence — dedup in O(n)
;; through an EQL hash table (ansi-test universe.lsp's `(remove-duplicates
;; (append …))` runs over hundreds of elements; the O(n^2) scan hung on it,
;; bliss-qxfg). Uses &rest+GETF, not &key, and avoids any inner LAMBDA, so the
;; hot function bytecode-compiles instead of falling back to the tree-walker.
;; A custom :test/:test-not or a :start/:end window takes the general path above.
;; A real &key lambda list, not `&rest keys` + GETF. The binder already rejects
;; an odd-length keyword list, a non-keyword in a keyword position, and an
;; unrecognized keyword (unless :allow-other-keys) with a PROGRAM-ERROR, exactly
;; as CLHS requires — GETF silently accepted all three, so (remove-duplicates
;; nil :start), (remove-duplicates nil 'bad t) and (remove-duplicates nil 1 2)
;; returned NIL instead of signalling (ansi REMOVE-DUPLICATES.ERROR.2/4/5/6 and
;; the DELETE-DUPLICATES ones).
;; Which HASH-TABLE test, if any, is equivalent to this :test argument?
;;
;; EQ, EQL, EQUAL and EQUALP are exactly the four standard hash-table tests, so
;; REMOVE-DUPLICATES with one of them can use the O(n) hash path below instead
;; of %REMOVE-DUPLICATES-GENERAL's O(n^2) pairwise scan. Measured on 873 short
;; sublists: 5 ms hashed against 1404 ms scanned with :test #'equal -- 280x, and
;; it is algorithmic, not funcall overhead (bliss-7lqe).
;;
;; Substituting a hash lookup for pairwise comparison is only sound because
;; these four are genuine EQUIVALENCE RELATIONS, so "matches something already
;; seen" and "matches some earlier element" pick out the same duplicates. An
;; arbitrary :test is not necessarily symmetric or transitive, which is why
;; anything else still takes the general path.
;;
;; Both a function object and a symbol designator are accepted; #'<builtin> is
;; EQ-stable (bliss-hb0q), which is what makes the EQ tests here work at all.
(defun %hash-test-for (test)
  (cond ((null test) 'eql)
        ((symbolp test) (and (member test '(eq eql equal equalp)) test))
        ((eq test #'eq) 'eq)
        ((eq test #'eql) 'eql)
        ((eq test #'equal) 'equal)
        ((eq test #'equalp) 'equalp)
        (t nil)))

(defun remove-duplicates (seq &key test test-not key from-end (start 0) end
                                   allow-other-keys)
  (declare (ignore allow-other-keys))
  (let ((hash-test (and (null test-not) (eql start 0) (null end)
                        (%hash-test-for test))))
    (if hash-test
        (let ((items (coerce seq 'list))
              (seen (make-hash-table :test hash-test))
              (out nil))
          (dolist (x (if from-end items (reverse items)))
            (let ((k (if key (funcall key x) x)))
              (multiple-value-bind (v present) (gethash k seen)
                (declare (ignore v))
                (unless present
                  (setf (gethash k seen) t)
                  (push x out)))))
          (%coerce-like (if from-end (reverse out) out) seq))
        (%remove-duplicates-general seq key test test-not from-end start end))))

(defun delete-duplicates (seq &key test test-not key from-end (start 0) end
                                   allow-other-keys)
  (declare (ignore allow-other-keys))
  (remove-duplicates seq :test test :test-not test-not :key key
                         :from-end from-end :start start :end end))

;;; --- FILL / REPLACE / SEARCH / MISMATCH / MERGE ----------------------------

;; FILL and REPLACE are DESTRUCTIVE: they mutate SEQ/SEQ1 in place (via
;; SETF ELT, which now works on strings, vectors and lists) and return it.
;; Callers such as UIOP's REDUCE/STRCAT rely on the in-place mutation.
;; :START and :END are bounding INDEX DESIGNATORS, so a negative one — or one
;; past the sequence, or a start after the end — is a TYPE-ERROR, not something
;; to silently clamp. (fill a 'x :end -1) quietly did nothing (ansi
;; ARRAY-FILL-9 and the FIXNUM / UNSIGNED-BYTE8 variants).
(defun %check-bounding-indices (seq start end)
  (let ((len (length seq)))
    (unless (and (integerp start) (<= 0 start len))
      (error 'type-error :datum start :expected-type (list 'integer 0 len)))
    (when end
      (unless (and (integerp end) (<= 0 end len))
        (error 'type-error :datum end :expected-type (list 'integer 0 len)))
      (unless (<= start end)
        (error 'type-error :datum start :expected-type (list 'integer 0 end))))
    t))

(defun fill (seq item &key (start 0) end)
  (%check-bounding-indices seq start end)
  (let ((stop (or end (length seq))) (i start))
    ;; Simple vectors and simple strings are filled natively.
    (unless (egcl::%vector-fill seq item start stop)
      (loop while (< i stop) do (setf (elt seq i) item) (incf i))))
  seq)

;; When SEQ1 and SEQ2 are the same object with OVERLAPPING ranges, the result
;; must be as if the source were copied first (CLHS replace). Writing straight
;; through clobbered the source as it went:
;;
;;   (replace x x :start1 1 :end1 4 :start2 0 :end2 3)  on (A B C D E F)
;;     =>  (A A A A E F)      want (A A B C E F)
;;
;; Snapshotting the source range is the whole fix, and it costs only the range
;; actually copied (ansi REPLACE-LIST.20, REPLACE-VECTOR/STRING/BIT-VECTOR.21).
;; Simple vectors and simple strings with valid bounds are copied natively,
;; overlap-safe, by %VECTOR-REPLACE; it returns NIL for any other pair.
(defun replace (seq1 seq2 &key (start1 0) end1 (start2 0) end2)
  (let ((e1 (or end1 (length seq1))) (e2 (or end2 (length seq2))))
    (when (and (vectorp seq1) (vectorp seq2)
               (integerp start1) (integerp e1) (integerp start2) (integerp e2)
               (<= 0 start1 e1 (length seq1)) (<= 0 start2 e2 (length seq2))
               (egcl::%vector-replace seq1 seq2 start1 start2
                                      (min (- e1 start1) (- e2 start2))))
      (return-from replace seq1)))
  (let* ((e1 (or end1 (length seq1))) (e2 (or end2 (length seq2)))
         (n (min (- e1 start1) (- e2 start2)))
         (src (make-array n))
         (k 0))
    (loop while (< k n) do
      (setf (aref src k) (elt seq2 (+ start2 k)))
      (incf k))
    (setq k 0)
    (loop while (< k n) do
      (setf (elt seq1 (+ start1 k)) (aref src k))
      (incf k)))
  seq1)

;; SEARCH advances the candidate tail once per position; matching must not
;; restart traversal from the sequence's head.
(defun %match-at (pat tail key testfn neg)
  (let ((ok t))
    (block nil
      (dolist (p pat ok)
        (when (null tail) (setq ok nil) (return))
        (unless (%seq-match (if key (funcall key p) p) (car tail) key testfn neg)
          (setq ok nil) (return))
        (setq tail (cdr tail))))))

(defun %match-vector-at (pat sequence start key testfn neg)
  (let ((position start))
    (dolist (p pat t)
      (unless (%seq-match (if key (funcall key p) p)
                          (aref sequence position) key testfn neg)
        (return nil))
      (incf position))))

(defun search (seq1 seq2 &key key (test (function eql)) test-not
                              (start1 0) end1 (start2 0) end2 from-end)
  (unless (or (listp seq2) (vectorp seq2))
    (error 'type-error :datum seq2 :expected-type 'sequence))
  ;; With no key and the EQL test (or CHAR= or EQUAL, which agree with EQL on
  ;; characters), simple vectors and simple strings with valid bounds are
  ;; scanned natively by %VECTOR-SEARCH; it returns T for any other pair.
  (when (and (vectorp seq1) (vectorp seq2) (null key) (null test-not)
             (or (eq test #'eql) (eq test 'eql)
                 (and (stringp seq1) (stringp seq2)
                      (member test (list #'char= 'char= #'equal 'equal)))))
    (let ((e1 (or end1 (length seq1))) (e2 (or end2 (length seq2))))
      (when (and (integerp start1) (integerp e1) (integerp start2) (integerp e2)
                 (<= 0 start1 e1 (length seq1)) (<= 0 start2 e2 (length seq2)))
        (let ((found (egcl::%vector-search seq1 seq2 start1 e1 start2 e2 from-end)))
          (unless (eq found t)
            (return-from search found))))))
  (let* ((l1 (coerce seq1 'list))
         (e1 (or end1 (length l1))) (e2 (or end2 (length seq2)))
         (pat (subseq l1 start1 e1)) (plen (length pat))
         (testfn (or test-not test)) (neg (if test-not t nil))
         (match nil))
    (if (= plen 0)
        (if from-end e2 start2)
        (if (listp seq2)
            (let ((tail (nthcdr start2 seq2)))
              (loop for i from start2 to (- e2 plen) do
                (when (%match-at pat tail key testfn neg)
                  (if from-end
                      (setq match i)
                      (return-from search i)))
                (setq tail (cdr tail)))
              match)
            (progn
              (loop for i from start2 to (- e2 plen) do
                (when (%match-vector-at pat seq2 i key testfn neg)
                  (if from-end
                      (setq match i)
                      (return-from search i))))
              match)))))

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

;; MERGE must reject a result type that cannot hold the merged elements — most
;; visibly (merge 'null (list 1 2 3) (list 4 5 6) #'<), which quietly answered
;; the six-element list instead of signalling (ansi MERGE.ERROR.1/6). Same
;; obligation and same checker as MAKE-SEQUENCE.
(defun merge (result-type seq1 seq2 predicate &key key)
  (let* ((l1 (coerce seq1 'list)) (l2 (coerce seq2 'list)) (res nil)
         (total (+ (length l1) (length l2)))
         ;; Same DEFTYPE expansion MAKE-SEQUENCE does, for the same reason.
         (rt (%expand-type-spec result-type))
         (head (if (consp rt) (car rt) rt)))
    (%check-sequence-result-type rt head
                                 (%sequence-type-length rt)
                                 total result-type)
    (block nil
      (loop
        (cond ((null l1) (setq res (append (reverse res) l2)) (return))
              ((null l2) (setq res (append (reverse res) l1)) (return))
              ((funcall predicate
                        (if key (funcall key (car l2)) (car l2))
                        (if key (funcall key (car l1)) (car l1)))
               (push (car l2) res) (setq l2 (cdr l2)))
              (t (push (car l1) res) (setq l1 (cdr l1))))))
    ;; RT, not RESULT-TYPE: the expanded specifier is what carries the
    ;; representation, so a DEFTYPE alias builds the sequence it names.
    (coerce res rt)))

;;; --- character predicates and naming ---------------------------------------

(defun char-int (c) (char-code c))
(defun both-case-p (c) (or (upper-case-p c) (lower-case-p c)))
(defun standard-char-p (c)
  (let ((code (char-code c)))
    (or (= code 10) (and (>= code 32) (< code 127)))))
(defun graphic-char-p (c)
  (let ((code (char-code c)))
    (or (= code 32) (and (> code 32) (< code 127)) (>= code 160))))

;; The case-insensitive family is likewise a Rust builtin (bliss-7oa5). These
;; consed a FRESH CLOSURE per call and drove %char-chain recursively, which is
;; why CHAR-EQUAL measured 54us -- 204x EQ, the worst builtin on the character
;; path.

(defun digit-char (weight &optional (radix 10))
  (if (and (integerp weight) (>= weight 0) (< weight radix) (< weight 36))
      (if (< weight 10)
          (code-char (+ 48 weight))
          (code-char (+ 55 weight)))
      nil))

(defun char-name (c)
  ;; The table lives in the reader (CONTROL_CHAR_NAMES), shared with the
  ;; printer, so every `#\` the printer writes reads back.
  (egcl-internal::%char-name c))

(defun name-char (name)
  ;; STRING does the designator coercion (and its type checking).
  (egcl-internal::%name-char (string name)))

;;; --- string builders and STRING-CAPITALIZE / N-string ops ------------------

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

;; The N-string operators are DESTRUCTIVE: they mutate STRING in place (via
;; (setf char)) over [start,end) and return the *same* string object, so
;; (eq s (nstring-upcase s)) holds (ansi-test nstring-*.1-.7). They accept the
;; ANSI &key (start 0) end bounding arguments; a bad/odd/unknown keyword or a
;; missing required argument is a PROGRAM-ERROR via ordinary &key processing.
(defun nstring-upcase (s &key (start 0) end)
  (let ((stop (or end (length s))))
    (do ((i start (+ i 1))) ((>= i stop) s)
      (setf (char s i) (char-upcase (char s i))))))
(defun nstring-downcase (s &key (start 0) end)
  (let ((stop (or end (length s))))
    (do ((i start (+ i 1))) ((>= i stop) s)
      (setf (char s i) (char-downcase (char s i))))))
(defun nstring-capitalize (s &key (start 0) end)
  (let ((stop (or end (length s))) (in-word nil))
    (do ((i start (+ i 1))) ((>= i stop) s)
      (let ((c (char s i)))
        (if (alphanumericp c)
            (progn
              (setf (char s i) (if in-word (char-downcase c) (char-upcase c)))
              (setq in-word t))
            (setq in-word nil))))))

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

;;; --- integer bit operations (non-negative) ---------------------------------

(defun floatp (x) (typep x 'float))
(defun integerp (x) (typep x 'integer))
(defun rationalp (x) (or (integerp x) (typep x 'ratio)))
(defun realp (x) (or (rationalp x) (floatp x)))
(defun complexp (x) (typep x 'complex))
(defun characterp (x) (typep x 'character))
;; CHARACTER (CLHS): coerce a character designator (a character, a 1-char string,
;; or a symbol whose name is 1 char) to a character; anything else is a
;; TYPE-ERROR. Exactly one required argument, so a wrong count signals
;; PROGRAM-ERROR via the lambda-list binder (character.error.1/2). Delegates to
;; the existing (coerce c 'character) machinery.
(defun character (c) (coerce c 'character))
(defun functionp (x) (typep x 'function))

;; ASH is a native stdlib primitive: direct fixnum/limb shifts instead of
;; EXPT followed by multiplication or FLOOR (bliss-7jt1).

;; LOGNOT/LOGAND/LOGIOR/LOGXOR are native builtins (limb-wise two's-complement
;; kernels in cli.rs, bliss-gvkz); the old bit-at-a-time recursive Lisp kernels
;; here cost O(bits^2) bignum divisions per call and made the ansi numbers LOG*
;; family pathologically slow.

;; LOGEQV is associative, and the complement CANCELS on every second argument:
;;   (logeqv a b)     = ~(a^b)
;;   (logeqv a b c)   = ~(~(a^b) ^ c) = a^b^c
;;   (logeqv a b c d) = ~(a^b^c^d)
;; so it is LOGXOR of everything, complemented only when the argument count is
;; EVEN. Defining it as a flat (lognot (logxor ...)) was right for two
;; arguments and off by a complement for every odd count -- (logeqv 1231)
;; answered -1232 instead of 1231 (ansi LOGEQV.2-4). Identity is -1.
(defun logeqv (&rest ints)
  (cond ((null ints) -1)
        (t (let ((x (apply (function logxor) ints)))
             (if (evenp (length ints)) (lognot x) x)))))
(defun lognand (a b) (lognot (logand a b)))
(defun lognor (a b) (lognot (logior a b)))
(defun logandc1 (a b) (logand (lognot a) b))
(defun logandc2 (a b) (logand a (lognot b)))
(defun logorc1 (a b) (logior (lognot a) b))
(defun logorc2 (a b) (logior a (lognot b)))

;; BOOLE and its 16 operation constants (CLHS 12.2). The constant values are
;; implementation-defined; we use 0–15 to select the bitwise operation, reusing
;; the (bignum-capable) LOG* functions above.
(defconstant boole-clr 0)
(defconstant boole-set 1)
(defconstant boole-1 2)
(defconstant boole-2 3)
(defconstant boole-c1 4)
(defconstant boole-c2 5)
(defconstant boole-and 6)
(defconstant boole-ior 7)
(defconstant boole-xor 8)
(defconstant boole-eqv 9)
(defconstant boole-nand 10)
(defconstant boole-nor 11)
(defconstant boole-andc1 12)
(defconstant boole-andc2 13)
(defconstant boole-orc1 14)
(defconstant boole-orc2 15)

(defun boole (op integer1 integer2)
  ;; Both operands are INTEGERS whatever OP does with them. The ops that IGNORE
  ;; an argument -- BOOLE-CLR, BOOLE-SET, BOOLE-1, BOOLE-2, BOOLE-C2 -- returned
  ;; without ever touching it, so (boole boole-1 nil 1) answered NIL instead of
  ;; signalling (ansi BOOLE.ERROR.6-7 collect exactly those five names). Same
  ;; shape as ASH's count-0 branch and ISQRT's (< n 2) branch: skipping the work
  ;; skipped the validation.
  (unless (integerp integer1)
    (error 'type-error :datum integer1 :expected-type 'integer))
  (unless (integerp integer2)
    (error 'type-error :datum integer2 :expected-type 'integer))
  (cond
    ((eql op boole-clr) 0)
    ((eql op boole-set) -1)
    ((eql op boole-1) integer1)
    ((eql op boole-2) integer2)
    ((eql op boole-c1) (lognot integer1))
    ((eql op boole-c2) (lognot integer2))
    ((eql op boole-and) (logand integer1 integer2))
    ((eql op boole-ior) (logior integer1 integer2))
    ((eql op boole-xor) (logxor integer1 integer2))
    ((eql op boole-eqv) (logeqv integer1 integer2))
    ((eql op boole-nand) (lognand integer1 integer2))
    ((eql op boole-nor) (lognor integer1 integer2))
    ((eql op boole-andc1) (logandc1 integer1 integer2))
    ((eql op boole-andc2) (logandc2 integer1 integer2))
    ((eql op boole-orc1) (logorc1 integer1 integer2))
    ((eql op boole-orc2) (logorc2 integer1 integer2))
    ;; An unrecognised OP is a TYPE-ERROR, not a SIMPLE-ERROR: ansi
    ;; BOOLE.ERROR.5-7 use SIGNALS-TYPE-ERROR over values outside *BOOLE-VALS*.
    (t (error 'type-error :datum op :expected-type '(integer 0 15)))))

(defun logtest (a b) (not (zerop (logand a b))))
;; LOGBITP is a native builtin (cli.rs apply_logbitp, bliss-gvkz).

;;; Complex-number helpers built on REALPART/IMAGPART/COMPLEX (CLHS 12.2).
;; CONJUGATE negates the imaginary part; a real is its own conjugate.
(defun conjugate (n)
  (if (complexp n)
      (complex (realpart n) (- (imagpart n)))
      n))
;; PHASE is the angle of the polar representation: atan(imagpart, realpart).
;; For a real it is 0 (non-negative) or pi (negative).
(defun phase (n)
  (atan (imagpart n) (realpart n)))
;; CIS: the unit complex number e^(i*radians) = cos(r) + i*sin(r).
(defun cis (radians)
  (complex (cos radians) (sin radians)))

;; INTEGER-LENGTH and LOGCOUNT are native builtins (cli.rs
;; apply_intlen_or_logcount, bliss-gvkz).

;;; --- byte specifiers: LDB / DPB / ... --------------------------------------

(defun byte (size position) (cons size position))
(defun byte-size (bytespec) (car bytespec))
(defun byte-position (bytespec) (cdr bytespec))

;; `(ash 1 k)` rather than `(expt 2 k)` throughout, and shifts rather than
;; multiplication by a power of two. A byte specifier's size and position are
;; non-negative (CLHS 12.1.1.1.1), so these are the same value by a cheaper
;; route. Measured on one promoted loop, release build: `(expt 2 32)` costs
;; 3898 ns/iter against 357 for `(ash 1 32)` -- a 23x difference that LDB paid
;; on EVERY call, because the mask is recomputed each time. It was ~34% of
;; LDB's 10.7 us. That matters well beyond bit-twiddling: Ironclad builds
;; mod32+/rol32/mod32ash on LDB, so it sits under every hash and cipher, and
;; is what makes SHA-256 unusably slow here (bliss-omaps).
(defun ldb (bytespec integer)
  (logand (ash integer (- (byte-position bytespec)))
          (1- (ash 1 (byte-size bytespec)))))

(defun ldb-test (bytespec integer) (not (zerop (ldb bytespec integer))))

(defun mask-field (bytespec integer)
  (ash (ldb bytespec integer) (byte-position bytespec)))

(defun dpb (newbyte bytespec integer)
  (let* ((size (byte-size bytespec)) (pos (byte-position bytespec))
         (mask (1- (ash 1 size))))
    (+ (- integer (ash (ldb bytespec integer) pos))
       (ash (logand newbyte mask) pos))))

(defun deposit-field (newbyte bytespec integer)
  ;; Replace the BYTESPEC field of INTEGER with the same-position bits of
  ;; NEWBYTE (both taken in place via MASK-FIELD).
  (+ (- integer (mask-field bytespec integer))
     (mask-field bytespec newbyte)))

;; The standard `(setf ACCESSOR)` FUNCTIONS (CLHS 5.1.2.9). These make
;; #'(setf car) and friends real callable functions for portable code. EGCL's
;; GET-SETF-EXPANSION uses SETF directly for known built-in places that lack a
;; writer, while retaining the canonical callable-writer form for other function
;; places (bliss-0qd8, bliss-42iv).
;;
;; They are written against the STORE PRIMITIVES (RPLACA/RPLACD/EGCL::SET-AREF/
;; …), never against SETF itself: routing them through `(setf (car o) v)` would
;; make each writer's correctness depend on SETF continuing to prefer its builtin
;; place handling over the writer we are defining here, which is exactly the kind
;; of mutual dependency that turns into unbounded recursion the day that
;; precedence changes.
;;
;; Defining these does NOT slow the `(setf (car x) v)` FORM down: SETF's builtin
;; place handling still wins (measured — no change beyond run-to-run noise), so
;; these are used for the function designator, not the common path.
;; Each returns NEW, as CLHS requires of a setf function.
(defun (setf car) (new cons) (rplaca cons new) new)
(defun (setf cdr) (new cons) (rplacd cons new) new)
(defun (setf first) (new cons) (rplaca cons new) new)
(defun (setf rest) (new cons) (rplacd cons new) new)
(defun (setf caar) (new x) (rplaca (car x) new) new)
(defun (setf cadr) (new x) (rplaca (cdr x) new) new)
(defun (setf cdar) (new x) (rplacd (car x) new) new)
(defun (setf cddr) (new x) (rplacd (cdr x) new) new)
(defun (setf second) (new x) (rplaca (cdr x) new) new)
(defun (setf third) (new x) (rplaca (cddr x) new) new)
(defun (setf caaar) (new x) (rplaca (caar x) new) new)
(defun (setf caadr) (new x) (rplaca (cadr x) new) new)
(defun (setf cadar) (new x) (rplaca (cdar x) new) new)
(defun (setf caddr) (new x) (rplaca (cddr x) new) new)
(defun (setf cdaar) (new x) (rplacd (caar x) new) new)
(defun (setf cdadr) (new x) (rplacd (cadr x) new) new)
(defun (setf cddar) (new x) (rplacd (cdar x) new) new)
(defun (setf cdddr) (new x) (rplacd (cddr x) new) new)
(defun (setf caaaar) (new x) (rplaca (caaar x) new) new)
(defun (setf caaadr) (new x) (rplaca (caadr x) new) new)
(defun (setf caadar) (new x) (rplaca (cadar x) new) new)
(defun (setf caaddr) (new x) (rplaca (caddr x) new) new)
(defun (setf cadaar) (new x) (rplaca (cdaar x) new) new)
(defun (setf cadadr) (new x) (rplaca (cdadr x) new) new)
(defun (setf caddar) (new x) (rplaca (cddar x) new) new)
(defun (setf cadddr) (new x) (rplaca (cdddr x) new) new)
(defun (setf cdaaar) (new x) (rplacd (caaar x) new) new)
(defun (setf cdaadr) (new x) (rplacd (caadr x) new) new)
(defun (setf cdadar) (new x) (rplacd (cadar x) new) new)
(defun (setf cdaddr) (new x) (rplacd (caddr x) new) new)
(defun (setf cddaar) (new x) (rplacd (cdaar x) new) new)
(defun (setf cddadr) (new x) (rplacd (cdadr x) new) new)
(defun (setf cdddar) (new x) (rplacd (cddar x) new) new)
(defun (setf cddddr) (new x) (rplacd (cdddr x) new) new)
(defun (setf fourth) (new x) (rplaca (nthcdr 3 x) new) new)
(defun (setf fifth) (new x) (rplaca (nthcdr 4 x) new) new)
(defun (setf sixth) (new x) (rplaca (nthcdr 5 x) new) new)
(defun (setf seventh) (new x) (rplaca (nthcdr 6 x) new) new)
(defun (setf eighth) (new x) (rplaca (nthcdr 7 x) new) new)
(defun (setf ninth) (new x) (rplaca (nthcdr 8 x) new) new)
(defun (setf tenth) (new x) (rplaca (nthcdr 9 x) new) new)
(defun (setf nth) (new n list) (rplaca (nthcdr n list) new) new)
;; Writers for places whose store SETF handles in the evaluator. The PLACE always
;; worked; the function NAME did not exist, so handing it to anything that takes a
;; function designator — #'(setf slot-value), FDEFINITION, APPLY, a FUNCTION type
;; check — got the bare (SETF x) cons and signalled a TYPE-ERROR. kitchen-sink
;; does exactly that with (SETF SLOT-VALUE) (bliss-6buay).
;;
;; Every writer here stores through a PRIMITIVE (or RPLACA/RPLACD). None is
;; written as `(setf (place …) new)`, and that is a hard rule, not a style
;; preference: whether the lowerer emits a direct store for a place or a CALL to
;; that place's writer function depends on the surrounding form, so a writer
;; whose body mentions its own place recurses into itself in whichever context
;; takes the call route. Measured — `(setf (find-class 'x) c)` at the top level
;; of a loaded file takes the direct store, and the SAME form inside a LET takes
;; the writer call and overflows the stack. The names still missing for want of a
;; store primitive are tracked in bliss-rwpmq; a wrapper for any of them would
;; also BREAK places that work today, which is how this rule was found.
(defun (setf slot-value) (new object slot-name)
  (egcl::set-slot-value object slot-name new)
  new)
(defun (setf symbol-function) (new symbol)
  (egcl::set-symbol-function symbol new)
  new)
;; No (SETF SYMBOL-PLIST) here, though EGCL::SET-SYMBOL-PLIST exists for it:
;; defining it regressed ironclad-text and pure-tls, which then failed with
;; FLEXI-STREAMS::+BUFFER-SIZE+ unbound whenever flexi-streams' fasls had been
;; compiled by a drakma- or cl+ssl-driven load. Bisected to this one line and
;; reverted pending a cause (bliss-rpo1w).
(defun (setf char) (new string index) (egcl::set-aref string index new) new)
(defun (setf schar) (new string index) (egcl::set-aref string index new) new)
(defun (setf row-major-aref) (new array index)
  ;; EGCL::SET-AREF indexes in row-major order already.
  (egcl::set-aref array index new)
  new)
;; EGCL::SET-AREF takes a single ROW-MAJOR index, so a multidimensional store
;; must flatten the subscripts first — passing them through verbatim silently
;; stored nothing and broke (setf (aref a 1 2) 99).
(defun (setf aref) (new array &rest subscripts)
  (egcl::set-aref array (apply (function array-row-major-index) array subscripts) new)
  new)
(defun (setf svref) (new v i) (egcl::set-aref v i new) new)
(defun (setf elt) (new seq i) (egcl::set-elt seq i new) new)
(defun (setf gethash) (new key table &optional default)
  (declare (ignore default))
  (egcl::put-gethash new key table)
  new)
(defun (setf symbol-value) (new sym) (set sym new) new)

;; (setf (ldb bytespec place) new) — CLHS 5.1.2.2 / the LDB dictionary entry.
;; LDB is a read-modify-write place: it reads the whole integer out of PLACE,
;; replaces just the BYTESPEC field via DPB, and writes the integer back. The
;; bytespec and the inner place's subforms are lifted into temporaries so each
;; is evaluated exactly once, and the expansion returns NEW (not the stored
;; integer), which is what SETF must yield (bliss-pbp8).
(define-setf-expander ldb (bytespec place &environment env)
  (multiple-value-bind (dummies vals newval setter getter)
      (get-setf-expansion place env)
    (let ((btemp (gensym)) (store (gensym)))
      (values (cons btemp dummies)
              (cons bytespec vals)
              (list store)
              ;; A getter is an expression, not necessarily a settable place.
              ;; Bind the inner store variables and invoke its actual writer.
              `(progn
                 (multiple-value-bind ,newval (dpb ,store ,btemp ,getter)
                   ,setter)
                 ,store)
              `(ldb ,btemp ,getter)))))

;; (setf (mask-field bytespec place) new) — same shape, but NEW is taken in
;; place rather than right-justified, so DEPOSIT-FIELD replaces DPB.
(define-setf-expander mask-field (bytespec place &environment env)
  (multiple-value-bind (dummies vals newval setter getter)
      (get-setf-expansion place env)
    (let ((btemp (gensym)) (store (gensym)))
      (values (cons btemp dummies)
              (cons bytespec vals)
              (list store)
              `(progn
                 (multiple-value-bind ,newval (deposit-field ,store ,btemp ,getter)
                   ,setter)
                 ,store)
              `(mask-field ,btemp ,getter)))))

;;; --- misc numeric functions -------------------------------------------------

;; SIGNUM (CLHS 12.2): rational => -1/0/1 (integer); float => a float of the
;; SAME format carrying the sign; complex z => z/|z| (a unit-magnitude complex),
;; or z itself when zero.
(defun signum (n)
  (cond ((complexp n)
         (if (zerop n) n (/ n (abs n))))
        ((zerop n) n)
        ((floatp n) (float (if (plusp n) 1 -1) n))
        ((plusp n) 1)
        (t -1)))

(defun isqrt (n)
  ;; ANSI: ISQRT takes a NON-NEGATIVE INTEGER. The (< n 2) branch returned its
  ;; argument unchecked, so (isqrt 1.2) answered 1.2 and (isqrt 3/5) answered
  ;; 3/5; a negative integer signalled a plain SIMPLE-ERROR rather than the
  ;; TYPE-ERROR ansi ISQRT.ERROR.5 requires.
  (unless (and (integerp n) (>= n 0))
    (error 'type-error :datum n :expected-type '(integer 0)))
  (cond ((< n 2) n)
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
;; PSETF assigns to all places in PARALLEL: every value form AND every place's
;; own subforms are evaluated before any assignment happens. The previous version
;; evaluated only the values up front and then did `(setf place temp)` in order,
;; so a later place that referenced an earlier place's variable read the mutated
;; value — e.g. cl-ppcre's parser `(psetf last-cdr cons (cdr last-cdr) cons)`
;; spliced into `(cdr cons)` (self-loop) instead of the old last-cdr, dropping a
;; sequence element (bliss-omw). We capture each accessor place's argument
;; subforms into fresh temps too, so the assignment targets the original cells.
;; PSETF via GET-SETF-EXPANSION (CLHS 5.1.3), like ROTATEF/SHIFTF below.
;; The previous version lifted each place ARGUMENT into a temporary and then
;; stored through `(setf (op . temps) v)`. That is wrong whenever an argument is
;; itself the thing being written: `(ldb (byte 5 1) x)` became
;; `(setf (ldb #:t1 #:t2) v)`, storing into the temporary #:t2 rather than into
;; X. It also never macroexpanded the place, so a SYMBOL-MACROLET place was
;; treated as a plain variable, and it had no expansion at all for places like
;; (FDEFINITION f) / (SYMBOL-FUNCTION f), which signalled UNBOUND-VARIABLE
;; (ansi PSETF.7 .24 .25 .28; bliss-pbp8).
;;
;; Order matters and is per PAIR, not per phase: CLHS evaluates place1's
;; subforms, then value1, then place2's subforms, then value2, … So the
;; bindings are built pair by pair rather than through %setf-expansions (which
;; collects every place first — correct for ROTATEF/SHIFTF, which have no value
;; forms interleaved between the places).
(defmacro psetf (&rest pairs &environment env)
  (let ((entries nil) (assigns nil) (p pairs))
    (loop while (consp (cdr p)) do
      ;; Via %SETF-EXPANSIONS, not GET-SETF-EXPANSION directly: a nested (VALUES
      ;; …) place needs its sub-places kept as places rather than lifted into
      ;; temporaries, which is what that helper exists to do. Calling
      ;; GET-SETF-EXPANSION here stored into the lifted temporaries and left the
      ;; real places untouched, so `(psetf (values a b c) (values 1 2 3))`
      ;; assigned nothing at all (ansi PSETF.41; bliss-hsn7).
      (multiple-value-bind (place-binds place-stores place-setters place-getters)
          (%setf-expansions (list (car p)) env)
        (declare (ignore place-getters))
        (push (list place-binds (car place-stores) (cadr p)) entries)
        (push (car place-setters) assigns))
      (setq p (cddr p)))
    (let ((form `(progn ,@(reverse assigns) nil)))
      ;; Wrap in reverse discovery order so place/value pairs still evaluate
      ;; from left to right, while every storing form remains in the innermost
      ;; body and therefore runs only after all value forms have completed.
      (dolist (entry entries form)
        (setq form
              `(let* ,(car entry)
                 (multiple-value-bind ,(cadr entry) ,(caddr entry)
                   ,form)))))))

;; ROTATEF / SHIFTF go through GET-SETF-EXPANSION (CLHS 5.1.3), like INCF/DECF
;; and PUSH/POP above. The previous definitions mentioned every PLACE TWICE —
;; once to read it in the LET binding and once to write it in the SETF — so a
;; place with a side-effecting subform ran that subform twice:
;;   (shiftf (aref x (incf i)) (incf i))  advanced I twice and stored through
;;                                        the wrong index
;;   (rotatef (aref x (incf i)) (aref x (incf i)))
;;                                        ran the index off the end of the
;;                                        vector and signalled a TYPE-ERROR
;; (ansi SHIFTF-ORDER.1/2, ROTATEF-ORDER.1/2; bliss-pbp8). Lifting the subforms
;; into temporaries evaluates each exactly once, left to right, and every place
;; is READ before any place is WRITTEN — which is what makes the rotate work.

;; Collect the setf expansions of PLACES into
;;   (values let*-bindings store-variable-lists storing-forms access-forms).
;; Keeping the storing forms is essential for custom places whose access form
;; is an arbitrary expression rather than a place that SETF can expand again.
(defun %values-place-p (getter)
  (and (consp getter) (eq (car getter) 'values)))

(defun %setf-expansions (places env)
  (let ((binds nil) (stores nil) (setters nil) (getters nil))
    (dolist (raw places)
      (let ((place (egcl::%macroexpand-place raw env)))
        (if (and (consp place) (eq (car place) 'values))
            ;; A nested (VALUES …) place: its arguments are PLACES, not value
            ;; subforms, so they must NOT be lifted into temporaries.
            ;; GET-SETF-EXPANSION treats VALUES as an ordinary accessor and does
            ;; lift them, which stored into the temporary and left the real
            ;; places untouched — `(setf (values a (values b c)) …)` never
            ;; assigned B or C (ansi VALUES.20). Recurse and rebuild a VALUES
            ;; access form out of the sub-places' own access forms.
            (multiple-value-bind (sub-binds sub-stores sub-setters sub-getters)
                (%setf-expansions (cdr place) env)
              (setq binds (append binds sub-binds))
              ;; VALUES supplies one value to each direct sub-place. A
              ;; sub-place with several store variables receives NIL for its
              ;; remaining variables, just as MULTIPLE-VALUE-BIND does.
              (let ((incoming (mapcar (lambda (g) (declare (ignore g)) (gensym))
                                      sub-getters)))
                (push incoming stores)
                (push (%bind-setf-stores sub-stores incoming sub-setters) setters))
              (push (cons 'values sub-getters) getters))
            (multiple-value-bind (dummies vals newvars setter getter)
                (get-setf-expansion place env)
              (do ((d dummies (cdr d)) (v vals (cdr v)))
                  ((null d))
                (setq binds (append binds (list (list (car d) (car v))))))
              (push newvars stores)
              (push setter setters)
              (push getter getters)))))
    (values binds (reverse stores) (reverse setters) (reverse getters))))

;; Bind each storing-form variable list to its corresponding source, then run
;; BODY only after all sources have been evaluated.
(defun %bind-setf-stores (stores sources body)
  (if (null stores)
      (cons 'progn body)
      (list 'multiple-value-bind (car stores) (car sources)
            (%bind-setf-stores (cdr stores) (cdr sources) body))))

;; ROTATEF: each place receives the (old) value of the next; last gets first.
(defmacro rotatef (&rest places &environment env)
  (if (or (null places) (null (cdr places)))
      nil
      (multiple-value-bind (binds stores setters getters)
          (%setf-expansions places env)
        (let ((sources (append (cdr getters) (list (car getters)))))
          `(let* ,binds
             ,(%bind-setf-stores stores sources (append setters (list nil))))))))

;; (setf (values p1 … pn) form) — CLHS 5.1.2.3. The SUBFORMS of every place are
;; evaluated, left to right, BEFORE the value form; then FORM's values are
;; distributed across the places (a missing value is NIL) and the PRIMARY value
;; is returned. The interpreter's SETF evaluates the value form up front for
;; every place in its generic branch, which reversed the order, so the VALUES
;; place defers and delegates here — expressing it in Lisp reuses
;; GET-SETF-EXPANSION rather than re-implementing setf expansion in Rust
;; (bliss-dj5k).
(defmacro egcl::%setf-values (places form &environment env)
  (if (null places)
      ;; (setf (values) form) evaluates FORM and returns NO values -- not NIL.
      ;; ansi SETF-VALUES.6 (bliss-prdk).
      (list 'progn form '(values))
      (multiple-value-bind (binds stores setters getters)
          (%setf-expansions places env)
        (declare (ignore getters))
        (let ((vals (mapcar (lambda (s) (declare (ignore s)) (gensym)) stores)))
          `(let* ,binds
             (multiple-value-bind ,vals ,form
               ,(%bind-setf-stores stores vals
                                   (append setters (list (cons 'values vals))))))))))

;; SHIFTF: return the old value of the first place; shift the rest leftward and
;; store NEWVALUE (the final argument) into the last place.
(defmacro shiftf (&rest args &environment env)
  (let ((places (butlast args))
        (newval (car (last args))))
    (multiple-value-bind (binds stores setters getters)
        (%setf-expansions places env)
      (let ((outvars (mapcar (lambda (s) (declare (ignore s)) (gensym))
                             (car stores)))
            (sources (append (cdr getters) (list newval))))
        ;; SHIFTF returns every old value of the first place, so a place whose
        ;; reading produces several -- a VALUES place, or an expander with more
        ;; than one store variable -- shifts them all out and can feed another
        ;; SHIFTF.  This used to return only the primary value, citing ansi
        ;; shiftf.7; that test is inside a #||...||# block in the suite,
        ;; disabled by its own authors, whose note reads "several lisps return
        ;; '(a b) and when viewed as a shift register it makes sense to have
        ;; all the values from first place to be returned".  SBCL returns
        ;; (A B) for (shiftf (values x y) (floor 10 3)); so do we.  A
        ;; single-store place still yields exactly one value.
        `(let* ,binds
           (multiple-value-bind ,outvars ,(car getters)
             ,(%bind-setf-stores stores sources
                                 (append setters
                                         (list `(values ,@outvars))))))))))

;;; PROG / PROG*: LET (or LET*) plus an implicit BLOCK NIL and TAGBODY.
;;; A leading (declare ...) belongs to the LET, not to the TAGBODY: CLHS 6.1.1.4
;;; puts PROG's declarations on its variable bindings, and a DECLARE left inside
;;; the tagbody is neither a declaration nor a valid statement there — so
;;; (prog ((v 1)) (declare (special v)) ...) bound V lexically (bliss-ge3g).
(defun %prog-split-declarations (body)
  "Return (values leading-declarations remaining-forms) for a PROG body."
  (let ((decls '()) (forms body))
    (do () ((not (and (consp forms)
                      (consp (car forms))
                      (eq (car (car forms)) 'declare))))
      (push (car forms) decls)
      (setq forms (cdr forms)))
    (values (nreverse decls) forms)))

(defmacro prog (bindings &rest body)
  (multiple-value-bind (decls forms) (%prog-split-declarations body)
    `(block nil (let ,bindings ,@decls (tagbody ,@forms)))))
(defmacro prog* (bindings &rest body)
  (multiple-value-bind (decls forms) (%prog-split-declarations body)
    `(block nil (let* ,bindings ,@decls (tagbody ,@forms)))))

;;; Keep file bodies visible to the compiler instead of interpreting the whole
;;; scope. Bind declarations to the stream, preserve all values, and request an
;;; abort only when control leaves the body without returning normally.
(defmacro with-open-file ((stream filespec &rest options) &body body)
  (multiple-value-bind (decls forms) (%prog-split-declarations body)
    (let ((abort (gensym "FILE-ABORT"))
          (results (gensym "FILE-VALUES")))
      `(let ((,stream (open ,filespec ,@options)))
         ,@decls
         (let ((,abort t))
           (values-list
             (unwind-protect
                 (let ((,results (multiple-value-list (progn ,@forms))))
                   (setq ,abort nil)
                   ,results)
               (when ,stream (close ,stream :abort ,abort)))))))))

;;; (SETF BIT) / (SETF SBIT) as real writer FUNCTIONS, not just SETF places.
;;; (setf (apply #'bit bv 4 nil) 1) expands to (apply #'(setf bit) 1 bv 4 nil)
;;; per CLHS 5.1.2.5, so the writer has to be callable and take its subscripts
;;; spread. BIT and SBIT index a bit array exactly as AREF does, so AREF's
;;; writer is the implementation (bliss-hzen).
(defun (setf bit) (new bit-array &rest subscripts)
  (setf (apply #'aref bit-array subscripts) new))
(defun (setf sbit) (new bit-array &rest subscripts)
  (setf (apply #'aref bit-array subscripts) new))

;;; CCASE / CTYPECASE: like ECASE / ETYPECASE but the key is a place and a
;;; correctable STORE-VALUE restart lets the handler supply a fresh value.
(defmacro ccase (keyplace &rest clauses)
  (let ((value (gensym)) (top (gensym))
        (all-keys nil))
    (dolist (clause clauses)
      (let ((keys (car clause)))
        ;; NIL designates the EMPTY key list, so it contributes no keys to the
        ;; expected type and its clause can never match (CLHS; bliss-gm4h).
        (cond ((null keys))
              ((consp keys) (dolist (k keys) (push k all-keys)))
              (t (push keys all-keys)))))
    `(block nil
       (tagbody
          ,top
          (return
            (let ((,value ,keyplace))
              (cond
                ,@(remove nil
                    (mapcar (lambda (clause)
                              (let ((keys (car clause)) (body (cdr clause)))
                                (cond ((null keys) nil)
                                      ((consp keys)
                                       `((or ,@(mapcar (lambda (k) `(eql ,value ',k)) keys))
                                         ,@(or body '(nil))))
                                      (t `((eql ,value ',keys) ,@(or body '(nil)))))))
                            clauses))
                ;; CCASE signals a correctable TYPE-ERROR whose expected type is
                ;; the set of keys (not T) — ansi CCASE.4/.5 require the datum to
                ;; NOT satisfy the expected type. STORE-VALUE retries.
                (t (restart-case
                       (error 'type-error :datum ,value
                              :expected-type '(member ,@(reverse all-keys)))
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
                            `((typep ,value ',(car clause)) ,@(or (cdr clause) '(nil))))
                          clauses)
                (t (restart-case
                       (error 'type-error :datum ,value
                              :expected-type '(or ,@(mapcar #'car clauses)))
                     (store-value (v) (setf ,keyplace v) (go ,top)))))))))))

;;; ---------------------------------------------------------------------------
;;; Gray streams: CLOS class hierarchy and generic-function protocol (spec
;;; §5.5.2, egcl-jtc.7b).
;;;
;;; Built-in streams stay Rust-backed for speed; these classes and generics let
;;; user code define its own stream types. The standard stream functions
;;; (read-char, write-char, …) dispatch to these generics when their argument is
;;; a FUNDAMENTAL-STREAM instance, and to the fast Rust path otherwise. The Gray
;;; generics use fixed arities — the standard functions fill in optional
;;; start/end/eof arguments before dispatching.
;;; ---------------------------------------------------------------------------

;;; 5.5.2.1  Base classes. FUNDAMENTAL-STREAM is a STREAM, as it is in every
;;; host that offers Gray streams: a method specialized on STREAM applies to a
;;; Gray stream instance, and STREAM is in its class precedence list. Only the
;;; prelude may name STREAM as a direct superclass (DEFCLASS refuses it for
;;; user code, as CLHS requires of a built-in class).
(defclass fundamental-stream (standard-object stream)
  ((egcl::%gray-open-p :initform t)))
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
(defgeneric stream-read-sequence (stream sequence start end))
;; FILE-POSITION on a Gray stream, as SBCL's SB-GRAY:STREAM-FILE-POSITION:
;; with no POSITION-SPEC return the position or NIL; with one (an integer,
;; :START or :END) move there and return true on success.
(defgeneric stream-file-position (stream &optional position-spec))

(defgeneric stream-write-char (stream character))
(defgeneric stream-line-column (stream))
(defgeneric stream-start-line-p (stream))
;; Gray's proposal (and SBCL's SB-GRAY) make START and END optional here, and
;; the portable Gray stream libraries define their methods that way.
(defgeneric stream-write-string (stream string &optional start end))
(defgeneric stream-terpri (stream))
(defgeneric stream-fresh-line (stream))
(defgeneric stream-finish-output (stream))
(defgeneric stream-force-output (stream))
(defgeneric stream-clear-output (stream))
(defgeneric stream-write-byte (stream integer))
(defgeneric stream-write-sequence (stream sequence start end))

(defgeneric gray-stream-element-type (stream))
(defgeneric gray-close (stream))

;;; Retain the native operations as least-specific methods. Libraries such as
;;; Flexi Streams extend these CL functions and call them on an underlying
;;; native stream; adding a wrapper method must not discard native support.
(defun egcl::%check-stream (object)
  (unless (streamp object)
    (error 'type-error :datum object :expected-type 'stream))
  object)
(defmethod open-stream-p ((stream t))
  (egcl::%native-open-stream-p (egcl::%check-stream stream)))
(defmethod open-stream-p ((stream fundamental-stream))
  (slot-value stream 'egcl::%gray-open-p))
(defmethod input-stream-p ((stream t))
  (egcl::%native-input-stream-p (egcl::%check-stream stream)))
(defmethod input-stream-p ((stream fundamental-stream)) nil)
(defmethod input-stream-p ((stream fundamental-input-stream)) t)
(defmethod output-stream-p ((stream t))
  (egcl::%native-output-stream-p (egcl::%check-stream stream)))
(defmethod output-stream-p ((stream fundamental-stream)) nil)
(defmethod output-stream-p ((stream fundamental-output-stream)) t)
(defmethod stream-element-type ((stream t))
  (let ((element-type (egcl::%native-stream-element-type stream)))
    (cond ((eq element-type t) 'character)
          ((eql element-type 8) '(unsigned-byte 8))
          ;; The native code is the bit width, negated for a signed type.
          ((and (integerp element-type) (plusp element-type))
           `(unsigned-byte ,element-type))
          ((integerp element-type) `(signed-byte ,(- element-type)))
          (t (error 'type-error :datum stream :expected-type 'stream)))))
(defmethod stream-element-type ((stream fundamental-stream))
  (gray-stream-element-type stream))
(defmethod close ((stream t) &key abort)
  (egcl::%native-close stream :abort abort))
(defmethod close ((stream fundamental-stream) &key abort)
  (declare (ignore abort))
  (gray-close stream))

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

;; A Gray stream that does not track its position reports it as unknown and
;; cannot be repositioned, which FILE-POSITION expresses as NIL.
(defmethod stream-file-position ((stream fundamental-stream) &optional position-spec)
  (declare (ignore position-spec))
  nil)

(defmethod stream-read-sequence ((stream fundamental-input-stream) sequence start end)
  (let ((read-element (if (subtypep (stream-element-type stream) 'character)
                          #'stream-read-char
                          #'stream-read-byte)))
    (do ((index start (1+ index)))
        ((>= index end) index)
      (let ((element (funcall read-element stream)))
        (when (eq element :eof)
          (return index))
        (setf (elt sequence index) element)))))

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

(defmethod stream-write-string ((stream fundamental-character-output-stream) string
                                &optional (start 0) end)
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

;;; Bulk write default: a string goes through STREAM-WRITE-STRING so a stream
;;; that specializes it sees one call, not one per character; anything else is
;;; written element by element, characters via STREAM-WRITE-CHAR and integers
;;; via STREAM-WRITE-BYTE. Portable libraries (trivial-gray-streams) specialize
;;; STREAM-WRITE-SEQUENCE itself and fall back here with CALL-NEXT-METHOD.
(defmethod stream-write-sequence ((stream fundamental-output-stream) sequence start end)
  (let ((end (or end (length sequence))))
    (if (stringp sequence)
        (stream-write-string stream sequence start end)
        (do ((index start (1+ index)))
            ((>= index end))
          (let ((element (elt sequence index)))
            (if (characterp element)
                (stream-write-char stream element)
                (stream-write-byte stream element)))))
    sequence))

(defmethod stream-finish-output ((stream fundamental-output-stream)) nil)
(defmethod stream-force-output ((stream fundamental-output-stream)) nil)
(defmethod stream-clear-output ((stream fundamental-output-stream)) nil)

;;; Query / lifecycle defaults (§5.5.2.4).
(defmethod gray-stream-element-type ((stream fundamental-character-stream)) 'character)
(defmethod gray-stream-element-type ((stream fundamental-binary-stream)) '(unsigned-byte 8))
(defmethod gray-close ((stream fundamental-stream))
  (setf (slot-value stream 'egcl::%gray-open-p) nil)
  t)

;;; Publish the existing protocol symbols, not a second same-named protocol.
;;; Portable libraries import these symbols and specialize their methods.
(defpackage :egcl-gray-streams (:use))
(let ((protocol '(fundamental-stream fundamental-input-stream fundamental-output-stream
                  fundamental-character-stream fundamental-binary-stream
                  fundamental-character-input-stream fundamental-character-output-stream
                  fundamental-binary-input-stream fundamental-binary-output-stream
                  stream-read-char stream-unread-char stream-read-char-no-hang
                  stream-peek-char stream-listen stream-read-line stream-clear-input
                  stream-write-char stream-line-column stream-start-line-p
                  stream-write-string stream-terpri stream-fresh-line
                  stream-finish-output stream-force-output stream-clear-output
                  stream-advance-to-column stream-read-byte stream-write-byte
                  stream-read-sequence stream-write-sequence stream-file-position)))
  ;; Bootstrap symbols were historically available by their unqualified names.
  ;; Keep them present in CL-USER as well as in the public protocol package.
  (import protocol :cl-user)
  (import protocol :egcl-gray-streams)
  (export protocol :egcl-gray-streams))

;;; ---------------------------------------------------------------------------
;;; Pathname namestring helpers (bliss-lb6). These are standard CL functions
;;; that UIOP/ASDF relies on the host to provide (UIOP only defines them for a
;;; few odd Lisps). Built on the wired pathname-component accessors.
;;; ---------------------------------------------------------------------------

(defun file-namestring (p)
  "The name, type, and version portion of pathname P as a string."
  (let ((p (pathname p)))
    (format nil "~@[~a~]~@[.~a~]" (pathname-name p) (pathname-type p))))

(defun directory-namestring (p)
  "The directory portion of pathname P as a string."
  #+windows
  (return-from directory-namestring
    (namestring (make-pathname :defaults (pathname p) :name nil :type nil :version nil)))
  (let* ((p (pathname p))
         (dir (pathname-directory p)))
    (if (null dir)
        ""
        (let ((result (if (eq (car dir) :absolute) "/" "")))
          (dolist (part (cdr dir) result)
            (setf result
                  (concatenate 'string result
                               (cond ((eq part :up) "..")
                                     ((eq part :wild) "*")
                                     ((eq part :wild-inferiors) "**")
                                     (t part))
                               "/")))))))

;; All egcl strings are simple (no fill pointers / displacement yet), so the
;; SIMPLE- predicates coincide with their general counterparts (bliss-d0b:
;; cl-cookie calls simple-string-p via ppcre).
(defun simple-string-p (x) (stringp x))

;; ANSI 14.2: SUBLIS/NSUBLIS require a proper association list. An improper
;; spine (e.g. ((a . 1) . bad)) is a TYPE-ERROR, not a silently-truncated walk
;; (ansi-test sublis.error.8 / nsublis.error.8).
(defun %check-sublis-alist (alist)
  (do ((a alist (cdr a)))
      ((null a))
    (unless (consp a)
      ;; Datum is the improper tail A (a non-list), not the whole ALIST: a
      ;; type-error whose datum satisfies its expected-type is rejected by
      ;; ANSI's SIGNALS-ERROR (sublis.error.8 / nsublis.error.8).
      (error 'type-error :datum a :expected-type 'list))))

(defun sublis (alist tree &key key (test #'eql) test-not)
  "Substitute through TREE: any subtree/leaf matching an ALIST key is replaced
by that pair's cdr (ANSI 14.2; bliss-d0b: flexi-streams)."
  (%check-sublis-alist alist)
  (labels ((lookup (x)
             (let ((k (if key (funcall key x) x)))
               (let ((found nil))
                 (dolist (pair alist)
                   (when (and (not found)
                              (consp pair)
                              (if test-not
                                  (not (funcall test-not k (car pair)))
                                  (funcall test k (car pair))))
                     (setq found pair)))
                 found)))
           (walk (x)
             (let ((pair (lookup x)))
               (cond (pair (cdr pair))
                     ((consp x) (cons (walk (car x)) (walk (cdr x))))
                     (t x)))))
    (walk tree)))

(defun nsublis (alist tree &rest args)
  (apply #'sublis alist tree args))

(defun host-namestring (pathname)
  "The host portion of pathname PATHNAME as a string, or NIL when it has no
host (ANSI 19.4). egcl physical pathnames carry no host, so this is NIL for
them and the host name for a logical pathname."
  (let* ((p (pathname pathname))
         (host (pathname-host p)))
    (if (stringp host) host nil)))

(defun file-error-pathname (condition)
  "The offending pathname of a FILE-ERROR (ANSI 19.5). Falls back to a null
pathname when the condition carries no stored pathname, so callers that only
check PATHNAMEP on the result behave sensibly."
  (or (ignore-errors (slot-value condition 'pathname))
      (make-pathname)))

(defun enough-namestring (pathname &optional (defaults *default-pathname-defaults*))
  "A namestring just sufficient to identify PATHNAME relative to DEFAULTS:
the namestring with DEFAULTS' directory prefix stripped when PATHNAME lies
under it, otherwise the full namestring (ANSI 19.4; bliss-s1k)."
  (let* ((p (pathname pathname))
         (d (pathname defaults))
         (full (namestring p))
         (base (directory-namestring d)))
    (if (and (> (length base) 0)
             (>= (length full) (length base))
             (string= base (subseq full 0 (length base))))
        (subseq full (length base))
        full)))

;;; ---------------------------------------------------------------------------
;;; Implementation / environment identity (bliss-lb6). ASDF/UIOP build cache
;;; and output-translation paths from these. Constant values are sufficient for
;;; loading; they can be wired to real system info later.
;;; ---------------------------------------------------------------------------

;;; Native mutex policy lives here; %NATIVE-MUTEX delegates to the stdlib.
(export (mapcar (lambda (name) (intern name "EGCL-THREAD"))
                '("MAKE-THREAD" "JOIN-THREAD" "CURRENT-THREAD" "THREAD-NAME"
                  "THREAD-ALIVE-P" "ALL-THREADS" "THREAD-YIELD"))
        "EGCL-THREAD")

(defun egcl-thread:make-mutex (&key name recursive)
  (egcl::%native-mutex :make name recursive))

(defun egcl-thread:mutex-p (object)
  (egcl::%native-mutex :p object))

(deftype egcl-thread:mutex () '(satisfies egcl-thread:mutex-p))

(defun egcl-thread:grab-mutex (mutex &key (waitp t) timeout)
  (unless (or (null timeout) (and (realp timeout) (not (minusp timeout))))
    (error 'type-error :datum timeout :expected-type '(or null (real 0))))
  (egcl::%native-mutex :grab mutex waitp (and timeout (float timeout 1d0))))

(defun egcl-thread:release-mutex (mutex &key (if-not-owner :error))
  (unless (member if-not-owner '(:error :warn :ignore))
    (error 'type-error :datum if-not-owner :expected-type '(member :error :warn :ignore)))
  (if (eq if-not-owner :error)
      (egcl::%native-mutex :release mutex)
      (unless (egcl::%native-mutex :release-if-owned mutex)
        (when (eq if-not-owner :warn)
          (warn "Attempt to release a mutex not owned by the current execution"))))
  nil)

(defmacro egcl-thread:with-mutex ((mutex &key (waitp t) timeout) &body body)
  (let ((lock (gensym "MUTEX"))
        (results (gensym "MUTEX-VALUES")))
    `(let ((,lock ,mutex) (,results nil))
       (when (egcl-thread:grab-mutex ,lock :waitp ,waitp :timeout ,timeout)
         ;; Keep all values explicit across cleanup until the general compiled
         ;; UNWIND-PROTECT multiple-value defect (bliss-pfgq) is resolved.
         (unwind-protect
             (setf ,results (multiple-value-list (progn ,@body)))
           (egcl-thread:release-mutex ,lock))
         (values-list ,results)))))

(export '(egcl-thread:make-mutex egcl-thread:mutex-p egcl-thread:mutex
          egcl-thread:grab-mutex egcl-thread:release-mutex egcl-thread:with-mutex)
        "EGCL-THREAD")

(defun egcl-thread:make-condition-variable (&key name)
  (egcl::%native-condition :make name))

(defun egcl-thread:condition-variable-p (object)
  (egcl::%native-condition :p object))

(deftype egcl-thread:condition-variable ()
  '(satisfies egcl-thread:condition-variable-p))

(defun egcl-thread:condition-wait (condition-variable mutex &key timeout)
  (unless (or (null timeout) (and (realp timeout) (not (minusp timeout))))
    (error 'type-error :datum timeout :expected-type '(or null (real 0))))
  (egcl::%native-condition :wait condition-variable mutex (and timeout (float timeout 1d0))))

(defun egcl-thread:condition-notify (condition-variable &optional (count 1))
  (unless (and (integerp count) (not (minusp count)))
    (error 'type-error :datum count :expected-type '(integer 0)))
  ;; There cannot be more live waiters than this process can address.
  (egcl::%native-condition :notify condition-variable (min count most-positive-fixnum)))

(defun egcl-thread:condition-broadcast (condition-variable)
  (egcl::%native-condition :broadcast condition-variable))

(export '(egcl-thread:make-condition-variable egcl-thread:condition-variable
          egcl-thread:condition-variable-p egcl-thread:condition-wait
          egcl-thread:condition-notify egcl-thread:condition-broadcast)
        "EGCL-THREAD")

(defun lisp-implementation-type () "EGCL")
(defun lisp-implementation-version () "0.0.10")
(defun machine-type () (egcl-ext::%machine-type))
(defun machine-version () (egcl-ext::%machine-type))
(defun machine-instance () "localhost")
(defun software-type () "Linux")
(defun software-version () "1.0")

;;; ---------------------------------------------------------------------------
;;; PARSE-INTEGER (bliss-lb6). Parses an integer from a (sub)string with an
;;; optional sign, honouring :start/:end/:radix/:junk-allowed, returning
;;; (values integer position). UIOP parses version strings with it.
;;; ---------------------------------------------------------------------------

;; ANSI: PARSE-INTEGER with junk-allowed NIL must signal a PARSE-ERROR (not a
;; plain SIMPLE-ERROR) when the substring is not an integer, so callers can
;; (handler-case ... (parse-error ...)). SIMPLE-PARSE-ERROR mixes in
;; SIMPLE-ERROR to carry a readable :format-control/:format-arguments report.
(define-condition simple-parse-error (parse-error simple-error) ())

(defun parse-integer (string &key (start 0) end (radix 10) junk-allowed)
  (let ((end (or end (length string)))
        (i start)
        (sign 1)
        (any nil)
        (value 0))
    (flet ((ws-p (c) (member c '(#\Space #\Tab #\Newline #\Return #\Page))))
      (loop while (and (< i end) (ws-p (char string i))) do (incf i))
      (when (< i end)
        (let ((c (char string i)))
          (cond ((eql c #\+) (incf i))
                ((eql c #\-) (setf sign -1) (incf i)))))
      (block digits
        (loop while (< i end) do
          (let ((d (digit-char-p (char string i) radix)))
            (if d
                (progn (setf value (+ (* value radix) d)) (setf any t) (incf i))
                (return-from digits)))))
      ;; Trailing whitespace is part of what the strict parse accepts (CLHS:
      ;; "possibly surrounded on either side by whitespace"); with JUNK-ALLOWED
      ;; the second value is the index of the delimiter that ended the parse,
      ;; and a space after the digits is that delimiter (cl-pdf reads
      ;; "FontBBox -113 -250 ..." by the index it hands back).
      (unless junk-allowed
        (loop while (and (< i end) (ws-p (char string i))) do (incf i)))
      (cond
        (junk-allowed (values (if any (* sign value) nil) i))
        ((and any (>= i end)) (values (* sign value) i))
        (t (error 'simple-parse-error
                  :format-control "PARSE-INTEGER: not an integer: ~s"
                  :format-arguments (list (subseq string start end))))))))

;;; ---------------------------------------------------------------------------
;;; EGCL-CLTL2 — CLtL2 lexical-environment access (R4.14).
;;;
;;; EGCL's counterpart of SB-CLTL2: the package a portability layer such as
;;; trivial-cltl2 USEs. Only what EGCL can answer truthfully is defined here.
;;; The rest of the CLtL2 environment API — VARIABLE-INFORMATION,
;;; FUNCTION-INFORMATION, AUGMENT-ENVIRONMENT, PARSE-MACRO, ENCLOSE,
;;; COMPILER-LET — is deliberately ABSENT rather than stubbed, so a caller's own
;;; FBOUNDP guard (as Serapeum's macro-tools uses) sees the truth instead of a
;;; function that lies (bliss-powf).
;;; ---------------------------------------------------------------------------

(defvar egcl-cltl2::*declaration-handlers* (make-hash-table :test 'eq))

;; A declaration handler is installed by DEFINE-DECLARATION and invoked when
;; DECLARATION-INFORMATION is asked about its name, with the raw source
;; specifier the environment recorded and the environment itself.
(defmacro egcl-cltl2:define-declaration (name lambda-list &rest body)
  (list 'eval-when '(:compile-toplevel :load-toplevel :execute)
        (list 'proclaim (list 'quote (list 'declaration name)))
        (list 'setf
              (list 'gethash (list 'quote name) 'egcl-cltl2::*declaration-handlers*)
              (cons 'lambda (cons lambda-list body)))
        (list 'quote name)))

;; (quality value) for `item`, which CLtL2 permits as a bare quality symbol or a
;; (quality) list — both meaning the value 3.
(defun egcl-cltl2::%optimize-entry (item)
  (cond ((symbolp item) (list item 3))
        ((and (consp item) (null (cdr item))) (list (car item) 3))
        ((consp item) (list (car item) (car (cdr item))))
        (t nil)))

;; The OPTIMIZE policy in force: the standard qualities at their default value
;; of 1, overridden by global proclamations, then by the lexical declarations of
;; `env` from outermost to innermost.
(defun egcl-cltl2::%optimize-policy (env)
  (let ((policy (list (list 'compilation-speed 1)
                      (list 'debug 1)
                      (list 'safety 1)
                      (list 'space 1)
                      (list 'speed 1))))
    (flet ((note (item)
             (let ((entry (egcl-cltl2::%optimize-entry item)))
               (when entry
                 (let ((existing (assoc (car entry) policy)))
                   (if existing
                       (rplaca (cdr existing) (car (cdr entry)))
                       (setf policy (append policy (list entry)))))))))
      (dolist (entry (egcl-ext:proclaimed-optimize))
        (note entry))
      (dolist (specifier (egcl-ext:declaration-specifiers 'optimize env))
        (dolist (item (cdr specifier))
          (note item))))
    policy))

(defun egcl-cltl2:declaration-information (decl-name &optional env)
  (cond
    ((eq decl-name 'optimize)
     (egcl-cltl2::%optimize-policy env))
    ((eq decl-name 'declaration)
     (egcl-ext:proclaimed-declarations))
    (t
     (let ((handler (gethash decl-name egcl-cltl2::*declaration-handlers*)))
       (cond
         (handler
          (let ((specifier (egcl-ext:declaration-specifier decl-name env)))
            (when specifier
              (multiple-value-bind (kind info) (funcall handler specifier env)
                (cond
                  ((eq kind :declare) (cdr info))
                  (t (error "EGCL-CLTL2: the ~s declaration kind returned by the ~s handler is not supported on EGCL"
                            kind decl-name)))))))
         ((member decl-name (egcl-ext:proclaimed-declarations))
          ;; Proclaimed, but nothing was taught how to read it.
          nil)
         (t
          (error "EGCL-CLTL2:DECLARATION-INFORMATION: ~s does not name a declaration EGCL can report"
                 decl-name)))))))

(export '(egcl-cltl2:define-declaration egcl-cltl2:declaration-information)
        "EGCL-CLTL2")

;;; Foreign addresses are opaque heap objects, never tagged Lisp addresses.
;;; Foreign storage has an explicit lifetime: C may retain it after Lisp drops
;;; its last wrapper. FREE invalidates every tracked alias of an allocation.
(export (mapcar (lambda (name) (intern name "EGCL-FFI"))
                '("FOREIGN-POINTER" "POINTERP" "MAKE-POINTER" "POINTER-ADDRESS"
                  "POINTER-EQ" "NULL-POINTER" "NULL-POINTER-P" "INC-POINTER"
                  "FOREIGN-ALLOC" "FOREIGN-FREE" "MEM-REF" "MEM-SET"
                  "FOREIGN-TYPE-SIZE" "FOREIGN-TYPE-ALIGNMENT" "FFI-ERROR"
                  "MAKE-SHAREABLE-BYTE-VECTOR" "WITH-POINTER-TO-VECTOR-DATA"
                  "FOREIGN-LIBRARY" "FOREIGN-LIBRARY-P" "LOAD-FOREIGN-LIBRARY"
                  "EMBED-FOREIGN-LIBRARY" "EMBEDDED-FOREIGN-LIBRARIES"
                  "EMBEDDED-FOREIGN-LIBRARY-P"
                  "CLOSE-FOREIGN-LIBRARY" "FOREIGN-SYMBOL-POINTER" "FOREIGN-CALL"
                  "FOREIGN-CALL-BUFFERED"
                  "FOREIGN-CALLBACK" "FOREIGN-CALLBACK-P" "MAKE-CALLBACK"
                  "CALLBACK-POINTER" "FREE-CALLBACK" "CALLBACK-ERROR"))
        "EGCL-FFI")
(define-condition egcl-ffi:ffi-error (simple-error) ())
(defun egcl-ffi:pointerp (value) (egcl::%foreign-memory :pointerp value))
(deftype egcl-ffi:foreign-pointer () '(satisfies egcl-ffi:pointerp))
(defun egcl-ffi:make-pointer (address) (egcl::%foreign-memory :make-pointer address))
(defun egcl-ffi:pointer-address (pointer) (egcl::%foreign-memory :pointer-address pointer))
(defun egcl-ffi:pointer-eq (a b) (egcl::%foreign-memory :pointer-eq a b))
(defun egcl-ffi:null-pointer () (egcl-ffi:make-pointer 0))
(defun egcl-ffi:null-pointer-p (pointer) (= 0 (egcl-ffi:pointer-address pointer)))
(defun egcl-ffi:inc-pointer (pointer bytes) (egcl::%foreign-memory :inc-pointer pointer bytes))
(defun egcl-ffi:foreign-alloc (bytes) (egcl::%foreign-memory :alloc bytes))
(defun egcl-ffi:foreign-free (pointer) (egcl::%foreign-memory :free pointer))
(defun egcl-ffi:mem-ref (pointer type &optional (offset 0))
  (egcl::%foreign-memory :ref pointer type offset))
(defun egcl-ffi:mem-set (value pointer type &optional (offset 0))
  (egcl::%foreign-memory :set pointer type offset value))
(defun (setf egcl-ffi:mem-ref) (value pointer type &optional (offset 0))
  (egcl-ffi:mem-set value pointer type offset))
(defun egcl-ffi:foreign-type-size (type) (egcl::%foreign-memory :type-size type))
(defun egcl-ffi:foreign-type-alignment (type) (egcl::%foreign-memory :type-alignment type))

(defun egcl-ffi:make-shareable-byte-vector (size)
  (make-array size :element-type '(unsigned-byte 8) :initial-element 0))

(defun egcl-ffi:foreign-library-p (value) (egcl::%foreign-library :p value))
(deftype egcl-ffi:foreign-library () '(satisfies egcl-ffi:foreign-library-p))
(defun egcl-ffi:load-foreign-library (path) (egcl::%foreign-library :load path))
(defun egcl-ffi:close-foreign-library (library) (egcl::%foreign-library :close library))
;;; Shared objects carried by the image (bliss-rhsy4). LOAD-FOREIGN-LIBRARY
;;; finds one by SONAME, by the path it was embedded from, or by an extra name,
;;; before it looks at the file system.
(defun egcl-ffi:embed-foreign-library (path &key names)
  (egcl::%foreign-library :embed (namestring path) (mapcar #'namestring names)))
(defun egcl-ffi:embedded-foreign-libraries () (egcl::%foreign-library :embedded))
(defun egcl-ffi:embedded-foreign-library-p (name)
  (egcl::%foreign-library :embedded-p (namestring name)))
(defun egcl-ffi:foreign-symbol-pointer (name &optional library)
  (egcl::%foreign-library :symbol name library))
(defun egcl-ffi:foreign-call (pointer return-type argument-types arguments &optional (fixed-count nil variadic-p))
  (if variadic-p
      (egcl::%ffi-call pointer return-type argument-types arguments fixed-count)
      (egcl::%ffi-call pointer return-type argument-types arguments)))

(defun egcl-ffi:foreign-call-buffered (pointer return-type argument-types argument-buffers result-buffer
                                      &optional (fixed-count nil variadic-p))
  (if variadic-p
      (egcl::%ffi-call-buffered pointer return-type argument-types argument-buffers result-buffer fixed-count)
      (egcl::%ffi-call-buffered pointer return-type argument-types argument-buffers result-buffer)))

;;;; ---------------------------------------------------------------------------
;;;; Embedded CPython: the PY package (spec 2.7.8, bliss-dk3nr)
;;;; ---------------------------------------------------------------------------
;;;;
;;;; Thin wrappers over EGCL::%PY-* primitives, following the same convention as
;;;; the FFI surface above. The indirection is not ceremony: a function named
;;;; PY:TYPEP is reduced to its BARE name by the bytecode lowerer and by the
;;;; FUNCALL fast path, both of which then find CL:TYPEP and answer a different
;;;; question entirely. A %-prefixed internal primitive collides with nothing, and
;;;; defining the PY functions here also gives them real function cells, so
;;;; (mapcar #'py:str objects) and (apply #'py:call ...) work.
;;;;
;;;; The PY package does not use COMMON-LISP, which is what lets IMPORT, TYPE-OF,
;;;; TYPEP and CALL-METHOD keep the names Python gives them.

;;; PYTHON'S OUTPUT REACHES *STANDARD-OUTPUT* (bliss-c4g9u).
;;;
;;; sys.stdout and sys.stderr are redirected into buffers on the Python side; this
;;; is where their contents are written, and it has to be here rather than in Rust
;;; because only here does *STANDARD-OUTPUT* mean what the caller intends -- a
;;; WITH-OUTPUT-TO-STRING or a rebinding in force is respected for free.
;;;
;;; Before this, Python's output was not merely interleaved unpredictably: it was
;;; SILENTLY LOST. CPython block-buffers a non-tty stdout, nothing flushed it, and
;;; the interpreter is usually never finalized, so (py:exec "print('hi')") printed
;;; nothing at all.
;;; Never signals. It runs in UNWIND-PROTECT cleanup on the way out of every entry
;;; point, so an error here would MASK the Python error being unwound -- replacing the
;;; useful report with a confusing one from the machinery that was trying to print it.
(defun py::drain-output ()
  (ignore-errors
    (let ((pair (egcl::%py-drain-output)))
      (when pair
        (let ((out (car pair)) (err (cdr pair)))
          (when (plusp (length out)) (write-string out *standard-output*))
          (when (plusp (length err)) (write-string err *error-output*))))))
  (values))

;;; Every entry point drains on the way out, including when it signals: a Python
;;; traceback's own output, and anything printed before the raise, is exactly what a
;;; reader needs and would otherwise be dropped.
(defmacro py::draining (&body body)
  `(unwind-protect (progn ,@body) (py::drain-output)))

;;; Bring an interpreter up and take it down. Starting is implicit in every other
;;; entry point, so START is only for choosing WHEN the cost is paid; STOP drains
;;; the pending releases first, since a reference released after shutdown would be
;;; a use-after-free.
(defun py:start () (py:exec "pass"))
(defun py:stop () (py::draining (egcl::%py-stop)))

;;; (py:import "numpy") -> the module, as a PY:OBJECT.
(defun py:import (name) (py::draining (egcl::%py-import name)))

;;; (py:exec "print('hello')") -> NIL. A statement, run for its effect.
(defun py:exec (source) (py::draining (egcl::%py-exec source)))

;;; (py:resolve "numpy.mean") -> the object that dotted name names, whether the
;;; segments are modules, attributes, or a builtin.
(defun py:resolve (name) (py::draining (egcl::%py-resolve name)))

;;; (py:call "numpy.mean" a) or (py:call f 1 2) -- the callable may be named or
;;; already in hand.
(defun py:call (callable &rest arguments)
  (py::draining (egcl::%py-call callable arguments)))

;;; (py:call-method x "reshape" 10 20)
(defun py:call-method (object name &rest arguments)
  (py::draining (egcl::%py-call-method object name arguments)))

;;; (py:getattr x "shape"), and settable: (setf (py:getattr x "n") 5).
(defun py:getattr (object name) (py::draining (egcl::%py-getattr object name)))
(defun py:setattr (object name value)
  (py::draining (egcl::%py-setattr object name value)))
(defsetf py:getattr (object name) (value) `(py:setattr ,object ,name ,value))

;;; (py:type-of x) -> the Python TYPE, as an object rather than a name, so it can
;;; be called, compared and asked for its own attributes as Python code would.
(defun py:type-of (object) (py::draining (egcl::%py-type-of object)))

;;; (py:typep x "numpy.ndarray")
(defun py:typep (object class) (py::draining (egcl::%py-typep object class)))

;;; str() and repr(). PY:REPR is what the Lisp printer shows inside
;;; #<PYTHON-OBJECT ...>.
(defun py:str (object) (py::draining (egcl::%py-str object)))
(defun py:repr (object) (py::draining (egcl::%py-repr object)))

;;; (py:export "calculate_price" #'calculate-price) makes a Lisp function callable
;;; from Python by that name:
;;;
;;;   (py:export "add" (lambda (a b) (+ a b)))
;;;   (py:exec "print(add(2, 3))")        =>  5
;;;
;;; The Python callable reaches the Lisp function through a STABLE HANDLE, not a
;;; pointer: the collector moves objects, and a Python callable can outlive any
;;; address. The handle keeps the function alive for the process lifetime -- a
;;; callable can be stored anywhere on the Python side, so there is no moment at
;;; which releasing it would be safe.
;;;
;;; Arguments and the result cross by the same policy as everything else, so a Lisp
;;; error becomes a Python exception rather than unwinding through CPython frames.
(defun py:export (name function) (py::draining (egcl::%py-export name function)))

;;; Flush Python's buffered output without doing anything else -- for a long
;;; computation whose progress prints would otherwise arrive only when it returns.
(defun py:flush () (py::drain-output) (values))

;;; Is this a Python object rather than a converted Lisp value? A number, string,
;;; NIL or T that crossed back is an ordinary Lisp object and answers NIL.
(defun py:objectp (object) (egcl::%py-objectp object))
(deftype py:object () '(satisfies py:objectp))

;;; ---------------------------------------------------------------------------
;;; A Python exception is a Lisp condition (bliss-wq5tw)
;;; ---------------------------------------------------------------------------
;;;
;;; Signalled for every Python raise, so HANDLER-CASE works on it the way it works
;;; on anything else, and so the failure carries its structure rather than a
;;; formatted string: the exception's class, its message, the Python frames, and the
;;; exception object itself, whose attributes are often the useful part (an
;;; HTTPError's status, a KeyError's key).
;;;
;;; FRAMES are (FILE LINE FUNCTION) lists, outermost first -- Python's own order.
;;;
;;; NAMED PY:EXCEPTION, NOT PY:ERROR. Originally that was forced: the condition and
;;; class registries were keyed by a class's BARE name, so a class named PY:ERROR
;;; registered under "ERROR" and REPLACED CL:ERROR for the whole image -- after which
;;; MAKE-CONDITION of any condition recursed until the stack was gone. It took a
;;; SIGSEGV in (make-condition 'c1), a definition with nothing to do with Python, to
;;; find it. That bug is FIXED (bliss-kliz4), so PY:ERROR would be safe now; the name
;;; stays EXCEPTION because it is simply the better word for what this is.
(define-condition py:exception (error)
  ((kind :initarg :kind :initform "PythonError" :reader py:exception-kind)
   (text :initarg :text :initform "" :reader py:exception-text)
   (frames :initarg :frames :initform nil :reader py:exception-frames)
   (object :initarg :object :initform nil :reader py:exception-object)
   ;; FORMAT-CONTROL carries the already-rendered report. It was once the ONLY thing
   ;; EGCL's printer read, because a DEFINE-CONDITION :report was not honoured at all
   ;; (bliss-e5eh6); the :report below is now consulted first and supplies the message,
   ;; so this slot no longer decides what ~A or an uncaught error shows. It is kept
   ;; because the signaller fills it and SIMPLE-CONDITION-FORMAT-CONTROL can read it;
   ;; the two agree by construction.
   (format-control :initarg :format-control :initform nil)
   (format-arguments :initarg :format-arguments :initform nil))
  (:report (lambda (condition stream)
             (format stream "~a: ~a"
                     (py:exception-kind condition) (py:exception-text condition))
             ;; The frames go beneath the message, innermost first, which is the
             ;; direction a reader looks first and the order a Lisp backtrace uses.
             (dolist (frame (reverse (py:exception-frames condition)))
               (format stream "~%  Python  ~a at ~a:~a"
                       (third frame) (first frame) (second frame))))))

;;; The Python half of a mixed-language backtrace, innermost first:
;;;
;;;   0: Lisp    PROCESS-DATA
;;;   1: Lisp    PY:CALL
;;;   2: Python  fit at sklearn/base.py:1389
;;;   3: Python  asarray at numpy/_core/numeric.py:330
;;;
;;; Returned as data rather than printed, so a debugger or a log formatter can
;;; interleave it with the Lisp frames however it presents them.
(defun py:backtrace (condition)
  (mapcar (lambda (frame)
            (list :python (third frame) (first frame) (second frame)))
          (reverse (py:exception-frames condition))))

;;; External, so TYPE-OF and error messages read PY:OBJECT rather than
;;; EGCL-PYTHON::OBJECT -- the nickname is the whole point of the package.
;;; INTERN by name rather than writing '(py:import ...): a quoted list is read
;;; before these are external, and the reader's own symbol for PY:IMPORT is not
;;; necessarily the one in the package table, so the export lands on nothing. Every
;;; other package here exports the same way for the same reason.
(export (mapcar (lambda (name) (intern name "EGCL-PYTHON"))
                '("OBJECT" "OBJECTP" "IMPORT" "EXEC" "RESOLVE" "CALL" "CALL-METHOD"
                  "GETATTR" "SETATTR" "TYPE-OF" "TYPEP" "STR" "REPR" "START" "STOP"
                  "EXCEPTION" "EXCEPTION-KIND" "EXCEPTION-TEXT" "EXCEPTION-FRAMES"
                  "EXCEPTION-OBJECT" "BACKTRACE" "FLUSH" "EXPORT"))
        "EGCL-PYTHON")

;;; Retention is explicit: C may keep the entry after Lisp drops the wrapper.
;;; Retire every C reference/invocation before FREE-CALLBACK. Callback failures
;;; return zero to C, then signal FFI-ERROR after the enclosing foreign call;
;;; CALLBACK-ERROR consumes diagnostic text, also for foreign-thread failures.
(defun egcl-ffi:foreign-callback-p (value) (egcl::%foreign-callback :p value))
(deftype egcl-ffi:foreign-callback () '(satisfies egcl-ffi:foreign-callback-p))
(defun egcl-ffi:make-callback (function return-type argument-types)
  (check-type function function)
  (egcl::%foreign-callback :make function return-type argument-types))
(defun egcl-ffi:callback-pointer (callback) (egcl::%foreign-callback :pointer callback))
(defun egcl-ffi:free-callback (callback) (egcl::%foreign-callback :free callback))
(defun egcl-ffi:callback-error (callback) (egcl::%foreign-callback :error callback))

;;; Copying is deliberate: Lisp storage can move, and upgraded array element
;;; types need not have C layout. The pointer is valid only inside this scope.
(defmacro egcl-ffi:with-pointer-to-vector-data ((pointer vector &optional (type :unsigned-char)) &body body)
  (let ((v (gensym "VECTOR")) (ty (gensym "TYPE")) (ready (gensym "COPIED"))
        (storage (gensym "STORAGE")))
    `(let* ((,v ,vector)
            (,ty ,type)
            (,storage (egcl-ffi:foreign-alloc (egcl::%foreign-memory :vector-size ,v ,ty)))
            (,pointer ,storage)
            (,ready nil))
       ;; Keep all values in the protected form's primary value across cleanup
       ;; (also on bytecode, whose general MV cleanup issue is bliss-pfgq).
       (values-list
         (unwind-protect
             (progn
               (egcl::%foreign-memory :copy-in ,storage ,v ,ty)
               (setf ,ready t)
               (multiple-value-list (progn ,@body)))
           (unwind-protect
               (when ,ready (egcl::%foreign-memory :copy-out ,storage ,v ,ty))
             (egcl-ffi:foreign-free ,storage)))))))

;;; ── CLOS slot-definition metaobjects (bliss-h1mx) ─────────────────────────
;;;
;;; closer-mop needs CLASS-SLOTS to return objects it can hand to the
;;; SLOT-DEFINITION-* accessors. The raw data comes from
;;; EGCL-INTERNAL::%CLASS-SLOT-DESCRIPTORS — one plist per EFFECTIVE slot, in
;;; class-precedence order, built from the same effective_slots_for_class walk
;;; MAKE-INSTANCE uses. The Lisp-visible surface lives here rather than in
;;; cli.rs, per the architecture principle: the interpreter exposes data, the
;;; library shapes it.
;;;
;;; CLASS-SLOTS takes a class DESIGNATOR, like MAKE-INSTANCE and
;;; ALLOCATE-INSTANCE, because FIND-CLASS returns the class NAME here
;;; (bliss-rj5o) — a caller has no separate class object to pass.
;;;
;;; AMOP distinguishes EFFECTIVE from DIRECT slot definitions, and so does this:
;;; CLASS-SLOTS walks the class precedence list, CLASS-DIRECT-SLOTS reports only
;;; the slots the class itself declared. That distinction is not cosmetic —
;;; SLOT-DEFINITION-READERS and -WRITERS are defined by AMOP only on DIRECT slot
;;; definitions, and SBCL signals NO-APPLICABLE-METHOD if you call them on an
;;; effective one. EGCL answers them for both, which is a permissive superset
;;; rather than a different answer.
;;;
;;; One honest limit: SLOT-DEFINITION-TYPE answers T for every slot, because
(defpackage :egcl-mop
  (:use :common-lisp)
  ;; Only names this package OWNS.
  ;;
  ;; CLASS-PRECEDENCE-LIST is one: exporting it here gives the builtin's own
  ;; identity a package that owns it, and the interpreter also dispatches the
  ;; EGCL-MOP spelling, so the exported symbol IS the builtin rather than a
  ;; fresh functionless lookalike. Exporting it WITHOUT that dispatch shadowed
  ;; the real builtin in COMMON-LISP-USER and made (class-precedence-list ...)
  ;; undefined there -- the shape bliss-8eq9b describes. With both in place, a
  ;; bare read of the name in an unrelated package interns that package's own
  ;; symbol, so a library's DEFUN of it no longer redefines ours (#370).
  ;;
  ;; METHOD-SPECIALIZERS is still dispatched only by its bare name and has no
  ;; owner, so it keeps the old behaviour; the same treatment for the remaining
  ;; extension builtins is bliss-7l7cn.
  (:export #:class-slots #:class-direct-slots #:class-precedence-list
           #:class-direct-superclasses #:class-direct-subclasses
           #:class-finalized-p #:finalize-inheritance
           ;; The metaclass of a class named as a superclass before it is
           ;; defined (spec R9.39). A CLASS; not a STANDARD-CLASS, since nothing
           ;; has said yet what its instances look like.
           #:forward-referenced-class
           #:generic-function-methods
           ;; Generic function and method introspection (GitHub #508). The
           ;; METHOD-* readers are real functions here over the builtins of the
           ;; same names, so these symbols are the package's own.
           #:generic-function-name #:generic-function-lambda-list
           #:generic-function-argument-precedence-order
           #:method-specializers #:method-generic-function #:method-lambda-list
           #:specializer-direct-methods #:specializer-direct-generic-functions
           #:compute-applicable-methods-using-classes
           #:eql-specializer-object
           #:method-function
           #:generic-function-method-combination #:find-method-combination
           #:generic-function-method-class #:generic-function-declarations
           #:intern-eql-specializer #:validate-superclass
           #:slot-definition-name #:slot-definition-initargs
           #:slot-definition-initform #:slot-definition-allocation
           #:slot-definition-readers #:slot-definition-writers
           #:slot-definition-type))

;;; COMMON-LISP-USER uses it, so the names stay reachable without qualification
;;; where a person is typing -- and so closer-mop's EGCL arm, which is
;;; (defun class-slots (class) (cl-user::class-slots class)), keeps resolving.
;;; A library package that does (:use :cl) and nothing else does not inherit
;;; them, which is the point of the move (GitHub #370).
(use-package :egcl-mop :common-lisp-user)

;;; These names are NOT ANSI -- CL has no CLASS-SLOTS or SLOT-DEFINITION-NAME --
;;; and they used to be interned in COMMON-LISP, so every package that did
;;; (:use :cl) inherited them. A library's own DEFUN of one then redefined the
;;; IMPLEMENTATION's, process-wide and silently (GitHub #370). They live in
;;; EGCL-MOP now; COMMON-LISP-USER uses that package, so interactive use and
;;; closer-mop's (cl-user::class-slots ...) keep working, while a library
;;; package that only uses CL no longer inherits them.
;;;
;;; DEFCLASS currently discards the :type option (bliss-52ze). T is the correct
;;; default, and wrong for a slot that declared a type.
(defclass egcl-mop::slot-definition ()
  ((name :initarg :name :reader egcl-mop:slot-definition-name)
   (initargs :initarg :initargs :reader egcl-mop:slot-definition-initargs)
   (initform :initarg :initform :reader egcl-mop:slot-definition-initform)
   (allocation :initarg :allocation :reader egcl-mop:slot-definition-allocation)
   (readers :initarg :readers :reader egcl-mop:slot-definition-readers)
   (writers :initarg :writers :reader egcl-mop:slot-definition-writers)))

(defun egcl-mop:class-slots (class)
  "Effective slot definitions of CLASS, in class-precedence order."
  (mapcar (lambda (descriptor)
            (apply #'make-instance 'egcl-mop::slot-definition descriptor))
          (egcl-internal::%class-slot-descriptors class)))

(defun egcl-mop:class-direct-slots (class)
  "Slot definitions CLASS itself declares, excluding inherited slots."
  (mapcar (lambda (descriptor)
            (apply #'make-instance 'egcl-mop::slot-definition descriptor))
          (egcl-internal::%class-slot-descriptors class t)))

(defun egcl-mop:slot-definition-type (slot)
  "Declared type of SLOT. Always T: DEFCLASS discards :type (bliss-52ze)."
  (declare (ignore slot))
  t)

;;; ── EGCL-MOP ──────────────────────────────────────────────────────
;;;
;;; The metaobject-protocol surface docs/egcl-lisp-api.md specifies. A program
;;; that wants these names asks for them -- (:use :egcl-mop), or a package-local
;;; nickname -- rather than inheriting them from COMMON-LISP, which is where the
;;; non-ANSI ones used to live and is why a library's own DEFUN of one
;;; redefined the implementation's (GitHub #370, #371).
;;;
;;; The introspection readers are real functions over the %-builtins, not
;;; name-dispatched builtins re-exported: an imported name-dispatched builtin
;;; forks into an undefined symbol at the call site, which is what made UIOP's
;;; import of the package-local-nickname API fail until those became real
;;; functions too (bliss-8eq9b).
;; (defpackage moved above the definitions it owns)

(defun egcl-mop:class-direct-superclasses (class)
  "The direct superclasses of CLASS, as class metaobjects."
  (egcl-internal::%class-direct-superclasses class))

(defun egcl-mop:class-direct-subclasses (class)
  "The direct subclasses of CLASS, as class metaobjects."
  (egcl-internal::%class-direct-subclasses class))

(defun egcl-mop:generic-function-methods (generic-function)
  "The methods of GENERIC-FUNCTION, as the method objects METHOD-SPECIALIZERS
takes."
  (egcl-internal::%generic-function-methods generic-function))

(defun egcl-mop:class-finalized-p (class)
  "Whether CLASS is finalized. Classes finalize eagerly here, so a defined class
is always finalized and portable code that finalizes before reading the
precedence list runs unchanged. A class is NOT finalized while it, or any class
it inherits from, is only a forward reference -- named as a superclass and never
defined -- because nothing has said yet what slots it has."
  (null (egcl-internal::%forward-referenced-superclass class)))

(defun egcl-mop:finalize-inheritance (class)
  "Return CLASS. Classes finalize eagerly, so this is the no-op AMOP permits --
except that a class cannot be finalized while it, or a class it inherits from,
is still only a forward reference, which is an error (spec R9.39)."
  (let ((pending (egcl-internal::%forward-referenced-superclass class)))
    (when pending
      (error "FINALIZE-INHERITANCE: ~s cannot be finalized while ~s is only a ~
              forward reference: it is named as a superclass but never defined."
             class pending)))
  class)

(defun egcl-mop:intern-eql-specializer (object)
  "The EQL specializer for OBJECT, in the shape METHOD-SPECIALIZERS answers it,
so the two compare with EQUAL as they do on other hosts."
  (list 'eql object))

;;; Generic function and method introspection (GitHub #508): what a library
;;; needs to find every generic function with a method on some class, and to
;;; write a congruent method of its own on each. Read from the registry dispatch
;;; uses, so a method REMOVE-METHOD took out, or a DEFMETHOD replaced, is gone
;;; from every answer.
;;;
;;; The METHOD-* readers answer what the builtins of the same names do, through
;;; %-entry points: those builtins dispatch by bare name, so a library's DEFUN of
;;; METHOD-SPECIALIZERS in a (:use :cl) package still replaces them for every
;;; caller (bliss-7l7cn), but cannot reach these.

(defun egcl-mop:method-specializers (method)
  "The specializers of METHOD's required parameters: class metaobjects, and
(EQL object) for an EQL specializer, as INTERN-EQL-SPECIALIZER makes them."
  (egcl-internal::%method-specializers method))

(defun egcl-mop:method-generic-function (method)
  "The generic function METHOD belongs to, EQ to #'name; NIL once removed."
  (egcl-internal::%method-generic-function method))

(defun egcl-mop:method-lambda-list (method)
  "METHOD's lambda list with the specializers stripped."
  (egcl-internal::%method-lambda-list method))

(defun egcl-mop:generic-function-name (generic-function)
  "The name GENERIC-FUNCTION was defined under: a symbol or a (SETF name) list."
  (egcl-internal::%generic-function-name generic-function))

(defun egcl-mop:generic-function-lambda-list (generic-function)
  "The lambda list of GENERIC-FUNCTION, against which DEFMETHOD checks a method
for congruence: DEFGENERIC's, or for a generic function only DEFMETHOD created,
the one it derived from the first method."
  (egcl-internal::%generic-function-lambda-list generic-function))

(defun egcl-mop:generic-function-argument-precedence-order (generic-function)
  "The required parameters of GENERIC-FUNCTION in the order dispatch compares
them: DEFGENERIC's :argument-precedence-order, or left to right."
  (let* ((lambda-list (egcl-mop:generic-function-lambda-list generic-function))
         (required (loop for parameter in lambda-list
                         until (member parameter lambda-list-keywords)
                         collect parameter))
         (name (egcl-mop:generic-function-name generic-function))
         (order (and (symbolp name)
                     (get name 'egcl::%argument-precedence-order))))
    (if order
        (mapcar (lambda (position) (nth position required)) order)
        required)))

(defun egcl-mop:specializer-direct-methods (specializer)
  "Every method, of any generic function, that has SPECIALIZER among its
specializers. SPECIALIZER is a class, or an EQL specializer from
INTERN-EQL-SPECIALIZER."
  (egcl-internal::%specializer-direct-methods specializer))

(defun egcl-mop:specializer-direct-generic-functions (specializer)
  "The generic functions of the methods SPECIALIZER-DIRECT-METHODS answers,
each once."
  (let ((generic-functions '()))
    (dolist (method (egcl-mop:specializer-direct-methods specializer))
      (let ((generic-function (egcl-mop:method-generic-function method)))
        (when generic-function
          (pushnew generic-function generic-functions))))
    (nreverse generic-functions)))

(defun egcl-mop:compute-applicable-methods-using-classes (generic-function classes)
  "AMOP: the methods of GENERIC-FUNCTION applicable to arguments of CLASSES,
most specific first, and T. When an EQL specializer makes the answer depend on
the arguments themselves rather than their classes, NIL and NIL."
  (values-list (egcl-internal::%compute-applicable-methods-using-classes
                generic-function classes)))

(defun egcl-mop:eql-specializer-object (eql-specializer)
  "The object of an EQL specializer from INTERN-EQL-SPECIALIZER."
  (second eql-specializer))

;;; The rest of the generic-function readers, and METHOD-FUNCTION (GitHub #508).

(defclass egcl-mop::standard-method-combination (method-combination)
  ((name :initarg :name :reader egcl-mop::method-combination-name)
   (options :initarg :options :reader egcl-mop::method-combination-options))
  (:documentation "A method combination as a generic function uses it: the
combination type's NAME, such as STANDARD or +, and the OPTIONS DEFGENERIC gave
it, such as (:MOST-SPECIFIC-LAST). One object per name and options, so two
generic functions combining methods the same way answer EQ objects."))

(defmethod print-object ((combination egcl-mop::standard-method-combination) stream)
  (print-unreadable-object (combination stream :type t)
    (format stream "~S~{ ~S~}" (egcl-mop::method-combination-name combination)
            (egcl-mop::method-combination-options combination))))

(defvar egcl-mop::*method-combinations* (make-hash-table :test 'equal :synchronized t)
  "(NAME . OPTIONS) -> the method combination metaobject for it.")

(defun egcl-mop::intern-method-combination (name options)
  (let ((key (cons name options)))
    (or (gethash key egcl-mop::*method-combinations*)
        (setf (gethash key egcl-mop::*method-combinations*)
              (make-instance 'egcl-mop::standard-method-combination
                             :name name :options options)))))

(defun egcl-mop:find-method-combination (generic-function type-name options)
  "AMOP: the method combination metaobject for the combination TYPE-NAME with
OPTIONS -- the one GENERIC-FUNCTION-METHOD-COMBINATION answers for a generic
function defined with (:method-combination TYPE-NAME . OPTIONS). EGCL knows
STANDARD and the built-in short forms (+, AND, APPEND, LIST, MAX, MIN, NCONC, OR,
PROGN); any other TYPE-NAME is an error."
  (declare (ignore generic-function))
  (unless (member type-name '(standard + and append list max min nconc or progn))
    (error "FIND-METHOD-COMBINATION: no method combination named ~S" type-name))
  (egcl-mop::intern-method-combination type-name options))

(defun egcl-mop:generic-function-method-combination (generic-function)
  "The method combination metaobject GENERIC-FUNCTION's dispatch uses: from
DEFGENERIC's (:method-combination name [order]), else STANDARD. It prints as
#<STANDARD-METHOD-COMBINATION name options...>, and is EQ to what
FIND-METHOD-COMBINATION answers for the same name and options."
  (let ((spec (egcl-internal::%generic-function-method-combination generic-function)))
    (egcl-mop::intern-method-combination (car spec) (cdr spec))))

(defun egcl-mop:generic-function-method-class (generic-function)
  "The class DEFGENERIC named with (:method-class name), else STANDARD-METHOD.
EGCL records the option and answers it here; DEFMETHOD still makes
STANDARD-METHODs whatever it says."
  (find-class (or (egcl-internal::%generic-function-method-class generic-function)
                  'standard-method)))

(defun egcl-mop:generic-function-declarations (generic-function)
  "The declaration specifiers of GENERIC-FUNCTION's DEFGENERIC (declare ...)
options, in order; NIL for a generic function DEFGENERIC gave none, or that only
DEFMETHOD created."
  (egcl-internal::%generic-function-declarations generic-function))

(defvar egcl-mop::*method-functions*
  (make-hash-table :test 'eq :weakness :key :synchronized t)
  "Method -> its METHOD-FUNCTION, so each method answers one function.")

(defun egcl-mop:method-function (method)
  "AMOP: METHOD's function, of two arguments (ARGS NEXT-METHODS). ARGS is the
list of arguments to run METHOD's body on, bound by its lambda list.
NEXT-METHODS is a list of methods, most specific first, that CALL-NEXT-METHOD
in the body runs in turn (with NEXT-METHOD-P answering whether one remains), as
the effective method of a standard generic function would chain primary methods.
The body's values are the function's values. Further arguments are ignored,
as closer-mop's method functions on other hosts take them. Repeated calls answer
the same function."
  (or (gethash method egcl-mop::*method-functions*)
      (progn
        ;; Rejects a non-method before a function is made for it.
        (egcl-mop:method-specializers method)
        (setf (gethash method egcl-mop::*method-functions*)
              (lambda (args next-methods &rest more-args)
                (declare (ignore more-args))
                (egcl-internal::%call-method method args next-methods))))))

(defgeneric egcl-mop:validate-superclass (class superclass)
  (:documentation "Whether CLASS may inherit from SUPERCLASS. T by default; a
metaclass specializes this to accept a superclass of another metaclass."))

(defmethod egcl-mop:validate-superclass (class superclass)
  (declare (ignore class superclass))
  t)

;;; Image lifecycle hooks belong to the implementation; libraries register here.
(defvar egcl-ext::*init-hooks* nil
  "Functions called without arguments, in list order, after image restoration
and process-state initialization, before init files or user code. Hooks are not
called on a cold start. A snapshot of the list is used for each restoration;
a hook error aborts startup.")
(export '(egcl-ext::*init-hooks*) :egcl-ext)

;;; Standard control macros also need real expanders for portable code walkers.
;;; Install these after the bootstrap definitions, whose evaluator fast paths
;;; supply the same operators while this file is being loaded (bliss-sudup).
(defmacro when (test &body body) `(if ,test (progn ,@body) nil))
(defmacro unless (test &body body) `(if ,test nil (progn ,@body)))
(defmacro and (&rest forms)
  (if forms
      (if (cdr forms) `(if ,(car forms) (and ,@(cdr forms)) nil) (car forms))
      t))
(defmacro or (&rest forms)
  (if forms
      (if (cdr forms)
          (let ((value (gensym "OR")))
            `(let ((,value ,(car forms))) (if ,value ,value (or ,@(cdr forms)))))
          (car forms))
      nil))
(defmacro return (&optional value) `(return-from nil ,value))
(defmacro dolist ((var list &optional result) &body body)
  (let ((tail (gensym "TAIL")) (top (gensym "TOP")) (end (gensym "END"))
        (declarations nil))
    (do () ((not (and (consp (car body)) (eq (caar body) 'declare))))
      (setq declarations (cons (car body) declarations)
            body (cdr body)))
    `(block nil
       (let ((,tail ,list))
         (tagbody
            ,top
            (if (endp ,tail) (go ,end))
            (let ((,var (car ,tail)))
              ,@(reverse declarations)
              (tagbody ,@body))
            (setq ,tail (cdr ,tail))
            (go ,top)
            ,end)
         (let ((,var nil)) ,@(reverse declarations) ,result)))))

(defmacro dotimes ((var count &optional result) &body body)
  (let ((limit (gensym "LIMIT")) (counter (gensym "COUNTER"))
        (top (gensym "TOP")) (end (gensym "END")))
    (multiple-value-bind (declarations forms) (%split-declares body)
      `(block nil
         (let ((,limit ,count) (,counter 0))
           (tagbody
              ,top
              (if (>= ,counter ,limit) (go ,end))
              (let ((,var ,counter))
                ,@declarations
                (tagbody ,@forms))
              (setq ,counter (1+ ,counter))
              (go ,top)
              ,end)
           (let ((,var ,counter)) ,@declarations ,result))))))

;; GET-SETF-EXPANSION supplies the place protocol. Built-in stores terminate
;; at EGCL::%SETF so this public expander never expands back into itself.
(defmacro setf (&rest pairs &environment env)
  (let ((tail pairs) (forms nil))
    (do () ((null tail))
      (unless (cdr tail) (error 'program-error))
      (let ((place (egcl::%macroexpand-place (car tail) env))
            (value (cadr tail)))
        (if (%values-place-p place)
            (setq forms (cons `(egcl::%setf-values ,(cdr place) ,value) forms))
            (multiple-value-bind (temps values stores writer reader)
                (get-setf-expansion place env)
              (declare (ignore reader))
              (setq forms
                    (cons `(let* ,(mapcar #'list temps values)
                             (multiple-value-bind ,stores ,value ,writer))
                          forms)))))
      (setq tail (cddr tail)))
    `(progn ,@(nreverse forms))))

;;; Text conversion for syscall buffers, ELF strings, and portable applications.
(defun egcl-ext::string-to-octets (string &key (external-format :utf-8) (start 0) end null-terminate)
  (check-type string string)
  (egcl::%text-codec :encode (subseq string start end) external-format null-terminate))
(defun egcl-ext::octets-to-string (vector &key (external-format :utf-8) (start 0) end)
  (check-type vector (vector (unsigned-byte 8)))
  (egcl::%text-codec :decode (subseq vector start end) external-format nil))
(export '(egcl-ext::string-to-octets egcl-ext::octets-to-string) :egcl-ext)

;;; Package-local nicknames (CDR 10): the EGCL-EXT functions
;;; docs/egcl-lisp-api.md lists, over the registry primitives.  Real
;;; functions rather than name-dispatched builtins, so that a package may
;;; import them -- UIOP's UIOP/PACKAGE-LOCAL-NICKNAMES does, and a
;;; name-dispatched builtin imported that way is forked into an undefined
;;; symbol.  DEFPACKAGE's :LOCAL-NICKNAMES option, and the reader's and
;;; FIND-PACKAGE's use of the nicknames, live in the runtime.
(defun egcl-ext::add-package-local-nickname (local-nickname actual-package
                                             &optional (package *package*))
  "Make LOCAL-NICKNAME designate ACTUAL-PACKAGE from inside PACKAGE only.
Returns PACKAGE.  A PACKAGE-ERROR when LOCAL-NICKNAME already designates
another package there, when ACTUAL-PACKAGE names no package, or when
LOCAL-NICKNAME is CL, COMMON-LISP or KEYWORD."
  (egcl::%add-package-local-nickname local-nickname actual-package package))

(defun egcl-ext::remove-package-local-nickname (local-nickname
                                                &optional (package *package*))
  "Remove LOCAL-NICKNAME from PACKAGE: T if it was a local nickname there,
else NIL."
  (egcl::%remove-package-local-nickname local-nickname package))

(defun egcl-ext::package-local-nicknames (&optional (package *package*))
  "The package-local nicknames of PACKAGE, an alist of (\"NICKNAME\" . package)
sorted by nickname."
  (egcl::%package-local-nicknames package))

(defun egcl-ext::package-locally-nicknamed-by-list (package)
  "The packages that have a package-local nickname for PACKAGE."
  (egcl::%package-locally-nicknamed-by-list package))

(export '(egcl-ext::add-package-local-nickname egcl-ext::remove-package-local-nickname
          egcl-ext::package-local-nicknames egcl-ext::package-locally-nicknamed-by-list)
        :egcl-ext)

;;; Native ordering fences, distinct from a thread rendezvous barrier.
(defun egcl-ext::memory-barrier (&optional (kind :full))
  (egcl::%memory-fence kind))
(defun egcl-ext::load-barrier () (egcl::%memory-fence :read))
(defun egcl-ext::store-barrier () (egcl::%memory-fence :write))
(define-compiler-macro egcl-ext::memory-barrier (&optional (kind :full))
  `(egcl::%memory-fence ,kind))
(define-compiler-macro egcl-ext::load-barrier ()
  '(egcl::%memory-fence :read))
(define-compiler-macro egcl-ext::store-barrier ()
  '(egcl::%memory-fence :write))
(export '(egcl-ext::memory-barrier egcl-ext::load-barrier egcl-ext::store-barrier) :egcl-ext)

(export (intern "IO-TIMEOUT" :egcl-ext) :egcl-ext)

(defun egcl-ext::socket-os-error-errno (condition)
  "The OS error number of an EGCL-EXT:SOCKET-OS-ERROR, or NIL when the failure
had none, as for a host name that did not resolve."
  (slot-value condition 'egcl-ext::errno))
(export (list (intern "SOCKET-OS-ERROR" :egcl-ext) 'egcl-ext::socket-os-error-errno)
        :egcl-ext)

(export (mapcar (lambda (name) (intern name :egcl-ext))
                '("CAS" "ATOMIC-INCF" "ATOMIC-DECF"))
        :egcl-ext)

(in-package :egcl-ext)

(eval-when (:compile-toplevel :load-toplevel :execute)
  (defun egcl::%cas-expansion (access old new)
    (cond
      ((symbolp access)
       `(egcl::%cas-symbol-value ',access ,old ,new))
      ((and (consp access) (eq (car access) 'quote)
            (symbolp (cadr access)) (null (cddr access)))
       `(egcl::%cas-symbol-value ',(cadr access) ,old ,new))
      ((not (consp access))
       (error "CAS does not support the place ~s" access))
      ((eq (car access) 'car)
       `(egcl::%cas-car ,(cadr access) ,old ,new))
      ((eq (car access) 'cdr)
       `(egcl::%cas-cdr ,(cadr access) ,old ,new))
      ((eq (car access) 'svref)
       `(egcl::%cas-svref ,(cadr access) ,(caddr access) ,old ,new))
      ((eq (car access) 'symbol-value)
       `(egcl::%cas-symbol-value ,(cadr access) ,old ,new))
      ((eq (car access) 'symbol-plist)
       `(egcl::%cas-symbol-plist ,(cadr access) ,old ,new))
      ((eq (car access) 'slot-value)
       `(egcl::%cas-slot ,(cadr access) ,(caddr access) ,old ,new))
      ((and (symbolp (car access)) (null (cddr access)))
       `(egcl::%cas-accessor ,(cadr access) ',(car access) ,old ,new))
      (t (error "CAS does not support the place ~s" access))))

  (defmacro cas (place old new &environment env)
    (let ((original-place place)
          (place (egcl::%macroexpand-place place env)))
      (multiple-value-bind (temps values stores writer reader)
          (get-setf-expansion place env)
        (declare (ignore stores writer))
        (let ((expected (gensym "EXPECTED"))
              (replacement (gensym "REPLACEMENT")))
          `(let* (,@(mapcar #'list temps values)
                  (,expected ,old)
                  (,replacement ,new))
             ,(if (symbolp original-place)
                  `(egcl::%cas-symbol-value
                     ',original-place ,expected ,replacement)
                  (egcl::%cas-expansion reader expected replacement)))))))

  (defun egcl::%atomic-update-expansion (operation place delta subtract env)
    (let ((amount (gensym "DELTA")))
      (if (symbolp place)
          `(let ((,amount ,delta))
             (egcl::%atomic-update-symbol ',place ,amount ,subtract))
          (let ((place (egcl::%macroexpand-place place env)))
            (multiple-value-bind (temps values stores writer reader)
                (get-setf-expansion place env)
              (declare (ignore stores writer))
              `(let* (,@(mapcar #'list temps values) (,amount ,delta))
                 ,(cond
                    ((and (consp reader) (eq (car reader) 'svref))
                     `(egcl::%atomic-update-svref
                        ,(cadr reader) ,(caddr reader) ,amount ,subtract))
                    ((and (consp reader) (symbolp (car reader))
                          (not (eq (car reader) 'quote))
                          (null (cddr reader)))
                     `(egcl::%atomic-update-accessor
                        ,(cadr reader) ',(car reader) ,amount ,subtract))
                    (t (error "~a does not support the place ~s"
                              operation place)))))))))

  (defmacro atomic-incf (place &optional (delta 1) &environment env)
    (egcl::%atomic-update-expansion "ATOMIC-INCF" place delta nil env))

  (defmacro atomic-decf (place &optional (delta 1) &environment env)
    (egcl::%atomic-update-expansion "ATOMIC-DECF" place delta t env)))

(in-package :common-lisp)

;;; Owned logical snapshots: no live stack pointers or invented native locals.
(defpackage :egcl-debug (:use :common-lisp)
  (:export :list-backtrace :print-backtrace))
(defun egcl-debug:list-backtrace (&key (count 20) (start 0))
  "Return innermost-first frame plists. Arguments are retained, not deep-copied."
  (check-type count (and fixnum (integer 0)))
  (check-type start (and fixnum (integer 0)))
  (mapcar (lambda (row)
            (list :function (first row)
                  :arguments (second row)
                  :arguments-available-p (third row)
                  :origin (nth (fourth row) '(:interpreted :managed :entry :foreign))))
          (egcl::%debug-backtrace count start nil)))
(defun egcl-debug:print-backtrace (&key (stream *debug-io*) (count 20) (start 0))
  "Print a bounded logical snapshot to STREAM and return NIL."
  (check-type count (and fixnum (integer 0)))
  (check-type start (and fixnum (integer 0)))
  (let ((lines (egcl::%debug-backtrace count start t)))
    (loop for line in lines for index from start
          do (format (or stream *standard-output*) "~&  ~D: ~A~%" index line)))
  nil)

;;; ---------------------------------------------------------------------------
;;; Standard functions that have no interactive environment behind them in a
;;; batch image, plus a few small CLHS accessors (ansi CL-FUNCTION-SYMBOLS.1).

;; BREAK (CLHS 9.2): enter the debugger on a SIMPLE-CONDITION, with
;; *DEBUGGER-HOOK* disabled so a hook cannot intercept it.
(defun break (&optional (format-control "Break") &rest format-arguments)
  (let ((*debugger-hook* nil))
    (invoke-debugger (make-condition 'simple-condition
                                     :format-control format-control
                                     :format-arguments format-arguments)))
  nil)

;; DRIBBLE (CLHS 25.1): there is no session transcript to record; a no-op.
(defun dribble (&optional pathname)
  (declare (ignore pathname))
  (values))

(defun long-site-name () nil)
(defun short-site-name () nil)

(defgeneric describe-object (object stream))
(defmethod describe-object (object stream)
  (format stream "~&~S is an object of type ~S.~%" object (type-of object))
  (values))

(defun egcl::%designated-output-stream (stream)
  (cond ((null stream) *standard-output*)
        ((eq stream t) *terminal-io*)
        (t stream)))

(defun describe (object &optional stream)
  (describe-object object (egcl::%designated-output-stream stream))
  (values))

(defun inspect (object)
  (describe object)
  (values))

(defun egcl::%query-answer (prompt-control prompt-arguments suffix)
  (let ((stream *query-io*))
    (when prompt-control
      (fresh-line stream)
      (apply #'format stream prompt-control prompt-arguments))
    (write-string suffix stream)
    (finish-output stream)
    (string-trim " " (read-line stream))))

(defun y-or-n-p (&optional control &rest arguments)
  (loop
    (let ((answer (egcl::%query-answer control arguments " (y or n) ")))
      (cond ((string-equal answer "y") (return t))
            ((string-equal answer "n") (return nil))
            (t (format *query-io* "~&Please answer y or n.~%"))))))

(defun yes-or-no-p (&optional control &rest arguments)
  (loop
    (let ((answer (egcl::%query-answer control arguments " (yes or no) ")))
      (cond ((string-equal answer "yes") (return t))
            ((string-equal answer "no") (return nil))
            (t (format *query-io* "~&Please answer yes or no.~%"))))))

;; A file stream's external format is the one it was opened with (UTF-8,
;; LATIN-1 or ASCII); every other stream answers :UTF-8, the image's own. A
;; broadcast stream answers for its last component, and :DEFAULT when it has
;; none (CLHS BROADCAST-STREAM; ansi MAKE-BROADCAST-STREAM.8).
(defun stream-external-format (stream)
  (check-type stream stream)
  (if (typep stream 'broadcast-stream)
      (let ((last (car (last (broadcast-stream-streams stream)))))
        (if last (stream-external-format last) :default))
      (egcl::%stream-external-format stream)))

;; FILE-STRING-LENGTH (CLHS 21.2): the octets OBJECT occupies on STREAM, whose
;; external format is UTF-8. A broadcast stream answers for its last
;; component, and 1 when it has none (CLHS BROADCAST-STREAM; ansi
;; BROADCAST-STREAM-STREAMS.3/.4).
(defun file-string-length (stream object)
  (check-type stream stream)
  (let ((text (if (characterp object) (string object) object)))
    (check-type text string)
    (if (typep stream 'broadcast-stream)
        (let ((last (car (last (broadcast-stream-streams stream)))))
          (if last (file-string-length last object) 1))
        (loop for ch across text
              sum (let ((code (char-code ch)))
                    (cond ((< code #x80) 1)
                          ((< code #x800) 2)
                          ((< code #x10000) 3)
                          (t 4)))))))

(defun print-not-readable-object (condition)
  (slot-value condition 'object))

(defun ensure-generic-function (name &rest options
                                &key (lambda-list nil lambda-list-p)
                                     (argument-precedence-order nil precedence-p)
                                &allow-other-keys)
  (declare (ignore options))
  (cond ((and (fboundp name) (typep (fdefinition name) 'generic-function))
         ;; The existing generic function keeps its methods; a new lambda list
         ;; must be congruent with them, a new precedence order re-sorts dispatch.
         (egcl-internal::%ensure-generic-options
          name lambda-list lambda-list-p argument-precedence-order precedence-p)
         (fdefinition name))
        ((fboundp name)
         (error 'program-error
                :format-control "~S already names an ordinary function, macro or special operator."
                :format-arguments (list name)))
        (t (eval `(defgeneric ,name ,lambda-list
                    ,@(when precedence-p
                        `((:argument-precedence-order ,@argument-precedence-order))))))))

(defun invalid-method-error (method format-control &rest format-arguments)
  (error "Invalid method ~S:~%~?" method format-control format-arguments))

(defun method-combination-error (format-control &rest format-arguments)
  (error "Method combination error:~%~?" format-control format-arguments))

;; MAKE-LOAD-FORM-SAVING-SLOTS (CLHS 7.7): a creation form that allocates an
;; instance of the object's class and an initialization form that fills the
;; (bound) slots, defaulting to every slot of the class.
(defun make-load-form-saving-slots (object &key (slot-names nil slot-names-p) environment)
  (declare (ignore environment))
  (let* ((class (class-of object))
         (names (if slot-names-p
                    slot-names
                    (mapcar #'egcl-mop:slot-definition-name
                            (egcl-mop:class-slots class)))))
    (values `(allocate-instance (find-class ',(class-name class)))
            `(progn
               ,@(loop for name in names
                       when (slot-boundp object name)
                         collect `(setf (slot-value ,object ',name)
                                        ',(slot-value object name)))
               ,object))))

;; PPRINT-TAB (CLHS 22.4): outside the pretty printer's logical blocks a tab
;; produces no output; the arguments are still checked.
(defun pprint-tab (kind colnum colinc &optional stream)
  (check-type kind (member :line :section :line-relative :section-relative))
  (check-type colnum (integer 0))
  (check-type colinc (integer 0))
  (egcl::%designated-output-stream stream)
  nil)

;; PPRINT-TABULAR (CLHS 22.4): columns are not laid out yet; the elements are
;; printed like PPRINT-FILL, which is the non-pretty rendering.
;; PPRINT-TABULAR (CLHS 22.2.2): each element starts at a column that is a
;; multiple of TABSIZE from the start of the block; elements that would pass
;; *PRINT-RIGHT-MARGIN* continue on a new line (ansi pprint-tabular.*).
(defun pprint-tabular (stream list &optional (colon-p t) at-sign-p tabsize)
  (declare (ignore at-sign-p))
  ;; A NIL tabsize (what `~v/pprint-tabular/` passes for a NIL argument)
  ;; selects the default of 16.
  (setf tabsize (or tabsize 16))
  (check-type tabsize (integer 0))
  (let ((stream (cond ((null stream) *standard-output*)
                      ((eq stream t) *terminal-io*)
                      (t stream))))
    (if (not (listp list))
        (write list :stream stream)
        (let* ((prefix (if colon-p "(" ""))
               (suffix (if colon-p ")" ""))
               (items (mapcar #'write-to-string list))
               (margin (or *print-right-margin* 72))
               ;; Columns are absolute: the block starts wherever the stream
               ;; already is, and continuation lines indent to the first item.
               (origin (or (egcl::%pprint-native-column stream) 0))
               (start (+ origin (length prefix)))
               (lines '())
               (line (copy-seq prefix))
               (col start))
          (flet ((next-tab (c)
                   (if (zerop tabsize)
                       (1+ c)
                       (+ start (* tabsize (ceiling (- (1+ c) start) tabsize))))))
            (loop for (item . rest) on items
                  ;; A fill-style break: the section after ITEM (its tab
                  ;; padding, or the suffix) must also fit on the line.
                  do (when (and (> col start)
                                (> (if rest
                                       (next-tab (+ col (length item)))
                                       (+ col (length item) (length suffix)))
                                   margin))
                       ;; The tab padding never ends a broken line.
                       (push (string-right-trim " " line) lines)
                       (setf line (make-string start :initial-element #\Space)
                             col start))
                     (setf line (concatenate 'string line item))
                     (incf col (length item))
                     (when rest
                       (let ((next (next-tab col)))
                         (setf line (concatenate 'string line
                                                 (make-string (- next col) :initial-element #\Space))
                               col next)))))
          (push (concatenate 'string line suffix) lines)
          (format stream "~{~a~^~%~}" (nreverse lines))))
    nil))

;;; ---------------------------------------------------------------------------
;;; Typed structures: DEFSTRUCT with (:TYPE LIST) or (:TYPE VECTOR) (CLHS
;;; DEFSTRUCT).  The evaluator hands such a definition to %DEFTYPEDSTRUCT.  The
;;; structure is a plain list or vector, so no class or type is defined; the
;;; layout is kept on the name's plist under EGCL::%TYPED-STRUCT so a later
;;; (:INCLUDE parent) can build on it:
;;;   (:type REP :length N
;;;    :slots ((NAME INDEX DEFAULT-FORM TYPE READ-ONLY) ...)
;;;    :names ((STRUCTURE-NAME . INDEX) ...))     ; the :NAMED markers
(defun egcl::%typed-struct-option (options key)
  "The option whose head is KEY, a bare KEY counting as (KEY); NIL when absent."
  (dolist (option options nil)
    (cond ((eq option key) (return (list key)))
          ((and (consp option) (eq (car option) key)) (return option)))))

(defun egcl::%typed-struct-list-name-p (list index name)
  "True when LIST has NAME at INDEX (the :NAMED marker of a list structure)."
  (loop repeat index
        do (if (consp list) (setq list (cdr list)) (return-from egcl::%typed-struct-list-name-p nil)))
  (and (consp list) (eq (car list) name)))

(defun egcl::%typed-struct-check-form (value-form slot-type)
  "VALUE-FORM wrapped so a value outside SLOT-TYPE signals a TYPE-ERROR whose
datum is the value and whose expected type is the declared one. A :TYPE slot
option is enforced for a typed structure exactly as for a standard one (CLHS
3.4.6): SBCL signals for (make-list-point :x :bad) with an INTEGER slot, and
this used to store :BAD silently because the typed path -- which boot.lisp owns
entirely, eval_defstruct delegating to %DEFTYPEDSTRUCT -- never checked. T
declares no constraint, so it is returned unwrapped and costs nothing."
  (if (eq slot-type t)
      value-form
      (let ((value (gensym "TYPED-SLOT")))
        `(let ((,value ,value-form))
           (check-type ,value ,slot-type)
           ,value))))

(defun egcl::%typed-struct-build-form (rep element-type total names slots values)
  "A form building the sequence: NAMES mark the :NAMED positions, VALUES is an
alist from slot name to the form supplying it."
  (let ((elements (make-list total :initial-element nil)))
    (dolist (entry names)
      (setf (nth (cdr entry) elements) `',(car entry)))
    (dolist (slot slots)
      ;; Enforce the slot's :TYPE here, where both the keyword constructor and
      ;; every BOA constructor pass through.
      (setf (nth (second slot) elements)
            (egcl::%typed-struct-check-form (cdr (assoc (first slot) values))
                                            (fourth slot))))
    (if (eq rep 'list)
        `(list ,@elements)
        `(make-array ,total :element-type ',element-type
                            :initial-contents (list ,@elements)))))

(defun egcl::%typed-struct-boa-constructor (ctor-name lambda-list rep element-type
                                            total names slots)
  "A BOA constructor (CLHS 3.4.6): a parameter naming a slot supplies it; an
&optional or &key parameter without a default takes the slot's default; slots
no parameter names take their defaults."
  (let ((section '&required) (new-lambda-list '()) (bound '()))
    (flet ((slot-default (var)
             (third (find var slots :key #'first))))
      (dolist (item lambda-list)
        (cond ((member item '(&optional &rest &key &aux &allow-other-keys))
               (setq section item)
               (push item new-lambda-list))
              ((eq section '&required)
               (push item bound)
               (push item new-lambda-list))
              ((eq section '&rest)
               (push item bound)
               (push item new-lambda-list))
              ((eq section '&aux)
               (let ((var (if (consp item) (car item) item)))
                 (push var bound)
                 (push (if (consp item) item (list var nil)) new-lambda-list)))
              (t ;; &optional / &key
               (let* ((spec (if (consp item) item (list item)))
                      (var (let ((v (car spec)))
                             (if (consp v) (second v) v)))
                      (has-default (cdr spec)))
                 (push var bound)
                 (push (if has-default
                           spec
                           (list (car spec) (slot-default var)))
                       new-lambda-list)))))
      `(defun ,ctor-name ,(nreverse new-lambda-list)
         ,(egcl::%typed-struct-build-form
           rep element-type total names slots
           (mapcar (lambda (slot)
                     (cons (first slot)
                           (if (member (first slot) bound) (first slot) (third slot))))
                   slots))))))

(defmacro egcl::%deftypedstruct (name-and-options &rest slot-descriptions)
  (let* ((name (if (consp name-and-options) (car name-and-options) name-and-options))
         (options (if (consp name-and-options) (cdr name-and-options) nil))
         (slot-descriptions (if (stringp (car slot-descriptions))
                                (cdr slot-descriptions)
                                slot-descriptions))
         (type-spec (second (egcl::%typed-struct-option options :type)))
         (rep (if (consp type-spec) (car type-spec) type-spec))
         (element-type (if (consp type-spec) (second type-spec) t))
         (named (not (null (egcl::%typed-struct-option options :named))))
         (initial-offset (or (second (egcl::%typed-struct-option options :initial-offset)) 0))
         (include-option (egcl::%typed-struct-option options :include))
         (parent (second include-option))
         (parent-layout (and parent (get parent 'egcl::%typed-struct)))
         (conc-option (egcl::%typed-struct-option options :conc-name))
         (conc-prefix (cond ((null conc-option) (concatenate 'string (string name) "-"))
                            ((null (second conc-option)) "")
                            (t (string (second conc-option)))))
         (predicate-option (egcl::%typed-struct-option options :predicate))
         (copier-option (egcl::%typed-struct-option options :copier))
         (constructor-options
           (remove-if-not (lambda (option)
                            (or (eq option :constructor)
                                (and (consp option) (eq (car option) :constructor))))
                          options))
         (base-length (if parent-layout (getf parent-layout :length) 0))
         (prefix (+ base-length initial-offset))
         (name-index (and named prefix))
         (first-slot (+ prefix (if named 1 0)))
         (inherited
           (when parent-layout
             (mapcar (lambda (slot)
                       (let ((override (find (first slot) (cddr include-option)
                                             :key (lambda (o) (if (consp o) (car o) o)))))
                         (if override
                             (list (first slot) (second slot)
                                   (if (consp override) (second override) nil)
                                   (fourth slot) (fifth slot))
                             slot)))
                     (getf parent-layout :slots))))
         (own (loop for description in slot-descriptions
                    for index from first-slot
                    collect (if (consp description)
                                (list (first description) index (second description)
                                      (getf (cddr description) :type t)
                                      (getf (cddr description) :read-only))
                                (list description index nil t nil))))
         (slots (append inherited own))
         (total (+ first-slot (length own)))
         (names (append (and parent-layout (getf parent-layout :names))
                        (and named (list (cons name name-index)))))
         (layout (list :type type-spec :length total :slots slots :names names))
         (forms '()))
    (unless (member rep '(list vector))
      (error "DEFSTRUCT ~S: :TYPE must be LIST or VECTOR, not ~S" name type-spec))
    (when (and parent (null parent-layout))
      (error "DEFSTRUCT ~S: included structure ~S is not a typed structure" name parent))
    (when (and (not named) (second predicate-option))
      (error "DEFSTRUCT ~S: an unnamed typed structure cannot have a predicate" name))
    ;; Constructors: the default MAKE-NAME, each (:constructor name [boa]),
    ;; none for (:constructor nil).
    (let ((constructors
            (if (null constructor-options)
                (list (list (intern (concatenate 'string "MAKE-" (string name)))))
                (loop for option in constructor-options
                      unless (and (consp option) (cdr option) (null (second option)))
                        collect (if (or (eq option :constructor) (null (cdr option)))
                                    (list (intern (concatenate 'string "MAKE-" (string name))))
                                    (cdr option))))))
      (dolist (constructor constructors)
        (if (cdr constructor)
            (push (egcl::%typed-struct-boa-constructor
                   (first constructor) (second constructor) rep element-type total names slots)
                  forms)
            (push `(defun ,(first constructor)
                       (&key ,@(mapcar (lambda (slot) (list (first slot) (third slot))) slots))
                     ,(egcl::%typed-struct-build-form
                       rep element-type total names slots
                       (mapcar (lambda (slot) (cons (first slot) (first slot))) slots)))
                  forms))))
    ;; Accessors read and write the slot's position in the sequence.
    (dolist (slot slots)
      (let ((accessor (intern (concatenate 'string conc-prefix (symbol-name (first slot)))))
            (index (second slot)))
        (push `(defun ,accessor (object) (elt object ,index)) forms)
        (unless (fifth slot)
          (push `(defun (setf ,accessor) (value object)
                   (setf (elt object ,index)
                         ,(egcl::%typed-struct-check-form 'value (fourth slot))))
                forms))))
    ;; The predicate exists only for a :NAMED structure.
    (let ((predicate (cond ((not named) nil)
                           ((or (null predicate-option) (null (cdr predicate-option)))
                            (intern (concatenate 'string (string name) "-P")))
                           (t (second predicate-option)))))
      (when predicate
        (push (if (eq rep 'list)
                  `(defun ,predicate (object)
                     (and (listp object)
                          (egcl::%typed-struct-list-name-p object ,name-index ',name)))
                  `(defun ,predicate (object)
                     (and (vectorp object)
                          (> (length object) ,name-index)
                          (eq (aref object ,name-index) ',name))))
              forms)))
    (let ((copier (cond ((or (null copier-option) (null (cdr copier-option)))
                         (intern (concatenate 'string "COPY-" (string name))))
                        (t (second copier-option)))))
      (when copier
        (push `(defun ,copier (object) (copy-seq object)) forms)))
    `(progn
       ,@(nreverse forms)
       (setf (get ',name 'egcl::%typed-struct) ',layout)
       ',name)))

;;; ---------------------------------------------------------------------------
;;; TRACE / UNTRACE (CLHS 25.1): a traced function's global definition is
;;; replaced by a wrapper that reports the call and its values on
;;; *TRACE-OUTPUT*; UNTRACE restores the original.  (TRACE) lists the traced
;;; names, (UNTRACE) untraces everything.
(defvar egcl::*traced-functions* '()
  "Alist of (FUNCTION-NAME . ORIGINAL-DEFINITION) for the traced functions.")

(defun egcl::%traced-names ()
  (mapcar #'car egcl::*traced-functions*))

(defun egcl::%trace-functions (names)
  (dolist (name names (egcl::%traced-names))
    (cond ((assoc name egcl::*traced-functions* :test #'equal)
           (warn "~S is already traced." name))
          ((not (fboundp name))
           (warn "~S is not a defined function; not traced." name))
          (t
           (let ((original (fdefinition name)))
             (push (cons name original) egcl::*traced-functions*)
             (setf (fdefinition name)
                   (lambda (&rest arguments)
                     (format *trace-output* "~&0: (~S~{ ~S~})~%" name arguments)
                     (let ((results (multiple-value-list (apply original arguments))))
                       (format *trace-output* "~&0: ~S returned~{ ~S~}~%" name results)
                       (values-list results)))))))))

(defun egcl::%untrace-functions (names)
  (dolist (name (or names (egcl::%traced-names)))
    (let ((entry (assoc name egcl::*traced-functions* :test #'equal)))
      (cond (entry
             (setf (fdefinition name) (cdr entry))
             (setq egcl::*traced-functions* (remove entry egcl::*traced-functions*)))
            (t (warn "~S is not traced." name)))))
  (egcl::%traced-names))

(defmacro trace (&rest names)
  (if names
      `(egcl::%trace-functions ',names)
      '(egcl::%traced-names)))

(defmacro untrace (&rest names)
  `(egcl::%untrace-functions ',names))

;;; ---------------------------------------------------------------------------
;;; WITH-LOCKED-HASH-TABLE (bliss-im7wv). Each table gets its own recursive
;;; mutex on first use, recorded in a weak table so it lives as long as the
;;; table. As in SBCL, the bodies of WITH-LOCKED-HASH-TABLE on one table exclude
;;; each other; every individual operation on a :SYNCHRONIZED table is atomic
;;; as before. The table's internal lock is a spin lock whose holder may not run
;;; Lisp code, so it cannot be held across BODY, and accesses outside any
;;; WITH-LOCKED-HASH-TABLE are not excluded by it.
;;; ---------------------------------------------------------------------------

(defvar egcl::*hash-table-locks* nil
  "Hash table -> its WITH-LOCKED-HASH-TABLE mutex, created on first use.")

(defvar egcl::*hash-table-locks-mutex* nil
  "Guards creating an entry of EGCL::*HASH-TABLE-LOCKS*, created on first use.")

(defun egcl::%hash-table-lock (table)
  (unless (hash-table-p table)
    (error 'type-error :datum table :expected-type 'hash-table))
  ;; Created lazily, so the boot image holds no native mutex; CAS keeps one.
  (unless egcl::*hash-table-locks-mutex*
    (egcl-ext:cas egcl::*hash-table-locks-mutex* nil
                  (egcl-thread:make-mutex :name "hash table locks")))
  (egcl-thread:with-mutex (egcl::*hash-table-locks-mutex*)
    (let ((locks (or egcl::*hash-table-locks*
                     (setq egcl::*hash-table-locks*
                           (make-hash-table :test 'eq :weakness :key :synchronized t)))))
      (or (gethash table locks)
          (setf (gethash table locks)
                (egcl-thread:make-mutex :name "hash table" :recursive t))))))

(defmacro egcl-ext:with-locked-hash-table ((table) &body body)
  "Run BODY holding TABLE's recursive lock, excluding every other
WITH-LOCKED-HASH-TABLE on TABLE for its dynamic extent."
  `(egcl-thread:with-mutex ((egcl::%hash-table-lock ,table))
     ,@body))

(export 'egcl-ext:with-locked-hash-table :egcl-ext)
