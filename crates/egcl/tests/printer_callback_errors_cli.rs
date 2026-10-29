// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(defclass callback-test-object () ((mode :initarg :mode)))
(defvar *callback-error-count* 0)
(defun run-printer-callback (object stream)
  ;; Successful nested printer calls must leave the surrounding operation usable.
  (assert (equal (prin1-to-string "nested") "\"nested\""))
  (case (slot-value object 'mode)
    (:error (error "expected printer callback failure"))
    (:throw (throw 'printer-callback-exit :escaped))
    (:restart (restart-case (error "recoverable printer callback failure")
                (printer-callback-recover () (write-string "RECOVERED" stream))))
    (t (write-string "CLEAN" stream))))
(defmethod print-object ((object callback-test-object) stream)
  (run-printer-callback object stream))
(declaim (notinline native-callback-princ native-callback-prin1 native-callback-print native-callback-write))
(defun native-callback-princ (object) (with-output-to-string (s) (princ object s)))
(defun native-callback-prin1 (object) (with-output-to-string (s) (prin1 object s)))
(defun native-callback-print (object) (with-output-to-string (s) (print object s)))
(defun native-callback-write (object) (with-output-to-string (s) (write object :stream s)))
(defun check-callback-errors (operation)
  (let ((*callback-error-count* 0))
    (assert (eq (handler-case
                    (handler-bind ((error (lambda (condition) (declare (ignore condition))
                                             (incf *callback-error-count*))))
                      (funcall operation (make-instance 'callback-test-object :mode :error))
                      :missed)
                  (error () :caught)) :caught))
    (assert (> *callback-error-count* 0)))
  (assert (eq (catch 'printer-callback-exit
                (funcall operation (make-instance 'callback-test-object :mode :throw)) :missed)
              :escaped))
  (let ((text (handler-bind ((error (lambda (condition) (declare (ignore condition))
                                     (invoke-restart 'printer-callback-recover))))
                (funcall operation (make-instance 'callback-test-object :mode :restart)))))
    (assert (search "RECOVERED" text)))
  ;; Neither a handled error nor a non-local exit may leak into the next call.
  (assert (search "clean" (funcall operation "clean"))))
(let ((operations (list #'native-callback-princ #'native-callback-prin1
                        #'native-callback-print #'native-callback-write)))
  (dolist (operation operations) (dotimes (i 80) (funcall operation "warm")))
  (when *callback-required-tier*
    (dolist (name '(native-callback-princ native-callback-prin1 native-callback-print native-callback-write))
      (loop repeat 100 until (= *callback-required-tier* (egcl-ext:function-tier name))
            do (funcall name "warm") (sleep 0.01))
      (assert (= *callback-required-tier* (egcl-ext:function-tier name)))))
  (dolist (operation operations) (check-callback-errors operation))
  (let ((*print-pretty* t) (*print-pprint-dispatch* (copy-pprint-dispatch nil)))
    (set-pprint-dispatch 'callback-test-object
      (lambda (stream object) (run-printer-callback object stream)) 100)
    (dolist (operation operations) (check-callback-errors operation)))
  (dolist (operation operations)
    (assert (search "CLEAN" (funcall operation (make-instance 'callback-test-object :mode :clean))))))
(write-line "PRINTER-CALLBACK-ERRORS-OK")
"##;

#[test]
fn printer_callbacks_preserve_errors_transfers_and_live_restarts_in_native_calls() {
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let program = format!("(defparameter *callback-required-tier* {required}){PROGRAM}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier} stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "PRINTER-CALLBACK-ERRORS-OK")
                .count(),
            1
        );
    }
}
