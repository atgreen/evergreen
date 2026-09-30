//! Printing a CLOS instance must survive a moving GC (bliss-phgt).
//!
//! Both printers — the stdlib formatter (`FORMAT ~A/~S`, and so
//! `princ-to-string`) and the interpreter's own `print_val_inner` (`PRINC` /
//! `PRIN1` to a stream) — first ask the interpreter whether a user
//! `PRINT-OBJECT` method applies. That dispatch allocates: it makes a string
//! output stream and searches for an applicable method. When it declines, which
//! is the common case, both printers went on to use their *unrooted local copy*
//! of the object. A minor GC inside the dispatch relocates the instance, so the
//! copy was stale, `class_of` no longer recognized it, and a live instance
//! printed as `#<HEAP-OBJECT>`.
//!
//! That silently destroyed condition reports in particular — `princ-to-string`
//! of a condition is how a message reaches a user, a log, or a test assertion —
//! so this is a wrong-result bug, not a crash, and nothing failed loudly.
//!
//! The stress run must produce output IDENTICAL to the plain run; comparing
//! against a crash would not have caught this.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defclass phgt-plain () ())
  (defclass phgt-slots () ((a :initarg :a :accessor phgt-a)))
  (format t "PRINT ~S~%"
    (list (princ-to-string (make-instance 'phgt-plain))
          (prin1-to-string (make-instance 'phgt-plain))
          (princ-to-string (make-condition 'simple-error :format-control "boom"))
          (princ-to-string (handler-case (error "signalled ~A" 42) (error (e) e)))
          (princ-to-string (make-instance 'phgt-slots :a 1))
          (with-output-to-string (s) (princ (make-instance 'phgt-plain) s))
          (with-output-to-string (s) (prin1 (make-instance 'phgt-plain) s))))
"#;

const EXPECTED: &str = r##"PRINT ("#<PHGT-PLAIN>" "#<PHGT-PLAIN>" "boom" "signalled 42" "#<PHGT-SLOTS>" "#<PHGT-PLAIN>" "#<PHGT-PLAIN>")"##;

#[test]
fn printing_an_instance_survives_a_moving_gc() {
    for stress in [false, true] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--eval", PROGRAM]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_VERIFY", "1");
        }
        let output = command.output().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "stress={stress}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            EXPECTED,
            "stress={stress}: an instance printed as something other than its class \
             (a stale pointer prints as #<HEAP-OBJECT>)"
        );
    }
}
