// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

// General arrays deliberately isolate bootstrap ordering from specialized storage.
const PROGRAM: &str = r##"
(progn
  (assert (string= (write-to-string #(0 1 255)) "#(0 1 255)"))
  (assert (string= (write-to-string #(10 15 255) :base 16 :radix t) "#(#xA #xF #xFF)"))
  (assert (string= (write-to-string "hello" :escape nil) "hello"))
  (assert (string= (write-to-string "hello" :escape t) (prin1-to-string "hello")))
  (assert (string= (write-to-string #(1 2 3) :length 2) "#(1 2 ...)"))
  (assert (equal (multiple-value-list (write-to-string 42)) '("42")))
  (let ((*print-base* 10) (*print-radix* nil) (s :lexical-shadow))
    (assert (string= (write-to-string 255 :base 16) "FF"))
    (assert (= *print-base* 10))
    (assert (null *print-radix*))
    (assert (eq s :lexical-shadow)))
  (assert (handler-case (progn (write-to-string #(1) :unknown-control t) nil)
            (program-error () t)))
  (format t "WRITE-TO-STRING-BOOTSTRAP-OK~%"))
"##;

#[test]
fn bootstrap_write_to_string_supports_every_tier_and_printer_controls() {
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_FORCE_TIER", tier)
            .args(["--no-init", "--eval", PROGRAM])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "tier={tier}\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("WRITE-TO-STRING-BOOTSTRAP-OK"));
    }
}
