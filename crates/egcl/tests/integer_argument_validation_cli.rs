// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Builtins that take an integer argument used to narrow it through an f64,
//! so a float, a negative number or a bignum was quietly turned into some
//! other integer instead of being rejected or handled exactly.
use std::process::Command;

#[test]
fn nth_value_and_decode_universal_time_check_their_integer_arguments() {
    let source = r#"
      (defmacro signals-type-error (form)
        `(handler-case (progn ,form nil) (type-error () t)))
      ;; NTH-VALUE: N is a non-negative integer, in every path. The COMPILE
      ;; path used to treat -1 as index 0 and answer 1.
      (assert (signals-type-error (nth-value -1 (values 1 2))))
      (assert (signals-type-error
               (funcall (compile nil '(lambda () (nth-value -1 (values 1 2)))))))
      (assert (signals-type-error
               (funcall (compile nil '(lambda () (nth-value 1.5 (values 1 2)))))))
      (assert (eql 2 (funcall (compile nil '(lambda () (nth-value 1 (values 1 2)))))))
      (assert (null (funcall (compile nil '(lambda () (nth-value 5 (values 1 2)))))))
      (assert (null (funcall (compile nil '(lambda () (nth-value (expt 10 30) (values 1 2)))))))
      ;; DECODE-UNIVERSAL-TIME: a universal time is a non-negative integer.
      (assert (signals-type-error (decode-universal-time 1.5 0)))
      (assert (signals-type-error (decode-universal-time -1 0)))
      (assert (signals-type-error (decode-universal-time 1/2 0)))
      (assert (handler-case (progn (decode-universal-time (expt 10 30) 0) nil)
                (error () t)))
      (assert (equal '(0 0 4 13 5 2026 2 nil 0)
                     (multiple-value-list (decode-universal-time 3987633600 0))))
      (assert (= 3987633600
                 (multiple-value-bind (s m h d mo y) (decode-universal-time 3987633600 -5)
                   (encode-universal-time s m h d mo y -5))))
      (format t "INTEGER-ARGS-OK~%")
    "#;
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", source])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("INTEGER-ARGS-OK"));
}
