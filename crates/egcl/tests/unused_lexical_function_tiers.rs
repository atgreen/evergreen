// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
;; EVAL creates source closures before lazy expansion examines their namespaces.
(defparameter *unused-local* (eval
 '(flet ((registration-helper () :unused))
    (macrolet ((advance (x) `(1+ ,x)))
      (labels ((generated-encoder (n)
                 (let ((sum 0))
                   (dotimes (i n) (setq sum (advance sum)))
                   (return-from generated-encoder sum))))
        #'generated-encoder)))))
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
;; The unused-namespace proof rejects references, including quoted data.
;; Captured namespace compilation independently supports these closures.
(defparameter *recursive-local* (eval
 '(labels ((recursive-helper (n)
             (if (zerop n) 0 (1+ (recursive-helper (1- n))))))
    #'recursive-helper)))
(dotimes (i 20) (assert (= 4 (funcall *recursive-local* 4))))
(defparameter *quoted-local* (eval
 '(flet ((encoding-helper () :unused))
    (lambda () 'encoding-helper))))
(dotimes (i 20) (assert (eq (funcall *quoted-local*) 'encoding-helper)))
(defparameter *quoted-control* (eval
 '(labels ((encoding-helper () '(block encoding-helper))) #'encoding-helper)))
(dotimes (i 20) (assert (equal (funcall *quoted-control*) '(block encoding-helper))))
;; A definition-site special binding remains an unsupported namespace capture.
;; Its dynamic extent has ended: the escaped helper must read the global value.
(set 'legacy-source-value 31)
(defparameter *legacy-source-local* (eval
 '(let ((legacy-source-value 9))
    (declare (special legacy-source-value))
    (flet ((legacy-helper (n) (+ n legacy-source-value)))
      (lambda (n) (legacy-helper n))))))
(dotimes (i 40) (assert (= 36 (funcall *legacy-source-local* 5))))
(assert (null (egcl-ext:function-tier *legacy-source-local*)))
(format t "UNUSED-LEXICAL-FUNCTIONS-OK~%")
"#;

#[test]
fn source_closure_preserves_unused_and_captured_namespace_policies() {
    let mut reference = None;
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        // Independently control unused-namespace erasure and actual capture.
        for (disabled, captured_disabled) in [(false, false), (true, false), (false, true)] {
            if captured_disabled && tier != "t2" {
                continue;
            }
            let unused_proof = if tier == "t2" && !disabled {
                "(loop repeat 200 until (eql 2 (egcl-ext:function-tier *unused-local*))
                   do (funcall *unused-local* 4) (sleep 0.01))
                 (assert (eql 2 (egcl-ext:function-tier *unused-local*)))"
            } else if disabled {
                "(assert (null (egcl-ext:function-tier *unused-local*)))"
            } else {
                "nil"
            };
            let captured_proof = if captured_disabled {
                "(dolist (f (list *used-local* *function-local* *macro-local*
                                  *recursive-local* *quoted-local* *quoted-control*))
                   (assert (null (egcl-ext:function-tier f))))"
            } else if tier == "t2" {
                "(loop repeat 200
                   until (and (eql 2 (egcl-ext:function-tier *recursive-local*))
                              (eql 2 (egcl-ext:function-tier *quoted-control*)))
                   do (assert (= 4 (funcall *recursive-local* 4)))
                      (assert (equal '(block encoding-helper) (funcall *quoted-control*)))
                      (sleep 0.01))
                 (assert (eql 2 (egcl-ext:function-tier *recursive-local*)))
                 (assert (eql 2 (egcl-ext:function-tier *quoted-control*)))"
            } else {
                "nil"
            };
            // Invoke again after installation, preserving all original results.
            let program = format!(
                "{PROGRAM}\n{unused_proof}\n{captured_proof}\n\
                 (assert (= 4 (funcall *recursive-local* 4)))\n\
                 (assert (equal '(block encoding-helper) (funcall *quoted-control*)))\n\
                 (assert (= 36 (funcall *legacy-source-local* 5)))"
            );
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
                "EGCL_DISABLE_UNUSED_CLOSURE_FUNCTIONS",
                "EGCL_DISABLE_CAPTURED_CLOSURE_FUNCTIONS",
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
                command.env("EGCL_DISABLE_UNUSED_CLOSURE_FUNCTIONS", "1");
            }
            if captured_disabled {
                command.env("EGCL_DISABLE_CAPTURED_CLOSURE_FUNCTIONS", "1");
            }
            let result = command.output().expect("run source closure regression");
            let stdout = String::from_utf8_lossy(&result.stdout);
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                result.status.success(),
                "tier={tier} unused_disabled={disabled} captured_disabled={captured_disabled}: {stdout}\n{stderr}"
            );
            assert!(
                stdout.contains("UNUSED-LEXICAL-FUNCTIONS-OK"),
                "{stdout}\n{stderr}"
            );
            if tier == "t2" && !disabled {
                assert!(
                    stderr.lines().any(|line| line.contains("<lambda>")
                        && line.contains("optimized worker result installed with ")),
                    "optimized closure installation required: {stderr}"
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
