// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FILE-POSITION on a Gray stream calls EGCL-GRAY-STREAMS:STREAM-FILE-POSITION.
//! It used to reach only the built-in stream code and signal a bare
//! STREAM-ERROR, which broke cl-tar, whose entry streams seek this way.

use std::process::Command;
const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn eval(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("spawn egcl");
    assert!(
        out.status.success(),
        "program failed: {program}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn file_position_reads_and_moves_a_gray_stream() {
    let program = r#"
      (progn
        (defclass seekable (egcl-gray-streams:fundamental-binary-input-stream)
          ((data :initarg :data :reader data) (pos :initform 0 :accessor pos)))
        (defmethod egcl-gray-streams:stream-read-byte ((s seekable))
          (if (< (pos s) (length (data s)))
              (prog1 (aref (data s) (pos s)) (incf (pos s)))
              :eof))
        (defmethod egcl-gray-streams:stream-file-position ((s seekable) &optional spec)
          (cond ((null spec) (pos s))
                ((eq spec :start) (setf (pos s) 0) t)
                ((eq spec :end) (setf (pos s) (length (data s))) t)
                ((<= 0 spec (length (data s))) (setf (pos s) spec) t)
                (t nil)))
        (defun seek-and-read (s n) (file-position s n) (read-byte s))
        (let ((s (make-instance 'seekable :data #(10 20 30 40 50 60))))
          (list (file-position s)
                (file-position s 3) (read-byte s) (file-position s)
                (file-position s :start) (read-byte s)
                (file-position s :end) (read-byte s nil :eof)
                (file-position s 99)
                (loop repeat 20 collect (seek-and-read s 4)))))
    "#;
    assert_eq!(
        eval(program),
        "(0 T 40 4 T 10 T :EOF NIL (50 50 50 50 50 50 50 50 50 50 50 50 50 50 50 50 50 50 50 50))"
    );
}

#[test]
fn a_gray_stream_without_a_position_reports_nil() {
    let program = r#"
      (progn
        (defclass plain (egcl-gray-streams:fundamental-binary-input-stream) ())
        (defmethod egcl-gray-streams:stream-read-byte ((s plain)) :eof)
        (let ((s (make-instance 'plain)))
          (list (file-position s) (file-position s 0))))
    "#;
    assert_eq!(eval(program), "(NIL NIL)");
}
