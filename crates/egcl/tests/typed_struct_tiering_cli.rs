// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A DEFSTRUCT constructor and writer whose slots declare types leave T0
//! (bliss-gg3i1.7.6). The inline failure path of the slot check used to make
//! them ineligible for native structure access, so SBCL's cross-compiler ran
//! every typed constructor and writer interpreted. The failure path keeps its
//! STORE-VALUE restart.

use std::process::Command;

const PROGRAM: &str = r#"
(defstruct (tt-node (:constructor make-tt-node (number kind)))
  (number 0 :type fixnum)
  (kind :normal :type symbol)
  (next nil :type (or tt-node null)))
(defun tt-run (n)
  (let ((head (make-tt-node 0 :head)))
    (dotimes (i n) (setf (tt-node-next head) (make-tt-node i :x)))
    (tt-node-number (tt-node-next head))))
(defun tt-tiers ()
  (list (egcl-ext:function-tier 'make-tt-node)
        (egcl-ext:function-tier (fdefinition '(setf tt-node-next)))))
;; Promotion is compiled in the background: give it a bounded time to land.
;; FUNCTION-TIER is 1 for T0 bytecode, 2 for T1 and 3 for T2.
(dotimes (round 400) (tt-run 100) (when (every (lambda (tier) (> tier 1)) (tt-tiers)) (return)) (sleep 0.025))
(format t "TIERS ~S~%" (mapcar (lambda (tier) (if (> tier 1) :native :t0)) (tt-tiers)))
(format t "RESULT ~S ~S ~S ~S~%"
        (tt-run 10)
        (handler-case (make-tt-node "no" :x) (type-error () :type-error))
        (handler-case (setf (tt-node-next (make-tt-node 1 :x)) 7) (type-error () :type-error))
        (handler-bind ((type-error (lambda (c) (declare (ignore c)) (invoke-restart 'store-value 5))))
          (tt-node-number (make-tt-node "no" :x))))
"#;

#[test]
fn typed_structure_constructor_and_writer_leave_t0() {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")])
        .env_remove("EGCL_FORCE_TIER")
        .env_remove("EGCL_DISABLE_T2")
        .env_remove("EGCL_GC_STRESS");
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("RESULT 9 :TYPE-ERROR :TYPE-ERROR 5"),
        "{stdout}"
    );
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        assert!(stdout.contains("TIERS (:NATIVE :NATIVE)"), "{stdout}");
    }
}
