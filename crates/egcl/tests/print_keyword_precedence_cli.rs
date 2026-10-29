// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline keyword-string keyword-stream))
(defun keyword-string (object) (write-to-string object :base 10 :base 3 :radix nil :radix t))
(defun keyword-stream (object stream)
  (write object :stream stream :stream nil :base 10 :base 3 :radix nil :radix t))
(dotimes (i 100)
  (assert (equal (keyword-string 7) "7"))
  (assert (equal (with-output-to-string (s) (assert (= (keyword-stream 7 s) 7))) "7")))
(when *required-tier*
  (dolist (function '(keyword-string keyword-stream))
    (loop repeat 100 until (= *required-tier* (egcl-ext:function-tier function))
          do (keyword-string 7) (with-output-to-string (s) (keyword-stream 7 s)) (sleep 0.01))
    (assert (= *required-tier* (egcl-ext:function-tier function)))))
(assert (equal (eval '(write-to-string 7 :base 10 :base 3)) "7"))
(assert (equal (funcall #'write-to-string "x" :escape t :escape nil) "\"x\""))
(assert (equal (apply #'write-to-string "x" '(:escape nil :escape t)) "x"))
(assert (equal (write-to-string 7 :radix nil :radix t) "7"))
(assert (equal (write-to-string (vector 1 2) :array t :array nil) "#(1 2)"))
(assert (search "#<" (write-to-string (vector 1 2) :array nil :array t :readably nil)))
(assert (equal (write-to-string (list 1 2) :length nil :length 0) "(1 2)"))
(assert (equal (write-to-string (list 1 2) :length 0 :length nil) "(...)"))
(assert (equal (write-to-string "x" :escape nil :readably nil :readably t) "x"))
;; Duplicate streams must not redirect WRITE away from its first stream, or
;; WRITE-TO-STRING away from the private stream that its wrapper prepends.
(let ((a (make-string-output-stream)) (b (make-string-output-stream)))
  (assert (= (funcall #'write 7 :stream a :stream b) 7))
  (assert (equal (get-output-stream-string a) "7"))
  (assert (equal (get-output-stream-string b) ""))
  (assert (equal (write-to-string 7 :stream b :allow-other-keys t) "7"))
  (assert (equal (get-output-stream-string b) "")))
(let ((effects nil) (stream (make-string-output-stream)))
  (assert (= (write (progn (push :object effects) 7)
                   :stream stream
                   :base (progn (push :first effects) 10)
                   :base (progn (push :second effects) 3)) 7))
  (assert (equal effects '(:second :first :object)))
  (assert (equal (get-output-stream-string stream) "7")))
(assert (equal (write-to-string 7 :unknown 1 :allow-other-keys t :allow-other-keys nil) "7"))
(let ((*print-base* 16) (*print-radix* nil) (*print-escape* nil))
  (assert (handler-case
            (progn (write-to-string 7 :base 10 :unknown 1 :allow-other-keys nil :allow-other-keys t) nil)
            (program-error () t)))
  (assert (= *print-base* 16))
  (assert (null *print-radix*))
  (assert (null *print-escape*)))
;; An error after controls were bound must restore them too.
(let ((*print-base* 16) (*print-radix* nil))
  (assert (handler-case
            (progn (write 7 :base 10 :radix t :stream (make-string-input-stream "")) nil)
            (error () t)))
  (assert (= *print-base* 16))
  (assert (null *print-radix*)))
(assert (handler-case (progn (apply #'write 7 '(:base)) nil) (program-error () t)))
(write-line "PRINT-KEYWORD-PRECEDENCE-OK")
"##;

#[test]
fn duplicate_printer_keywords_use_leftmost_pairs_in_all_tiers() {
    let mut expected = None;
    for (tier, stress) in [
        ("interp", false),
        ("t1", false),
        ("t2", false),
        ("t2", true),
    ] {
        let required = match tier {
            "t1" => "1",
            "t2" => "2",
            _ => "nil",
        };
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args([
            "--no-init",
            "--eval",
            &format!("(defparameter *required-tier* {required}){PROGRAM}"),
        ]);
        for key in [
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
            "{tier} stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("PRINT-KEYWORD-PRECEDENCE-OK"));
        if let Some(expected) = &expected {
            assert_eq!(&output.stdout, expected);
        } else {
            expected = Some(output.stdout);
        }
    }
}
