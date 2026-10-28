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
