// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
const DEFINITIONS: &str = r#"
(defmacro with-mini-reader ((name input) &body body)
 (let ((word (gensym "WORD")) (count (gensym "COUNT")))
  `(let ((,word ,input) (,count 0))
    (flet ((,name () (prog1 (logand ,word 63) (setq ,word (ash ,word -6)) (incf ,count))))
     ,@body))))
(defun optional-reader (input &optional (output (make-array 2)) supplied)
 (with-mini-reader (next-field input)
  (setf (svref output 0) (next-field) (svref output 1) (next-field))
  (values output supplied)))
"#;
const CHECKS: &str = r#"
(dotimes (i 150)
 (assert (equalp #(3 2) (optional-reader 131)))
 (let ((out (make-array 2)))
  (multiple-value-bind (value supplied) (optional-reader 131 out t)
   (assert (eq out value)) (assert supplied))
  (assert (equalp #(3 2) out))))
(multiple-value-bind (value supplied) (optional-reader 131)
 (assert (equalp #(3 2) value)) (assert (null supplied)))
(write-line "OPTIONAL-CAPTURE-OK")
"#;
static NEXT: AtomicU64 = AtomicU64::new(0);
fn directory() -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!(
        "egcl-optional-capture-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn run(program: &str, forced: bool, stress: bool) -> String {
    let dir = directory();
    let script = dir.join("run.lisp");
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load", script.to_str().unwrap()])
        .env("EGCL_FORCE_TIER", if forced { "t2" } else { "" })
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env_remove("EGCL_DISABLE_ORDINARY_CAPTURED_CALLEES")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(dir).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "forced={forced} stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}
#[test]
fn optional_reader_captures_work_from_source_in_default_and_forced_tiers() {
    let program = format!("{DEFINITIONS}{CHECKS}");
    let normal = run(&program, false, false);
    assert!(normal.contains("OPTIONAL-CAPTURE-OK"));
    assert_eq!(normal, run(&program, true, false));
    assert_eq!(normal, run(&program, true, true));
}
#[test]
fn fresh_bytecode_optional_reader_keeps_exact_captures_under_forced_t2() {
    let dir = directory();
    let source = dir.join("reader.lisp");
    let artifact = dir.join("reader.fasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure) (compile-file {:?} :output-file {:?}) (declare (ignore warnings)) (assert file) (assert (not failure)))",
            source.to_str().unwrap(),
            artifact.to_str().unwrap()
        ),
        false,
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        assert!(
            bytes.windows(4).any(|w| w == b"BBU\0"),
            "must use genuine bytecode, not source fallback"
        );
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {:?}){CHECKS}", artifact.to_str().unwrap());
    let normal = run(&program, false, false);
    assert!(normal.contains("OPTIONAL-CAPTURE-OK"));
    assert_eq!(normal, run(&program, true, false));
    assert_eq!(normal, run(&program, true, true));
    fs::remove_dir_all(dir).unwrap();
}
