// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

fn run(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env_remove("EGCL_NATIVE_TRANSFER")
        .env("EGCL_T0_T1_THRESHOLD", "2")
        .env("EGCL_T2_THRESHOLD", "1000000")
        .env("EGCL_T2_BACKEDGE_THRESHOLD", "1000000")
        .env("EGCL_OSR_THRESHOLD", "2")
        .output()
        .expect("spawn egcl");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

#[test]
fn linux_default_installs_mapped_native_calls() {
    let output = run("(defun policy-leaf (x) (list x))
        (defun policy-caller (x) (policy-leaf x))
        (dotimes (i 40) (assert (equal '(8) (policy-caller 8))))
        (format t \"POLICY-TIER=~D~%\" (egcl-ext:function-tier 'policy-caller))
        (disassemble 'policy-caller)");
    assert!(output.contains("POLICY-TIER=1"), "{output}");
    assert!(output.contains("Mapped native transfer ABI"), "{output}");
    assert!(!output.contains("Legacy checked ABI"), "{output}");
}

#[test]
fn linux_default_interprets_unsupported_parameters_and_preserves_transfers() {
    let output = run("(defun policy-optional-leaf (x) (values (list x) 19))
        (defun policy-optional (x &optional (y 1)) (policy-optional-leaf (+ x y)))
        (defun policy-escape (x &optional (y 1))
          (unwind-protect (throw 'policy-exit (values (+ x y) 23))
            (incf *policy-cleanups*)))
        (setq *policy-cleanups* 0)
        (dotimes (i 40)
          (assert (equal '((8) 19) (multiple-value-list (policy-optional 7))))
          (assert (equal '(8 23) (multiple-value-list
            (catch 'policy-exit (policy-escape 7))))))
        (format t \"POLICY-OPTIONAL=~D CLEANUPS=~D~%\"
          (egcl-ext:function-tier 'policy-optional) *policy-cleanups*)
        (disassemble 'policy-optional)");
    assert!(output.contains("POLICY-OPTIONAL=0 CLEANUPS=40"), "{output}");
    assert!(!output.contains("Legacy checked ABI"), "{output}");
}

#[test]
fn linux_default_keeps_hot_unsupported_loop_in_interpreter() {
    let output = run("(defun policy-loop (n &optional (sum 0))
          (dotimes (i n sum) (incf sum i)))
        (assert (= 780 (policy-loop 40)))
        (format t \"POLICY-LOOP=~D OSR=~D~%\"
          (egcl-ext:function-tier 'policy-loop)
          (egcl-ext:function-osr-count 'policy-loop))");
    assert!(output.contains("POLICY-LOOP=0 OSR=0"), "{output}");
}
