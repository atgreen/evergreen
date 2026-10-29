// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A LABELS function resolves its siblings and itself in the LABELS form's own
//! scope, not the caller's. An inner MACROLET of the same name shadows the
//! function only lexically inside that MACROLET, so calling a LABELS function
//! from there must still reach its sibling. EGCL resolved the call through the
//! caller's namespace, where the MACROLET had removed it, and signalled
//! UNDEFINED-FUNCTION (or, before lexical MACROLET capture, expanded the
//! caller's macro). SBCL returns :FN in every case (bliss-yp2w5).
use std::process::Command;

const FORMS: &str = r#"
  (list
    (macrolet ((f () :macro))
      (labels ((f () :fn) (g () (f)))
        (macrolet ((f () :inner)) (g))))
    (labels ((f () :fn) (g () (f)))
      (macrolet ((f () :inner)) (g)))
    (funcall (labels ((f () :fn) (g () (f)))
               (macrolet ((f () :inner)) #'g)))
    (labels ((f (n) (if (< n 2) 1 (* n (f (1- n))))))
      (macrolet ((f (n) `(list :macro ,n))) (list (f 3))))
    (labels ((fact (n) (if (< n 2) 1 (* n (fact (1- n)))))) (fact 10)))"#;

const EXPECTED: &str = "(:FN :FN :FN ((:MACRO 3)) 3628800)";

fn run(tier: &str, program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{tier}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn labels_functions_see_their_own_scope_under_an_inner_macrolet() {
    let program = format!(
        r#"(progn
             (format t "~&EVAL ~s~%" (eval '{FORMS}))
             (defun shadowed () {FORMS})
             (let (r) (dotimes (i 50) (setq r (shadowed))) (format t "~&DEFUN ~s~%" r)))"#
    );
    for tier in ["interp", "t0", "t1", "t2"] {
        let stdout = run(tier, &program);
        assert!(
            stdout.contains(&format!("EVAL {EXPECTED}")),
            "{tier}: {stdout}"
        );
        assert!(
            stdout.contains(&format!("DEFUN {EXPECTED}")),
            "{tier}: {stdout}"
        );
    }
}
