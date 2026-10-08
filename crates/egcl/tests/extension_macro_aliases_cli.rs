// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
  (defparameter *extension-counter* 4)
  (defun extension-atomic-step (cell)
    (list (egcl-ext:cas (car cell) nil :ready)
          (egcl-ext:atomic-incf *extension-counter* 3)
          (egcl-ext:atomic-decf *extension-counter* 2)))
  (compile 'extension-atomic-step)
  (dotimes (iteration 4)
    (setq *extension-counter* 4)
    (let ((cell (cons nil nil)))
      (assert (equal '(nil 4 7) (extension-atomic-step cell)))
      (assert (eq :ready (car cell)))
      (assert (= 5 *extension-counter*))))
  (defmacro egcl-ext::alias-test (value) `(list :external ,value))
  (defmacro egcl-internal::alias-test (value) `(list :internal ,value))
  (defun extension-macro-caller ()
    (list (egcl-ext::alias-test :one)
          (egcl-internal::alias-test :two)
          (flet ((egcl-ext::alias-test (x) (list :local x)))
            (egcl-ext::alias-test :three))
          (macrolet ((egcl-internal::alias-test (x) `(list :lexical ,x)))
            (egcl-internal::alias-test :four))))
  (compile 'extension-macro-caller)
  (dotimes (iteration 4)
    (assert (equal '((:external :one) (:internal :two)
                     (:local :three) (:lexical :four))
                   (extension-macro-caller))))
  (disassemble 'extension-atomic-step)
  (disassemble 'extension-macro-caller)
  (fmakunbound 'egcl-ext::alias-test)
  (assert (null (macro-function 'egcl-ext::alias-test)))
  (assert (macro-function 'egcl-internal::alias-test))
  (defun egcl-ext::alias-test (value) (list :redefined value))
  (assert (equal '(:redefined :five) (egcl-ext::alias-test :five)))
  (fmakunbound 'egcl-internal::alias-test)
  (assert (null (macro-function 'egcl-internal::alias-test)))
  (defmacro egcl-ext::scope-test () :global)
  (defmacro snapshot-test () :original)
  (defun flet-macro-snapshot ()
    (flet ((egcl-ext::scope-test ()
             (list (egcl-ext::scope-test) (snapshot-test))))
      (list (egcl-ext::scope-test) (snapshot-test))))
  (defun labels-macro-snapshot ()
    (labels ((egcl-ext::scope-test (recurse)
               (if recurse
                   (egcl-ext::scope-test nil)
                   (snapshot-test))))
      (list (egcl-ext::scope-test t) (snapshot-test))))
  (fmakunbound 'snapshot-test)
  (dotimes (iteration 4)
    (assert (equal '((:global :original) :original)
                   (flet-macro-snapshot)))
    (assert (equal '(:original :original) (labels-macro-snapshot))))
  (format t "EXTENSION-MACRO-ALIASES-PASS~%")
"#;

fn check(tier: &str) {
    let directory = std::env::temp_dir().join(format!(
        "egcl-extension-macros-{}-{tier}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let source = directory.join("check.lisp");
    std::fs::write(&source, PROGRAM).unwrap();
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "120",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--load",
        ])
        .arg(&source)
        .env("EGCL_FORCE_TIER", tier)
        .output()
        .expect("run extension macro regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("EXTENSION-MACRO-ALIASES-PASS"),
        "{stdout}\n{stderr}"
    );
    if tier != "interp" {
        assert!(
            stdout
                .matches(&format!("; {}", tier.to_uppercase()))
                .count()
                == 2,
            "requested {tier} was not installed: {stdout}"
        );
        assert!(!stdout.contains("sym: EGCL-EXT::CAS,"), "{stdout}");
        assert!(!stdout.contains("sym: EGCL-EXT::ATOMIC-INCF,"), "{stdout}");
    }
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn interpreter() {
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
fn optimizing_native() {
    check("t2");
}

#[test]
fn local_macro_scopes_under_allocation_stress() {
    // This path needs no bootstrap macros: stress the definition-time walker
    // directly without spending the timeout rebuilding the standard library.
    for stress in ["0", "1"] {
        let output = Command::new("timeout")
            .args([
                "--kill-after=5",
                "120",
                env!("CARGO_BIN_EXE_egcl"),
                "--no-init",
                "--no-bootstrap",
                "--eval",
                include_str!("fixtures/extension-macro-scope.lisp"),
            ])
            .env("EGCL_FORCE_TIER", "interp")
            .env("EGCL_GC_STRESS", stress)
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "stress={stress}: {stdout}\n{stderr}");
        assert!(stdout.contains("FLET-SNAPSHOT-PASS"), "{stdout}\n{stderr}");
        assert!(stdout.contains("LABELS-SNAPSHOT-PASS"), "{stdout}\n{stderr}");
    }
}
