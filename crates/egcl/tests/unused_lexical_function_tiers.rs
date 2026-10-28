// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
;; EVAL deliberately creates source closures rather than compiled creators.
(defparameter *unused-local* (eval
 '(flet ((registration-helper () :unused))
    (macrolet ((advance (x) `(1+ ,x)))
      (lambda (n)
        (let ((sum 0))
          (dotimes (i n sum) (setq sum (advance sum)))))))))
(defparameter *used-local* (eval
 '(flet ((encoding-helper (x) (+ x 11)))
    (lambda (n) (encoding-helper n)))))
(defparameter *function-local* (eval
 '(labels ((encoding-helper (x) (+ x 17)))
    (lambda (n) (funcall #'encoding-helper n)))))
(defparameter *macro-local* (eval
 '(flet ((encoding-helper (x) (+ x 23)))
    (macrolet ((encode (x) `(encoding-helper ,x)))
      (lambda (n) (encode n))))))
(dotimes (i 150)
  (assert (= (funcall *unused-local* 4) 4))
  (assert (= (funcall *used-local* 5) 16))
  (assert (= (funcall *function-local* 5) 22))
  (assert (= (funcall *macro-local* 5) 28)))
;; The initial proof intentionally rejects all references, even quoted data.
(defparameter *quoted-local* (eval
 '(flet ((encoding-helper () :unused))
    (lambda () 'encoding-helper))))
(dotimes (i 20) (assert (eq (funcall *quoted-local*) 'encoding-helper)))
(format t "UNUSED-LEXICAL-FUNCTIONS-OK~%")
"#;

#[test]
fn source_closure_ignores_only_proven_unused_definition_site_functions() {
    let mut reference = None;
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        for disabled in [false, true] {
            let proof = if tier == "t2" && !disabled {
                "(loop repeat 200 until (eql 2 (egcl-ext:function-tier *unused-local*))
                   do (funcall *unused-local* 4) (sleep 0.01))
                 (assert (= 2 (egcl-ext:function-tier *unused-local*)))"
            } else {
                "nil"
            };
            let program = format!("{PROGRAM}\n{proof}");
            let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
            command
                .args(["--no-init", "--eval", &program])
                .env("EGCL_FORCE_TIER", tier)
                .env("EGCL_T2_LOG", "stderr")
                .env("EGCL_LAZY_COMPILE", "1")
                .env("EGCL_LAZY_THRESHOLD", "10")
                .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
                .env("EGCL_T2_THREADS", "1");
            for key in [
                "EGCL_DISABLE_TREE_CLOSURE_EXPANSION",
                "EGCL_PROFILING_DISABLED",
                "EGCL_T2_NO_QUEUE",
                "EGCL_GC_STRESS",
                "EGCL_GC_STRESS_SKIP",
                "EGCL_GC_STRESS_AT",
                "EGCL_GC_STRESS_AFTER_INIT",
                "EGCL_GC_POISON",
            ] {
                command.env_remove(key);
            }
            if stress {
                command
                    .env("EGCL_GC_STRESS", "17")
                    .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                    .env("EGCL_GC_POISON", "1");
            }
            if disabled {
                command.env("EGCL_DISABLE_TREE_CLOSURE_EXPANSION", "1");
            }
            let result = command.output().expect("run source closure regression");
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                result.status.success(),
                "tier={tier} disabled={disabled}: {stdout}\n{stderr}"
            );
            assert!(
                stdout.contains("UNUSED-LEXICAL-FUNCTIONS-OK"),
                "{stdout}\n{stderr}"
            );
            if tier == "t2" && !disabled {
                assert!(
                    stderr.lines().any(|line| line.contains("<lambda>")
                        && line.contains("optimized worker result installed with ")),
                    "actual native closure installation required: {stderr}"
                );
            }
            if let Some(expected) = &reference {
                assert_eq!(stdout.as_ref(), expected);
            } else {
                reference = Some(stdout.into_owned());
            }
        }
    }
}
