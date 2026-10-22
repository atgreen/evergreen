// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

fn eval(program: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

/// Native entry loads each parameter from its interpreter frame slot at
/// `[rdi + 8*i]`. The load was always encoded with a disp8, so slot 16 (offset
/// 128) became `[rdi-128]` and read the caller's stack: once a function with 17
/// or more parameters tiered up, its later parameters held stale words. Since
/// &optional, &rest and &key functions enter native code, this hit pure-tls's
/// keyword-heavy TLS handshake and every `ocicl install`.
#[test]
fn parameters_past_the_sixteenth_survive_tier_up() {
    let stdout = eval(
        r#"
      (progn
        (defmacro define-wide (name lambda-list-keyword count)
          (let ((params (loop for i below count collect (intern (format nil "P~D" i)))))
            `(defun ,name (,@(and lambda-list-keyword (list lambda-list-keyword)) ,@params)
               (list ,@params))))
        (define-wide required-17 nil 17)
        (define-wide required-40 nil 40)
        (define-wide optional-20 &optional 20)
        (define-wide key-17 &key 17)
        (defun bad-calls ()
          (let ((bad 0))
            (dotimes (n 200 bad)
              (let ((expected-17 (append (loop for i below 16 collect i) (list n)))
                    (expected-40 (append (loop for i below 39 collect i) (list n))))
                (unless (equal (required-17 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 n) expected-17)
                  (incf bad))
                (unless (equal (apply #'required-40 (append (loop for i below 39 collect i) (list n)))
                               expected-40)
                  (incf bad))
                (unless (equal (optional-20 0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 n)
                               (append expected-17 (list nil nil nil)))
                  (incf bad))
                (unless (equal (key-17 :p0 0 :p16 n)
                               (append (list 0) (make-list 15) (list n)))
                  (incf bad))))))
        (format t "BAD:~D~%" (bad-calls))
        (disassemble #'required-17))
    "#,
    );
    assert!(stdout.contains("BAD:0"), "{stdout}");
    assert!(
        stdout.contains("; REQUIRED-17 —"),
        "REQUIRED-17 stayed interpreted: {stdout}"
    );
    assert!(
        !stdout.contains("[rdi-"),
        "a frame slot load wrapped negative: {stdout}"
    );
}
