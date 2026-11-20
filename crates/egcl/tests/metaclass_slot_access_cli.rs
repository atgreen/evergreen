// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Instances of a STANDARD-CLASS subclass metaclass (GH #954). With no method
//! of its own on the slot-access protocol, such a metaclass gets direct slot
//! access, as STANDARD-CLASS does; before, every read searched CLASS-SLOTS
//! through SLOT-DEFINITION-NAME. Reads, writes, SLOT-BOUNDP, the unbound-slot
//! error and SLOT-MAKUNBOUND keep their answers, and a
//! SLOT-VALUE-USING-CLASS method added to the metaclass after warm-up runs
//! on every later access (three per row, so 300). SBCL 2.6.9 agrees with
//! every value but that count: its already-optimized accessors and
//! SLOT-VALUE never call the late method.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defclass mm-meta (standard-class) ())
  (defmethod egcl-mop:validate-superclass
      ((class mm-meta) (super standard-class)) t)
  (defclass mm-obj () ((a :initform 1 :accessor mm-a) (b :accessor mm-b) (c :initform 3)) (:metaclass mm-meta))
  (defvar *mm-hits* 0)
  (defun mm-row (o)
    (list (mm-a o) (slot-value o 'c) (slot-boundp o 'b)
          (handler-case (mm-b o) (unbound-slot () :unbound))))
  (let ((o (make-instance 'mm-obj)) r1 r2 r3)
    (dotimes (i 3000) (setq r1 (mm-row o)))
    (setf (mm-b o) 20 (slot-value o 'c) 30)
    (dotimes (i 3000) (setq r2 (mm-row o)))
    (defmethod egcl-mop:slot-value-using-class
        :around ((class mm-meta) object slotd)
      (incf *mm-hits*) (call-next-method))
    (dotimes (i 100) (setq r3 (mm-row o)))
    (let ((hits *mm-hits*))
      (slot-makunbound o 'a)
      (let ((*print-pretty* nil))
        (format t "RESULT ~S ~S ~S ~S ~S~%" r1 r2 r3 hits
                (handler-case (mm-a o) (unbound-slot () :unbound)))))))
"#;

const EXPECTED: &str = "RESULT (1 3 NIL :UNBOUND) (1 30 T 20) (1 30 T 20) 300 :UNBOUND";

fn run(tier: Option<&str>) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", PROGRAM]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed
stdout: {stdout}
stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

#[test]
fn standard_class_subclass_metaclass_slots_keep_the_protocol() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let out = run(tier);
        let line = out
            .lines()
            .find(|line| line.starts_with("RESULT "))
            .unwrap_or_else(|| {
                panic!(
                    "tier {tier:?}: no RESULT line in:
{out}"
                )
            });
        assert_eq!(line, EXPECTED, "tier {tier:?}");
    }
}
