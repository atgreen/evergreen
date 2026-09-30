//! An INHERITED structure slot keeps its own keyword (bliss-fskhm).
//!
//! A direct slot's name arrives bare; an inherited one arrives from the class
//! metadata fully QUALIFIED. Concatenating the qualified spelling produced the
//! initarg `:|COMMON-LISP-USER::SOCKET|` and the accessor
//! `CHILD-COMMON-LISP-USER::SOCKET`.
//!
//! Harmless while constructors went through MAKE-INSTANCE, which matched
//! initargs by slot NAME and never compared the keyword. The moment the keyword
//! appears in a &key lambda list, every inherited slot silently takes its
//! DEFAULT instead of the supplied value — and a default that signals, which is
//! exactly how slynk marks a required slot, turns that into an error the first
//! time a client connects.
//!
//! The test therefore checks the value is CARRIED, not merely that nothing
//! signalled: a struct that quietly defaults every inherited slot to NIL would
//! pass a weaker test.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defun required () (error "a required argument was not supplied"))
  (defstruct (par (:constructor %make-par))
    (socket (required)) (socket-io (required)) (channels '()))
  (defstruct (kid (:include par) (:constructor %make-kid)) (extra 7))
  (let ((k (%make-kid :socket 1 :socket-io 2)))
    (format t "CARRIED ~A ~A ~A ~A~%"
            (par-socket k) (par-socket-io k) (par-channels k) (kid-extra k)))
  ;; An omitted required slot must still signal: the initform is the point.
  (format t "OMITTED ~A~%"
          (handler-case (progn (%make-kid :socket 1) :no-error)
            (error () :signalled)))
"#;

#[test]
fn an_inherited_slot_keeps_its_own_initarg() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.lines().any(|l| l.trim() == "CARRIED 1 2 NIL 7"),
        "supplied values did not reach the inherited slots:\n{stdout}\n{stderr}"
    );
    assert!(
        stdout.lines().any(|l| l.trim() == "OMITTED SIGNALLED"),
        "an omitted required slot must still run its initform:\n{stdout}\n{stderr}"
    );
}
