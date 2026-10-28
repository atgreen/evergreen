// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

const PROGRAM: &str = r#"
(defun ct-free () (lambda (x) (+ x 1)))
(defun ct-captured (x) (lambda (delta) (incf x delta)))
(defun ct-control (x)
  (block ct-exit
    (funcall (lambda () (return-from ct-exit (values x :exit))))))
(defun ct-warm ()
  (assert (= 4 (funcall (ct-free) 3)))
  (assert (= 12 (funcall (ct-captured 10) 2)))
  (assert (equal '(7 :exit) (multiple-value-list (ct-control 7)))))
(dotimes (i 100) (ct-warm))
"#;
const READY: &str = r#"
(loop repeat 150 until
  (every (lambda (s) (= 2 (egcl-ext:function-tier s))) '(ct-free ct-captured ct-control))
  do (ct-warm) (sleep 0.01))
(dolist (s '(ct-free ct-captured ct-control)) (assert (= 2 (egcl-ext:function-tier s))))
"#;
const CHECK: &str = r#"
(let ((a (ct-captured 10)) (b (ct-captured 100)))
  (assert (not (eq a b)))
  (assert (= 11 (funcall a 1)))
  (assert (= 102 (funcall b 2)))
  (assert (= 14 (funcall a 3))))
;; Distinct activations must resolve the free RETURN-FROM to their own token.
(assert (equal '(20 :exit) (multiple-value-list (ct-control 20))))
(assert (equal '(30 :exit) (multiple-value-list (ct-control 30))))
;; Two closures over one lexical binding must still share the authoritative cell.
(let ((pair (let ((x 0)) (list (lambda () (incf x)) (lambda () x)))))
  (assert (= 1 (funcall (first pair))))
  (assert (= 1 (funcall (second pair)))))
(assert (eq :type (handler-case (progn (funcall (ct-free) "bad") :wrong) (type-error () :type))))
(ct-warm)
(format t "CLOSURE-TEMPLATES-OK~%")
"#;

#[test]
fn closure_templates_preserve_captures_control_and_actual_native_production() {
    let mut reference = None;
    for (tier, stress, disabled) in [
        ("interp", false, false),
        ("t2", false, false),
        ("t2", true, false),
        ("t2", false, true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        for key in [
            "EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
        ] {
            command.env_remove(key);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        if disabled {
            command.env("EGCL_DISABLE_CLOSURE_TEMPLATE_CACHE", "1");
        }
        let ready = if tier == "t2" { READY } else { "" };
        // This observes the real native MakeClosure producer, not a standalone
        // metadata helper. No allocation separates the two cache-hit requests.
        let hit = if tier == "t2" && !stress && !disabled {
            "(let ((a (ct-free))) (assert (eq a (ct-free))))"
        } else {
            ""
        };
        let program = format!("{PROGRAM}\n{ready}\n{hit}\n{CHECK}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress} disabled={disabled}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("CLOSURE-TEMPLATES-OK"));
        if tier == "t2" {
            for name in ["CT-FREE", "CT-CAPTURED", "CT-CONTROL"] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: optimized worker result installed with "))
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "{name}\n{stderr}"
                );
            }
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected);
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
