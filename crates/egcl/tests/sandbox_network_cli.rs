// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Sandbox mode denies network access in both directions: it refused
//! %SOCKET-CONNECT but created listeners with %SOCKET-LISTEN, which binds a
//! port (bliss-hyw5y). %SOCKET-ACCEPT is refused too. Outside the sandbox a
//! listener still works.
use std::process::Command;

fn run(sandbox: bool, tier: &str, program: &str) -> String {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.env("EGCL_FORCE_TIER", tier);
    if sandbox {
        command.arg("--sandbox");
    }
    let output = command.args(["--eval", program]).output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{tier}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn sandbox_denies_socket_listeners() {
    let program = r#"(progn
      (defun try (f) (handler-case (funcall f) (error (e) (list :denied (princ-to-string e)))))
      (defun listen-probe () (try (lambda () (egcl::%socket-listen "127.0.0.1" 0))))
      (dotimes (i 20) (listen-probe))
      (format t "~&RESULT ~s~%"
              (list (listen-probe)
                    (try (lambda () (egcl::%socket-accept 1)))
                    (try (lambda () (egcl::%socket-connect "127.0.0.1" 9))))))"#;
    for tier in ["interp", "t0", "t2"] {
        let stdout = run(true, tier, program);
        assert!(
            stdout.contains(
                "RESULT ((:DENIED \"network access denied\") (:DENIED \"network access denied\") \
                 (:DENIED \"network access denied\"))"
            ),
            "{tier}: {stdout}"
        );
    }
}

#[test]
fn listeners_still_work_outside_the_sandbox() {
    let stdout = run(
        false,
        "interp",
        r#"(let ((listener (egcl::%socket-listen "127.0.0.1" 0)))
             (format t "~&RESULT ~s~%" (list (integerp listener)
                                             (integerp (egcl::%socket-local-port listener)))))"#,
    );
    assert!(stdout.contains("RESULT (T T)"), "{stdout}");
}
