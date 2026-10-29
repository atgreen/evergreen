// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline justification-run justification-formatter))
(defun justification-run (control args) (apply #'format nil control args))
(defun justification-formatter () (format nil (formatter "~<X~:;Y~>~3:T")))
(defun justification-error-p (thunk)
  (handler-case (progn (funcall thunk) nil) (error () t)))
(dotimes (i 80) (justification-run "~<~A~>" '("ok")))
(when *justification-required-tier*
  (loop repeat 100 until (= *justification-required-tier* (egcl-ext:function-tier 'justification-run))
        do (justification-run "~<~A~>" '("ok")) (sleep 0.01))
  (assert (= *justification-required-tier* (egcl-ext:function-tier 'justification-run))))
(dolist (control '("~<XXX~1,1:TYYY~>" "~<XXX~:;YYY~>ZZZ~4,5:tWWW"
                   "AAAA~1,1:TBBB~<XXX~:;YYY~>ZZZ" "~<~:;~>~<~:>"
                   "~<~:>~<~:;~>" "1~<X~<Y~:>Z~>2"
                   "~<X~_Y~>" "~<X~:;Y~>~W" "~<X~I~>"
                   "~<~(~3:@T~)~>" "~<~[ok~;~W~]~>"
                   "~<~{~_~}~>" "~<~<X~:;Y~>~:>"))
  (assert (justification-error-p (lambda () (justification-run control '(nil nil nil))))))
(assert (justification-error-p #'justification-formatter))
(assert (justification-error-p (lambda () (format nil (formatter "~<X~3:@TY~>")))))
(dolist (control '("~<X~3:TY~>" "~<X~:;Y~>~<~:>"))
  (assert (justification-error-p
            (lambda () (justification-run "~?" (list control '(nil nil nil))))))
  (assert (justification-error-p
            (lambda () (justification-run "~@?" (list control nil nil nil)))))
  (assert (justification-error-p
            (lambda () (justification-run "~{~}" (list control '(nil nil nil)))))))
;; Ordinary tabs, quoted directive text, and argument transport stay legal.
(assert (equal (justification-run "~<~~:T~>" nil) "~:T"))
(assert (equal (justification-run "~<X~3,0TY~>" nil) "X  Y"))
(assert (equal (justification-run "~<~vA~>~A" '(3 "x" "tail")) "x  tail"))
;; Distinct string controls are not syntactically unioned through ~?.
(assert (stringp (justification-run "~<X~:;Y~>~?" '("~W" (7)))))
(write-line "FORMAT-JUSTIFICATION-VALIDATION-OK")
"##;

#[test]
fn justification_rejects_all_incompatible_pretty_directives() {
    for tier in ["interp", "t2"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(name);
        }
        let required = if tier == "t2" { "2" } else { "nil" };
        let output = command
            .args([
                "--no-init",
                "--eval",
                &format!("(defparameter *justification-required-tier* {required}){PROGRAM}"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "FORMAT-JUSTIFICATION-VALIDATION-OK")
        );
    }
}
