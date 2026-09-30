//! A listener can be polled without blocking (bliss-1ebb1).
//!
//! `%SOCKET-ACCEPT` blocks, and a listener is an id into a table rather than a
//! stream, so `%SOCKET-WAIT-FOR-INPUT` cannot see one. That left no way to ask
//! "is anybody waiting?" — and therefore no way for a program with its own event
//! loop to offer a REPL, because the first accept would stall it until somebody
//! connected. A Bliss UI serving slynk between frames is exactly that program.
//!
//! The test connects to itself so there is no second process, and checks all
//! three states rather than only the interesting one: a poll that always said T
//! would pass a test that only checked "T when a client is waiting", and would
//! make the caller accept on every frame and block on the first.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

// From a DEFUN as well as from the top level. These builtins need TWO
// registrations -- the evaluated table in evaluated_builtins.rs and the
// operator-position arm in cli.rs -- and with only the first, a top-level call
// works while the same call inside a compiled function reports "undefined
// function". That is the two-evaluators trap in AGENTS.md, and it cost an hour
// here: the caller is always a defun in real use.
const PROGRAM: &str = r#"
  (defun ready-from-a-defun (l) (egcl::%socket-listener-ready-p l 0))
  (let* ((l (egcl::%socket-listen "127.0.0.1" 0 5))
         (port (egcl::%socket-local-port l)))
    (format t "IDLE ~A~%" (egcl::%socket-listener-ready-p l 0))
    (let ((client (egcl::%socket-connect "127.0.0.1" port)))
      (format t "PENDING ~A~%" (egcl::%socket-listener-ready-p l 100))
      (let ((served (egcl::%socket-accept l)))
        (write-line "ok" served)
        (finish-output served)
        (format t "ECHO ~S~%" (read-line client)))
      (format t "COMPILED ~A~%" (ready-from-a-defun l))
      (format t "DRAINED ~A~%" (egcl::%socket-listener-ready-p l 0))
      (close client))
    (egcl::%socket-close l))
"#;

#[test]
fn a_listener_can_be_polled_without_blocking() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "polling a listener failed: {stdout}\n{stderr}"
    );
    for expected in [
        "IDLE NIL",
        "PENDING T",
        "ECHO \"ok\"",
        "COMPILED NIL",
        "DRAINED NIL",
    ] {
        assert!(
            stdout.lines().any(|l| l.trim() == expected),
            "missing {expected:?} in:\n{stdout}\n{stderr}"
        );
    }
}
