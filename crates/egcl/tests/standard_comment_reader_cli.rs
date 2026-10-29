// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn callable_comment_reader_preserves_zero_values() {
    let program = r#"
      (defun consume-comment (reader)
        (with-input-from-string (s (format nil "ignored comment~%42"))
          (list (multiple-value-list (funcall reader s #\;)) (read s))))
      (let ((reader (get-macro-character #\;)))
        (assert (equal (consume-comment reader) '(nil 42)))
        ;; Exercise calls after lowering as well as the initial source call.
        (dotimes (i 500) (assert (equal (consume-comment reader) '(nil 42))))
        (assert (equal (eval '(consume-comment (get-macro-character #\;))) '(nil 42))))
      (write-line "COMMENT-ZERO-VALUES-OK")
    "#;
    check(program);
}

#[test]
fn custom_list_reader_skips_comments_and_preserves_literal_nil() {
    // Like a portable cross-compiler reader, distinguish a macro returning no
    // values from a macro returning one NIL using MULTIPLE-VALUE-CALL.
    let program = r#"
      (defun read-one-or-skip (stream)
        (let* ((char (read-char stream)) (reader (get-macro-character char)))
          (if reader
              (multiple-value-call
                  (lambda (&rest args) (values (first args) (null args)))
                (funcall reader stream char))
              (progn (unread-char char stream) (values (read stream) nil)))))
      (defun read-custom-list (stream char)
        (declare (ignore char))
        (let ((items nil))
          (loop
            (when (eql (peek-char t stream) #\))
              (read-char stream)
              (return (nreverse items)))
            (multiple-value-bind (object skipped) (read-one-or-skip stream)
              (unless skipped (push object items))))))
      (let ((*readtable* (copy-readtable nil)))
        (set-macro-character #\( #'read-custom-list)
        (let ((source (format nil "(; first~% 1 ; middle~% nil ; last~% 2)")))
          (assert (equal (read-from-string source) '(1 nil 2)))
          (dotimes (i 500)
            (assert (equal (read-from-string source) '(1 nil 2))))))
      (write-line "COMMENT-ZERO-VALUES-OK")
    "#;
    check(program);
}

#[test]
fn copied_semicolon_syntax_consumes_the_opening_character_before_its_body() {
    let program = r##"
      (defun check-comment-object (source expected)
        (multiple-value-bind (object position) (read-from-string source)
          (assert (equalp object expected))
          (assert (= position (length source))))
        (with-input-from-string (stream source)
          (assert (equalp (read stream) expected))
          (assert (eq (read stream nil :end) :end))))
      (dolist (character '(#\Newline #\Space #\Tab #\Return #\x #\;))
        (let ((*readtable* (copy-readtable nil)))
          (set-syntax-from-char character #\;)
          (assert (eq (get-macro-character character) (get-macro-character #\;)))
          (let ((input (coerce (list character #\2 #\Newline #\0) 'string)))
            (check-comment-object input 0)
            ;; Copying the modified table retains its comment syntax too.
            (let ((*readtable* (copy-readtable *readtable*)))
              (check-comment-object input 0)
              ;; Both an explicit destination and a self-copy retain syntax.
              (assert (eq (copy-readtable *readtable* *readtable*) *readtable*))
              (check-comment-object input 0)
              (let ((destination (copy-readtable nil)))
                (assert (eq (copy-readtable *readtable* destination) destination))
                (let ((*readtable* destination))
                  (check-comment-object input 0)))))
          (check-comment-object
            (coerce (list #\( #\1 character #\2 #\Newline #\3 #\)) 'string)
            '(1 3))
          (check-comment-object
            (coerce (list #\# #\( #\1 character #\2 #\Newline #\3 #\)) 'string)
            #(1 3))
          (check-comment-object
            (coerce (list #\( #\1 character #\2 #\Newline #\)) 'string)
            '(1))
          (check-comment-object
            (coerce (list #\' character #\2 #\Newline #\0) 'string)
            '(quote 0))
          ;; The callable standard handler consumes the terminator exactly once
          ;; and contributes no value, regardless of its invocation character.
          (with-input-from-string (stream (format nil "ignored~%0"))
            (assert (null (multiple-value-list
                           (funcall (get-macro-character character) stream character))))
            (assert (= (read stream) 0)))))
      (let ((*readtable* (copy-readtable nil)))
        (set-syntax-from-char #\Newline #\;)
        (set-syntax-from-char #\Newline #\Space)
        (assert (null (get-macro-character #\Newline)))
        (assert (= (read-from-string (format nil "~%2~%0")) 2)))
      (assert (null (get-macro-character #\Newline)))
      (write-line "COMMENT-ZERO-VALUES-OK")
    "##;
    check(program);
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_GC_STRESS", "1")
        .env("EGCL_GC_STRESS_AFTER_INIT", "1")
        .env("EGCL_GC_POISON", "1")
        .env_remove("EGCL_GC_STRESS_SKIP")
        .env_remove("EGCL_GC_STRESS_AT")
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run copied comment syntax under GC stress");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert_eq!(
        stdout
            .lines()
            .filter(|line| line.trim() == "COMMENT-ZERO-VALUES-OK")
            .count(),
        1,
        "{stdout}\n{stderr}"
    );
}

fn check(program: &str) {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--eval", program])
            .output()
            .expect("run comment reader regression");
        assert!(
            output.status.success(),
            "{tier}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("COMMENT-ZERO-VALUES-OK"));
    }
}
