// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::{fs, process::Command};

const DEFINITIONS: &str = r##"
  (defvar *custom-backquote-depth* 0)
  (defun custom-backquote (stream char)
    (declare (ignore char))
    (let ((*custom-backquote-depth* (1+ *custom-backquote-depth*)))
      (list :backquote (read stream t nil t))))
  (defun custom-comma (stream char)
    (declare (ignore char))
    (assert (> *custom-backquote-depth* 0))
    (let ((*custom-backquote-depth* (1- *custom-backquote-depth*)))
      (list :comma (read stream t nil t))))
  (defun custom-read-one (stream)
    (let* ((char (read-char stream)) (reader (get-macro-character char)))
      (if reader
          (multiple-value-call
              (lambda (&rest args) (values (first args) (null args)))
            (funcall reader stream char))
          (progn (unread-char char stream) (values (read stream) nil)))))
  (defun custom-read-list (stream char)
    (declare (ignore char))
    (let ((items nil))
      (loop
        (when (eql (peek-char t stream) #\))
          (read-char stream)
          (return (nreverse items)))
        (multiple-value-bind (object skipped) (custom-read-one stream)
          (unless skipped (push object items))))))
  (defun install-custom-readers ()
    (set-macro-character #\` #'custom-backquote)
    (set-macro-character #\, #'custom-comma)
    (set-macro-character #\( #'custom-read-list))
(defun check-callable-label-readers ()
  (let ((*readtable* (copy-readtable nil)))
    (install-custom-readers)
    (dolist (source '("`(#1=,x #1#)" "`(#123456789123456789=,x #123456789123456789#)"))
      (let* ((result (read-from-string source)) (items (second result)))
        (assert (equal result '(:backquote ((:comma x) (:comma x)))))
        (assert (eq (first items) (second items)))))
    (let* ((result (read-from-string "`(#1=(a ,x) #1#)")) (items (second result)))
      (assert (equal (first items) '(a (:comma x))))
      (assert (eq (first items) (second items))))
    (let ((result (read-from-string "#1=(self #1#)")))
      (assert (eq result (second result))))
    (let ((definition (get-dispatch-macro-character #\# #\=))
          (reference (get-dispatch-macro-character #\# #\#)))
      #+egcl
      (dolist (invalid '(nil -1 18446744073709551616))
        (assert (handler-case
                  (progn (funcall definition (make-string-input-stream "x") #\= invalid) nil)
                  (reader-error () t))))
      (set-macro-character #\@ (lambda (s c) (declare (ignore c)) (funcall definition s #\= 7)))
      (set-macro-character #\% (lambda (s c) (declare (ignore c)) (funcall reference s #\# 7)))
      (let* ((result (read-from-string "`(@,x %)")) (items (second result)))
        (assert (equal result '(:backquote ((:comma x) (:comma x)))))
        (assert (eq (first items) (second items)))))
    (assert (handler-case (progn (read-from-string "#7#") nil) (reader-error () t))))
  (let ((*readtable* (copy-readtable nil)))
    (let ((result (read-from-string "(#1=(a b) #1#)")))
      (assert (eq (first result) (second result)))))
  t)
"##;

fn run(program: &str, stress: bool) -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let script = std::env::temp_dir().join(format!(
        "egcl-callable-label-run-{}-{serial}.lisp",
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
    assert!(stdout.contains("CALLABLE-LABEL-OK"), "{stdout}");
    stdout
}

#[test]
fn callable_label_readers_preserve_custom_operands_and_shared_context() {
    let program = format!(
        "{DEFINITIONS} (assert (check-callable-label-readers)) (write-line \"CALLABLE-LABEL-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
}

#[test]
fn callable_label_reader_functions_survive_fresh_bytecode_loading() {
    let directory = std::env::temp_dir().join(format!(
        "egcl-callable-label-compiled-{}",
        std::process::id()
    ));
    fs::create_dir_all(&directory).unwrap();
    let source = directory.join("structures.lisp");
    let artifact = directory.join("structures.bfasl");
    fs::write(&source, DEFINITIONS).unwrap();
    run(
        &format!(
            "(multiple-value-bind (file warnings failure)
                 (compile-file {source:?} :output-file {artifact:?})
               (declare (ignore warnings)) (assert file) (assert (not failure)))
             (write-line \"CALLABLE-LABEL-OK\")"
        ),
        false,
    );
    let bytes = fs::read(&artifact).unwrap();
    if bytes.starts_with(b"BFASL\0") {
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
        assert!(bytecode, "expected portable BYTECODE_UNIT");
    }
    fs::remove_file(source).unwrap();
    let program = format!(
        "(load {artifact:?}) (assert (check-callable-label-readers)) (write-line \"CALLABLE-LABEL-OK\")"
    );
    assert_eq!(run(&program, false), run(&program, true));
    fs::remove_dir_all(directory).unwrap();
}
