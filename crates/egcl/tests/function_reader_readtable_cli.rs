// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::process::Command;

const READERS: &str = r#"
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
"#;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run function reader regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("FUNCTION-READER-OK"));
}

#[test]
fn callable_function_reader_preserves_recursive_reader_semantics() {
    run(r##"(progn
      (let ((*readtable* (copy-readtable nil))
            (reader (get-dispatch-macro-character #\# #\'))
            (observed nil) (suppressed nil) (numeric-argument nil))
        (set-macro-character #\! (lambda (s c)
          (declare (ignore c)) (values (list :custom (read s)) :extra)))
        (set-macro-character #\? (lambda (s c)
          (declare (ignore s c)) (values)))
        ;; Calling a standard reader macro is portable inside a recursive READ.
        (set-macro-character #\@ (lambda (s c)
          (declare (ignore c))
          (let ((*read-suppress* suppressed))
            (let ((values (multiple-value-list (funcall reader s #\' numeric-argument))))
              (setq observed values)
              (values-list values)))))
        (with-input-from-string (s "@!42 tail")
          (assert (equal (read s) '(function (:custom 42))))
          (assert (equal observed '((function (:custom 42)))))
          (assert (eq (read s) 'tail)))
        (assert (equal (read-from-string "@?foo") '(function foo)))
        (setq suppressed t numeric-argument 3)
        (assert (equal (read-from-string "@!42") '(function nil)))
        (assert (equal observed '((function nil))))
        (setq suppressed nil)
        (assert (equal (read-from-string "@foo") '(function foo)))
        (assert (equal (read-from-string "#'!42") '(function (:custom 42)))))
      (write-line "FUNCTION-READER-OK"))"##);
}

#[test]
fn compile_file_preserves_function_quote_in_custom_backquote_reader() {
    let dir = std::env::temp_dir().join(format!("egcl-function-reader-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("custom-reader.lisp");
    let artifact = dir.join("custom-reader.bfasl");
    fs::write(&source, "(defparameter *custom-read-result* '`(,x #',x))\n").unwrap();
    run(&format!(
        "(progn {READERS}
           (let ((*readtable* (copy-readtable nil)))
             (install-custom-readers)
             (assert (equal (read-from-string \"`(,x #',x)\")
                      '(:backquote ((:comma x) (function (:comma x))))))
             (multiple-value-bind (file warnings failure)
                 (compile-file {source:?} :output-file {artifact:?})
               (declare (ignore file warnings)) (assert (not failure))))
           (write-line \"FUNCTION-READER-OK\"))"
    ));
    fs::remove_file(&source).unwrap();
    run(&format!(
        "(progn (load {artifact:?})
           (assert (equal *custom-read-result*
                    '(:backquote ((:comma x) (function (:comma x))))))
           (write-line \"FUNCTION-READER-OK\"))"
    ));
    fs::remove_dir_all(dir).unwrap();
}
