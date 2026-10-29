// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{fs, path::PathBuf, process::Command};
const DEFINITIONS: &str = r#"(defstruct place-state (map 0))
(defmacro map-alias (s) `(place-state-map ,s))
(defun write-map (s offset)
 (declare (special s))
 (setf (ldb (byte 1 offset) (map-alias s)) 1)
 (map-alias s))
(defun write-lexical (s)
 (declare (special s))
 (macrolet ((local-map (v) `(place-state-map ,v)))
  (setf (mask-field (byte 1 4) (local-map s)) 16))
 (place-state-map s))
(defun write-symbol (s)
 (declare (special s))
 (symbol-macrolet ((bits (map-alias s)))
  (setf (ldb (byte 1 5) bits) 1))
 (place-state-map s))
(defmacro preferred-place (cell) (declare (ignore cell)) (error "must use DEFSETF"))
(defun preferred-reader (cell) (car cell))
(defun preferred-writer (cell value) (rplaca cell value) value)
(defsetf preferred-place preferred-writer)
(defun write-preferred (cell)
 (declare (special cell))
 (setf (preferred-place cell) 42)
 (preferred-reader cell))
(defmacro getter-form (place &environment env)
 `(quote ,(nth-value 4 (get-setf-expansion place env))))
(defun lexical-getter ()
 (macrolet ((local-map (v) `(car ,v)))
  (getter-form (local-map value))))
(defmacro environment-getters (&environment env)
 `(quote ,(list (nth-value 4 (get-setf-expansion 'bits env))
                (nth-value 4 (get-setf-expansion 'bits nil))
                (nth-value 4 (get-setf-expansion 'bits)))))
(defun compare-environments ()
 (symbol-macrolet ((bits (car holder))) (environment-getters)))
"#;
const CHECKS: &str = r#"(let ((state (make-place-state)))
 (assert (= 8 (write-map state 3)))
 (assert (= 24 (write-lexical state)))
 (assert (= 56 (write-symbol state))))
(assert (= 42 (write-preferred (list 0))))
(assert (eq 'car (first (lexical-getter))))
(let ((forms (compare-environments)))
 (assert (eq 'car (first (first forms))))
 (assert (eq 'bits (second forms)))
 (assert (eq 'bits (third forms))))
#+egcl
(let ((caught nil) (count 0))
 (handler-case (get-setf-expansion '(map-alias s) (progn (incf count) 42))
  (program-error () (setq caught t)))
 (assert caught) (assert (= count 1)))
(write-line "MACRO-PLACE-OK")
"#;
fn scratch(name: &str) -> PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "egcl-fallback-{name}-{}-{serial}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn run(program: &str, stress: bool) -> String {
    let directory = scratch("runner");
    let script = directory.join("run.lisp");
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&script)
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .output()
        .unwrap();
    fs::remove_dir_all(directory).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("MACRO-PLACE-OK"), "{stdout}");
    stdout
}
fn require_bytecode(artifact: &std::path::Path) {
    let bytes = fs::read(artifact).unwrap();
    assert!(bytes.starts_with(b"BFASL\0"));
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut cursor = 24;
    let mut bbu = false;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[cursor + 2..cursor + 6].try_into().unwrap()) as usize;
        cursor += 6;
        assert_ne!(
            kind, 11,
            "whole-source fallback loses compile-time expansion"
        );
        if kind == 12 {
            bbu = bytes[cursor..cursor + size].starts_with(b"BBU\0");
        }
        cursor += size;
    }
    assert!(bbu);
}
#[test]
fn macro_places_preserve_lexical_environments_and_explicit_setf_precedence() {
    let program = format!("{DEFINITIONS}\n{CHECKS}\n(compile 'write-map)\n{CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn macro_places_survive_source_free_compiled_fallbacks() {
    let directory = scratch("macro-place");
    let source = directory.join("places.lisp");
    let artifact = directory.join("places.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            r#"
      (multiple-value-bind (file warnings failure)
          (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
      (write-line "MACRO-PLACE-OK")"#
        ),
        false,
    );
    require_bytecode(&artifact);
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?})\n{CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
