// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Portable U8 literals cross COMPILE-FILE and a fresh loader process with no
//! source file or source fallback section. Wrapper state need not survive
//! constant externalization; specialization, dimensions and contents do.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
            .join(format!("packed-u8-bfasl-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn egcl(dir: &Path, program: &str, stress: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .current_dir(dir)
        .args(["--no-init", "--eval", program]);
    command
        .env_remove("EGCL_GC_STRESS")
        .env_remove("EGCL_GC_STRESS_AFTER_INIT")
        .env_remove("EGCL_GC_POISON")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    }
    command.output().expect("spawn egcl")
}

fn require_success(output: &Output) {
    assert!(
        output.status.success(),
        "status={}\n{}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn packed_u8_literals_load_in_fresh_processes_without_source_fallback() {
    let dir = Scratch::new();
    std::fs::write(
        dir.0.join("octets.lisp"),
        r#"
(defparameter *u8-compiled-literals*
  #.(let* ((base (make-array '(8) :element-type '(unsigned-byte 8)
                                  :initial-contents '(0 1 127 128 254 255 17 33)))
           (matrix (make-array '(2 3) :element-type '(unsigned-byte 8)
                                      :initial-element 255))
           (adjustable (make-array 6 :element-type '(unsigned-byte 8)
                                    :initial-element 42 :fill-pointer 3 :adjustable t))
           (view (make-array 4 :element-type '(unsigned-byte 8)
                              :displaced-to base :displaced-index-offset 2 :fill-pointer 2))
           (matrix-view (make-array '(2 2) :element-type '(unsigned-byte 8)
                                         :displaced-to base :displaced-index-offset 3)))
      (vector base base matrix adjustable view matrix-view)))
"#,
    )
    .unwrap();
    let compiled = egcl(
        &dir.0,
        r#"(progn (compile-file "octets.lisp" :output-file "octets.bfasl") (format t "U8-COMPILED~%"))"#,
        false,
    );
    require_success(&compiled);
    let compile_log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&compiled.stdout),
        String::from_utf8_lossy(&compiled.stderr)
    );
    assert!(compile_log.contains("U8-COMPILED"), "{compile_log}");
    assert!(
        !compile_log.to_ascii_lowercase().contains("source-only"),
        "{compile_log}"
    );
    let image = egcl_rt::bfasl::load(&std::fs::read(dir.0.join("octets.bfasl")).unwrap()).unwrap();
    assert!(
        image
            .section(egcl_rt::bfasl::section::TOPLEVEL_FORMS)
            .is_none(),
        "literal fixture must compile entirely to bytecode"
    );
    let bbu = image
        .section(egcl_rt::bfasl::section::BYTECODE_UNIT)
        .expect("compile-file must emit BYTECODE_UNIT");
    assert!(bbu.len() >= 40);
    assert_eq!(&bbu[..4], b"BBU\0");
    assert!(u16::from_le_bytes(bbu[4..6].try_into().unwrap()) >= 0x0204);
    assert_ne!(
        u32::from_le_bytes(bbu[8..12].try_into().unwrap()) & 1,
        0,
        "BYTECODE_UNIT must be complete and authoritative"
    );
    // Retain only the authoritative unit. Neither a source file nor an outer
    // source/debug fallback can rescue failed constant externalization.
    let portable = egcl_rt::bfasl::BfaslBuilder::new()
        .content_hash(image.content_hash)
        .section(egcl_rt::bfasl::section::BYTECODE_UNIT, bbu.to_vec())
        .build();
    std::fs::write(dir.0.join("portable.bfasl"), portable).unwrap();
    std::fs::remove_file(dir.0.join("octets.lisp")).unwrap();
    std::fs::remove_file(dir.0.join("octets.bfasl")).unwrap();

    const CHECKS: &str = r#"(progn
      (load "portable.bfasl")
      (let ((base (aref *u8-compiled-literals* 0))
            (same (aref *u8-compiled-literals* 1))
            (matrix (aref *u8-compiled-literals* 2))
            (adjustable (aref *u8-compiled-literals* 3))
            (view (aref *u8-compiled-literals* 4))
            (matrix-view (aref *u8-compiled-literals* 5)))
        (assert (eq base same))
        (dolist (array (list base matrix adjustable view matrix-view))
          (assert (equal (array-element-type array) '(unsigned-byte 8))))
        (assert (equal (array-dimensions base) '(8)))
        (assert (equalp base #(0 1 127 128 254 255 17 33)))
        (assert (equal (array-dimensions matrix) '(2 3)))
        (dotimes (i 6) (assert (= (row-major-aref matrix i) 255)))
        (assert (equal (array-dimensions adjustable) '(3)))
        (assert (equalp adjustable #(42 42 42)))
        (assert (equal (array-dimensions view) '(2)))
        (assert (equalp view #(127 128)))
        (assert (equal (array-dimensions matrix-view) '(2 2)))
        (dotimes (i 4)
          (assert (= (row-major-aref matrix-view i) (aref base (+ i 3)))))
        (setf (aref base 0) 255)
        (assert (= (aref same 0) 255)))
      (format t "PACKED-U8-FRESH-BFASL-OK~%"))"#;
    for stress in [false, true] {
        let loaded = egcl(&dir.0, CHECKS, stress);
        require_success(&loaded);
        let stdout = String::from_utf8_lossy(&loaded.stdout);
        let stderr = String::from_utf8_lossy(&loaded.stderr);
        assert!(
            stdout.contains("PACKED-U8-FRESH-BFASL-OK"),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        if stress {
            assert!(
                stderr.contains("[gc-stress] EGCL_GC_STRESS_AFTER_INIT: stressing from allocation"),
                "stress must actually be enabled: {stderr}"
            );
        }
    }
}
