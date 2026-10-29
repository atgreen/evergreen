// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(declaim (notinline arity-prin1 arity-princ arity-print))
(defun arity-prin1 (object stream bad)
  (if (zerop bad) (prin1 object stream) (prin1 object stream nil)))
(defun arity-princ (object stream bad)
  (if (zerop bad) (princ object stream) (princ object stream nil)))
(defun arity-print (object stream bad)
  (if (zerop bad) (print object stream) (print object stream nil)))
(defun printer-arity-check (name helper expected)
  (let ((object (copy-list '(a b))) (effects nil))
    (let ((sink (make-broadcast-stream)))
      (dotimes (i 80) (assert (eq object (funcall helper object sink 0))))
      (when *printer-required-tier*
        (loop repeat 100 until (= *printer-required-tier* (egcl-ext:function-tier helper))
              do (funcall helper object sink 0) (sleep 0.01))
        (assert (= *printer-required-tier* (egcl-ext:function-tier helper)))))
    (let ((stream (make-string-output-stream)))
      (assert (eq object (funcall helper object stream 0)))
      (assert (equal (get-output-stream-string stream) expected)))
    ;; Invalid calls must never emit any portion of the printed object.
    (dolist (route '(:eval :function :apply :compiled))
      (let ((stream (make-string-output-stream)))
        (assert (eq :program-error
          (handler-case
            (progn
              (case route
                (:eval (eval (list name (list 'quote object) stream nil)))
                (:function (funcall (symbol-function name) object stream nil))
                (:apply (apply name (list object stream nil)))
                (:compiled (funcall helper object stream 1)))
              :no-error)
            (program-error () :program-error))))
        (assert (equal (get-output-stream-string stream) ""))))
    (assert (eq :program-error
      (handler-case (progn (funcall name) :no-error) (program-error () :program-error))))
    (assert (eq :program-error
      (handler-case (progn (eval (list name)) :no-error) (program-error () :program-error))))
    ;; Omitted, NIL and T designate the normal default streams.
    (dolist (designator '(:omitted nil t))
      (let* ((output (make-string-output-stream))
             (*standard-output* output)
             (*terminal-io* (make-two-way-stream (make-string-input-stream "") output)))
        (assert (eq object (if (eq designator :omitted)
                              (funcall name object)
                              (funcall name object designator))))
        (assert (equal (get-output-stream-string output) expected))))
    ;; Legal actual arguments still evaluate once, left to right.
    (let ((stream (make-string-output-stream)))
      (assert (eq object
        (funcall name (progn (push :object effects) object)
                      (progn (push :stream effects) stream))))
      (assert (equal (reverse effects) '(:object :stream)))
      (assert (equal (get-output-stream-string stream) expected)))))
(printer-arity-check 'prin1 'arity-prin1 "(A B)")
(printer-arity-check 'princ 'arity-princ "(A B)")
(printer-arity-check 'print 'arity-print (format nil "~%(A B) "))
"#;

#[test]
fn printers_reject_excess_arguments_and_preserve_legal_stream_calls() {
    let mut baseline = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        let mut program = format!(
            "(defparameter *printer-required-tier* {}){}",
            match tier {
                "t1" => "1",
                "t2" => "2",
                _ => "nil",
            },
            PROGRAM
        );
        program.push_str("(write-line \"PRINTER-ARITY-OK\")");
        command.args(["--no-init", "--eval", &program]);
        for key in [
            "EGCL_FORCE_TIER",
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(key);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("PRINTER-ARITY-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
