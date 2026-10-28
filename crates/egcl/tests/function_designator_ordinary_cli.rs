// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

const PROGRAM: &str = r#"
(defun fd-target (x) (1+ x))
(defun fd-local (n)
  (let ((f #'fd-target) (s 0))
    (dotimes (i n s) (incf s (funcall f i)))))
(defun fd-symbol (f n)
  (let ((s 0)) (dotimes (i n s) (incf s (funcall f i)))))
(defun fd-single (f x) (funcall f x))
(defun fd-no-actual (f) (funcall f))
(defun fd-mv (x) (values x (1+ x)))
(dotimes (i 40)
  (assert (= 5050 (fd-local 100)))
  (assert (= 5050 (fd-symbol 'fd-target 100)))
  (assert (= 2 (fd-single 'fd-target 1))))
;; NATIVE-READY
(flet ((fd-target (x) (+ x 999)))
  (assert (= 5050 (fd-symbol 'fd-target 100))))
(flet ((funcall (f x) (declare (ignore f x)) :lexical-operator))
  (assert (eq :lexical-operator (funcall 'fd-target 1))))
(assert (eq :arity (handler-case (fd-no-actual 'fd-target)
                     (program-error () :arity))))
(assert (= 2 (fd-single 'fd-target 1)))
(assert (equal '(4 5) (multiple-value-list (fd-single 'fd-mv 4))))
(let ((saved #'fd-target))
  ;; Resolve the SYMBOL designator after the side effect in its actual.
  (assert (= 420 (fd-single 'fd-target
                   (progn (setf (symbol-function 'fd-target) (lambda (x) (* x 10))) 42))))
  ;; Retained function objects continue to call the old definition.
  (assert (= 43 (fd-single saved 42))))
(assert (= 420 (fd-single 'fd-target 42)))
(assert (eq :undefined
  (handler-case (fd-single 'fd-never-defined 1) (undefined-function () :undefined))))
(format t "FUNCTION-DESIGNATORS-OK~%")
"#;

#[test]
fn ordinary_function_designators_preserve_lookup_redefinition_errors_and_values() {
    let mut reference = None;
    for (tier, disabled, stress) in [
        ("interp", false, false),
        ("t2", false, false),
        ("t2", true, false),
        ("t2", false, true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        for key in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_BACKEND",
            "EGCL_T2",
            "EGCL_DISABLE_ORDINARY_FUNCTION_DESIGNATORS",
            "EGCL_PROFILING_DISABLED",
            "EGCL_DISABLE_T2",
            "EGCL_T2_NO_QUEUE",
        ] {
            command.env_remove(key);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        if disabled {
            command.env("EGCL_DISABLE_ORDINARY_FUNCTION_DESIGNATORS", "1");
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let ready = if tier == "t2" {
            r#"
(loop repeat 200 until (and (= 2 (or (egcl-ext:function-tier 'fd-local) -1))
                            (= 2 (or (egcl-ext:function-tier 'fd-symbol) -1))
                            (= 2 (or (egcl-ext:function-tier 'fd-target) -1)))
 do (fd-local 100) (fd-symbol 'fd-target 100) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'fd-local)))
(assert (= 2 (egcl-ext:function-tier 'fd-symbol)))
(assert (= 2 (egcl-ext:function-tier 'fd-target)))
"#
        } else {
            ""
        };
        let program = PROGRAM.replace(";; NATIVE-READY", ready);
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} disabled={disabled} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("FUNCTION-DESIGNATORS-OK"));
        if tier == "t2" && !disabled {
            assert!(
                stderr.lines().any(|line| line.contains("FD-LOCAL:")
                    && line.contains(
                        "optimized worker result installed with ordinary x86-64 Lisp T2 ABI"
                    )),
                "FUNCTION lookup must retain actual ordinary native execution\n{stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(&output.stdout, expected);
        } else {
            reference = Some(output.stdout);
        }
    }
}
