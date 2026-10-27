// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
(declaim (notinline constraint-predicate constraint-branch constraint-unconditional))
(defun constraint-predicate (x) (typep x 'fixnum))
(defun constraint-branch (x)
  (if (typep x 'fixnum)
      (+ x 10)
      (if (typep x 'double-float) (+ x 0.5d0)
          (if (stringp x) (length x) (+ x 10)))))
(defun constraint-unconditional (x)
  (let ((predicate (typep x 'fixnum)))
    ;; Predicate result is live: DCE cannot hide an invalid operand refinement.
    (cons predicate (+ x 1))))
(defun constraint-check (x predicate branch unconditional)
  (assert (eq (constraint-predicate x) predicate))
  (assert (eql (constraint-branch x) branch))
  (if (eq unconditional :type-error)
      (assert (eq :type-error
        (handler-case (progn (constraint-unconditional x) :no-error)
          (type-error () :type-error))))
      (let ((result (constraint-unconditional x)))
        (assert (eq (car result) predicate))
        (assert (eql (cdr result) unconditional))))
  t)
(defun constraint-warm ()
  (constraint-check 0 t 10 1)
  (constraint-check 1 t 11 2)
  (constraint-check 2.5d0 nil 3.0d0 3.5d0)
  (constraint-check (ash 1 80) nil (+ (ash 1 80) 10) (+ (ash 1 80) 1))
  (constraint-check "abc" nil 3 :type-error))
(dotimes (i 40) (constraint-warm))
"#;

const NATIVE_READY: &str = r#"
(loop repeat 100
      until (and (= 2 (egcl-ext:function-tier 'constraint-predicate))
                 (= 2 (egcl-ext:function-tier 'constraint-branch))
                 (= 2 (egcl-ext:function-tier 'constraint-unconditional)))
      do (constraint-warm) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'constraint-predicate)))
(assert (= 2 (egcl-ext:function-tier 'constraint-branch)))
(assert (= 2 (egcl-ext:function-tier 'constraint-unconditional)))
"#;

const CHECK: &str = r#"
(constraint-warm)
(dolist (x (list 0 1 2.5d0 (ash 1 80) "abc"))
  (format t "CONSTRAINT ~S ~S ~S ~S~%" x (constraint-predicate x)
          (constraint-branch x)
          (handler-case (constraint-unconditional x) (type-error () :type-error))))
;; The false predicate must not refine an unrelated unconditional later use.
(assert (eql (cdr (constraint-unconditional 2.5d0)) 3.5d0))
(assert (= (cdr (constraint-unconditional (ash 1 80))) (1+ (ash 1 80))))
(format t "TYPE-CONSTRAINT-FACTS-OK~%")
"#;

#[test]
fn type_predicates_refine_only_the_true_branch_and_keep_boolean_results() {
    let mut reference: Option<String> = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_T2_NO_QUEUE")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON")
            .args(["--no-init", "--eval", SOURCE]);
        if tier == "t2" {
            command.args(["--eval", NATIVE_READY]);
        }
        command.args(["--eval", CHECK]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run type constraint differential");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("TYPE-CONSTRAINT-FACTS-OK"));
        if tier == "t2" {
            for name in [
                "CONSTRAINT-PREDICATE",
                "CONSTRAINT-BRANCH",
                "CONSTRAINT-UNCONDITIONAL",
            ] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: optimized worker result installed with "))
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "{name} must install an actual native T2 artifact\n{stderr}"
                );
            }
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
