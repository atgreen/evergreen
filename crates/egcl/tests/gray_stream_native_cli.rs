// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn gray_stream_predicates_recognize_inherited_directions() {
    for backend in ["bytecode", "tree-walker"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_BACKEND", backend)
            .args([
                "--no-init",
                "--eval",
                r#"
              (defclass probe-input (egcl-gray-streams:fundamental-binary-input-stream) ())
              (defclass probe-output (egcl-gray-streams:fundamental-character-output-stream) ())
              (defclass probe-io (probe-input probe-output) ())
              (deftype stream-alias () 'stream)
              (defclass non-stream () ())
              (defun check-directions (s expected)
                (assert (equal expected (list (streamp s) (input-stream-p s) (output-stream-p s)))))
              (let ((input (make-instance 'probe-input))
                    (output (make-instance 'probe-output))
                    (io (make-instance 'probe-io)))
                ;; Warm the function to exercise the compiled path as well.
                (dotimes (i 500)
                  (check-directions input '(t t nil))
                  (check-directions output '(t nil t))
                  (check-directions io '(t t t)))
                (assert (funcall #'streamp io))
                (assert (typep io 'stream))
                (assert (typep io 'stream-alias))
                (assert (not (typep io 'file-stream)))
                (assert (funcall #'input-stream-p io))
                (assert (apply #'output-stream-p (list io)))
                (assert (open-stream-p io))
                (assert (close io :abort t))
                (assert (not (open-stream-p io)))
                (assert (close io))
                (assert (not (open-stream-p io)))
                (check-directions io '(t t t)))
              (check-directions (make-string-input-stream "x") '(t t nil))
              (check-directions (make-string-output-stream) '(t nil t))
              (assert (not (streamp (make-instance 'non-stream))))
              (assert (not (streamp 42)))
              (format t "GRAY-PREDICATES-OK~%")
            "#,
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{backend}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("GRAY-PREDICATES-OK"), "{backend}: {stdout}");
    }
}

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

#[test]
fn bulk_sequence_generics_dispatch_and_fall_back() {
    // Per R5.113, READ-SEQUENCE and WRITE-SEQUENCE on a Gray stream dispatch
    // through the public STREAM-READ-SEQUENCE / STREAM-WRITE-SEQUENCE generics
    // (spec 5.5.2.2 / 5.5.2.3): a class that specializes them sees one bulk
    // call with the caller's bounds, and a class that does not still gets the
    // scalar fallbacks. trivial-gray-streams' EGCL bridge relies on both halves.
    for backend in ["bytecode", "tree-walker"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_BACKEND", backend)
            .args([
                "--no-init",
                "--eval",
                r#"
              ;; Specialized bulk input: must be called instead of STREAM-READ-BYTE.
              (defclass bulk-in (egcl-gray-streams:fundamental-binary-input-stream) ())
              (defmethod egcl-gray-streams:stream-read-byte ((s bulk-in))
                (error "scalar read must not be used when the bulk method exists"))
              (defmethod egcl-gray-streams:stream-read-sequence ((s bulk-in) seq start end)
                (loop for i from start below (min end (+ start 2))
                      do (setf (elt seq i) 7))
                (min end (+ start 2)))
              (let ((v (make-array 5 :initial-element 0)))
                (assert (= 3 (read-sequence v (make-instance 'bulk-in) :start 1 :end 4)))
                (assert (equalp v #(0 7 7 0 0))))
              ;; Specialized bulk output: must be called with the caller's bounds.
              (defclass bulk-out (egcl-gray-streams:fundamental-character-output-stream)
                ((calls :initform nil :accessor calls)))
              (defmethod egcl-gray-streams:stream-write-char ((s bulk-out) c)
                (error "scalar write must not be used when the bulk method exists"))
              (defmethod egcl-gray-streams:stream-write-sequence ((s bulk-out) seq start end)
                (push (list (subseq seq start end) start end) (calls s))
                seq)
              (let ((s (make-instance 'bulk-out)))
                (assert (string= "hello" (write-sequence "hello" s :start 1 :end 3)))
                (assert (equal '(("el" 1 3)) (calls s))))
              ;; No specialization: the defaults fall back to the scalar generics.
              (defclass scalar-in (egcl-gray-streams:fundamental-binary-input-stream)
                ((bytes :initform (list 1 2 3) :accessor bytes)))
              (defmethod egcl-gray-streams:stream-read-byte ((s scalar-in))
                (if (bytes s) (pop (bytes s)) :eof))
              (let ((v (make-array 4 :initial-element -1)))
                (assert (= 3 (read-sequence v (make-instance 'scalar-in))))
                (assert (equalp v #(1 2 3 -1))))
              (defclass scalar-out (egcl-gray-streams:fundamental-character-output-stream)
                ((log :initform nil :accessor log-of)))
              (defmethod egcl-gray-streams:stream-write-char ((s scalar-out) c)
                (push c (log-of s)) c)
              (defmethod egcl-gray-streams:stream-write-byte ((s scalar-out) b)
                (push b (log-of s)) b)
              (let ((s (make-instance 'scalar-out)))
                (write-sequence "abc" s :start 1)
                (write-sequence #(9 8) s)
                (write-sequence (list #\z) s)
                (assert (equal '(#\z 8 9 #\c #\b) (log-of s))))
              ;; A subclass method reaches the default with CALL-NEXT-METHOD, which
              ;; is exactly what trivial-gray-streams' OR-FALLBACK does.
              (defclass chained-in (scalar-in) ())
              (defmethod egcl-gray-streams:stream-read-sequence ((s chained-in) seq start end)
                (call-next-method))
              (let ((v (make-array 2 :initial-element -1)))
                (assert (= 2 (read-sequence v (make-instance 'chained-in))))
                (assert (equalp v #(1 2))))
              (format t "GRAY-BULK-OK~%")
            "#,
            ])
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{backend}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("GRAY-BULK-OK"), "{backend}: {stdout}");
    }
}
