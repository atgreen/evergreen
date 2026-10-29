// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline graphic-colon-c graphic-formatter-c))
(defun graphic-colon-c (character) (format nil "~:C" character))
(defun graphic-formatter-c (character)
  (with-output-to-string (stream) (funcall (formatter "~:C") stream character)))
(dotimes (i 80) (graphic-colon-c #\a) (graphic-formatter-c #\a))
(when *graphic-required-tier*
  (dolist (name '(graphic-colon-c graphic-formatter-c))
    (loop repeat 100 until (= *graphic-required-tier* (egcl-ext:function-tier name))
          do (funcall name #\a) (sleep 0.01))
    (assert (= *graphic-required-tier* (egcl-ext:function-tier name)))))
(dolist (code '(0 8 9 10 12 13 32 65 97 127 160 5760 8194 8195 8232 8233 8239 8287 12288 128512))
  (let* ((character (code-char code)) (plain (string character))
         (output (graphic-colon-c character)))
    (assert (equal output (graphic-formatter-c character)))
    (assert (equal output (format nil "~:@C" character)))
    (assert (equal plain (format nil "~C" character)))
    (if (and (graphic-char-p character) (not (char= character #\Space)))
        (assert (equal plain output))
        (assert (> (length output) 1)))
    (assert (eql character (read-from-string (format nil "~@C" character))))))
(dolist (code '(160 12288))
  (let ((character (code-char code)))
    (assert (graphic-char-p character))
    (assert (= (length (graphic-colon-c character)) 1))
    (assert (> (length (prin1-to-string character)) 3))
    (assert (eql character (read-from-string (prin1-to-string character))))))
(write-line "FORMAT-GRAPHIC-CHARACTER-OK")
"##;

#[test]
fn format_and_formatter_preserve_graphic_unicode_characters() {
    for tier in ["interp", "t2"] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(name);
        }
        let required = if tier == "t2" { "2" } else { "nil" };
        let output = command
            .args([
                "--no-init",
                "--eval",
                &format!("(defparameter *graphic-required-tier* {required}){PROGRAM}"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "FORMAT-GRAPHIC-CHARACTER-OK")
        );
    }
}
