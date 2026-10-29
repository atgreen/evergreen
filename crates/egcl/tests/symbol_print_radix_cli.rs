// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r#"
(defun radix-render (object) (prin1-to-string object))
(dotimes (i 80) (assert (equal (radix-render 'hello) "HELLO")))
(when *required-tier*
  (assert (= *required-tier* (egcl-ext:function-tier 'radix-render))))
(defun radix-read-back (text)
  (multiple-value-bind (value end) (read-from-string text)
    (assert (= end (length text))) value))
(defun radix-check-symbol (object)
  (dolist (text (list (write-to-string object) (radix-render object)
                     (format nil "~S" object)
                     (with-output-to-string (s) (prin1 object s))))
    (assert (eq object (radix-read-back text)))))
(let ((objects (list nil t 'cddr 'face (intern "E5") (intern "1E5")
                     (intern "1E+5") (intern "123.") (intern "F/F")
                     (intern "F/0") (intern "A/-B") (intern "A/+B")
                     (intern "FFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFFF")
                     (intern "A-B") (intern "+") (intern "-"))))
  (dolist (base *tested-bases*)
    (dolist (print-case '(:upcase :downcase :capitalize))
      (dolist (readtable-case '(:upcase :downcase :preserve :invert))
        (let ((*print-base* base) (*read-base* base) (*print-case* print-case)
              (*readtable* (copy-readtable nil)))
          (setf (readtable-case *readtable*) readtable-case)
          (dolist (object objects) (radix-check-symbol object))
          (let* ((*print-readably* t) (*print-escape* nil)
                 (tree (list nil t 'face (vector nil t 'cddr)))
                 (text (write-to-string tree)))
            ;; Readable printing assumes the standard readtable, independently
            ;; of the custom case mode used for the other round-trip checks.
            (let ((*readtable* (copy-readtable nil)))
              (assert (equalp tree (radix-read-back text)))))
          (let* ((symbol (make-symbol "FACE"))
                 (*print-circle* t)
                 (copy (radix-read-back (write-to-string (vector symbol symbol)))))
            (assert (eq (aref copy 0) (aref copy 1)))
            (assert (string= (symbol-name (aref copy 0)) "FACE")))
          ;; PRINC deliberately emits the name without reader escapes.
          (assert (not (find #\| (princ-to-string 'face)))))))))
(let ((*print-base* 29))
  (assert (string= (write-to-string nil) "|NIL|"))
  (assert (string= (write-to-string 'cddr) "|CDDR|")))
(let ((*print-base* 30)) (assert (string= (write-to-string t) "|T|")))
(let ((*print-base* 16))
  (assert (string= (write-to-string 'face) "|FACE|"))
  (assert (string= (write-to-string (intern "E5")) "|E5|")))
(let ((*print-base* 10))
  (assert (string= (write-to-string (intern "E5")) "E5"))
  (assert (string= (write-to-string 'face) "FACE")))
(write-line "SYMBOL-RADIX-OK")
"#;

#[test]
fn symbol_names_round_trip_in_every_print_base() {
    let mut baseline = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let program = format!(
            "(defparameter *required-tier* {})(defparameter *tested-bases* {}){}",
            if tier == "t2" { "2" } else { "nil" },
            if stress {
                "'(16 29 30 36)"
            } else {
                "(loop for b from 2 to 36 collect b)"
            },
            PROGRAM
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &program]);
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
        let output = command.output().expect("run symbol radix regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{tier}/{stress}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(stdout.contains("SYMBOL-RADIX-OK"));
        if let Some(expected) = &baseline {
            assert_eq!(&output.stdout, expected);
        } else {
            baseline = Some(output.stdout);
        }
    }
}
