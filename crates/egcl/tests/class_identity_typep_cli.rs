// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const HELPER: &str = "(declaim (notinline class-identity-member))";
const FUNCTION: &str = "(defun class-identity-member (object class) (typep object class))";
const CLASSES: &str = r#"
(defclass identity-parent () ())
(defclass identity-child (identity-parent) ())
(defclass identity-unrelated () ())
(defparameter *identity-parent* (find-class 'identity-parent))
(defparameter *identity-class* (find-class 'identity-child))
(defparameter *identity-unrelated* (find-class 'identity-unrelated))
(defparameter *identity-object* (make-instance 'identity-child))
(defun identity-warm ()
  (assert (class-identity-member *identity-object* *identity-class*))
  (assert (class-identity-member *identity-object* *identity-parent*))
  (assert (not (class-identity-member *identity-object* *identity-unrelated*))))
(dotimes (i 40) (identity-warm))
"#;
const READY: &str = r#"
(loop repeat 100 until (= 2 (egcl-ext:function-tier 'class-identity-member))
      do (identity-warm) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'class-identity-member)))
"#;
const CHECK: &str = r#"
(assert (typep *identity-object* *identity-class*))
(setf (find-class 'identity-child) nil)
(assert (eq (class-of *identity-object*) *identity-class*))
(assert (typep *identity-object* *identity-class*))
(assert (typep *identity-object* (class-of *identity-object*)))
(assert (class-identity-member *identity-object* *identity-class*))
(assert (class-identity-member *identity-object* *identity-parent*))
(assert (not (class-identity-member *identity-object* *identity-unrelated*)))
(multiple-value-bind (yes certain) (subtypep *identity-class* *identity-class*)
  (assert yes) (assert certain))
(defclass identity-child () ())
(assert (not (eq (find-class 'identity-child) *identity-class*)))
(assert (class-identity-member *identity-object* *identity-class*))
(assert (not (class-identity-member *identity-object* (find-class 'identity-child))))
;; The superclass remains a type after its own registration is removed too.
(setf (find-class 'identity-parent) nil)
(assert (class-identity-member *identity-object* *identity-parent*))
;; Built-in and opaque representations keep their established predicates.
(assert (typep 1 (find-class 'integer)))
(assert (typep :keyword (find-class 'symbol)))
(assert (typep "abc" (find-class 'string)))
(assert (typep (make-array 2) (find-class 'array)))
(defgeneric identity-generic (object))
(defmethod identity-generic ((object t)) object)
(assert (typep #'identity-generic (find-class 'standard-generic-function)))
(format t "CLASS-IDENTITY-TYPEP-OK~%")
"#;

fn run(program: &str, tier: &str, stress: bool, image: Option<&PathBuf>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    for name in [
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_GC_STRESS_SKIP",
        "EGCL_GC_STRESS_AT",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
    ] {
        command.env_remove(name);
    }
    command
        .env("EGCL_FORCE_TIER", tier)
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_T2_THREADS", "1")
        .env("EGCL_T2_LOG", "stderr")
        .arg("--no-init");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1");
    }
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    command.args(["--eval", program]).output().unwrap()
}

fn passed(output: &Output) -> String {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn typep_keeps_class_identity_in_interpreter_native_and_stress_modes() {
    let mut reference = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let ready = if tier == "t2" { READY } else { "" };
        let output = run(
            &format!("{HELPER}{FUNCTION}{CLASSES}{ready}{CHECK}"),
            tier,
            stress,
            None,
        );
        let stdout = passed(&output);
        assert!(stdout.contains("CLASS-IDENTITY-TYPEP-OK"));
        if tier == "t2" {
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                stderr.lines().any(|line| line
                    .contains("CLASS-IDENTITY-MEMBER: optimized worker result installed with ")
                    && (line.contains("ordinary x86-64 Lisp T2 ABI")
                        || line.contains("mapped T2 ABI"))),
                "the public helper must install actual native T2 code\n{stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}

fn sections(bytes: &[u8]) -> Vec<u16> {
    assert!(bytes.len() >= 28, "truncated BFASL header");
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let end = bytes.len() - 4;
    let mut cursor = 24usize;
    let mut kinds = Vec::new();
    for _ in 0..count {
        assert!(cursor + 6 <= end, "truncated section header");
        let kind = u16::from_le_bytes(bytes[cursor..cursor + 2].try_into().unwrap());
        let length = u32::from_le_bytes(bytes[cursor + 2..cursor + 6].try_into().unwrap()) as usize;
        cursor = cursor.checked_add(6).unwrap().checked_add(length).unwrap();
        assert!(cursor <= end, "truncated section payload");
        kinds.push(kind);
    }
    kinds
}

#[test]
fn class_identity_survives_source_free_calls_and_core_images() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-class-identity-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("member.lisp");
    let fasl = directory.join("member.bfasl");
    let image = directory.join("identity.core");
    fs::write(&source, FUNCTION).unwrap();
    passed(&run(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "interp",
        false,
        None,
    ));
    let kinds = sections(&fs::read(&fasl).unwrap());
    assert!(kinds.contains(&12), "must contain a bytecode unit");
    assert!(!kinds.contains(&11), "must not contain source fallback");
    fs::remove_file(&source).unwrap();
    let setup = format!("{HELPER}(load {fasl:?}){CLASSES}{CHECK}");
    let saved = run(
        &format!("{setup}(egcl-ext:save-lisp-and-die {image:?})"),
        "interp",
        false,
        None,
    );
    assert!(passed(&saved).contains("CLASS-IDENTITY-TYPEP-OK"));
    let check = r#"
(assert (not (eq *identity-class* (find-class 'identity-child))))
(assert (class-identity-member *identity-object* *identity-class*))
(assert (class-identity-member *identity-object* *identity-parent*))
(assert (not (class-identity-member *identity-object* (find-class 'identity-child))))
(format t "CLASS-IDENTITY-IMAGE-OK~%")
"#;
    assert!(passed(&run(check, "t2", true, Some(&image))).contains("CLASS-IDENTITY-IMAGE-OK"));
    fs::remove_dir_all(directory).unwrap();
}
