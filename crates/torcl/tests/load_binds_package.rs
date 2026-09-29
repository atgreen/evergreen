//! LOAD binds *PACKAGE*, including when loading from a STREAM (bliss-pkgleak).
//!
//! CLHS says LOAD binds *PACKAGE* for its dynamic extent, so an IN-PACKAGE
//! inside a loaded file cannot leak into the caller. The file path did this;
//! the STREAM path did not, and that is the path a SLIME client injects its
//! runtime through:
//!
//!   (with-input-from-string (s "…") (load s))
//!
//! So connecting icl to TorCL left the session in ICL-RUNTIME before the user
//! had typed anything — the prompt read ICL-RUNTIME>, and so did every bare
//! symbol read there, which makes it a wrong-answer bug rather than a cosmetic
//! one.
//!
//! The test checks the package is RESTORED and that the load still took effect,
//! because "restore it" is trivially satisfiable by not loading anything.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_torcl");

const PROGRAM: &str = r#"
  (format t "BEFORE ~A~%" (package-name *package*))
  (with-input-from-string
      (s "(defpackage :leaky (:use :cl)) (in-package :leaky) (defun f () 42)")
    (load s))
  (format t "AFTER ~A~%" (package-name *package*))
  (format t "LOADED ~A~%" (funcall (find-symbol "F" "LEAKY")))
  ;; An error part-way through must restore it too.
  (ignore-errors
    (with-input-from-string (s "(in-package :leaky) (error \"boom\")") (load s)))
  (format t "AFTER-ERROR ~A~%" (package-name *package*))
"#;

#[test]
fn a_stream_load_does_not_leak_its_package() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run torcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for expected in [
        "BEFORE COMMON-LISP-USER",
        "AFTER COMMON-LISP-USER",
        "LOADED 42",
        "AFTER-ERROR COMMON-LISP-USER",
    ] {
        assert!(
            stdout.lines().any(|l| l.trim() == expected),
            "missing {expected:?} in:\n{stdout}\n{stderr}"
        );
    }
}
