// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn format_writes_to_gray_output_streams() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defclass format-sink (egcl-gray-streams:fundamental-character-output-stream)
            ((output :initform (make-string-output-stream) :reader sink-output)))
          (defmethod egcl-gray-streams:stream-write-char ((s format-sink) c)
            (write-char c (sink-output s)))
          (let ((s (make-instance 'format-sink)))
            (assert (null (format s "~A:~D~%" "hello" 42)))
            (assert (null (funcall #'format s "~A" "call")))
            (assert (null (eval (list 'format s "~A" "eval"))))
            (let ((*standard-output* s)) (format t "~A" "bound"))
            (assert (string= (get-output-stream-string (sink-output s))
                             (format nil "hello:42~%callevalbound")))
            (princ "princ" s)
            (prin1 "prin1" s)
            (print "print" s)
            (write "write" :stream s)
            (assert (string= (get-output-stream-string (sink-output s))
                             (format nil "princ~S~%~S ~S" "prin1" "print" "write"))))
          (format t "GRAY-FORMAT-OK~%")
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("GRAY-FORMAT-OK"), "{stdout}");
}

#[test]
fn stream_element_type_preserves_native_and_gray_methods() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defclass typed-wrapper (egcl-gray-streams:fundamental-character-input-stream) ())
          (assert (eq 'character (stream-element-type (make-string-input-stream "abc"))))
          (assert (eq 'character (stream-element-type (make-instance 'typed-wrapper))))
          (defmethod stream-element-type ((s typed-wrapper)) '(unsigned-byte 16))
          (let ((s (make-instance 'typed-wrapper)))
            (assert (equal '(unsigned-byte 16) (stream-element-type s)))
            (assert (equal '(unsigned-byte 16) (funcall #'stream-element-type s)))
            (assert (equal '(unsigned-byte 16) (apply #'stream-element-type (list s)))))
          (assert (eq 'character (funcall #'stream-element-type (make-string-output-stream))))
          (assert (handler-case (progn (stream-element-type 42) nil) (type-error () t)))
          (format t "STREAM-TYPES-OK~%")
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("STREAM-TYPES-OK"), "{stdout}");
}

#[test]
fn open_is_a_callable_builtin_without_bootstrap() {
    let path = std::env::temp_dir().join(format!("egcl-open-function-{}.txt", std::process::id()));
    std::fs::write(&path, "from-open\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--no-bootstrap",
            "--eval",
            &format!(
                r#"
          (format t "BOUND ~S~%" (fboundp 'open))
          (let ((s (funcall (fdefinition 'open) {path:?})))
            (format t "READ ~S~%" (read-line s))
            (close s))
        "#
            ),
        ])
        .output()
        .unwrap();
    std::fs::remove_file(path).unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("BOUND T"), "{stdout}");
    assert!(stdout.contains("READ \"from-open\""), "{stdout}");
}

const PROGRAM: &str = r#"
  (defclass native-wrapper () ((stream :initarg :stream :reader wrapped-stream)))
  (defmethod open-stream-p ((s native-wrapper)) (open-stream-p (wrapped-stream s)))
  (defmethod input-stream-p ((s native-wrapper)) (input-stream-p (wrapped-stream s)))
  (defmethod output-stream-p ((s native-wrapper)) (output-stream-p (wrapped-stream s)))
  (defmethod close ((s native-wrapper) &key abort) (close (wrapped-stream s) :abort abort))
  (defun stream-check (s)
    (list (open-stream-p s) (funcall #'input-stream-p s) (apply 'output-stream-p (list s))))
  (let* ((s (make-string-input-stream "abc"))
         (w (make-instance 'native-wrapper :stream s)))
    (format t "NATIVE ~S WRAPPER ~S~%" (stream-check s) (stream-check w))
    (format t "EVAL ~S~%" (eval (list 'open-stream-p (list 'quote w))))
    (close w :abort t)
    (format t "CLOSED ~S ~S~%" (open-stream-p s) (open-stream-p w)))
  (let ((s (make-string-output-stream)))
    (format t "OUTPUT ~S~%" (stream-check s))
    (close s))
"#;

#[test]
fn native_streams_keep_their_methods_when_gray_methods_are_added() {
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T1_THRESHOLD", "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{tier}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            stdout.contains("NATIVE (T T NIL) WRAPPER (T T NIL)"),
            "{tier}: {stdout}"
        );
        assert!(stdout.contains("CLOSED NIL NIL"), "{tier}: {stdout}");
        assert!(stdout.contains("EVAL T"), "{tier}: {stdout}");
        assert!(stdout.contains("OUTPUT (T NIL T)"), "{tier}: {stdout}");
    }
}

#[test]
fn exported_gray_protocol_symbols_reach_standard_stream_dispatch() {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args([
            "--no-init",
            "--eval",
            r#"
          (defclass gray-probe (egcl-gray-streams:fundamental-character-input-stream) ())
          (defmethod egcl-gray-streams:stream-read-char ((s gray-probe)) #\Q)
          (format t "SAME ~S~%" (eq 'stream-read-char 'egcl-gray-streams:stream-read-char))
          (format t "GRAY ~S~%" (read-char (make-instance 'gray-probe)))
        "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("GRAY #\\Q"), "{stdout}");
    assert!(stdout.contains("SAME T"), "{stdout}");
}
