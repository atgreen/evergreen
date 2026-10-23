// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! (mod n) is (integer 0 (n)) for any integer n, fixnum or bignum, and any
//! integer object (bliss-tgt4). TYPEP answered NIL for a bignum object and
//! panicked in the runtime on a bignum modulus. Expected values are SBCL's.
use std::process::Command;

#[test]
fn mod_types_accept_bignum_objects_and_moduli() {
    let program = r#"(progn
      (defun probe ()
        (list (typep (expt 2 70) (list 'mod (expt 2 80)))
              (typep (expt 2 90) (list 'mod (expt 2 80)))
              (typep 5 (list 'mod (expt 2 80)))
              (typep -1 '(mod 10)) (typep 9 '(mod 10)) (typep 10 '(mod 10))
              (typep (expt 2 70) '(mod 100)) (typep 1.0 '(mod 10))
              (typep (- (expt 2 80)) (list 'mod (expt 2 80)))))
      (dotimes (i 20) (probe))
      (format t "~&RESULT ~s~%" (probe)))"#;
    for tier in ["interp", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", program])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("RESULT (T NIL T NIL T NIL NIL NIL NIL)"), "{tier}: {stdout}\n{stderr}");
    }
}
