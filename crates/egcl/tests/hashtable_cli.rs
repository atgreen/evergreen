// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! End-to-end hash-table protocol tests through the full CLI (bliss-jtc.8),
//! including the boot.lisp WITH-HASH-TABLE-ITERATOR macro. Each program is a
//! single top-level form whose value `--eval` echoes.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn with_hash_table_iterator_sums_values() {
    let out = eval(
        "(let ((h (make-hash-table)) (total 0)) \
           (setf (gethash :a h) 10) (setf (gethash :b h) 20) \
           (with-hash-table-iterator (nx h) \
             (loop (multiple-value-bind (more k v) (nx) k \
                     (if more (setq total (+ total v)) (return total))))))",
    );
    assert_eq!(out, "30");
}

#[test]
fn maphash_dispatches_an_interpreted_lambda() {
    let out = eval(
        "(let ((h (make-hash-table)) (s 0)) \
           (setf (gethash 1 h) 5) (setf (gethash 2 h) 7) \
           (maphash (lambda (k v) k (setq s (+ s v))) h) s)",
    );
    assert_eq!(out, "12");
}

#[test]
fn sxhash_is_equal_consistent() {
    assert_eq!(
        eval("(eql (sxhash (list 1 2 3)) (sxhash (list 1 2 3)))"),
        "T"
    );
}

/// bliss-jtc.22 / bliss-cpje: an EQUAL hash table keyed by a movable object
/// (a CLOS instance, hashed by address) must still find its live keys after the
/// moving GC relocates them. Before the rehash-on-move fix, gethash probed the
/// bucket for the key's NEW address while the entry sat in its OLD-address
/// bucket, so lookups silently missed — which broke ASDF's visited-actions and
/// blocked real-library self-host loads. GC stress relocates the keys between
/// insertion and lookup; all 40 must still be found.
#[test]
fn equal_hash_instance_keys_survive_moving_gc() {
    let out = Command::new(BIN)
        .args([
            "--no-init",
            "--eval",
            "(progn (defclass k () ()) \
               (let ((h (make-hash-table :test 'equal :size 8192)) (keys nil)) \
                 (dotimes (i 40) \
                   (let ((key (cons i (make-instance 'k)))) \
                     (push key keys) (setf (gethash key h) i))) \
                 (dotimes (i 60) (make-instance 'k)) \
                 (let ((hits 0)) \
                   (dolist (key keys) (when (nth-value 1 (gethash key h)) (incf hits))) \
                   hits)))",
        ])
        .env("EGCL_GC_STRESS", "1")
        // Exercise the hash operations under stress without re-testing prelude startup.
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed; stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        "40",
        "all instance keys must be found after GC relocation (bliss-jtc.22)"
    );
}

#[test]
fn large_tables_match_equivalent_numeric_and_structural_keys() {
    assert_eq!(
        eval(
            r##"(progn
      (dolist (case '((eql "100000000000000000000" "100000000000000000000")
                     (eql "-100000000000000000000" "-100000000000000000000")
                     (eql "100000000000000000000/3" "100000000000000000000/3")
                     (eql "1.5d0" "1.5d0")
                     (equal "(1 2 3)" "(1 2 3)")
                     (equalp "#(1 2 3)" "#(1.0 2.0 3.0)")
                     (equalp "#\\A" "#\\a")))
        (let ((h (make-hash-table :test (first case) :size 8192))
              (a (read-from-string (second case)))
              (b (read-from-string (third case))))
          (setf (gethash a h) 42)
          (assert (= (gethash b h) 42))
          (setf (gethash b h) 43)
          (assert (= (hash-table-count h) 1))
          (assert (= (gethash a h) 43))
          (assert (remhash b h))))
      :ok)"##
        ),
        ":OK"
    );
}

/// A table must find every key its test function calls equal, and no other.
/// The tables used to carry their own EQUAL/EQUALP, which had drifted from the
/// predicates: an EQUALP table missed 1d0 for 1, 0.5 for 1/2, a fill-pointer
/// string, an adjustable vector and an equal structure, while EQL and EQUAL
/// tables (and EQL itself) took -0d0 for 0d0.
#[test]
fn tables_agree_with_their_test_predicates() {
    assert_eq!(
        eval(
            r##"(progn
      (defstruct ht-point x y)
      (defun fill-pointer-string (s)
        (make-array (length s) :element-type 'character :initial-contents s
                               :fill-pointer (length s)))
      (defun table-agrees (test a b)
        (let ((h (make-hash-table :test test)))
          (setf (gethash a h) :hit)
          (assert (eq (and (funcall test a b) t) (and (gethash b h) t))
                  () "~S ~S ~S: predicate ~S, table ~S" test a b (funcall test a b) (gethash b h))
          (and (funcall test a b) t)))
      (let ((a (make-hash-table)) (b (make-hash-table)))
        (setf (gethash 1 a) "x" (gethash 1 b) "X")
        (list
         ;; EQUALP: numbers by =, strings and characters ignoring case, arrays
         ;; by elements whatever their representation, structures by slots.
         (table-agrees 'equalp 1 1d0)
         (table-agrees 'equalp 1.5f0 1.5d0)
         (table-agrees 'equalp 1/2 0.5)
         (table-agrees 'equalp 1/2 0.5d0)
         (table-agrees 'equalp (expt 2 70) (float (expt 2 70) 1d0))
         (table-agrees 'equalp (1+ (expt 2 53)) (float (1+ (expt 2 53)) 1d0))
         (table-agrees 'equalp #c(1 2) #c(1.0 2.0))
         (table-agrees 'equalp 1 #c(1.0 0.0))
         (table-agrees 'equalp 0 -0d0)
         (table-agrees 'equalp "ABC" (fill-pointer-string "abc"))
         (table-agrees 'equalp (vector 1 2)
                       (make-array 2 :initial-contents '(1 2) :adjustable t))
         (table-agrees 'equalp "abc" (vector #\a #\B #\c))
         (table-agrees 'equalp #*101 (vector 1 0 1))
         (table-agrees 'equalp (make-ht-point :x 1 :y "a") (make-ht-point :x 1 :y "A"))
         (table-agrees 'equalp a b)
         ;; EQUAL: strings by characters however they are stored; numbers by EQL.
         (table-agrees 'equal "abc" (fill-pointer-string "abc"))
         (table-agrees 'equal 0d0 -0d0)
         (table-agrees 'equal 0.0 -0.0)
         ;; EQL: same type and representation.
         (table-agrees 'eql 0d0 -0d0)
         (table-agrees 'eql (expt 2 70) (expt 2 70))
         (table-agrees 'eql 1/3 (/ 2 6)))))"##
        ),
        "(T T T T T NIL T T T T T T T T T T NIL NIL NIL T T)"
    );
}

/// EQUAL compares a non-string, non-bit array by identity, so changing an
/// element changes neither its EQUAL identity nor its SXHASH. Hashing the
/// elements of a fill-pointer vector lost the key as soon as one changed.
#[test]
fn equal_table_keeps_a_mutated_general_vector_key() {
    assert_eq!(
        eval(
            "(let ((h (make-hash-table :test 'equal)) \
                   (v (make-array 3 :adjustable t :fill-pointer 3 :initial-element 0))) \
               (setf (gethash v h) :found) \
               (let ((before (sxhash v))) \
                 (setf (aref v 0) 99) \
                 (list (gethash v h) (= before (sxhash v)))))"
        ),
        "(:FOUND T)"
    );
}

/// Threads may read one unsynchronized table at once, as in SBCL. After a
/// collection moves its cons keys, each reader that touched the table used to
/// rebuild its buckets in place, freeing the ones the other reader was probing:
/// two compiled readers crashed on every run while a third thread collected.
#[test]
fn concurrent_readers_survive_a_moving_gc() {
    let out = eval(CONCURRENT_READERS);
    assert_eq!(out, "(42 42)");
}

const CONCURRENT_READERS: &str = "(progn \
   (defparameter *reader-keys* (loop for i below 300 collect (list i))) \
   (defparameter *reader-table* (make-hash-table :test 'eq)) \
   (loop for key in *reader-keys* for i from 0 \
         do (setf (gethash key *reader-table*) i)) \
   (defun read-every-key () \
     (loop repeat 20 do \
       (loop for key in *reader-keys* for i from 0 \
             unless (eql (gethash key *reader-table*) i) \
               do (error \"lost key ~a\" i))) \
     42) \
   (let ((one (egcl-thread:make-thread 'read-every-key)) \
         (two (egcl-thread:make-thread 'read-every-key))) \
     (loop repeat 200 do (egcl-ext:gc)) \
     (list (egcl-thread:join-thread one) (egcl-thread:join-thread two))))";

/// GitHub #518: MAKE-HASH-TABLE dropped `:synchronized`, so every such table
/// was unlocked. The option must reach the table however the call is made,
/// including from a compiled function.
#[test]
fn make_hash_table_honours_synchronized() {
    let out = eval(
        "(progn \
           (defun make-locked () (make-hash-table :test 'equal :synchronized t)) \
           (dotimes (i 20) (make-locked)) \
           (mapcar #'egcl-ext:hash-table-synchronized-p \
                   (list (make-hash-table :synchronized t) \
                         (funcall #'make-hash-table :synchronized t) \
                         (apply #'make-hash-table '(:test equalp :synchronized t)) \
                         (make-locked) \
                         (make-hash-table :synchronized nil) \
                         (make-hash-table))))",
    );
    assert_eq!(out, "(T T T T NIL NIL)");
}

/// GitHub #518: threads that miss and then store into one synchronized table
/// must leave exactly one entry per key, each with its own value. Unlocked,
/// this lost writes and kept hundreds of duplicate vector keys per run.
#[test]
fn concurrent_writers_keep_a_synchronized_table_consistent() {
    let out = eval(CONCURRENT_WRITERS);
    assert_eq!(out, "(0 0)");
}

const CONCURRENT_WRITERS: &str = "(progn \
   (defun intern-key (table k) \
     (multiple-value-bind (v found) (gethash (vector k) table) \
       (if found v (setf (gethash (vector k) table) (+ k 2))))) \
   (let ((extra 0) (wrong 0)) \
     (dotimes (round 3) \
       (let* ((table (make-hash-table :test 'equalp :synchronized t)) \
              (threads (loop for r below 4 collect \
                         (let ((offset (* r 7))) \
                           (egcl-thread:make-thread \
                            (lambda () \
                              (dotimes (c 4000) \
                                (intern-key table (mod (+ offset c) 1500))))))))) \
         (mapc #'egcl-thread:join-thread threads) \
         (incf extra (- (hash-table-count table) 1500)) \
         (maphash (lambda (k v) (unless (eql v (+ (aref k 0) 2)) (incf wrong))) table))) \
     (list extra wrong)))";

/// EGCL-EXT:WITH-LOCKED-HASH-TABLE (bliss-im7wv) excludes other bodies on the
/// same table, so a read-modify-write inside it is atomic. Without the lock,
/// four threads of 2000 increments each kept about 2500. It is recursive,
/// releases on a non-local exit, and rejects a non-table.
#[test]
fn with_locked_hash_table_serializes_bodies() {
    let out = eval(
        "(flet ((hammer (table) \
                  (let ((threads \
                          (loop repeat 4 collect \
                            (egcl-thread:make-thread \
                              (lambda () \
                                (dotimes (i 2000) \
                                  (egcl-ext:with-locked-hash-table (table) \
                                    (let ((v (gethash :k table 0))) \
                                      (when (zerop (mod i 7)) (egcl-thread:thread-yield)) \
                                      (setf (gethash :k table) (1+ v)))))))))) \
                    (mapc #'egcl-thread:join-thread threads) \
                    (gethash :k table)))) \
           (let ((h (make-hash-table))) \
             (list (hammer (make-hash-table :synchronized t)) \
                   (hammer (make-hash-table)) \
                   (egcl-ext:with-locked-hash-table (h) \
                     (egcl-ext:with-locked-hash-table (h) \
                       (setf (gethash 1 h) :in))) \
                   (catch 'out \
                     (egcl-ext:with-locked-hash-table (h) (throw 'out :thrown))) \
                   (egcl-ext:with-locked-hash-table (h) :reacquired) \
                   (handler-case (egcl-ext:with-locked-hash-table (42) :no) \
                     (type-error () :type-error)))))",
    );
    assert_eq!(out, "(8000 8000 :IN :THROWN :REACQUIRED :TYPE-ERROR)");
}
