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
fn linux_default_installs_ordinary_register_calls() {
    let output = run("(defun policy-leaf (x) (list x))
        (defun policy-caller (x) (policy-leaf x))
        (dotimes (i 40) (assert (equal '(8) (policy-caller 8))))
        (format t \"POLICY-TIER=~D~%\" (egcl-ext:function-tier 'policy-caller))
        (disassemble 'policy-caller)");
    assert!(output.contains("POLICY-TIER=1"), "{output}");
    assert!(output.contains("Ordinary x86-64 Lisp ABI"), "{output}");
    assert!(!output.contains("Legacy checked ABI"), "{output}");
}

#[test]
fn linux_default_compiles_enclosing_let_bodies_and_preserves_transfers() {
    // A body that reads its enclosing LET's binding runs under the callable
    // instance's captured environment, which the mapped entry now builds its
    // frame under (bliss-6fim1): it promotes and keeps MV, THROW and the
    // UNWIND-PROTECT cleanup intact.
    let output = run("(defun policy-key-leaf (x) (values (list x) 19))
        (let ((one 1))
          (defun policy-key (x &key (y 1))
            (if (eql one 1) (funcall (lambda () (policy-key-leaf (+ x y)))) :never))
          (defun policy-escape (x &key (y 1))
            (unwind-protect
                 (throw 'policy-exit (values (* one (funcall (lambda () (+ x y)))) 23))
              (incf *policy-cleanups*))))
        (setq *policy-cleanups* 0)
        (dotimes (i 40)
          (assert (equal '((8) 19) (multiple-value-list (policy-key 7))))
          (assert (equal '(8 23) (multiple-value-list
            (catch 'policy-exit (policy-escape 7))))))
        (format t \"POLICY-KEY=~D CLEANUPS=~D~%\"
          (egcl-ext:function-tier 'policy-key) *policy-cleanups*)
        (disassemble 'policy-key)");
    assert!(output.contains("POLICY-KEY=1 CLEANUPS=40"), "{output}");
    assert!(!output.contains("Legacy checked ABI"), "{output}");
}

#[test]
fn linux_default_compiles_keyword_parameters_and_preserves_transfers() {
    // Non-capturing &key bodies bind their slots in the mapped activation
    // entry (bliss-shih7.27): they promote and keep MV and THROW intact.
    let output = run("(defun policy-key-leaf (x) (values (list x) 19))
        (defun policy-key (x &key (y 1)) (policy-key-leaf (+ x y)))
        (defun policy-escape (x &key (y 1))
          (unwind-protect (throw 'policy-exit (values (+ x y) 23))
            (incf *policy-cleanups*)))
        (setq *policy-cleanups* 0)
        (dotimes (i 40)
          (assert (equal '((8) 19) (multiple-value-list (policy-key 7))))
          (assert (equal '((9) 19) (multiple-value-list (policy-key 7 :y 2))))
          (assert (equal '(8 23) (multiple-value-list
            (catch 'policy-exit (policy-escape 7))))))
        (format t \"POLICY-KEY=~D ESCAPE=~D CLEANUPS=~D~%\"
          (egcl-ext:function-tier 'policy-key)
          (egcl-ext:function-tier 'policy-escape) *policy-cleanups*)
        (disassemble 'policy-key)");
    assert!(
        output.contains("POLICY-KEY=1 ESCAPE=1 CLEANUPS=40"),
        "{output}"
    );
    assert!(!output.contains("Legacy checked ABI"), "{output}");
}

#[test]
fn linux_default_compiles_optional_parameters_and_preserves_transfers() {
    // &optional bodies are positional, and the mapped entry pads omitted
    // arguments (bliss-shih7.27): they promote and keep MV and THROW intact.
    let output = run("(defun policy-optional-leaf (x) (values (list x) 19))
        (defun policy-optional (x &optional (y 1)) (policy-optional-leaf (+ x y)))
        (defun policy-optional-escape (x &optional (y 1))
          (unwind-protect (throw 'policy-exit (values (+ x y) 23))
            (incf *policy-cleanups*)))
        (setq *policy-cleanups* 0)
        (dotimes (i 40)
          (assert (equal '((8) 19) (multiple-value-list (policy-optional 7))))
          (assert (equal '((9) 19) (multiple-value-list (policy-optional 7 2))))
          (assert (equal '(8 23) (multiple-value-list
            (catch 'policy-exit (policy-optional-escape 7))))))
        (format t \"POLICY-OPTIONAL=~D ESCAPE=~D CLEANUPS=~D~%\"
          (egcl-ext:function-tier 'policy-optional)
          (egcl-ext:function-tier 'policy-optional-escape) *policy-cleanups*)");
    assert!(output.contains("POLICY-OPTIONAL=1 ESCAPE=1 CLEANUPS=40"), "{output}");
}

#[test]
fn linux_default_promotes_a_once_called_captured_loop_by_osr() {
    // The closure captures the &KEY parameter SUM. Called once, the function
    // never reaches the invocation gate: its loop enters mapped T1 by OSR
    // (bliss-s0v50), carrying the captured binding with its activation.
    let output = run("(defun policy-loop (n &key (sum 0))
          (dotimes (i n (funcall (lambda () sum))) (incf sum i)))
        (assert (= 780 (policy-loop 40)))
        (format t \"POLICY-LOOP=~D OSR=~D~%\"
          (egcl-ext:function-tier 'policy-loop)
          (egcl-ext:function-osr-count 'policy-loop))");
    assert!(output.contains("POLICY-LOOP=1 OSR=1"), "{output}");
}
