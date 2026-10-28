// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::process::Command;

const SOURCE: &str = r#"
(defpackage "STRUCT-PUBLIC" (:use "CL")
  (:export "PACKET-VALUE" "PACKET-READONLY" "PACKET-P" "MAKE-PACKET" "COPY-PACKET"
           "CHILD-VALUE" "CHILD-READONLY" "SHARED-VALUE"))
(defpackage "STRUCT-CUSTOM" (:use "CL") (:export "CONSTRUCT" "CLONE" "CHECK"))
(defpackage "STRUCT-IMPL" (:use "CL" "STRUCT-PUBLIC"))
(in-package "STRUCT-IMPL")
(defstruct packet (value 41) (readonly 42 :read-only t))
(defstruct (child (:include packet)) (extra 43))
(defstruct (custom (:conc-name shared-)) (value 44))
(defstruct (explicit (:constructor struct-custom:construct)
                     (:copier struct-custom:clone)
                     (:predicate struct-custom:check))
  (value 45))
(defun verify-published-defaults (value)
  (let* ((packet (funcall (symbol-function 'struct-public:make-packet) :value value))
         (copy (funcall (symbol-function 'struct-public:copy-packet) packet)))
    (assert (funcall (symbol-function 'struct-public:packet-p) copy))
    (assert (= (struct-public:packet-value copy) value))))
(defun verify-inherited-accessors ()
  (assert (eq (symbol-package 'packet-value) (find-package "STRUCT-PUBLIC")))
  (assert (eq (symbol-package 'child-value) (find-package "STRUCT-PUBLIC")))
  (assert (eq (symbol-package 'shared-value) (find-package "STRUCT-PUBLIC")))
  (assert (eq 'make-packet 'struct-public:make-packet))
  (assert (eq 'copy-packet 'struct-public:copy-packet))
  (assert (eq 'packet-p 'struct-public:packet-p))
  (assert (fboundp 'packet-value))
  (assert (fboundp 'packet-readonly))
  (assert (not (fboundp '(setf packet-readonly))))
  (let* ((packet (struct-public:make-packet)) (child (make-child)) (custom (make-custom)))
    (assert (struct-public:packet-p packet))
    (assert (= (packet-value packet) 41))
    (assert (= (funcall #'packet-readonly packet) 42))
    (setf (packet-value packet) 51)
    (assert (= (packet-value (struct-public:copy-packet packet)) 51))
    (assert (= (child-value child) 41))
    (assert (= (child-readonly child) 42))
    (setf (child-value child) 52)
    (assert (= (packet-value child) 52))
    (assert (= (shared-value custom) 44))
    (setf (shared-value custom) 53)
    (assert (= (shared-value custom) 53)))
  (let ((instance (funcall (symbol-function 'struct-custom:construct) :value 45)))
    (assert (funcall (symbol-function 'struct-custom:check) instance))
    (assert (= (explicit-value (funcall (symbol-function 'struct-custom:clone) instance)) 45)))
  ;; Warm the real public callable objects so a bare-name tree-walker fallback
  ;; cannot conceal a private generated definition in the implementation package.
  (dotimes (i 500) (verify-published-defaults i))
  (write-line "INHERITED-ACCESSORS-OK"))
(verify-inherited-accessors)
"#;

fn run(args: &[&str], stress: bool) {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init").args(args);
    if stress {
        command.env("EGCL_GC_STRESS", "17");
        command.env("EGCL_GC_STRESS_AFTER_INIT", "1");
        command.env("EGCL_GC_POISON", "1");
    }
    let output = command.output().expect("run inherited accessor regression");
    assert!(
        output.status.success(),
        "stress={stress} stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("INHERITED-ACCESSORS-OK"));
}

#[test]
fn defstruct_preserves_inherited_generated_accessor_symbols() {
    let dir = std::env::temp_dir().join(format!(
        "egcl-inherited-struct-accessor-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("definitions.lisp");
    let artifact = dir.join("definitions.bfasl");
    fs::write(&source, SOURCE).unwrap();
    run(&["--load", source.to_str().unwrap()], false);
    run(&["--load", source.to_str().unwrap()], true);
    run(
        &[
            "--eval",
            &format!(
                "(progn (multiple-value-bind (file warnings failure)\n\
                   (compile-file {source:?} :output-file {artifact:?})\n\
                   (declare (ignore file warnings)) (assert (not failure)))\n\
                   (write-line \"INHERITED-ACCESSORS-OK\"))"
            ),
        ],
        false,
    );
    fs::remove_file(&source).unwrap();
    run(&["--load", artifact.to_str().unwrap()], false);
    run(&["--load", artifact.to_str().unwrap()], true);
    fs::remove_dir_all(dir).unwrap();
}
