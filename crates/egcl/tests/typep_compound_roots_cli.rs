// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! TYPEP's OR, AND and COMPLEX branches test one operand after another, and a
//! SATISFIES predicate or DEFTYPE expander in an earlier operand can allocate
//! and move the later ones. They were read from an unrooted copy of the operand
//! list, so a later `(MEMBER obj)` was tested through a stale pointer
//! (bliss-m0mwt).
//!
//! The specifiers are built just before each TYPEP call, so they are still in
//! the nursery when the predicate's allocation triggers an ordinary minor
//! collection. Under EGCL_GC_STRESS each forced collection would promote them
//! before TYPEP runs, and nothing would move. With EGCL_GC_POISON the stale
//! copy reads freed memory: before the fix this printed `HITS (5 0 3)`.
use std::process::Command;

#[test]
fn compound_typep_operands_survive_a_collection_in_an_earlier_operand() {
    let program = r#"(progn
      (defun churn (x)
        (declare (ignore x))
        (let ((l nil)) (dotimes (i 400000) (push (list i i) l)))
        nil)
      (deftype churned () (churn 0) 'null)
      (let ((or-hits 0) (and-hits 0) (complex-hits 0))
        (dotimes (i 5)
          (let ((obj (list :fresh i)))
            (when (typep obj (list 'or '(satisfies churn) (list 'member obj)))
              (incf or-hits))
            (when (typep obj (list 'and (list 'not 'churned) (list 'member obj)))
              (incf and-hits))
            (when (typep (complex (+ 1d0 i) 2d0)
                         (list 'complex (list 'or 'churned 'double-float)))
              (incf complex-hits))))
        (format t "~&HITS ~s~%" (list or-hits and-hits complex-hits))))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_GC_POISON", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("HITS (5 5 5)"), "{stdout}\n{stderr}");
}
