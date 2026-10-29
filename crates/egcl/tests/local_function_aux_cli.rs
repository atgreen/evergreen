// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r#"
(defun aux-map-names (entries)
  (let ((result nil))
    (flet ((included-slot-name (slot) (car slot)))
      (mapl (lambda (slots &aux (name (included-slot-name (car slots))))
              (push name result)) entries))
    (nreverse result)))
(defun aux-map-with-function-reference (entries)
  (let ((result nil))
    (flet ((included-slot-name (slot) (car slot)))
      (mapl (lambda (slots &aux (name (included-slot-name (car slots))))
              (assert (eq name (funcall #'included-slot-name (car slots))))
              (push name result)) entries))
    (nreverse result)))
(defun make-aux-capturing-reader (offset)
  (flet ((add-offset (value) (+ value offset)))
    (lambda (value &aux (first (add-offset value)) (second (add-offset first)))
      (list first second (funcall #'add-offset second)))))
(defun aux-shadowed-body (value)
  (flet ((helper (value) (+ value 7)))
    (funcall (lambda (value &aux (initial (helper value)))
               (flet ((helper (value) (+ value 1000)))
                 (list initial (helper initial)))) value)))
(defun aux-labels-recursion ()
  (labels ((sum-down (value)
             (if (zerop value) 0 (+ value (sum-down (1- value))))))
    (funcall (lambda (&aux (total (sum-down 5))) total))))
(defun aux-after-optional-and-key ()
  (flet ((helper (value) (1+ value)))
    (let ((function (lambda (&optional (left 3) &key (right 4)
                             &aux (total (helper (+ left right)))) total)))
      (list (funcall function) (funcall function 2 :right 9)))))
(defun aux-empty-initializers ()
  (funcall (lambda (&aux empty (full t)) (list empty full))))
(defvar *aux-special-control* :global)
(defun aux-special-control ()
  (let ((*aux-special-control* :outer))
    (list (funcall (lambda (&aux (*aux-special-control* :inner))
                     (declare (special *aux-special-control*))
                     *aux-special-control*))
          *aux-special-control*)))
"#;

const CHECKS: &str = r#"
(assert (equal (aux-map-names '((a 1) (b 2))) '(a b)))
(assert (equal (aux-map-with-function-reference '((a 1) (b 2))) '(a b)))
(let ((function (make-aux-capturing-reader 7)))
  (flet ((add-offset (value) (+ value 10000)))
    (assert (equal (funcall function 2) '(9 16 23)))))
(assert (equal (aux-shadowed-body 2) '(9 1009)))
(assert (= (aux-labels-recursion) 15))
(assert (equal (aux-after-optional-and-key) '(8 12)))
(assert (equal (aux-empty-initializers) '(nil t)))
(assert (equal (aux-special-control) '(:inner :outer)))
(assert (eq *aux-special-control* :global))
"#;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-local-function-aux-{}-{serial}.lisp",
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
    assert!(stdout.contains("LOCAL-FUNCTION-AUX-OK"), "{stdout}");
    stdout
}

#[test]
fn local_functions_remain_visible_in_compiled_aux_initializers() {
    let program = format!(
        "{DEFINITIONS} {CHECKS}
         (dolist (name '(aux-map-names aux-map-with-function-reference
                         make-aux-capturing-reader aux-shadowed-body aux-labels-recursion
                         aux-after-optional-and-key aux-empty-initializers aux-special-control))
           (compile name))
         {CHECKS}
         (write-line \"LOCAL-FUNCTION-AUX-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn local_function_aux_initializers_survive_fresh_bytecode_loading() {
    let directory = std::env::temp_dir().join(format!("egcl-aux-compiled-{}", std::process::id()));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("aux.lisp");
    let artifact = directory.join("aux.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
             (compile-file {source:?} :output-file {artifact:?})
           (declare (ignore warnings)) (assert file) (assert (not failure)))
         (write-line \"LOCAL-FUNCTION-AUX-OK\")"
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
    let program = format!("(load {artifact:?}) {CHECKS} (write-line \"LOCAL-FUNCTION-AUX-OK\")");
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
