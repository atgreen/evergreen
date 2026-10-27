// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! ENDP's T2 intrinsic must be as cheap as a list-end test without weakening
//! its signaling contract. In particular, interpreter closures carry the cons
//! tag and therefore exercise the guard's conservative closure exclusion.

use std::process::Command;

const PROGRAM: &str = r#"
(defun t2-endp-probe (x)
  (values :stale :secondary)
  (multiple-value-call #'list (endp x)))

;; Install the function at T2 on ordinary proper-list inputs.
(dotimes (i 200) (t2-endp-probe (if (evenp i) '(a b) nil)))
(assert (= 2 (egcl-ext:function-tier 't2-endp-probe)))
(assert (equal '(t) (t2-endp-probe nil)))
(assert (equal '(nil) (t2-endp-probe '(a b))))

;; A valid dotted cons with a fixnum cdr takes the conservative deopt and still
;; has ENDP's NIL result. Non-lists and closure-shaped conses must signal.
(assert (equal '(nil) (t2-endp-probe (cons 'a 3))))
(assert (eq :type-error
            (handler-case (t2-endp-probe 7)
              (type-error () :type-error))))
(assert (eq :type-error
            (handler-case
                (let ((captured 9))
                  (t2-endp-probe (lambda () captured)))
              (type-error () :type-error))))
(format t "ENDP-T2-OK~%")
(egcl::%exit 0)
"#;

#[test]
fn t2_endp_preserves_list_errors_closures_and_single_value_results() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T2_THRESHOLD", "20")
        .env("EGCL_T2_THREADS", "1")
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("ENDP-T2-OK"));
}
