// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! User dispatch handlers take precedence over built-in syntax (R4.06).
use std::process::Command;

fn run(program: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("DISPATCH-OK"));
}

#[test]
fn custom_handlers_override_builtin_dispatch_with_and_without_infix() {
    run(r##"
      (let ((*readtable* (copy-readtable nil)))
        (dolist (sub '(#\\ #\' #\( #\C #\R #\A #\= #\#))
          (let ((calls 0))
            (set-dispatch-macro-character #\# sub
              (lambda (stream char arg)
                (declare (ignore stream))
                (incf calls)
                (list char arg)))
            (assert (equal (read-from-string (format nil "#~C" sub)) (list sub nil)))
            (assert (= calls 1))
            (assert (equal (read-from-string (format nil "#7~C" sub)) (list sub 7)))
            (assert (= calls 2)))))
      (assert (char= (read-from-string "#\\x") #\x))
      (format t "DISPATCH-OK~%")
    "##);
}

#[test]
fn original_character_handler_remains_callable_after_override() {
    run(r##"
      (let* ((*readtable* (copy-readtable nil))
             (original (get-dispatch-macro-character #\# #\\)))
        (assert (functionp original))
        (set-dispatch-macro-character #\# #\\
          (lambda (stream char arg)
            (funcall original stream char arg)))
        (assert (char= (read-from-string "#\\Space") #\Space))
        (assert (char= (read-from-string "#\\)") #\)))
        (assert (char= (read-from-string "#\\a") #\a))
        (with-input-from-string (s "Space)")
          (assert (char= (funcall original s #\\ nil) #\Space))
          (assert (char= (read-char s) #\)))))
      (format t "DISPATCH-OK~%")
    "##);
}

#[test]
fn stream_read_executes_each_reader_macro_once() {
    run(r##"
      (let ((*readtable* (copy-readtable nil)) (calls 0))
        (set-dispatch-macro-character #\# #\h
          (lambda (s c n)
            (declare (ignore c n))
            (incf calls)
            (read s)))
        (with-input-from-string (s "(#h\"abc\" #h\"def\") 9")
          (assert (equal (read s) '("abc" "def")))
          (assert (= calls 2))
          (assert (= (read s) 9))
          (assert (eq (read s nil :eof) :eof))))
      (with-input-from-string (s "123 456")
        (assert (= (read-preserving-whitespace s) 123))
        (assert (char= (read-char s) #\Space))
        (unread-char #\Space s)
        (assert (= (read s) 456)))
      (defvar *reader-side-effects* 0)
      (with-input-from-string (s "(#.(incf *reader-side-effects*) 2) 7")
        (assert (equal (read s) '(1 2)))
        (assert (= *reader-side-effects* 1))
        (assert (= (read s) 7)))
      (with-input-from-string (s "#+(and egcl (not egcl)) ignored ; comment")
        (assert (eq (read s nil :eof) :eof)))
      (format t "DISPATCH-OK~%")
    "##);
}

#[test]
fn file_read_executes_each_reader_macro_once() {
    let path = std::env::temp_dir().join(format!("egcl-reader-once-{}.lisp", std::process::id()));
    std::fs::write(&path, "(#h\"abc\" #h\"def\") 9").unwrap();
    run(&format!(r##"
      (let ((*readtable* (copy-readtable nil)) (calls 0))
        (set-dispatch-macro-character #\# #\h
          (lambda (s c n)
            (declare (ignore c n))
            (incf calls)
            (read s)))
        (with-open-file (s {:?})
          (assert (equal (read s) '("abc" "def")))
          (assert (= calls 2))
          (assert (= (read s) 9))
          (assert (eq (read s nil :eof) :eof))))
      (format t "DISPATCH-OK~%")
    "##, path.to_str().unwrap()));
    std::fs::remove_file(path).unwrap();
}
