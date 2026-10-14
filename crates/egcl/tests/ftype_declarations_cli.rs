// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Global FTYPE metadata must survive the real bootstrap and compile/load phases.
use std::{fs, process::Command};

fn run(source: &str) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", source])
        .output()
        .expect("run FTYPE declaration regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn boot_retains_ftype_for_multiple_names_and_distinct_setf_functions() {
    let output = run(r#"
        (assert (equal
          (macroexpand-1 '(declaim (ftype (function () fixnum) retained-result-a)))
          '(progn (eval-when (:compile-toplevel :load-toplevel :execute)
                    (proclaim '(ftype (function () fixnum) retained-result-a))))))
        (declaim (ftype (function () fixnum) retained-result-a retained-result-b))
        (assert (equal (get 'retained-result-a 'egcl-internal::function-type)
                       '(function () fixnum)))
        (assert (equal (get 'retained-result-b 'egcl-internal::function-type)
                       '(function () fixnum)))
        (proclaim '(ftype (function (t t) t) (setf retained-result-a)))
        (assert (equal (get 'retained-result-a 'egcl-internal::setf-function-type)
                       '(function (t t) t)))
        (assert (equal (get 'retained-result-a 'egcl-internal::function-type)
                       '(function () fixnum)))
        (proclaim '(ftype (function () (values fixnum &optional)) retained-result-b))
        (assert (equal (get 'retained-result-b 'egcl-internal::function-type)
                       '(function () (values fixnum &optional))))
        (assert (null (proclaim '(ftype function retained-unrestricted))))
        (assert (eq (get 'retained-unrestricted 'egcl-internal::function-type)
                    'function))
        (defun retained-result-a () (values 7 8))
        (assert (equal (multiple-value-list (retained-result-a)) '(7 8)))
        (format t "FTYPE-RETAINED~%")
    "#);
    assert!(output.contains("FTYPE-RETAINED"));
}

#[test]
fn global_ftype_properties_preserve_package_identity() {
    let output = run(r#"
        (let* ((left-package (make-package "FTYPE-LEFT" :use '()))
               (right-package (make-package "FTYPE-RIGHT" :use '()))
               (left (intern "RESULT" left-package))
               (right (intern "RESULT" right-package)))
          (proclaim (list 'ftype '(function () fixnum) left))
          (proclaim (list 'ftype '(function () single-float) right))
          (assert (equal (get left 'egcl-internal::function-type)
                         '(function () fixnum)))
          (assert (equal (get right 'egcl-internal::function-type)
                         '(function () single-float))))
        (format t "FTYPE-IDENTITY~%")
    "#);
    assert!(output.contains("FTYPE-IDENTITY"));
}

#[test]
fn source_free_bfasl_retains_ftype_at_compile_and_load_time() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("egcl-ftype-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("declarations.lisp");
    let fasl = directory.join("declarations.bfasl");
    fs::write(
        &source,
        r#"
        (declaim (ftype (function () fixnum) persisted-ftype-result))
        (eval-when (:compile-toplevel)
          (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                         '(function () fixnum))))
        (defun persisted-ftype-result () (values 7 8))
    "#,
    )
    .unwrap();
    run(&format!(
        r#"
        (multiple-value-bind (output warnings failure)
            (compile-file {source:?} :output-file {fasl:?})
          (declare (ignore warnings))
          (assert output)
          (assert (not failure)))
        (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                       '(function () fixnum)))
    "#
    ));
    let bytes = fs::read(&fasl).unwrap();
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut position = 24;
    let mut bytecode = false;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[position..position + 2].try_into().unwrap());
        let length =
            u32::from_le_bytes(bytes[position + 2..position + 6].try_into().unwrap()) as usize;
        assert_ne!(
            kind, 11,
            "FTYPE retention must not require embedded source fallback"
        );
        bytecode |= kind == 12;
        position += 6 + length;
    }
    assert!(bytecode, "artifact must contain a bytecode unit");
    fs::remove_file(&source).unwrap();
    let output = run(&format!(
        r#"
        (assert (null (get 'persisted-ftype-result 'egcl-internal::function-type)))
        (load {fasl:?})
        (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                       '(function () fixnum)))
        (assert (equal (multiple-value-list (persisted-ftype-result)) '(7 8)))
        (format t "FTYPE-LOADED~%")
    "#
    ));
    assert!(output.contains("FTYPE-LOADED"));
    fs::remove_file(fasl).unwrap();
    fs::remove_dir(directory).unwrap();
}
