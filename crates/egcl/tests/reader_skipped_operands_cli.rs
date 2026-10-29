// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn recursive_reader_operands_skip_zero_values() {
    let program = r###"
      (defstruct skipped-reader-structure value)
      (defun check-reader-object (source expected)
        (multiple-value-bind (value position) (read-from-string source)
          (assert (equalp value expected))
          (assert (= position (length source)))))
      (check-reader-object "'#+nil ignored (3 4)" '(quote (3 4)))
      (check-reader-object "#'#+nil ignored car" '(function car))
      (assert (= (eval (read-from-string "`#+nil ignored 17")) 17))
      (assert (equal (eval (read-from-string "`(,#+nil ignored (+ 1 2))")) '(3)))
      (assert (equal (eval (read-from-string "`(,@#+nil ignored (list 1 2))")) '(1 2)))
      (check-reader-object "#1a#+nil ignored (3 4)" #(3 4))
      (check-reader-object "#c#+nil ignored (3 4)" #c(3 4))
      (check-reader-object "#c(#+nil ignored 3 #+nil ignored 4)" #c(3 4))
      (check-reader-object "#p#+nil ignored \"file.txt\"" #p"file.txt")
      (assert (= (skipped-reader-structure-value
                  (read-from-string "#s#+nil ignored (skipped-reader-structure :value 9)")) 9))
      (check-reader-object "(1 . #+nil ignored 2)" '(1 . 2))
      (let ((x (read-from-string "#1=#+nil ignored (a #1#)"))) (assert (eq x (second x))))
      (check-reader-object "'#| first |# #+nil ignored #-nil 19" '(quote 19))
      ;; A silent final list/vector element still permits its delimiter.
      (check-reader-object "(1 #+nil ignored)" '(1))
      (check-reader-object "#(1 #+nil ignored)" #(1))
      (assert (null (read-from-string "#+nil ignored" nil nil)))
      (assert (handler-case (progn (read-from-string "'#+nil ignored") nil)
                (end-of-file () t)))
      ;; Custom zero-value handlers obey the same recursive-read rule.
      (let ((*readtable* (copy-readtable nil)))
        (set-macro-character #\! (lambda (stream char) (declare (ignore stream char)) (values)))
        (check-reader-object "'!!(3 4)" '(quote (3 4))))
      (write-line "READER-SKIPPED-OPERANDS-OK")
    "###;
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.env("EGCL_FORCE_TIER", tier);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command
            .args(["--no-init", "--eval", program])
            .output()
            .expect("reader skipped operands");
        assert!(
            output.status.success(),
            "{tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("READER-SKIPPED-OPERANDS-OK"));
    }
}
