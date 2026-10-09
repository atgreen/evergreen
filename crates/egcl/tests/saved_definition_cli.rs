// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Saved compiled functions own their definitions across replacement and images.
use std::path::{Path, PathBuf};
use std::process::Command;

struct Fixture(PathBuf);

impl Fixture {
    fn new() -> Self {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "egcl-saved-definition-{}-{nonce}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }

    fn compile(&self, version: i32) -> PathBuf {
        let source = self.0.join(format!("version-{version}.lisp"));
        let fasl = source.with_extension("bfasl");
        std::fs::write(
            &source,
            format!("(defun retained-definition (x) (values (+ x {version}) :version-{version}))"),
        )
        .unwrap();
        run(
            &format!(
                "(compile-file {source:?} :output-file {fasl:?}) (format t \"COMPILED-OK~%\")"
            ),
            "COMPILED-OK",
            "0",
            None,
        );
        std::fs::remove_file(source).unwrap();
        fasl
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(program: &str, marker: &str, native: &str, image: Option<&Path>) {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=5",
        "180",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
    ]);
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
    let output = command
        .args(["--eval", program])
        .env("EGCL_NATIVE_TRANSFER", native)
        .env("EGCL_T0_T1_THRESHOLD", "1")
        .env("EGCL_T2_THRESHOLD", "3")
        .output()
        .expect("run saved-definition regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if std::env::var_os("EGCL_GC_STRESS_AFTER_INIT").is_some()
        && std::env::var("EGCL_GC_STRESS")
            .ok()
            .and_then(|stride| stride.parse::<u64>().ok())
            .is_some_and(|stride| stride > 0)
    {
        assert!(
            stderr.contains("EGCL_GC_STRESS_AFTER_INIT: stressing from allocation"),
            "stress must arm before the user forms: {marker}: {stderr}"
        );
    }
    assert!(
        output.status.success(),
        "{marker}: {}: {stdout}\n{stderr}",
        output.status
    );
    assert!(
        stdout.contains(marker),
        "missing {marker}: {stdout}\n{stderr}"
    );
}

fn replacement_preserves_saved_definition(native: &str, compiled_replacement: bool) {
    let fixture = Fixture::new();
    let old = fixture.compile(1);
    let replacement = if compiled_replacement {
        format!("(load {:?})", fixture.compile(2))
    } else {
        "(defun retained-definition (x) (values (+ x 2) :version-2))".into()
    };
    run(
        &format!(
            r#"
      (load {old:?})
      (defvar *saved-definition* #'retained-definition)
      (assert (null (egcl::%fn-body *saved-definition*)))
      (defun invoke-retained (f x) (funcall f x))
      (dotimes (i 40)
        (assert (equal '(42 :version-1)
                       (multiple-value-list (invoke-retained *saved-definition* 41)))))
      {replacement}
      (assert (not (eq *saved-definition* #'retained-definition)))
      (dotimes (i 40)
        (assert (equal '(42 :version-1)
                       (multiple-value-list (invoke-retained *saved-definition* 41))))
        (assert (equal '(43 :version-2)
                       (multiple-value-list (invoke-retained #'retained-definition 41)))))
      (fmakunbound 'retained-definition)
      (assert (equal '(42 :version-1)
                     (multiple-value-list (apply *saved-definition* '(41)))))
      (format t "SAVED-DEFINITION-OK~%")
    "#
        ),
        "SAVED-DEFINITION-OK",
        native,
        None,
    );
}

#[test]
fn saved_fasl_function_survives_source_defun() {
    replacement_preserves_saved_definition("0", false);
}

#[test]
fn saved_fasl_function_survives_fasl_reload() {
    replacement_preserves_saved_definition("0", true);
}

#[test]
fn native_saved_fasl_function_survives_source_defun() {
    replacement_preserves_saved_definition("1", false);
}

#[test]
fn native_saved_fasl_function_survives_fasl_reload() {
    replacement_preserves_saved_definition("1", true);
}

#[test]
fn restored_function_survives_redefinition() {
    let fixture = Fixture::new();
    let image = fixture.0.join("saved.bimg");
    run(
        &format!(
            r#"
      (defun retained-definition (x) (values (+ x 1) :version-1))
      (defvar *saved-definition* #'retained-definition)
      (dotimes (i 40) (funcall *saved-definition* i))
      (format t "IMAGE-SAVED-OK~%")
      (save-lisp-and-die {image:?})
    "#
        ),
        "IMAGE-SAVED-OK",
        "1",
        None,
    );
    run(
        r#"
      (assert (null (egcl::%fn-body *saved-definition*)))
      (defun retained-definition (x) (values (+ x 2) :version-2))
      (dotimes (i 40)
        (assert (equal '(42 :version-1)
                       (multiple-value-list (funcall *saved-definition* 41))))
        (assert (equal '(43 :version-2)
                       (multiple-value-list (retained-definition 41)))))
      (format t "IMAGE-REDEFINED-OK~%")
    "#,
        "IMAGE-REDEFINED-OK",
        "1",
        Some(&image),
    );
}

#[test]
fn image_preserves_multiple_retired_definitions_without_a_binding() {
    let fixture = Fixture::new();
    let first = fixture.compile(1);
    let second = fixture.compile(2);
    let image = fixture.0.join("retired.bimg");
    run(
        &format!(
            r#"
      (load {first:?})
      (defvar *first-definition* #'retained-definition)
      (load {second:?})
      (defvar *second-definition* #'retained-definition)
      (fmakunbound 'retained-definition)
      (format t "RETIRED-SAVED-OK~%")
      (save-lisp-and-die {image:?})
    "#
        ),
        "RETIRED-SAVED-OK",
        "1",
        None,
    );
    run(
        r#"
      (assert (not (fboundp 'retained-definition)))
      (dotimes (i 40)
        (assert (equal '(42 :version-1)
                       (multiple-value-list (funcall *first-definition* 41))))
        (assert (equal '(43 :version-2)
                       (multiple-value-list (apply *second-definition* '(41))))))
      (assert (equal '(42 :version-1)
        (egcl-thread:join-thread (egcl-thread:make-thread
          (lambda () (multiple-value-list (funcall *first-definition* 41)))))))
      (format t "RETIRED-RESTORED-OK~%")
    "#,
        "RETIRED-RESTORED-OK",
        "1",
        Some(&image),
    );
}

#[test]
fn saved_named_definitions_keep_distinct_captures_across_an_image() {
    let fixture = Fixture::new();
    let image = fixture.0.join("captured.bimg");
    let check = r#"
      (dotimes (i 40)
        (assert (equal '(42) (funcall *first-captured*)))
        (assert (equal '(99) (funcall *second-captured*))))
    "#;
    run(
        &format!(
            r#"
      (let ((capture (list 42))) (defun retained-captured () capture))
      (defvar *first-captured* #'retained-captured)
      (dotimes (i 40) (funcall *first-captured*))
      (let ((capture (list 99))) (defun retained-captured () capture))
      (defvar *second-captured* #'retained-captured)
      (dotimes (i 40) (funcall *second-captured*))
      (fmakunbound 'retained-captured)
      {check}
      (format t "CAPTURES-SAVED-OK~%")
      (save-lisp-and-die {image:?})
    "#
        ),
        "CAPTURES-SAVED-OK",
        "1",
        None,
    );
    run(
        &format!(
            r#"
      {check}
      (format t "CAPTURES-RESTORED-OK~%")
    "#
        ),
        "CAPTURES-RESTORED-OK",
        "1",
        Some(&image),
    );
}

#[test]
fn legacy_native_funcall_keeps_named_captures() {
    run(
        r#"
      (let ((capture (list 42))) (defun named-captured (x) (+ (car capture) x)))
      (defun invoke-named-captured (f x) (funcall f x))
      (dotimes (i 100)
        (assert (= 43 (invoke-named-captured #'named-captured 1))))
      (assert (= 2 (egcl-ext:function-tier 'named-captured)))
      (assert (= 2 (egcl-ext:function-tier 'invoke-named-captured)))
      (defvar *saved-named-captured* #'named-captured)
      (let ((capture (list 99))) (defun named-captured (x) (+ (car capture) x)))
      (dotimes (i 100)
        (assert (= 43 (invoke-named-captured *saved-named-captured* 1)))
        (assert (= 100 (invoke-named-captured #'named-captured 1))))
      (format t "LEGACY-CAPTURES-OK~%")
    "#,
        "LEGACY-CAPTURES-OK",
        "0",
        None,
    );
}
