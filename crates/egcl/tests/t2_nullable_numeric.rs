// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

#[test]
fn conditional_numeric_parameters_stay_t2_when_the_numeric_path_is_skipped() {
    let program = r#"
      (defun maybe-increment (x) (if x (+ x 1) 0))
      (defun unless-null (x) (if (null x) 0 (+ x 1)))
      (defun unless-absent (x) (if (eq x :absent) 0 (+ x 1)))
      (dotimes (i 20)
        (maybe-increment 4) (unless-null 4) (unless-absent 4))
      (dotimes (i 30)
        (unless (and (= (maybe-increment nil) 0)
                     (= (unless-null nil) 0)
                     (= (unless-absent :absent) 0)
                     (= (maybe-increment 4) 5)
                     (= (unless-null 4) 5)
                     (= (unless-absent 4) 5))
          (error "Wrong conditional result")))
      (format t "TIERS ~S~%"
        (mapcar #'egcl-ext:function-tier
          '(maybe-increment unless-null unless-absent)))
      (dotimes (i 30)
        (unless (equal (remove-if-not #'digit-char-p "9/02-461") "902461")
          (error "Wrong filtering result")))
      (format t "FILTER-TIER ~D~%" (egcl-ext:function-tier '%match-positions))
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_DEOPT_BLACKLIST_THRESHOLD", "3")
        .output()
        .expect("run EGCL");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("TIERS (2 2 2)"),
        "nullable parameters must not blacklist T2: {stdout}"
    );
    assert!(
        stdout.contains("FILTER-TIER 2"),
        "filtering must keep its native helper: {stdout}"
    );
}
