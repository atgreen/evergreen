// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defstruct data-parent (value nil :type (or integer null)))
(macrolet ((initial-value () 42))
  (defstruct scoped-data
    (value nil :type (or integer null))
    (frozen (initial-value) :type integer :read-only t))
  (defstruct (included-data (:include data-parent
                              (value (initial-value) :type (or integer null)))))
  (defstruct (boa-data (:constructor make-boa-data
                         (&optional (value (initial-value)))))
    (value nil :type (or integer null)))
  (defstruct named-data (or (initial-value) :type integer)))
"#;
const CHECKS: &str = r#"
(dotimes (iteration 20)
  (assert (null (data-parent-value (make-data-parent))))
  (assert (null (scoped-data-value (make-scoped-data))))
  (assert (= (scoped-data-value (make-scoped-data :value 41)) 41))
  (assert (= (scoped-data-frozen (make-scoped-data)) 42))
  (assert (= (included-data-value (make-included-data)) 42))
  (assert (= (boa-data-value (make-boa-data)) 42))
  (assert (null (boa-data-value (make-boa-data nil))))
  (assert (= (named-data-or (make-named-data)) 42)))
(write-line "DEFSTRUCT-DATA-OK")
"#;

fn run(program: &str, stress: bool) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .arg("--no-init")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .args(["--eval", &format!("(progn {program})")])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "stress={stress}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("DEFSTRUCT-DATA-OK"), "{stdout}");
    stdout
}

#[test]
fn source_macrolet_structure_types_and_slot_names_remain_data() {
    let program = r#"
      (macrolet ((unused () 42))
        (defstruct source-data (or nil :type (or integer null))))
      (assert (null (source-data-or (make-source-data))))
      (assert (= (source-data-or (make-source-data :or 42)) 42))
      (write-line "DEFSTRUCT-DATA-OK")"#;
    assert_eq!(run(program, false), run(program, true));
}

#[test]
fn lexical_structure_initializers_and_types_survive_source_free_bytecode() {
    let directory =
        std::env::temp_dir().join(format!("egcl-defstruct-data-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("structures.lisp");
    let artifact = directory.join("structures.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line \"DEFSTRUCT-DATA-OK\")"
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
