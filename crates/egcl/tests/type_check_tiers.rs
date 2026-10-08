// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Type predicates must answer identically at every tier, on every
//! architecture, and the compiled ones must actually reach T2 (bliss-nlpd4).
//!
//! Like bitwise_shift_tiers, this is deliberately architecture-agnostic: it
//! runs the same program at all four tiers against the same expected answers,
//! so it checks whichever emitter the binary was built for. `TypeCheck` was the
//! one opcode a decline histogram over real code (the bootstrap, ASDF, the
//! Gabriel benchmarks) found the s390x emitter refusing; SYMBOL-NAME and the
//! LOOP keyword helper stayed at T1 for it. The header's type id is the byte
//! at offset 7 on little-endian x86-64 and offset 0 on big-endian System Z, so a
//! wrong byte here would still assemble, run, and answer NIL for every heap
//! predicate; the cross-tier comparison is what would catch it.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Every predicate shape the emitters compile inline, plus the ones they must
/// decline (CONSP, LISTP, STRINGP on a complex string) so a decline that turned
/// into a wrong inline answer shows up as a tier difference.
const PROGRAM: &str = r#"
  (defun p-fixnum (x) (typep x 'fixnum))
  (defun p-symbol (x) (symbolp x))
  (defun p-integer (x) (integerp x))
  (defun p-string (x) (stringp x))
  (defun p-package (x) (packagep x))
  (defun p-hash (x) (hash-table-p x))
  (defun p-null (x) (null x))
  (defun p-boolean (x) (typep x 'boolean))
  (defun p-cons (x) (consp x))
  (defun p-list (x) (listp x))
  (defvar *samples*
    (list 1 -7 (expt 2 70) 'a nil t "abc" (make-array 3 :element-type 'character :fill-pointer 2 :initial-element #\x)
          (find-package :cl) (make-hash-table) (cons 1 2) 1.5 #\c (vector 1 2)))
  (dotimes (i 3000)
    (dolist (x *samples*)
      (p-fixnum x) (p-symbol x) (p-integer x) (p-string x) (p-package x)
      (p-hash x) (p-null x) (p-boolean x) (p-cons x) (p-list x)))
  (dolist (x *samples*)
    (format t "~a ~a ~a ~a ~a ~a ~a ~a ~a ~a~%"
            (p-fixnum x) (p-symbol x) (p-integer x) (p-string x) (p-package x)
            (p-hash x) (p-null x) (p-boolean x) (p-cons x) (p-list x)))
  (format t "TIERS ~a ~a ~a ~a~%"
          (egcl-ext:function-tier 'p-fixnum) (egcl-ext:function-tier 'p-symbol)
          (egcl-ext:function-tier 'p-integer) (egcl-ext:function-tier 'p-package))
"#;

const EXPECTED: &[&str] = &[
    "T NIL T NIL NIL NIL NIL NIL NIL NIL",     // 1
    "T NIL T NIL NIL NIL NIL NIL NIL NIL",     // -7
    "NIL NIL T NIL NIL NIL NIL NIL NIL NIL",   // 2^70 bignum
    "NIL T NIL NIL NIL NIL NIL NIL NIL NIL",   // a
    "NIL T NIL NIL NIL NIL T T NIL T",         // nil
    "NIL T NIL NIL NIL NIL NIL T NIL NIL",     // t
    "NIL NIL NIL T NIL NIL NIL NIL NIL NIL",   // "abc"
    "NIL NIL NIL T NIL NIL NIL NIL NIL NIL",   // complex string with fill pointer
    "NIL NIL NIL NIL T NIL NIL NIL NIL NIL",   // package
    "NIL NIL NIL NIL NIL T NIL NIL NIL NIL",   // hash table
    "NIL NIL NIL NIL NIL NIL NIL NIL T T",     // cons
    "NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL", // 1.5
    "NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL", // #\c
    "NIL NIL NIL NIL NIL NIL NIL NIL NIL NIL", // vector
];

fn run(tier: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .output()
        .expect("run egcl");
    assert!(
        out.status.success(),
        "tier={tier}: {}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn type_predicates_agree_at_every_tier() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let stdout = run(tier);
        // Keep the ten-field predicate rows: the TIERS line and the lone NIL
        // the --eval runner prints for the last form are not rows.
        let lines: Vec<&str> = stdout
            .lines()
            .filter(|line| line.split(' ').count() == 10)
            .collect();
        assert_eq!(lines, EXPECTED, "tier={tier}\n{stdout}");
    }
}

/// STRINGP is not in this list on purpose: a complex string is a COMPLEX_ARRAY,
/// so the emitters decline the STRING class rather than answer NIL where the
/// other tiers answer T (bliss-c02n). The inline ones must install at T2.
#[test]
fn inline_predicates_reach_t2() {
    let stdout = run("t2");
    let tiers = stdout
        .lines()
        .find(|line| line.starts_with("TIERS"))
        .expect("tier line");
    assert_eq!(tiers, "TIERS 2 2 2 2", "{stdout}");
}
