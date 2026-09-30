//! Funcallable instances: a class whose metaclass is FUNCALLABLE-STANDARD-CLASS
//! produces instances that ARE functions and whose function is installed after
//! the fact (AMOP; bliss-cr53).
//!
//! This is the shape closer-mop users rely on — `iparse`, which the pure-tls test
//! system pulls in, does exactly `(defclass iparser () … (:metaclass
//! c2mop:funcallable-standard-class))` and then installs a closure so the parser
//! can be FUNCALLed. EGCL had none of it.
//!
//! Three things are easy to get wrong and are each asserted here:
//!
//! - FUNCTIONP and (TYPEP x 'FUNCTION) must be true BEFORE anything is installed:
//!   AMOP makes such an instance a function from the moment it exists. (UIOP's
//!   ENSURE-FUNCTION etypecase has already broken once on a callable EGCL's
//!   predicate denied — bliss-aid.)
//! - A plain instance must NOT become a function, and installing on one is a
//!   TYPE-ERROR.
//! - Calling one with nothing installed is a catchable TYPE-ERROR, not a crash.
//!
//! Run in both tiers and under GC stress: the dispatch reads the function back
//! out of a GC-heap slot on every call, and the `drive` loop funcalls the instance
//! from compiled code.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (defclass cr53-p ()
    ((label :initarg :label :reader cr53-label))
    (:metaclass egcl-ext:funcallable-standard-class))
  (defclass cr53-plain () ())
  (defun drive (p n) (let ((acc 0)) (dotimes (i n) (setq acc (+ acc (funcall p i)))) acc))
  (let ((p (make-instance 'cr53-p :label "L"))
        (before nil))
    (setq before (list (functionp p) (typep p 'function)))
    (egcl-ext:set-funcallable-instance-function
     p (lambda (i &key (bump 0)) (+ (* 2 i) bump)))
    (format t "FI ~S~%"
            (list before
                  (funcall p 5)
                  (funcall p 5 :bump 1)
                  (apply p (list 7))
                  (drive p 3)
                  (functionp p)
                  (functionp (make-instance 'cr53-plain))
                  (functionp (egcl-ext:funcallable-instance-function p))
                  (handler-case (egcl-ext:set-funcallable-instance-function
                                 (make-instance 'cr53-plain) (lambda () 1))
                    (type-error () :type-error))
                  (handler-case (funcall (make-instance 'cr53-p))
                    (type-error () :type-error))
                  (cr53-label p))))
"#;

const EXPECTED: &str = r#"FI ((T T) 10 11 14 6 T NIL T :TYPE-ERROR :TYPE-ERROR "L")"#;

#[test]
fn a_funcallable_instance_is_a_function_in_both_tiers() {
    for (tier, stress) in [("t0", false), ("t1", false), ("t0", true), ("t1", true)] {
        let mut command = Command::new(BIN);
        command.args(["--no-init", "--eval", PROGRAM]);
        command.env("EGCL_FORCE_TIER", tier);
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
            "tier={tier} stress={stress}: {stdout}\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            stdout.lines().next().unwrap_or("").trim(),
            EXPECTED,
            "tier={tier} stress={stress}"
        );
    }
}

/// A hot loop that funcalls the instance must keep answering correctly once the
/// loop has been promoted — the dispatch happens per call, so a tier that
/// resolved the callee once would go wrong here rather than at the first call.
#[test]
fn a_promoted_loop_keeps_calling_through_the_instance() {
    let program = r#"
      (defclass cr53-hot () () (:metaclass egcl-ext:funcallable-standard-class))
      (defun drive (p n) (let ((acc 0)) (dotimes (i n) (setq acc (+ acc (funcall p i)))) acc))
      (let ((p (make-instance 'cr53-hot)))
        (egcl-ext:set-funcallable-instance-function p (lambda (i) (* 2 i)))
        (format t "HOT ~S~%" (drive p 200000)))
    "#;
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    // 2 * sum(0..199999)
    assert_eq!(
        stdout.lines().next().unwrap_or("").trim(),
        "HOT 39999800000"
    );
}
