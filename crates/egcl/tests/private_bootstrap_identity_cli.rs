// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defpackage :private-a (:use :cl))
(defpackage :private-b (:use :cl))
(in-package :private-a)
(defun %defconstant (name value source-location &optional doc)
  (list :a name value source-location doc))
(defun %mark-constant (value) (list :a value))
(in-package :private-b)
(defun %defconstant (name value source-location &optional doc)
  (list :b name value source-location doc))
(defun %mark-constant (value) (list :b value))
(in-package :cl-user)
(defun %defconstant (name value source-location &optional doc)
  (list :user name value source-location doc))
(defun %mark-constant (value) (list :user value))
(defconstant +host-private-isolation+ 42)
(eval-when (:compile-toplevel :load-toplevel :execute)
  (export '(private-a::%defconstant private-a::%mark-constant) :private-a))
(defpackage :private-import (:use :cl) (:import-from :private-a :%defconstant :%mark-constant))
(defpackage :private-inherit (:use :cl :private-a))
"#;

const CHECKS: &str = r#"
(assert (= +host-private-isolation+ 42))
(dolist (entry '(("PRIVATE-A" :a) ("PRIVATE-B" :b) ("COMMON-LISP-USER" :user)))
  (let ((package (find-package (first entry))))
    (dolist (name '("%DEFCONSTANT" "%MARK-CONSTANT"))
      (let ((read (let ((*package* package)) (read-from-string name)))
            (qualified (read-from-string (concatenate 'string (first entry) "::" name))))
        (multiple-value-bind (found status) (find-symbol name package)
          (assert (eq found read)) (assert status)
          (assert (eq read qualified))
          (assert (eq (symbol-package read) package))
          (assert (eq read (intern name package))))))
    (assert (equal (funcall (symbol-function (find-symbol "%DEFCONSTANT" package)) 'name 41 nil)
                   (list (second entry) 'name 41 nil nil)))
    (assert (equal (funcall (symbol-function (find-symbol "%MARK-CONSTANT" package)) 7)
                   (list (second entry) 7)))))
(assert (not (eq 'private-a::%defconstant 'private-b::%defconstant)))
(assert (not (eq 'private-a::%defconstant '%defconstant)))
(assert (eq 'private-import::%defconstant 'private-a:%defconstant))
(assert (eq 'private-inherit::%defconstant 'private-a:%defconstant))
(assert (equal (private-import::%defconstant 'name 41 nil) '(:a name 41 nil nil)))
(assert (equal (private-inherit::%mark-constant 7) '(:a 7)))
#+egcl
(let ((primitive (car (second (macroexpand-1 '(defconstant +unused-private-probe+ 99))))))
  (assert (not (eq primitive '%defconstant)))
  (assert (eq primitive (read-from-string "COMMON-LISP::%DEFCONSTANT")))
  (assert (eq primitive (read-from-string "EGCL-INTERNAL::%DEFCONSTANT")))
  (assert (eq primitive (read-from-string "EGCL-EXT::%DEFCONSTANT")))
  (egcl-internal::%defconstant '+internal-private-isolation+ 43)
  (egcl-ext::%defconstant '+extension-private-isolation+ 44)
  (assert (= +internal-private-isolation+ 43))
  (assert (= +extension-private-isolation+ 44))
  (assert (constantp '+internal-private-isolation+))
  (assert (functionp (symbol-function 'egcl::%standard-list-reader)))
  (assert (functionp (symbol-function 'egcl-ext::%save-core))))
"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-private-bootstrap-identity-{}-{serial}.lisp",
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
    assert!(stdout.contains("PRIVATE-BOOTSTRAP-IDENTITY-OK"), "{stdout}");
    stdout
}

#[test]
fn private_bootstrap_names_belong_to_the_readers_user_package() {
    let program = format!("{DEFINITIONS} {CHECKS} (write-line \"PRIVATE-BOOTSTRAP-IDENTITY-OK\")");
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn private_functions_survive_fresh_bytecode_loading_without_replacing_bootstrap_helpers() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-private-bootstrap-compiled-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("private.lisp");
    let artifact = directory.join("private.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    let compiler_output = run(
        &format!(
            "(multiple-value-bind (file warnings failure)
             (compile-file {source:?} :output-file {artifact:?})
           (declare (ignore warnings)) (assert file) (assert (not failure)))
         (write-line #+egcl \"PRIVATE-BOOTSTRAP-EGCL\" #-egcl \"PRIVATE-BOOTSTRAP-ORACLE\")
             (write-line \"PRIVATE-BOOTSTRAP-IDENTITY-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    assert!(
        compiler_output.contains("PRIVATE-BOOTSTRAP-EGCL")
            || compiler_output.contains("PRIVATE-BOOTSTRAP-ORACLE"),
        "compile process must identify its artifact kind"
    );
    if compiler_output.contains("PRIVATE-BOOTSTRAP-EGCL") {
        assert!(bytes.starts_with(b"BFASL\0"), "expected a BFASL artifact");
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
        assert!(bytecode, "expected a portable BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program =
        format!("(load {artifact:?}) {CHECKS} (write-line \"PRIVATE-BOOTSTRAP-IDENTITY-OK\")");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
