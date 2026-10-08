// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
  (defvar *mv-effects* nil)
  (defun mv-prog1-probe (producer)
    (multiple-value-prog1 (funcall producer)
      (push :first *mv-effects*)
      (values :discarded :also-discarded)
      (push :second *mv-effects*)))
  (defun mv-prog1-single (x)
    (multiple-value-prog1 (progn (values :stale :secondary) x)
      (values :discarded :also-discarded)))
  (defun mv-prog1-empty () (multiple-value-prog1 (values)))
  (defun mv-prog1-nested ()
    (multiple-value-prog1
        (multiple-value-prog1 (values :a :b) (values :inner))
      (multiple-value-prog1 (values :c :d) (values :outer))))
  (defun mv-prog1-exit ()
    (block done
      (multiple-value-prog1 (values :discarded)
        (return-from done (values :exit :secondary)))))
  (defun mv-prog1-first-exit ()
    (block done
      (multiple-value-prog1 (return-from done (values :early :exit))
        (error "Trailing form must not run"))))
  (defun mv-prog1-allocate ()
    (multiple-value-prog1 (values (list :saved) (vector :secondary))
      (let ((keep nil))
        (dotimes (i 20000) (push (list i i i) keep))
        (length keep))))
  (dolist (producer (list (lambda () (values))
                         (lambda () :one)
                         (lambda () (values :one :two :three))))
    (setq *mv-effects* nil)
    (assert (equal (multiple-value-list (mv-prog1-probe producer))
                   (multiple-value-list (funcall producer))))
    (assert (equal *mv-effects* '(:second :first))))
  (assert (equal (multiple-value-list (mv-prog1-single :one)) '(:one)))
  (assert (null (multiple-value-list (mv-prog1-empty))))
  (assert (equal (multiple-value-list (mv-prog1-nested)) '(:a :b)))
  (assert (equal (multiple-value-list (mv-prog1-exit)) '(:exit :secondary)))
  (assert (equal (multiple-value-list (mv-prog1-first-exit)) '(:early :exit)))
  (assert (equalp (multiple-value-list (mv-prog1-allocate))
                  '((:saved) #(:secondary))))
  (disassemble #'mv-prog1-probe)
  (format t "MV-PROG1-COMPILED-OK~%")
"#;

fn check(tier: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", tier)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(stdout.contains("MV-PROG1-COMPILED-OK"), "{stdout}");
    if tier != "interp" {
        // ValuesToList currently keeps this body at T1 even with T2 requested.
        let installed = if tier == "t2" {
            "T1"
        } else {
            &tier.to_uppercase()
        };
        assert!(
            stdout.contains(&format!("; {installed}")),
            "requested {tier} was not installed: {stdout}\n{stderr}"
        );
    }
}

#[test]
fn interpreted() {
    check("interp");
}

#[test]
fn bytecode() {
    check("t0");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn baseline_native() {
    check("t1");
}

#[test]
#[cfg(target_arch = "x86_64")]
fn optimizing_mode_retains_native_baseline() {
    check("t2");
}
