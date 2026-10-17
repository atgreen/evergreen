// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A generic function dispatched on a class metaobject must not replay the
//! effective method the FIRST such call decided (bliss-xv37u).
//!
//! The dispatch cache keys each argument on `egcl_stdlib::class_of` plus a
//! built-in type discriminator. Both are blind to which class a class
//! metaobject is: a DEFCLASS or DEFSTRUCT class IS its own name symbol, so
//! `class_of` answers the SYMBOL class for every one of them, and a built-in
//! class is a negative fixnum, so it answers FIXNUM — the same answers an
//! unrelated symbol or integer gives. Two arguments needing different
//! effective methods produced a bit-identical key.
//!
//! It is the ARGUMENT keys that collide, not method selection, so one method
//! is enough to show it: `(std-only (find-class 'user))` answered `:STD`, and
//! then `(std-only (find-class 'a-struct))` answered `:STD` too, from a cache
//! entry recording a method that does not apply to it at all.
//!
//! This is the same shape as bliss-qvv3, where a VECTOR method swallowed
//! strings; the `builtin` field of the key exists because of that bug. Class
//! metaobjects were missed because the collision is invisible while a class is
//! an immediate.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", BIN, "--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
}

fn expect(stdout: &str, rows: &[&str]) {
    for row in rows {
        assert!(stdout.contains(row), "missing {row:?} in:\n{stdout}");
    }
}

/// The decisive property: the answer must not depend on which class went
/// first. Each ordering uses its own generic so that the two orderings cannot
/// influence each other.
#[test]
fn the_answer_does_not_depend_on_which_class_was_dispatched_first() {
    let stdout = run(r#"
      (defclass uc () ())
      (defstruct st x)
      (defgeneric g1 (c))
      (defmethod g1 ((c standard-class)) :std)
      (defmethod g1 ((c structure-class)) :struct)
      (format t "STRUCT-FIRST ~s ~s~%" (g1 (find-class 'st)) (g1 (find-class 'uc)))
      (defgeneric g2 (c))
      (defmethod g2 ((c standard-class)) :std)
      (defmethod g2 ((c structure-class)) :struct)
      (format t "USER-FIRST ~s ~s~%" (g2 (find-class 'uc)) (g2 (find-class 'st)))
      ;; Three metaclasses at once, and the same questions from inside a
      ;; compiled function: a check in the tree-walking evaluator does not
      ;; cover cli/bytecode.rs.
      (defgeneric g3 (c))
      (defmethod g3 ((c standard-class)) :std)
      (defmethod g3 ((c built-in-class)) :builtin)
      (defmethod g3 ((c structure-class)) :struct)
      (format t "THREE-WAY ~s ~s ~s~%"
              (g3 (find-class 'uc)) (g3 (find-class 'integer)) (g3 (find-class 'st)))
      (defun compiled-three-way ()
        (list (g3 (find-class 'uc)) (g3 (find-class 'integer)) (g3 (find-class 'st))))
      (format t "COMPILED-THREE-WAY ~s~%" (compiled-three-way))
      (format t "ORDER-OK~%")
    "#);
    expect(&stdout, &[
        "STRUCT-FIRST :STRUCT :STD",
        "USER-FIRST :STD :STRUCT",
        "THREE-WAY :STD :BUILTIN :STRUCT",
        "COMPILED-THREE-WAY (:STD :BUILTIN :STRUCT)",
        "ORDER-OK",
    ]);
}

/// One method is enough: a cached APPLICABLE decision was replayed for an
/// argument the method does not apply to.
#[test]
fn a_cached_decision_is_not_replayed_for_an_inapplicable_argument() {
    let stdout = run(r#"
      (defclass uc () ())
      (defstruct st x)
      (defun applies (fn c) (handler-case (funcall fn c) (error () :none)))
      (defgeneric only-std (c))
      (defmethod only-std ((c standard-class)) :std)
      ;; The user class first, which is what primed the wrong entry.
      (format t "PRIMED ~s~%" (applies #'only-std (find-class 'uc)))
      (format t "THEN-STRUCT ~s~%" (applies #'only-std (find-class 'st)))
      (format t "THEN-BUILTIN ~s~%" (applies #'only-std (find-class 'integer)))
      (format t "AND-USER-AGAIN ~s~%" (applies #'only-std (find-class 'uc)))
      (format t "REPLAY-OK~%")
    "#);
    expect(&stdout, &[
        "PRIMED :STD",
        "THEN-STRUCT :NONE",
        "THEN-BUILTIN :NONE",
        "AND-USER-AGAIN :STD",
        "REPLAY-OK",
    ]);
}

/// A built-in class metaobject is a negative fixnum, so it shared a key with
/// an ordinary integer. This is the cross-kind case: the cached decision came
/// from a value that is not a class at all.
#[test]
fn a_built_in_class_does_not_share_a_key_with_an_ordinary_integer() {
    let stdout = run(r#"
      (defgeneric h1 (x))
      (defmethod h1 ((x integer)) :int)
      (defmethod h1 ((x built-in-class)) :bic)
      (format t "INT-FIRST ~s ~s~%" (h1 7) (h1 (find-class 'integer)))
      (defgeneric h2 (x))
      (defmethod h2 ((x integer)) :int)
      (defmethod h2 ((x built-in-class)) :bic)
      (format t "BIC-FIRST ~s ~s~%" (h2 (find-class 'integer)) (h2 7))
      (format t "BUILTIN-OK~%")
    "#);
    expect(&stdout, &[
        "INT-FIRST :INT :BIC",
        "BIC-FIRST :BIC :INT",
        "BUILTIN-OK",
    ]);
}

/// The finer key is computed only for generics whose methods specialize on a
/// metaclass, because it costs a class-registry lookup per argument. Pin that
/// ordinary dispatch — the overwhelming majority — still works, including the
/// cases the `builtin` discriminator was added for (bliss-qvv3).
#[test]
fn ordinary_dispatch_is_unaffected() {
    let stdout = run(r#"
      (defclass animal () ())
      (defclass dog (animal) ())
      (defgeneric speak (x))
      (defmethod speak ((x animal)) :animal)
      (defmethod speak ((x dog)) :dog)
      (defmethod speak ((x string)) :string)
      (defmethod speak ((x vector)) :vector)
      (defmethod speak ((x integer)) :int)
      (defmethod speak ((x symbol)) :symbol)
      (defmethod speak ((x cons)) :cons)
      (format t "INSTANCES ~s ~s~%" (speak (make-instance 'dog)) (speak (make-instance 'animal)))
      ;; A string must not take the VECTOR method, nor a vector the STRING one.
      (format t "VEC-STR ~s ~s~%" (speak "abc") (speak #(1 2 3)))
      (format t "STR-VEC ~s ~s~%" (speak "def") (speak #(4 5)))
      (format t "IMMEDIATES ~s ~s ~s~%" (speak 5) (speak 'zork) (speak '(1 2)))
      ;; Repeat calls to exercise the cache rather than only cold misses.
      (format t "REPEAT ~s~%"
              (list (speak "abc") (speak #(1)) (speak 5) (speak 'z)
                    (speak (make-instance 'dog))))
      (format t "ORDINARY-OK~%")
    "#);
    expect(&stdout, &[
        "INSTANCES :DOG :ANIMAL",
        "VEC-STR :STRING :VECTOR",
        "STR-VEC :STRING :VECTOR",
        "IMMEDIATES :INT :SYMBOL :CONS",
        "REPEAT (:STRING :VECTOR :INT :SYMBOL :DOG)",
        "ORDINARY-OK",
    ]);
}
