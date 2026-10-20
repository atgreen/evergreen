// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! EQ / NULL / NOT, the TYPEP predicates and LENGTH on a simple string must
//! give the same answers at every tier, on every architecture.
//!
//! WHY THIS FILE IS ARCHITECTURE-AGNOSTIC ON PURPOSE: like
//! `bitwise_shift_tiers.rs`, it runs one program at all four tiers and checks
//! the answers, so it exercises whichever emitter the test binary was built
//! for -- emit.rs on x86-64, emit_a64.rs on AArch64, emit_s390x.rs on System Z.
//!
//! These shapes earn their own file because of how expensive refusing them is.
//! An emitter that lacks an arm does not merely run the operation slowly: it
//! declines the opcode, which costs the WHOLE FUNCTION its T2 code.
//!
//!   * `GenericEq` is what `speculate.rs` rewrites NULL and NOT into, as well
//!     as EQ itself. AArch64 was the only backend without an arm for it, so on
//!     that port every function containing an EQ, a NULL or a NOT was barred
//!     from the top tier. The same omission on System Z was measured as
//!     costing "most of them" (bliss-pig52).
//!   * `TypeCheck` covers FIXNUMP / SYMBOLP / INTEGERP / STRINGP / PACKAGEP /
//!     HASH-TABLE-P / NULL / BOOLEAN.
//!
//! The cases below are chosen for the ways a hand-written arm goes wrong
//! rather than for coverage of CL semantics:
//!
//!   * NIL and T must answer true to SYMBOLP. They are symbols, but they do
//!     not carry the symbol tag, so an arm that only tests the tag answers NIL
//!     for both.
//!   * INTEGERP must hold for a bignum as well as a fixnum, which is the one
//!     predicate here that has to read a heap object's header type-id byte.
//!     That byte is at offset 7 on a little-endian target and offset 0 on
//!     big-endian System Z; reading the wrong end answers from an unrelated
//!     header field, and does so consistently enough to look deliberate.
//!   * STRINGP must answer T for a string with a fill pointer, which is a
//!     COMPLEX_ARRAY rather than one of the two simple layouts. Every emitter
//!     DECLINES the STRING class for exactly this reason (bliss-c02n); the
//!     case is here to catch an "optimisation" that stops declining it.
//!   * (EQ x x) on a bignum is pointer equality on a heap object, which must
//!     still hold after the value has moved.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Warmed, so the functions are compiled and speculated rather than tree-walked.
const PROGRAM: &str = r#"
  (defun f-eq   (a b) (eq a b))
  (defun f-null (a)   (null a))
  (defun f-not  (a)   (not a))
  (defun f-fixp (a)   (typep a 'fixnum))
  (defun f-intp (a)   (typep a 'integer))
  (defun f-symp (a)   (symbolp a))
  (defun f-strp (a)   (stringp a))
  (defun f-nullp(a)   (typep a 'null))
  (defun f-boolp(a)   (typep a 'boolean))
  (defun f-len  (a)   (length a))
  (dotimes (i 3000)
    (f-eq 1 1) (f-null nil) (f-not nil) (f-fixp 1) (f-intp 1)
    (f-symp 'a) (f-strp "x") (f-nullp nil) (f-boolp t) (f-len "abc"))
  (let ((big (* 1000000000000 1000000000000))
        (fp  (make-array 5 :element-type 'character
                           :fill-pointer 3 :initial-element #\a)))
    (format t "EQ ~s ~s ~s ~s~%"
            (f-eq 'a 'a) (f-eq 'a 'b) (f-eq nil nil) (f-eq big big))
    (format t "NULL ~s ~s ~s~%" (f-null nil) (f-null t) (f-null '(1)))
    (format t "NOT ~s ~s ~s~%" (f-not nil) (f-not t) (f-not 0))
    (format t "FIXP ~s ~s ~s~%" (f-fixp 7) (f-fixp big) (f-fixp 'a))
    (format t "INTP ~s ~s ~s~%" (f-intp 7) (f-intp big) (f-intp "x"))
    (format t "SYMP ~s ~s ~s ~s~%" (f-symp 'a) (f-symp nil) (f-symp t) (f-symp 7))
    (format t "STRP ~s ~s ~s~%" (f-strp "ab") (f-strp fp) (f-strp 7))
    (format t "NULLP ~s ~s~%" (f-nullp nil) (f-nullp t))
    (format t "BOOLP ~s ~s ~s~%" (f-boolp t) (f-boolp nil) (f-boolp 7))
    (format t "LEN ~s ~s ~s~%" (f-len "") (f-len "abc") (f-len fp)))
"#;

/// Every line the program must print, at every tier.
const EXPECTED: [&str; 10] = [
    // A symbol is EQ to itself; two symbols are not; NIL is EQ to NIL; and a
    // bignum is EQ to itself by pointer, across any relocation.
    "EQ T NIL T T",
    "NULL T NIL NIL",
    // 0 is true in CL, so (not 0) is NIL.
    "NOT T NIL NIL",
    "FIXP T NIL NIL",
    // The bignum case: this is the one that reads the header type-id byte.
    "INTP T T NIL",
    // NIL and T are symbols without carrying the symbol tag.
    "SYMP T T T NIL",
    // A fill-pointer string is a COMPLEX_ARRAY, not a simple layout.
    "STRP T T NIL",
    "NULLP T NIL",
    "BOOLP T T NIL",
    // LENGTH of a fill-pointer string is the fill pointer, not the capacity.
    "LEN 0 3 3",
];

#[test]
fn equality_and_type_predicates_agree_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        for line in EXPECTED {
            assert!(
                stdout.lines().any(|l| l.trim() == line),
                "tier={tier}: missing {line:?} in:\n{stdout}\n{stderr}"
            );
        }
    }
}

#[test]
fn equality_survives_a_collection_between_the_operands() {
    // EQ on heap objects is pointer equality, and the AArch64 arm selects on a
    // register compare. If either operand were left unrooted across the
    // allocation between them, the surviving copy would be stale and the
    // comparison would answer from a moved-from address. Allocating hard
    // between the two reads is what makes that visible.
    let program = r#"
      (defun f-eq (a b) (eq a b))
      (defun churn () (let ((x nil)) (dotimes (i 200) (setq x (cons i x))) (length x)))
      (let ((big (* 1000000000000 1000000000000))
            (sym (gensym))
            (ok t))
        (dotimes (i 2000)
          (unless (and (f-eq big big) (f-eq sym sym) (not (f-eq sym big)))
            (setq ok nil))
          (churn))
        (format t "STABLE ~s~%" ok))
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        let out = Command::new(BIN)
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .expect("run egcl");
        let stdout = String::from_utf8_lossy(&out.stdout);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "tier={tier}: {stdout}\n{stderr}");
        assert!(
            stdout.lines().any(|l| l.trim() == "STABLE T"),
            "tier={tier}: EQ disagreed across a collection:\n{stdout}\n{stderr}"
        );
    }
}
