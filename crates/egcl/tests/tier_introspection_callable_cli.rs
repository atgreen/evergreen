// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defpackage :tier-api-imports (:use :cl)
  (:import-from :egcl-ext :function-tier :function-osr-count))
(defpackage :tier-api-foreign (:use :cl)
  (:shadow :function-tier :function-osr-count))
(defun tier-api-target () 42)
(defun tier-api-apply (function name) (funcall function name))
(defun tier-api-map (names)
  (list (mapcar #'egcl-ext:function-tier names)
        (mapcar #'egcl-ext:function-osr-count names)))
(defun tier-api-replacement (name) (declare (ignore name)) :replacement)
(defun check-callable-tier-api ()
  (dolist (name '(egcl-ext:function-tier egcl-ext:function-osr-count))
    (assert (fboundp name))
    (assert (functionp (fdefinition name)))
    (assert (eq (fdefinition name) (symbol-function name)))
    (let ((value (tier-api-apply (fdefinition name) 'tier-api-target)))
      (assert (integerp value))
      (assert (>= value 0))))
  (assert (eq 'egcl-ext:function-tier 'egcl-ext::function-tier))
  (assert (eq 'egcl-ext:function-tier 'tier-api-imports::function-tier))
  (assert (eq 'egcl-ext:function-osr-count 'tier-api-imports::function-osr-count))
  (assert (functionp #'egcl-ext:function-tier))
  (assert (functionp #'egcl-ext:function-osr-count))
  (let ((answer (tier-api-map '(tier-api-target tier-api-target))))
    (assert (= 2 (length answer)))
    (dolist (values answer)
      (assert (= 2 (length values)))
      (dolist (value values) (assert (integerp value)) (assert (>= value 0)))))
  (assert (not (fboundp 'tier-api-foreign::function-tier)))
  (assert (not (fboundp 'tier-api-foreign::function-osr-count)))
  (assert (integerp (egcl-ext:function-tier 'tier-api-target)))
  (assert (= 0 (egcl-ext:function-osr-count 'tier-api-target)))
  (assert (integerp (eval '(egcl-ext::function-tier 'tier-api-target))))
  (assert (= 0 (eval '(egcl-ext::function-osr-count 'tier-api-target))))
  t)
"#;

// Each test process is disposable: replacements are made after all original
// API checks, avoiding assumptions about retained wrappers after redefinition.
const REPLACEMENTS: &str = r#"
(dolist (name '(egcl-ext:function-tier egcl-ext:function-osr-count))
  (setf (fdefinition name) #'tier-api-replacement)
  (assert (eq (fdefinition name) #'tier-api-replacement))
  (assert (eq (tier-api-apply (fdefinition name) 'tier-api-target) :replacement)))
(defun tier-api-foreign::function-tier (name) (declare (ignore name)) :foreign-tier)
(defun tier-api-foreign::function-osr-count (name) (declare (ignore name)) :foreign-osr)
(assert (eq (funcall #'tier-api-foreign::function-tier 'tier-api-target) :foreign-tier))
(assert (eq (funcall #'tier-api-foreign::function-osr-count 'tier-api-target) :foreign-osr))
"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "egcl-tier-api-{}-{serial}.lisp",
        std::process::id()
    ));
    fs::write(&path, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&path)
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .output()
        .unwrap();
    fs::remove_file(path).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("CALLABLE-TIER-API-OK"), "{stdout}");
    stdout
}

#[test]
fn tier_introspection_functions_are_first_class_from_source_and_compile() {
    let program = format!(
        "{DEFINITIONS}
        (assert (check-callable-tier-api))
        (compile 'tier-api-apply) (compile 'tier-api-map)
        (assert (check-callable-tier-api))
        {REPLACEMENTS} (write-line \"CALLABLE-TIER-API-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn tier_introspection_function_references_survive_fresh_bytecode_loading() {
    let directory = std::env::temp_dir().join(format!("egcl-tier-api-bbu-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("tier-api.lisp");
    let artifact = directory.join("tier-api.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line \"CALLABLE-TIER-API-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    assert!(bytes.starts_with(b"BFASL\0"));
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut offset = 24;
    let mut bytecode = false;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
        offset += 6;
        assert_ne!(
            kind, 11,
            "source fallback cannot prove compiled function references"
        );
        if kind == 12 {
            bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
        }
        offset += size;
    }
    assert!(bytecode, "expected portable BYTECODE_UNIT");
    fs::remove_file(&source).unwrap();
    let program = format!(
        "(load {artifact:?}) (assert (check-callable-tier-api))
        (compile 'tier-api-apply) (assert (check-callable-tier-api))
        {REPLACEMENTS} (write-line \"CALLABLE-TIER-API-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
