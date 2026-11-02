// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! STRING accepts only string designators, including when called from native
//! code. The lazy-publication error fixture needs this actual runtime error.

use std::process::Command;

const PROGRAM: &str = r#"
(defun designator-string (object) (string object))
(dotimes (i 500) (designator-string 'warm))
(dolist (object (list 5 1.5 '(a) #(a) #p"file"))
  (assert
    (handler-case (progn (designator-string object) nil)
      (type-error (condition) (eql object (type-error-datum condition))))))
(let ((text (copy-seq "abc")))
  (assert (eq text (designator-string text))))
(assert (equal (designator-string 'hello) "HELLO"))
(assert (equal (designator-string nil) "NIL"))
(assert (equal (designator-string t) "T"))
(assert (equal (designator-string #\a) "a"))
(assert (= (char-code (char (designator-string (code-char #xd800)) 0)) #xd800))
(write-line "STRING-DESIGNATORS-OK")
"#;

#[test]
fn string_rejects_non_designators_with_the_original_type_error_datum() {
    for tier in ["interp", "t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_THREADS", "0")
            .output()
            .expect("run STRING regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("STRING-DESIGNATORS-OK"), "{tier}: {stdout}");
    }
}
