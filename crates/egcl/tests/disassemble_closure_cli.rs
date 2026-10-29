// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! DISASSEMBLE shows the code a tiered-up closure object runs, as
//! EGCL-EXT:FUNCTION-TIER already reported (bliss-v8res). A LAMBDA closure or a
//! LABELS function taken with #' and stored in a variable reported "not a
//! compiled EGCL function" after it had been compiled, which misled a
//! diagnosis of Babel's mapping closures. A closure that never got hot still
//! reports that it is not compiled.
use std::process::Command;

#[test]
fn disassemble_follows_a_compiled_closure_object() {
    let program = r#"(progn
      (defparameter *labels* (let ((k 2)) (labels ((bar (x) (* k x))) #'bar)))
      (defparameter *lambda* (let ((k 2)) (lambda (x) (* k x))))
      (defparameter *cold* (let ((k 3)) (lambda (x) (+ k x))))
      (dotimes (i 300) (funcall *labels* i) (funcall *lambda* i))
      (format t "~&TIERS ~s~%" (list (egcl-ext:function-tier *labels*)
                                     (egcl-ext:function-tier *lambda*)))
      (format t "~&== LABELS~%") (disassemble *labels*)
      (format t "~&== LAMBDA~%") (disassemble *lambda*)
      (format t "~&== COLD~%") (disassemble *cold*)
      (format t "~&RESULT ~s~%" (list (funcall *labels* 2) (funcall *lambda* 3) (funcall *cold* 1))))"#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("RESULT (4 6 4)"), "{stdout}");
    let section = |name: &str| -> String {
        let start = stdout.find(&format!("== {name}")).expect(name);
        let rest = &stdout[start + 3..];
        rest[..rest
            .find("\n== ")
            .or_else(|| rest.find("RESULT"))
            .unwrap_or(rest.len())]
            .to_string()
    };
    for name in ["LABELS", "LAMBDA"] {
        let listing = section(name);
        assert!(!listing.contains("not a compiled"), "{name}: {listing}");
        assert!(listing.contains("CLOSURE —"), "{name}: {listing}");
    }
    assert!(section("COLD").contains("not a compiled"), "{stdout}");
}
