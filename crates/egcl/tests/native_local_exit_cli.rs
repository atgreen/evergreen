// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Semantic coverage under requested tiers; native entry is proved separately
//! by the native unit fixture, not by these environment flags.

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defvar *native-local-exit-special* :global)
(defun local-exit-return-values (mode marker)
  (let ((*native-local-exit-special* :outer))
    (catch 'local-exit-outer
      (multiple-value-prog1
          (block done
            (let ((*native-local-exit-special* :inner))
              (catch 'local-exit-inner
                (let ((*native-local-exit-special* :deep))
                  (ecase mode
                    (:zero (return-from done (values)))
                    (:one (return-from done marker))
                    (:many (return-from done (values marker nil marker 42)))
                    (:outer (return-from done marker))
                    (:throw (throw 'local-exit-inner marker)))))))
        (assert (eq *native-local-exit-special* :outer))
        ;; The enclosing catch must remain usable after the local exit removed
        ;; only the inner catch and both nested special bindings.
        (when (eq mode :outer) (throw 'local-exit-outer marker))))))
(defun local-exit-go (throwp marker)
  (let ((*native-local-exit-special* :outer) (answer :unset))
    (catch 'local-exit-outer
      (tagbody
        (let ((*native-local-exit-special* :inner))
          (catch 'local-exit-inner
            (let ((*native-local-exit-special* :deep))
              (if throwp
                  (throw 'local-exit-inner marker)
                  (progn (setf answer marker) (go done))))))
        (setf answer (list :thrown marker))
       done)
      (assert (eq *native-local-exit-special* :outer))
      (throw 'local-exit-outer answer))))
"#;

const CHECKS: &str = r#"
(dotimes (iteration 64)
  (let ((marker (list :heap-marker iteration (vector iteration))))
    (assert (null (multiple-value-list (local-exit-return-values :zero marker))))
    (let ((one (multiple-value-list (local-exit-return-values :one marker))))
      (assert (= (length one) 1))
      (assert (eq (car one) marker)))
    (let ((many (multiple-value-list (local-exit-return-values :many marker))))
      (assert (= (length many) 4))
      (assert (eq (first many) marker))
      (assert (null (second many)))
      (assert (eq (third many) marker))
      (assert (= (fourth many) 42)))
    (assert (eq (local-exit-return-values :outer marker) marker))
    (assert (eq (local-exit-return-values :throw marker) marker))
    (assert (eq (local-exit-go nil marker) marker))
    (let ((thrown (local-exit-go t marker)))
      (assert (eq (first thrown) :thrown))
      (assert (eq (second thrown) marker)))
    (assert (eq *native-local-exit-special* :global))))
"#;

fn unique_path(label: &str) -> std::path::PathBuf {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "egcl-native-local-exit-{label}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ))
}

fn run(program: &str, tier: &str, stress: bool) -> String {
    let script = unique_path("script").with_extension("lisp");
    fs::write(&script, program).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--load"])
        .arg(&script)
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_LAZY_COMPILE", "0")
        .env_remove("EGCL_T2_LOG")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .env("EGCL_GC_STRESS", if stress { "17" } else { "0" })
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", if stress { "1" } else { "0" })
        .output()
        .unwrap();
    fs::remove_file(script).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "tier={tier}, stress={stress}\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("NATIVE-LOCAL-EXITS-OK"), "{stdout}");
    stdout
}

fn check_modes(program: &str) {
    for tier in ["t1", "t2"] {
        assert_eq!(run(program, tier, false), run(program, tier, true));
    }
}

#[test]
fn local_exits_restore_bindings_catches_and_values_in_source_and_compile() {
    check_modes(&format!(
        "{DEFINITIONS}\n{CHECKS}\n\
         (compile 'local-exit-return-values)\n\
         (compile 'local-exit-go)\n{CHECKS}\n\
         (write-line \"NATIVE-LOCAL-EXITS-OK\")"
    ));
}

#[test]
fn local_exits_survive_source_free_portable_bytecode_loading() {
    let directory = unique_path("artifact");
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("definitions.lisp");
    let artifact = directory.join("definitions.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)\n\
             (compile-file {source:?} :output-file {artifact:?})\n\
             (declare (ignore warnings)) (assert file) (assert (not failure)))\n\
             (write-line \"NATIVE-LOCAL-EXITS-OK\")"
        ),
        "t1",
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    assert!(
        bytes.starts_with(b"BFASL\0"),
        "expected an actual BFASL artifact"
    );
    assert!(bytes.len() >= 24);
    let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut offset = 24;
    let mut bytecode = false;
    for _ in 0..sections {
        assert!(offset + 6 <= bytes.len());
        let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
        offset += 6;
        assert!(offset + size <= bytes.len());
        assert_ne!(kind, 11, "unexpected TOPLEVEL_FORMS source fallback");
        if kind == 12 {
            bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
        }
        offset += size;
    }
    assert!(bytecode, "expected a portable BYTECODE_UNIT");
    fs::remove_file(source).unwrap();
    check_modes(&format!(
        "(load {artifact:?})\n{CHECKS}\n(write-line \"NATIVE-LOCAL-EXITS-OK\")"
    ));
    fs::remove_dir_all(directory).unwrap();
}
