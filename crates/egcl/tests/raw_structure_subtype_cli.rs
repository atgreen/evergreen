// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defstruct ssub-base value)
(defstruct (ssub-left (:include ssub-base)) left)
(defstruct (ssub-right (:include ssub-base)) right)
(defstruct ssub-separate other)
(defstruct (ssub-list (:type list)) value)
(defstruct (ssub-vector (:type vector)) value)
(deftype ssub-instance ()
  '(or (and structure-object (not ssub-separate)) package))
(defclass ssub-standard-left () ())
(defclass ssub-standard-right () ())
(defclass ssub-standard-both (ssub-standard-left ssub-standard-right) ())
(defun ssub-overlap (object) (typep object 'ssub-base))
(defun ssub-prove (left right)
  (multiple-value-bind (yes certain) (subtypep left right)
    (assert yes) (assert certain)))
(defun ssub-no-positive (left right)
  ;; Undefined type names may be rejected or conservatively left unknown.
  ;; Keep the assertion outside IGNORE-ERRORS so a false proof cannot hide.
  (multiple-value-bind (answer failure)
      (ignore-errors (multiple-value-list (subtypep left right)))
    (unless failure (assert (not (first answer))))))
"#;

const CHECKS: &str = r#"
(dotimes (iteration 20)
  (ssub-prove 'ssub-base 'structure-object)
  (ssub-prove 'ssub-left 'ssub-base)
  (ssub-prove 'ssub-base '(not ssub-separate))
  (ssub-prove 'ssub-left '(not ssub-right))
  (ssub-prove 'ssub-right '(not ssub-left))
  (ssub-prove 'ssub-left '(and structure-object ssub-base))
  (ssub-prove 'ssub-left '(or ssub-left package))
  (ssub-prove 'ssub-left
              '(and ssub-base (or ssub-left package) (not ssub-right)))
  (ssub-prove 'ssub-base 'ssub-instance)
  (ssub-prove 'ssub-left 'ssub-instance)
  (ssub-no-positive 'ssub-base '(not ssub-base))
  (ssub-no-positive 'ssub-left '(not ssub-base))
  (ssub-no-positive 'ssub-base '(not ssub-left))
  ;; Multiple inheritance makes the standard classes overlap.
  (ssub-no-positive 'ssub-standard-left '(not ssub-standard-right))
  (ssub-no-positive 'ssub-list '(not ssub-vector))
  (ssub-no-positive 'ssub-base '(not (satisfies ssub-overlap)))
  (ssub-no-positive 'ssub-base '(not ssub-never-defined)))
(let ((object (make-instance 'ssub-standard-both)))
  (assert (typep object 'ssub-standard-left))
  (assert (typep object 'ssub-standard-right)))
(assert (listp (make-ssub-list)))
(assert (vectorp (make-ssub-vector)))
(write-line "STRUCTURE-SUBTYPE-PROOF-OK")
"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-structure-subtype-run-{}-{serial}.lisp",
        std::process::id()
    ));
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--no-init")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .arg("--load")
        .arg(&script)
        .output()
        .unwrap();
    fs::remove_file(script).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("STRUCTURE-SUBTYPE-PROOF-OK"), "{stdout}");
    stdout
}

#[test]
fn raw_structure_boolean_proofs_preserve_overlap_and_unknown_cases() {
    let program = format!(
        "{DEFINITIONS}
         (let ((function (compile nil '(lambda ()
                             (multiple-value-list
                               (subtypep 'ssub-base 'ssub-instance))))))
           (assert (equal (funcall function) '(t t))))
         {CHECKS}"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn raw_structure_proofs_survive_fresh_bytecode_loading() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-structure-subtype-compiled-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("structures.lisp");
    let artifact = directory.join("structures.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
                 (compile-file {source:?} :output-file {artifact:?})
               (declare (ignore warnings)) (assert file) (assert (not failure)))
             (write-line \"STRUCTURE-SUBTYPE-PROOF-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
        let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..sections {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            assert_ne!(kind, 11, "unexpected source fallback");
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "expected portable BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS}");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
