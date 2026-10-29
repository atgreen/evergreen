// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defmacro the-value-form () '42)
(defun union-the (value) (the (or integer null) value))
(defun nested-the (value) (the (and (or integer null) (not string)) value))
(defun declared-union (value) (declare (type (or integer null) value)) value)
(defun expanded-the-value () (the (or integer null) (the-value-form)))
"#;
const CHECKS: &str = r#"
(assert (null (union-the nil)))
(assert (= (union-the 41) 41))
(assert (null (nested-the nil)))
(assert (= (nested-the 42) 42))
(assert (null (declared-union nil)))
(assert (= (expanded-the-value) 42))
(write-line "THE-TYPE-POSITION-OK")
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
    assert!(stdout.contains("THE-TYPE-POSITION-OK"), "{stdout}");
    stdout
}

#[test]
fn the_preserves_type_data_and_expands_its_executable_value() {
    let program = format!(
        "{DEFINITIONS}
        (defmacro the-value-form () '(error \"value macro expanded too late\"))
        {CHECKS}
        (let ((function (compile nil '(lambda (value) (the (or integer null) value)))))
          (assert (null (funcall function nil)))
          (assert (= (funcall function 42) 42)))
        (let ((function (compile nil '(lambda () (the (or integer null) (+ 20 22))))))
          (assert (= (funcall function) 42)))"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn fresh_bytecode_load_preserves_the_when_creating_functions() {
    let directory =
        std::env::temp_dir().join(format!("egcl-the-type-position-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("types.lisp");
    let artifact = directory.join("types.bfasl");
    fs::write(
        &source,
        format!(
            "{DEFINITIONS}
        (defparameter *loaded-the-function*
          (compile nil '(lambda (value) (the (or integer null) value))))"
        ),
    )
    .unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
        (compile-file {source:?} :output-file {artifact:?})
        (declare (ignore warnings)) (assert file) (assert (not failure)))
        (write-line \"THE-TYPE-POSITION-OK\")"
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
    let program = format!(
        "(load {artifact:?}) {CHECKS}
        (assert (null (funcall *loaded-the-function* nil)))
        (assert (= (funcall *loaded-the-function* 42) 42))"
    );
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
