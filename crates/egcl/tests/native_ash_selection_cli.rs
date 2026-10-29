// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, process::Command};
const DEFINITIONS: &str = r#"
(defun variable-ash-proof (value count)
  (declare (type fixnum value count)) (ash value count))
(defun constant-ash-proof (value) (ash value 1))
(defun large-ash-proof (value) (ash value 100))
"#;
const CHECKS: &str = r#"
(dotimes (i 100)
  (assert (= (variable-ash-proof 7 2) 28))
  (assert (= (constant-ash-proof 7) 14))
  (assert (= (large-ash-proof 1) (expt 2 100))))
#+egcl (assert (= (eval '(egcl-ext::function-tier 'variable-ash-proof)) 2))
#+egcl (assert (= (eval '(egcl-ext::function-tier 'constant-ash-proof)) 2))
#+egcl (assert (= (eval '(egcl-ext::function-tier 'large-ash-proof)) 2))
(let ((wide (expt 2 100)))
  (assert (= (variable-ash-proof 1 100) wide))
  (assert (= (variable-ash-proof -1 100) (- wide)))
  (assert (= (variable-ash-proof -7 -1) -4))
  (assert (= (variable-ash-proof -7 -100) -1))
  (assert (= (variable-ash-proof 7 -100) 0))
  (assert (= (variable-ash-proof most-positive-fixnum 0) most-positive-fixnum))
  (assert (= (variable-ash-proof most-positive-fixnum 1) (* 2 most-positive-fixnum))))
(write-line "ASH-SELECTION-OK")
"#;
fn run(program: &str, stress: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", &format!("(progn {program})")])
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}
#[test]
fn variable_shift_counts_keep_t2_and_exact_integer_results() {
    let program = format!("{DEFINITIONS}{CHECKS}");
    let normal = run(&program, false);
    assert!(normal.contains("ASH-SELECTION-OK"));
    assert_eq!(normal, run(&program, true));
}
#[test]
fn compiled_shift_counts_preserve_large_and_negative_results() {
    let dir = std::env::temp_dir().join(format!("egcl-ash-selection-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("shifts.lisp");
    let artifact = dir.join("shifts.fasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
            source.to_str().unwrap(),
            artifact.to_str().unwrap()
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        assert!(
            bytes.windows(4).any(|s| s == b"BBU\0"),
            "must compile bytecode, not embedded-source fallback"
        );
    }
    fs::remove_file(&source).unwrap();
    let program = format!("(load {:?}){CHECKS}", artifact.to_str().unwrap());
    let normal = run(&program, false);
    assert!(normal.contains("ASH-SELECTION-OK"));
    assert_eq!(normal, run(&program, true));
    fs::remove_dir_all(dir).unwrap();
}
