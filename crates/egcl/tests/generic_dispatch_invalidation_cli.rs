// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled callers of generic functions keep a dispatch cache per generic
//! (bliss-gg3i1.7.5). Every change that can alter a cached decision must be
//! seen by the next call from an already-compiled caller: adding, removing and
//! redefining methods (including in the middle of a running loop), class
//! redefinition, CHANGE-CLASS, EQL specializers, NO-APPLICABLE-METHOD,
//! shadowed primaries, keyword and arity checks, a replaced function binding,
//! a redefined method combination, and the slot-access protocol. The
//! transcript must be identical in every tier and under GC stress.

use std::process::Command;

const PROGRAM: &str = r#"
(defclass inv-a () ((x :initarg :x :initform 1 :accessor inv-x)))
(defclass inv-b (inv-a) ())
(defclass inv-c () ())
(defstruct inv-s (v 7))

(defmacro check (form expected)
  `(let ((got ,form))
     (unless (equal got ,expected)
       (error "CHECK FAILED: ~s => ~s, expected ~s" ',form got ,expected))
     (format t "~&~s => ~s~%" ',form got)))

(defun warm (fn arg &optional (n 60))
  (let ((last nil)) (dotimes (i n) (setf last (funcall fn arg))) last))

;; 1. ADD-METHOD / REMOVE-METHOD mid-loop through a compiled caller.
(defgeneric inv-kind (o))
(defmethod inv-kind ((o inv-a)) :a)
(defun call-kind (o) (inv-kind o))
(defun loop-kind (o n) (let (r) (dotimes (i n) (push (inv-kind o) r)) (car r)))
(defparameter *a* (make-instance 'inv-a))
(defparameter *b* (make-instance 'inv-b))
(check (loop-kind *b* 100) :a)
(defmethod inv-kind ((o inv-b)) :b)
(check (loop-kind *b* 100) :b)
(check (loop-kind *a* 100) :a)
(defparameter *removed* (find-method #'inv-kind nil (list (find-class 'inv-b))))
(remove-method #'inv-kind *removed*)
(check (loop-kind *b* 100) :a)
(add-method #'inv-kind *removed*)
(check (loop-kind *b* 100) :b)
;; Mid-loop: the method set changes while the compiled loop is running.
(defun loop-kind-changing (o n)
  (let (r)
    (dotimes (i n)
      (when (= i 50) (eval '(defmethod inv-kind ((o inv-b)) :b-mid)))
      (push (inv-kind o) r))
    (list (car (last r)) (car r))))
(check (second (loop-kind-changing *b* 100)) :b-mid)

;; 2. A redefined method body replaces the cached effective method.
(defgeneric inv-body (o))
(defmethod inv-body ((o inv-a)) 1)
(defun loop-body (o n) (let ((s 0)) (dotimes (i n) (incf s (inv-body o))) s))
(check (loop-body *a* 100) 100)
(defmethod inv-body ((o inv-a)) 2)
(check (loop-body *a* 100) 200)

;; 3. Class redefinition changes applicability.
(defgeneric inv-redef (o))
(defmethod inv-redef ((o inv-a)) :inherits-a)
(defun loop-redef (o n)
  (let (r) (dotimes (i n) (setf r (handler-case (inv-redef o) (error () :no-method)))) r))
(defparameter *c* (make-instance 'inv-c))
(check (loop-redef *c* 100) :no-method)
(defclass inv-c (inv-a) ())
(check (loop-redef *c* 100) :inherits-a)
(defclass inv-c () ())
(check (loop-redef *c* 100) :no-method)

;; 4. CHANGE-CLASS gives the same object a new dispatch class.
(defgeneric inv-cc (o))
(defmethod inv-cc ((o inv-a)) :a)
(defmethod inv-cc ((o inv-b)) :b)
(defun loop-cc (o n) (let (r) (dotimes (i n) (setf r (inv-cc o))) r))
(defparameter *m* (make-instance 'inv-b))
(check (loop-cc *m* 100) :b)
(change-class *m* 'inv-a)
(check (loop-cc *m* 100) :a)
(change-class *m* 'inv-b)
(check (loop-cc *m* 100) :b)

;; 5. EQL specializers on immediates, with class fallbacks, and an EQL method
;;    added after the class method was cached for that value.
(defgeneric inv-eql (k))
(defmethod inv-eql ((k (eql :x))) :eql-x)
(defmethod inv-eql ((k (eql 3))) :eql-3)
(defmethod inv-eql ((k (eql #\z))) :eql-z)
(defmethod inv-eql ((k symbol)) :symbol)
(defmethod inv-eql ((k integer)) :integer)
(defmethod inv-eql ((k t)) :t)
(defun loop-eql (keys n)
  (let (r) (dotimes (i n) (setf r (mapcar #'inv-eql keys))) r))
(defun loop-eql-direct (n)
  (let (r) (dotimes (i n) (setf r (list (inv-eql :x) (inv-eql :y) (inv-eql 3) (inv-eql 4)
                                        (inv-eql #\z) (inv-eql #\q) (inv-eql "s"))))
    r))
(check (loop-eql-direct 100) '(:eql-x :symbol :eql-3 :integer :eql-z :t :t))
(defmethod inv-eql ((k (eql :y))) :eql-y)
(defmethod inv-eql ((k (eql 4))) :eql-4)
(check (loop-eql-direct 100) '(:eql-x :eql-y :eql-3 :eql-4 :eql-z :t :t))
(remove-method #'inv-eql (find-method #'inv-eql nil (list '(eql :x))))
(check (loop-eql-direct 100) '(:symbol :eql-y :eql-3 :eql-4 :eql-z :t :t))
;; An EQL specializer on a heap object (a list) keeps the uncached path.
(defparameter *key* (list 1 2))
(defmethod inv-eql ((k (eql *key*))) :eql-list)
(check (loop-eql (list *key* (list 1 2) :y) 100) '(:eql-list :t :eql-y))
(check (loop-eql-direct 100) '(:symbol :eql-y :eql-3 :eql-4 :eql-z :t :t))

;; 6. No applicable method, then one appears.
(defgeneric inv-nam (o))
(defun loop-nam (o n)
  (let (r) (dotimes (i n) (setf r (handler-case (inv-nam o) (error () :error)))) r))
(check (loop-nam *a* 100) :error)
(defmethod inv-nam ((o inv-a)) :found)
(check (loop-nam *a* 100) :found)
(check (loop-nam 42 100) :error)

;; 7. Shadowed primaries: a most specific method without CALL-NEXT-METHOD is
;;    the whole effective method; adding CALL-NEXT-METHOD later combines them.
(defgeneric inv-chain (o))
(defmethod inv-chain ((o inv-a)) (list :a))
(defmethod inv-chain ((o inv-b)) (list :b))
(defun loop-chain (o n) (let (r) (dotimes (i n) (setf r (inv-chain o))) r))
(check (loop-chain *b* 100) '(:b))
(defmethod inv-chain ((o inv-b)) (cons :b (call-next-method)))
(check (loop-chain *b* 100) '(:b :a))
(defmethod inv-chain :around ((o inv-b)) (cons :around (call-next-method)))
(defmethod inv-chain :before ((o inv-a)) (setf (inv-x o) (1+ (inv-x o))))
(let ((before (inv-x *b*)))
  (check (loop-chain *b* 100) '(:around :b :a))
  (check (- (inv-x *b*) before) 100))

;; 8. Keyword arguments are still checked on a cached dispatch.
(defgeneric inv-key (o &key))
(defmethod inv-key ((o inv-a) &key (k 1)) k)
(defun loop-key (o n) (let (r) (dotimes (i n) (setf r (inv-key o :k 2))) r))
(defun call-bad-key (o) (handler-case (inv-key o :bogus 1) (error () :bad-key)))
(check (loop-key *a* 100) 2)
(check (warm #'call-bad-key *a*) :bad-key)
(check (loop-key *a* 100) 2)

;; 9. Arity is still checked once the dispatch is cached.
(defun call-arity () (handler-case (funcall 'inv-kind) (error () :arity)))
(defun call-arity-2 (o) (handler-case (funcall 'inv-kind o o) (error () :arity)))
(check (warm (lambda (x) (declare (ignore x)) (call-arity)) nil) :arity)
(check (warm #'call-arity-2 *a*) :arity)
(check (loop-kind *a* 100) :a)

;; 10. Replacing the generic's function binding redirects compiled callers.
(defgeneric inv-swap (o))
(defmethod inv-swap ((o t)) :generic)
(defun loop-swap (o n) (let (r) (dotimes (i n) (setf r (inv-swap o))) r))
(check (loop-swap 1 100) :generic)
(fmakunbound 'inv-swap)
(defun inv-swap (o) (declare (ignore o)) :function)
(check (loop-swap 1 100) :function)
(fmakunbound 'inv-swap)
(defgeneric inv-swap (o))
(defmethod inv-swap ((o integer)) :generic-again)
(check (loop-swap 1 100) :generic-again)

;; 11. Structure, built-in and two-argument dispatch.
(defgeneric inv-two (p q))
(defmethod inv-two ((p inv-a) (q inv-a)) :aa)
(defmethod inv-two ((p inv-b) (q inv-a)) :ba)
(defmethod inv-two ((p inv-a) (q inv-b)) :ab)
(defmethod inv-two ((p inv-s) q) (list :struct (inv-s-v p) (if (stringp q) :string :other)))
(defmethod inv-two ((p string) (q vector)) :string-vector)
(defmethod inv-two ((p vector) (q vector)) :vector-vector)
(defun loop-two (pairs n)
  (let (r) (dotimes (i n) (setf r (loop for (p q) in pairs collect (inv-two p q)))) r))
(check (loop-two (list (list *a* *a*) (list *b* *a*) (list *a* *b*) (list (make-inv-s) "x")
                       (list (make-inv-s :v 9) 1) (list "s" "t") (list (vector 1) "t"))
                 100)
       '(:aa :ba :ab (:struct 7 :string) (:struct 9 :other) :string-vector :vector-vector))

;; 12. Obsolete instances still dispatch on their (updated) class.
(defgeneric inv-obs (o))
(defmethod inv-obs ((o inv-a)) (inv-x o))
(defun loop-obs (o n) (let (r) (dotimes (i n) (setf r (inv-obs o))) r))
(defparameter *o* (make-instance 'inv-a :x 5))
(check (loop-obs *o* 100) 5)
(make-instances-obsolete 'inv-a)
(check (loop-obs *o* 100) 5)
(defclass inv-a () ((x :initarg :x :initform 1 :accessor inv-x) (y :initform 2)))
(check (loop-obs *o* 100) 5)
(check (slot-value *o* 'y) 2)

;; 13. Redefining the generic function itself (DEFGENERIC with a new
;;     method combination) discards cached decisions.
(defgeneric inv-comb (o) (:method-combination list))
(defmethod inv-comb list ((o inv-a)) :a)
(defmethod inv-comb list ((o inv-b)) :b)
(defun loop-comb (o n) (let (r) (dotimes (i n) (setf r (inv-comb o))) r))
(check (loop-comb *b* 100) '(:b :a))
(defgeneric inv-comb (o) (:method-combination list :most-specific-last))
(check (loop-comb *b* 100) '(:a :b))

;; 14. Slot readers and SLOT-VALUE from methods and compiled code.
(defgeneric inv-slot (o))
(defmethod inv-slot ((o inv-a)) (slot-value o 'x))
(defun loop-slot (o n) (let ((s 0)) (dotimes (i n) (incf s (inv-slot o))) s))
(defun loop-reader (o n) (let ((s 0)) (dotimes (i n) (incf s (inv-x o))) s))
(defparameter *sa* (make-instance 'inv-a :x 3))
(check (loop-slot *sa* 100) 300)
(check (loop-reader *sa* 100) 300)
(defmethod egcl-mop:slot-value-using-class :around ((class standard-class) (object inv-a) slot)
  (let ((v (call-next-method))) (if (numberp v) (* 10 v) v)))
(check (loop-slot *sa* 100) 3000)
(check (loop-reader *sa* 100) 3000)
(remove-method #'egcl-mop:slot-value-using-class
               (find-method #'egcl-mop:slot-value-using-class '(:around)
                            (list (find-class 'standard-class) (find-class 'inv-a) (find-class t))))
(check (loop-slot *sa* 100) 300)
(check (loop-reader *sa* 100) 300)
(write-line "GENERIC-DISPATCH-INVALIDATION-OK")
"#;

fn run(tier: Option<&str>, stress: bool) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", PROGRAM]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_PROFILING_DISABLED",
        "EGCL_GC_DISABLE",
        "EGCL_GC_STRESS",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_POISON",
    ] {
        command.env_remove(name);
    }
    if let Some(tier) = tier {
        command.env("EGCL_FORCE_TIER", tier);
    }
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    let output = command.output().expect("run generic dispatch invalidation");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success() && stdout.contains("GENERIC-DISPATCH-INVALIDATION-OK"),
        "tier={tier:?}, stress={stress}, status={:?}\n{stdout}\n{stderr}",
        output.status
    );
    stdout
}

#[test]
fn compiled_generic_calls_see_every_dispatch_change_in_every_tier() {
    let reference = run(None, false);
    for tier in ["interp", "t0", "t1", "t2"] {
        assert_eq!(run(Some(tier), false), reference, "tier {tier}");
    }
}

#[test]
fn compiled_generic_calls_see_every_dispatch_change_under_gc_stress() {
    assert_eq!(run(None, true), run(None, false));
}
