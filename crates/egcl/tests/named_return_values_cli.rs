// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]
use std::process::Command;

const SOURCE: &str = r#"
(defvar *nrm-effects* 0)
(defvar *nrm-cleanups* nil)
(defun nrm-exit (n)
  (block nrm-target
    (unwind-protect
      (funcall (lambda ()
        (return-from nrm-target
          (case n (0 (values)) (1 (values n)) (otherwise (values n :secondary))))))
      (incf *nrm-effects*)
      (push (cons n :cleanup) *nrm-cleanups*))))
(defun nrm-nested (n)
  (block nrm-outer
    (unwind-protect
      (block nrm-inner
        (funcall (lambda ()
          (funcall (lambda () (return-from nrm-outer (values (list n :heap) :outer)))))))
      (incf *nrm-effects*))))
(defun nrm-throw (n)
  (catch 'nrm-tag
    (unwind-protect
      (funcall (lambda () (throw 'nrm-tag (values n :throw))))
      (incf *nrm-effects*))))
(defun nrm-warm ()
  (assert (null (multiple-value-list (nrm-exit 0))))
  (assert (equal '(1) (multiple-value-list (nrm-exit 1))))
  (assert (equal '(2 :secondary) (multiple-value-list (nrm-exit 2))))
  (assert (equal '((5 :heap) :outer) (multiple-value-list (nrm-nested 5))))
  (assert (equal '(9 :throw) (multiple-value-list (nrm-throw 9)))))
(dotimes (i 100) (nrm-warm))
"#;
const READY: &str = r#"
(loop repeat 150 until
 (every (lambda (s) (= 2 (egcl-ext:function-tier s))) '(nrm-exit nrm-nested nrm-throw))
 do (nrm-warm) (sleep 0.01))
(dolist (s '(nrm-exit nrm-nested nrm-throw)) (assert (= 2 (egcl-ext:function-tier s))))
"#;
const CHECK: &str = r#"
(setq *nrm-effects* 0 *nrm-cleanups* nil)
(nrm-warm)
(assert (= 5 *nrm-effects*))
(assert (equal '((2 . :cleanup) (1 . :cleanup) (0 . :cleanup)) *nrm-cleanups*))
(format t "NAMED-RETURN-VALUES-OK~%")
"#;

#[test]
fn named_nonlocal_returns_preserve_all_values_through_cleanup_and_native_calls() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr");
        for key in [
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
        let ready = if tier == "t2" { READY } else { "" };
        let output = command
            .args([
                "--no-init",
                "--eval",
                &format!("{SOURCE}\n{ready}\n{CHECK}"),
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("NAMED-RETURN-VALUES-OK"));
        if tier == "t2" {
            for name in ["NRM-EXIT", "NRM-NESTED", "NRM-THROW"] {
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
