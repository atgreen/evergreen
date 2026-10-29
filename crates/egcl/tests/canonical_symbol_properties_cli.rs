// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{
    fs,
    path::PathBuf,
    process::{Command, Output},
};

const CHECKS: &str = r#"
(defun canonical-get-or-initialize (symbol indicator thunk)
  (let (result)
    (funcall (lambda ()
               (setq result (get symbol indicator))
               (unless result
                 (setq result (funcall thunk))
                 (setf (get symbol indicator) result))))
    result))
(defun canonical-property-check ()
  (let ((package (or (find-package "CANONICAL-PROPERTY-IMPORT")
                     (make-package "CANONICAL-PROPERTY-IMPORT" :use nil))))
    (import (list nil t) package)
    (dolist (symbol (list nil t 'canonical-ordinary (make-symbol "T")))
      (let ((saved (symbol-plist symbol))
            (indicator (gensym "PROPERTY"))
            (calls 0))
        (unwind-protect
             (progn
               (assert (eq (get symbol indicator :absent) :absent))
               ;; SBCL's globaldb callback retains its new cell in a lexical
               ;; variable and publishes it through SETF GET on the class name.
               (let ((first (canonical-get-or-initialize
                             symbol indicator (lambda () (incf calls) (list :cell 42)))))
                 (assert (= calls 1))
                 (assert (eq first (get symbol indicator)))
                 (assert (eq first (canonical-get-or-initialize
                                    symbol indicator (lambda () (incf calls) (list :wrong)))))
                 (assert (= calls 1))
                 (assert (equal first '(:cell 42))))
               (setf (get symbol indicator) (list :replacement 43))
               (assert (equal (get symbol indicator) '(:replacement 43)))
               (assert (remprop symbol indicator))
               (assert (eq (get symbol indicator :absent) :absent))
               (assert (null (remprop symbol indicator)))
               (let ((replacement (list indicator (list :whole 44))))
                 (setf (symbol-plist symbol) replacement)
                 (assert (eq replacement (symbol-plist symbol)))
                 (assert (equal (get symbol indicator) '(:whole 44)))
                 (setf (getf (symbol-plist symbol) indicator) (list :getf 45))
                 (assert (equal (get symbol indicator) '(:getf 45)))))
          (setf (symbol-plist symbol) saved))))
    (dolist (symbol (list nil t))
      (let ((name (symbol-name symbol)))
        (assert (eq symbol (find-symbol name "COMMON-LISP")))
        (assert (eq symbol (intern name "COMMON-LISP")))
        (assert (eq symbol (find-symbol name package)))
        (assert (eq (symbol-package symbol) (find-package "COMMON-LISP")))
        (assert (eq (symbol-value symbol) symbol))
        (assert (constantp symbol))))
    (let ((seen nil))
      (do-external-symbols (symbol "COMMON-LISP")
        (when (member (symbol-name symbol) '("NIL" "T") :test #'string=)
          (assert (or (eq symbol nil) (eq symbol t)))
          (push symbol seen)))
      (assert (= (length seen) 2)))
    (assert (null nil))
    (assert (eq t (not nil)))
    t))
(canonical-property-check)
"#;

fn dir(label: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!(
        "egcl-canonical-properties-{label}-{}",
        std::process::id()
    ));
    fs::create_dir_all(&path).unwrap();
    path
}

fn command(stress: bool) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT");
    if stress {
        command
            .env("EGCL_GC_STRESS", "17")
            .env("EGCL_GC_STRESS_AFTER_INIT", "1")
            .env("EGCL_GC_POISON", "1");
    } else {
        command
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_POISON");
    }
    command
}

fn check(output: Output, marker: &str) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains(marker), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn nil_and_t_properties_preserve_callback_identity_and_package_identity() {
    let program = format!("(progn {CHECKS} (write-line \"CANONICAL-PROPERTIES-OK\"))");
    let normal = check(
        command(false)
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap(),
        "CANONICAL-PROPERTIES-OK",
    );
    let stressed = check(
        command(true)
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap(),
        "CANONICAL-PROPERTIES-OK",
    );
    assert_eq!(normal, stressed);
}

#[test]
fn compiled_nil_and_t_properties_work_without_source() {
    let directory = dir("compiled");
    let source = directory.join("properties.lisp");
    let artifact = directory.join("properties.bfasl");
    fs::write(&source, CHECKS).unwrap();
    check(command(false).args(["--no-init", "--eval", &format!(
        "(progn (multiple-value-bind (file warnings failure) (compile-file {source:?} :output-file {artifact:?}) (declare (ignore file warnings)) (assert (not failure))) (write-line \"CANONICAL-COMPILED-OK\"))"
    )]).output().unwrap(), "CANONICAL-COMPILED-OK");
    let bytes = fs::read(&artifact).unwrap();
    // SBCL oracle emits its own FASL format; EGCL must contain a real BBU.
    if bytes.starts_with(b"BFASL\0") {
        let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let mut offset = 24;
        let mut bytecode = false;
        for _ in 0..count {
            let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
            let size =
                u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
            offset += 6;
            if kind == 12 {
                bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
            }
            offset += size;
        }
        assert!(bytecode, "compiled properties must emit a BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program = format!(
        "(progn (load {artifact:?}) (assert (canonical-property-check)) (write-line \"CANONICAL-COMPILED-OK\"))"
    );
    let normal = check(
        command(false)
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap(),
        "CANONICAL-COMPILED-OK",
    );
    let stressed = check(
        command(true)
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap(),
        "CANONICAL-COMPILED-OK",
    );
    assert_eq!(normal, stressed);
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn canonical_properties_and_identity_survive_saved_core() {
    // Saving and restoring an EGCL core is an implementation-specific neighbor.
    let is_egcl = command(false)
        .args([
            "--no-init",
            "--eval",
            "(write-line (if (member :egcl *features*) \"EGCL\" \"ORACLE\"))",
        ])
        .output()
        .unwrap();
    if !String::from_utf8_lossy(&is_egcl.stdout)
        .lines()
        .any(|line| line == "EGCL")
    {
        assert!(is_egcl.status.success());
        return;
    }
    let directory = dir("image");
    let image = directory.join("properties.core");
    let save = format!(
        r#"(progn
      (setf (get nil :canonical-image) (list :nil 71))
      (setf (get t :canonical-image) (list :t 72))
      (egcl-ext:save-lisp-and-die {image:?}))"#
    );
    let saved = command(false)
        .args(["--no-init", "--eval", &save])
        .output()
        .unwrap();
    assert!(
        saved.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&saved.stdout),
        String::from_utf8_lossy(&saved.stderr)
    );
    check(
        command(true)
            .args(["--no-init", "--image"])
            .arg(&image)
            .args([
                "--eval",
                r#"(progn
      (assert (equal (get nil :canonical-image) '(:nil 71)))
      (assert (equal (get t :canonical-image) '(:t 72)))
      (dolist (symbol (list nil t))
        (assert (eq symbol (find-symbol (symbol-name symbol) "COMMON-LISP")))
        (assert (eq symbol (intern (symbol-name symbol) "COMMON-LISP")))
        (assert (eq (symbol-package symbol) (find-package "COMMON-LISP"))))
      (dolist (symbol (list nil t))
        (let* ((old (symbol-plist symbol))
               (replacement (list :cas-value (list :moved 74))))
          (assert (eq (egcl-ext:cas (symbol-plist symbol) :wrong nil) old))
          (assert (eq (egcl-ext:cas (symbol-plist symbol) old replacement) old))
          (assert (eq (symbol-plist symbol) replacement))
          (assert (equal (get symbol :cas-value) '(:moved 74)))
          (setf (symbol-plist symbol) old)))
      (setf (get t :canonical-image) (list :updated 73))
      (assert (equal (get t :canonical-image) '(:updated 73)))
      (assert (remprop nil :canonical-image))
      (assert (null (get nil :canonical-image)))
      (write-line "CANONICAL-IMAGE-OK"))"#,
            ])
            .output()
            .unwrap(),
        "CANONICAL-IMAGE-OK",
    );
    fs::remove_dir_all(directory).unwrap();
}
