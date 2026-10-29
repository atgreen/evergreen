// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defpackage :callback-probe (:use :cl))
(in-package :callback-probe)
(defvar *callbacks* (make-hash-table))
(defmacro register-callback (name params &body body)
  `(setf (gethash ',name *callbacks*) (lambda ,params ,@body)))
(flet ((emit* (segment thing opcode subcode) (list segment thing opcode subcode)))
  (register-callback push (segment thing) (emit* segment thing 80 6))
  (register-callback pop (segment thing) (emit* segment thing 88 0)))
(flet ((helper (value) (1+ value)))
  (setf (gethash 'direct *callbacks*) (lambda (value) (helper value))))
(flet ((helper (value) (1+ value)))
  (declare (notinline helper))
  (setf (gethash 'notinline *callbacks*) #'(lambda (value) (helper value))))
(flet ((helper (value) (if (minusp value) (- value) (1+ value))))
  (setf (gethash 'complex *callbacks*) (lambda (value) (helper value))))
(flet ((helper (value) (+ value 7)))
  (setf (gethash 'function-ref *callbacks*) (lambda (value) (funcall #'helper value))))
(labels ((sum-down (value) (if (zerop value) 0 (+ value (sum-down (1- value))))))
  (setf (gethash 'recursive *callbacks*) (lambda (value) (sum-down value))))
;; The only syntactic callback here is hidden behind a macro. Its helper's
;; variable capture must survive the inline-eligibility and boxing analyses.
(let ((offset 40))
  (flet ((helper (value) (+ value offset)))
    (register-callback hidden-capture (value) (helper value)))
  (setf offset 41))
(let ((offset 7))
  (flet ((helper () offset))
    (register-callback reader () (helper)))
  (setf (gethash 'writer *callbacks*) (lambda (value) (setf offset value))))
(defun callback-control ()
  (flet ((helper (value) (1+ value)))
    (lambda (value) (helper value))))
;; Fresh private names must stop changing after the actual boxing bailout.
(defmacro fresh-boxing-callback ()
  (let ((offset (gensym "PRIVATE-CALLBACK-OFFSET")))
    `(let ((,offset 40))
       (flet ((helper (value) (+ ,offset value)))
         (let ((callback (lambda (value) (helper value))))
           (setq ,offset 41)
           callback)))))
(setf (gethash 'fresh-gensym *callbacks*) (fresh-boxing-callback))
(symbol-macrolet ((offset 700))
  (let ((offset 40))
    (flet ((helper (value) (+ offset value)))
      (register-callback shadowed-symbol-macro (value) (helper value)))))
(defvar *callback-special* 7)
(let ((*callback-special* 40))
  (flet ((helper (value) (+ *callback-special* value)))
    (declare (special *callback-special*))
    (register-callback declared-special (value) (helper value))))
;; Keep compile-file's phase processing in charge of definition availability.
(eval-when (:compile-toplevel :load-toplevel :execute)
  (defmacro phase-local-callback () '(lambda (value) (helper value))))
(flet ((helper (value) (+ value 19)))
  (setf (gethash 'phase-order *callbacks*) (phase-local-callback)))

"#;

const CHECKS: &str = r#"
(in-package :callback-probe)
(assert (equal (funcall (gethash 'push *callbacks*) :segment :register) '(:segment :register 80 6)))
(assert (equal (funcall (gethash 'pop *callbacks*) :segment :register) '(:segment :register 88 0)))
(dolist (name '(direct notinline complex))
  (assert (= (funcall (gethash name *callbacks*) 2) 3)))
(assert (= (funcall (gethash 'function-ref *callbacks*) 2) 9))
(assert (= (funcall (gethash 'recursive *callbacks*) 5) 15))
(assert (= (funcall (gethash 'hidden-capture *callbacks*) 2) 43))
(assert (= (funcall (gethash 'writer *callbacks*) 7) 7))
(assert (= (funcall (gethash 'reader *callbacks*)) 7))
(assert (= (funcall (gethash 'writer *callbacks*) 23) 23))
(assert (= (funcall (gethash 'reader *callbacks*)) 23))
(assert (= (funcall (callback-control) 2) 3))
(dotimes (i 100)
  (assert (equal (funcall (gethash 'push *callbacks*) i :reg) (list i :reg 80 6)))
  (assert (= (funcall (gethash 'hidden-capture *callbacks*) i) (+ i 41))))
(assert (= (funcall (gethash 'fresh-gensym *callbacks*) 2) 43))
(assert (= (funcall (gethash 'shadowed-symbol-macro *callbacks*) 2) 42))
(assert (= (funcall (gethash 'declared-special *callbacks*) 2) 9))
(assert (= (funcall (gethash 'phase-order *callbacks*) 2) 21))

"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-toplevel-callback-{}-{serial}.lisp",
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
    assert!(stdout.contains("TOPLEVEL-CALLBACKS-OK"), "{stdout}");
    stdout
}

#[test]
fn top_level_callbacks_retain_compiled_local_function_bindings() {
    let program = format!(
        "{DEFINITIONS} {CHECKS} (compile 'callback-control) {CHECKS} (write-line \"TOPLEVEL-CALLBACKS-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn top_level_callbacks_survive_fresh_bytecode_loading() {
    let directory =
        std::env::temp_dir().join(format!("egcl-toplevel-callback-bbu-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("callbacks.lisp");
    let artifact = directory.join("callbacks.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
             (compile-file {source:?} :output-file {artifact:?})
           (declare (ignore warnings)) (assert file) (assert (not failure)))
         (write-line \"TOPLEVEL-CALLBACKS-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    assert!(bytes.starts_with(b"BFASL\0"), "expected a BFASL artifact");
    let sections = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut offset = 24;
    let mut bytecode = false;
    for _ in 0..sections {
        let kind = u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap());
        let size = u32::from_le_bytes(bytes[offset + 2..offset + 6].try_into().unwrap()) as usize;
        offset += 6;
        assert_ne!(kind, 11, "unexpected source fallback");
        if kind == 12 {
            bytecode = bytes[offset..offset + size].starts_with(b"BBU\0");
        }
        offset += size;
    }
    assert!(bytecode, "expected a portable BYTECODE_UNIT");
    fs::remove_file(source).unwrap();
    let program = format!("(load {artifact:?}) {CHECKS} (write-line \"TOPLEVEL-CALLBACKS-OK\")");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
