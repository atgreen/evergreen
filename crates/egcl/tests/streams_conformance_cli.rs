// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! ANSI stream conformance fixes (bliss-k6ymj), checked in every tier:
//!
//! - an exhausted string stream is at end of file for READ-CHAR-NO-HANG, which
//!   also rejects a fifth argument (ansi READ-CHAR-NO-HANG.6, .ERROR.1-4);
//! - END-OF-FILE carries the stream that ran out (STREAM-ERROR-STREAM.1);
//! - an echo stream echoes a character once, however often it is unread and
//!   peeked (UNREAD-CHAR.3, PEEK-CHAR.17);
//! - PEEK-CHAR T skips whitespace as the current readtable defines it
//!   (PEEK-CHAR.20/.21);
//! - a broadcast stream answers STREAM-EXTERNAL-FORMAT and FILE-STRING-LENGTH
//!   for its last component, :DEFAULT and 1 when it has none
//!   (MAKE-BROADCAST-STREAM.8, BROADCAST-STREAM-STREAMS.3/.4);
//! - a BASE-CHAR string output stream produces a SIMPLE-BASE-STRING
//!   (WITH-OUTPUT-TO-STRING.8).
use std::process::Command;

fn run_tiers(program: &str, expected: &str) {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains(expected), "{tier}: {stdout}\n{stderr}");
    }
}

#[test]
fn read_char_no_hang_end_of_file_and_arity() {
    run_tiers(
        r#"(progn
(defun no-hang-eof-value () (with-input-from-string (s "") (read-char-no-hang s nil 'foo)))
(defun no-hang-eof-error (&rest args)
  (with-input-from-string (s "")
    (handler-case (apply #'read-char-no-hang s args) (end-of-file () :eof))))
(defun no-hang-too-many ()
  (with-input-from-string (s "abc")
    (handler-case (funcall #'read-char-no-hang s nil nil nil nil) (program-error () :arity))))
(defun no-hang-unread ()
  (with-input-from-string (s "a")
    (let ((c (read-char s))) (unread-char c s) (list (read-char-no-hang s nil :eof) (read-char-no-hang s nil :eof)))))
(format t "~&RESULT ~s~%"
  (list (no-hang-eof-value) (no-hang-eof-error) (no-hang-eof-error t) (no-hang-eof-error t t)
        (no-hang-too-many) (no-hang-unread) (with-input-from-string (s "") (listen s)))))"#,
        "RESULT (FOO :EOF :EOF :EOF :ARITY (#\\a :EOF) NIL)",
    );
}

#[test]
fn end_of_file_carries_its_stream() {
    run_tiers(
        r#"(progn
(defun eof-stream (reader)
  (with-input-from-string (s "")
    (handler-case (funcall reader s)
      (end-of-file (c) (eq (stream-error-stream c) s)))))
(format t "~&RESULT ~s~%"
  (list (eof-stream #'read-char) (eof-stream #'read-char-no-hang)
        (eof-stream (lambda (s) (peek-char nil s)))
        (eof-stream (lambda (s) (peek-char t s))))))"#,
        "RESULT (T T T T)",
    );
}

#[test]
fn echo_stream_echoes_each_character_once() {
    run_tiers(
        r#"(progn
(defun echo-unread ()
  (with-input-from-string (is "abc")
    (with-output-to-string (os)
      (let ((s (make-echo-stream is os)))
        (read-char s) (unread-char #\a s) (read-char s) (read-char s) (read-char s)
        (unread-char #\c s) (read-char s)))))
(defun echo-peek ()
  (with-input-from-string (is "  ab")
    (let* ((os (make-string-output-stream)) (es (make-echo-stream is os)))
      (list (peek-char nil es) (get-output-stream-string os)
            (peek-char t es) (get-output-stream-string os)
            (read-char es) (peek-char #\b es) (read-char es)
            (get-output-stream-string os) (listen es)))))
(format t "~&RESULT ~s~%" (list (echo-unread) (echo-peek))))"#,
        r#"RESULT ("abc" (#\Space "" #\a "  " #\a #\b #\b "ab" NIL))"#,
    );
}

#[test]
fn peek_char_skips_readtable_whitespace() {
    run_tiers(
        r#"(progn
(defun peek-with (to from text)
  (let ((*readtable* (copy-readtable)))
    (set-syntax-from-char to from)
    (with-input-from-string (*standard-input* text)
      (list (peek-char) (read-char) (peek-char t) (read-char)))))
(format t "~&RESULT ~s~%"
  (list (peek-with #\Space #\a "  x") (peek-with #\x #\Space "xxa")
        (with-input-from-string (s (format nil " ~c~cz" #\Tab #\Newline)) (peek-char t s)))))"#,
        r#"RESULT ((#\Space #\Space #\Space #\Space) (#\x #\x #\a #\a) #\z)"#,
    );
}

#[test]
fn broadcast_stream_answers_for_its_last_component() {
    let dir = std::env::temp_dir().join(format!("egcl-bcast-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let program = format!(
        r#"(progn
(defun bcast-file (path)
  (with-open-file (last path :direction :output :if-exists :supersede :if-does-not-exist :create
                             :external-format :latin-1)
    (format last "Hello world!~%")
    (finish-output last)
    (let ((b (make-broadcast-stream (make-string-output-stream) last)))
      (list (file-length b) (file-position b) (file-string-length b "jd")
            (stream-external-format b)))))
(defun bcast-empty ()
  (let ((b (make-broadcast-stream)))
    (list (file-length b) (file-position b) (file-string-length b "foo")
          (stream-external-format b))))
(format t "~&RESULT ~s~%"
  (list (bcast-file "{}/bss-last.txt") (bcast-empty)
        (stream-external-format (make-broadcast-stream (make-broadcast-stream))))))"#,
        dir.display()
    );
    run_tiers(
        &program,
        "RESULT ((13 13 2 :LATIN-1) (0 0 1 :DEFAULT) :DEFAULT)",
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn base_char_string_output_stream_makes_a_base_string() {
    run_tiers(
        r#"(progn
(defun base-out () (with-output-to-string (s nil :element-type 'base-char) (write-char #\8 s)))
(defun std-out ()
  (let ((s (make-string-output-stream :element-type 'standard-char)))
    (write-string "ok" s) (get-output-stream-string s)))
(defun char-out () (with-output-to-string (s nil :element-type 'character) (write-char #\8 s)))
(defun plain-out () (with-output-to-string (s) (write-char #\8 s)))
(format t "~&RESULT ~s~%"
  (list (base-out) (typep (base-out) 'simple-base-string) (typep (std-out) 'simple-base-string)
        (array-element-type (char-out)) (array-element-type (plain-out)))))"#,
        r#"RESULT ("8" T T CHARACTER CHARACTER)"#,
    );
}
