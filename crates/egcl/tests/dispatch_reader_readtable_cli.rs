// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::process::Command;

const READERS: &str = r#"
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
  (defvar *target-features* nil)
  (defun target-feature-reader (stream subchar arg)
    (assert (null arg))
    (if (char= (if (let ((*package* (find-package "KEYWORD"))
                         (*read-suppress* nil))
                     (member (read stream t nil t) *target-features*))
                   #\+ #\-)
               subchar)
        (read stream t nil t)
        (let ((*read-suppress* t)) (read stream t nil t) (values))))
  (defun install-target-readers ()
    (set-macro-character #\( #'custom-read-list)
    (set-dispatch-macro-character #\# #\+ #'target-feature-reader)
    (set-dispatch-macro-character #\# #\- #'target-feature-reader))
"#;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run callable dispatch reader regression");
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("DISPATCH-READER-OK"));
}

#[test]
fn callable_dispatch_reader_uses_current_sub_character_table() {
    run(&format!(
        "(progn {READERS}
           (let ((*readtable* (copy-readtable nil)))
             (set-macro-character #\\( #'custom-read-list)
             (set-dispatch-macro-character #\\# #\\!
               (lambda (s c arg) (declare (ignore c))
                 (values (list :custom arg (read s t nil t)) :extra)))
             (set-dispatch-macro-character #\\# #\\?
               (lambda (s c arg) (declare (ignore s c arg)) (values)))
             (assert (equal (read-from-string \"#3!42\") '(:custom 3 42)))
             (assert (equal (read-from-string \"(#3!42 #? nil #!99)\")
                            '((:custom 3 42) nil (:custom nil 99))))
             (assert (equalp (read-from-string \"(#(1 2) #\\\\a)\") '(#(1 2) #\\a))))
           (write-line \"DISPATCH-READER-OK\"))"
    ));
}

#[test]
fn compile_file_uses_custom_feature_readers_with_dynamic_target_features() {
    let dir = std::env::temp_dir().join(format!("egcl-dispatch-reader-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("target-reader.lisp");
    let artifact = dir.join("target-reader.bfasl");
    fs::write(
        &source,
        "(defparameter *target-read-result*\n\
           (list :before #-target-host (error \"wrong target branch\")\n\
                         #+target-host :host))\n",
    )
    .unwrap();
    run(&format!(
        "(progn {READERS}
           (let ((*readtable* (copy-readtable nil))
                 (*features* (remove :target-host *features*))
                 (*target-features* '(:target-host)))
             (install-target-readers)
             (assert (equal (read-from-string
                       \"(:before #-target-host :target #+target-host :host)\")
                      '(:before :host)))
             (multiple-value-bind (file warnings failure)
                 (compile-file {source:?} :output-file {artifact:?})
               (declare (ignore file warnings)) (assert (not failure))))
           (write-line \"DISPATCH-READER-OK\"))"
    ));
    fs::remove_file(&source).unwrap();
    run(&format!(
        "(progn (load {artifact:?})
           (assert (equal *target-read-result* '(:before :host)))
           (write-line \"DISPATCH-READER-OK\"))"
    ));
    fs::remove_dir_all(dir).unwrap();
}
