// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! MULTIPLE-VALUE-PROG1's trailing forms must survive its first form (bliss-c0a).
//!
//! The interpreter read `(multiple-value-prog1 <first> <rest>…)` apart into two
//! Rust locals and then evaluated `<first>` — which, for this operator above all
//! others, may be an entire program. `rest_forms` was a bare local across that,
//! invisible to the precise moving collector, so a minor GC fired by the first
//! form relocated the body and left the cursor pointing at an ADDRESS rather
//! than an object. Whatever was allocated there next became the trailing forms.
//!
//! Found on a phone: a Bliss UI frame is written
//! `(multiple-value-prog1 (progn <one frame>) (when *animating* (invalidate)))`,
//! and the app died after its first frame with "The function
//! #<heap-object type=27> is undefined" — type 27 is FOREIGN-POINTER, because
//! what had moved into that address was a JNI argument list.
//!
//! NOTE ON THE SHAPE OF THIS TEST, because the obvious version proves nothing:
//! `EGCL_GC_STRESS` does NOT reproduce it. Stressing collects so often that the
//! freshly read form is promoted out of the nursery before the first form has
//! allocated anything, and a promoted object does not move (AGENTS.md, "A CLEAN
//! STRESS RUN CAN MEAN NOTHING MOVED"). What reproduces it is the ordinary
//! configuration and a first form that allocates hard enough to trigger a real
//! minor collection while the form is still in the nursery. The form is
//! therefore built at RUNTIME with READ-FROM-STRING: a loaded literal has
//! survived several collections already and is no longer movable.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defparameter *c0a-cleanup* nil)
  (defparameter *c0a-source*
    "(multiple-value-prog1
         (progn (let ((keep nil))
                  (dotimes (i 200000) (push (list i i i) keep))
                  (length keep))
                (values :a :b))
       (setq *c0a-cleanup* t))")
  (dotimes (trial 3)
    (setq *c0a-cleanup* nil)
    (multiple-value-bind (a b) (eval (read-from-string *c0a-source*))
      (format t "C0A ~A ~A ~A~%" a b *c0a-cleanup*)))
"#;

#[test]
fn multiple_value_prog1_trailing_forms_survive_the_first_form() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "MULTIPLE-VALUE-PROG1 did not survive a moving GC: {stdout}\n{stderr}"
    );
    let lines: Vec<&str> = stdout.lines().filter(|l| l.starts_with("C0A ")).collect();
    assert_eq!(lines.len(), 3, "expected three trials: {stdout}\n{stderr}");
    for line in lines {
        assert_eq!(
            line, "C0A A B T",
            "the trailing form did not run, or the returned values were lost, \
             which is what a relocated body cursor looks like: {stdout}\n{stderr}"
        );
    }
}
