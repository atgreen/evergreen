// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! EGCL-EXT:ATOMIC-INCF/DECF on a structure or CLOS accessor place work in
//! compiled code. The expansion passed the accessor as a keyword that the
//! store primitive re-resolved by name and found no slot for, so a DEFUN at
//! T0 or a COMPILE'd lambda signalled "not a structure-slot accessor"
//! (bliss-n824t).
use std::process::Command;

const PROGRAM: &str = r#"(progn
(defstruct ctr (n 0 :type fixnum))
(defclass obj () ((v :accessor obj-v :initform 10)))
(defvar *c* (make-ctr))
(defvar *o* (make-instance 'obj))
(defun ext-incf () (egcl-ext:atomic-incf (ctr-n *c*) 3))
(defun ext-decf-obj () (egcl-ext:atomic-decf (obj-v *o*) 1))
(dotimes (i 20) (ext-incf) (ext-decf-obj))
(format t "~&RESULT ~s~%"
        (list (ext-incf) (ctr-n *c*) (ext-decf-obj) (obj-v *o*)
              (funcall (compile nil '(lambda () (egcl-ext:atomic-decf (ctr-n *c*) 1))))
              (ctr-n *c*))))"#;

#[test]
fn atomic_update_of_an_accessor_place_works_in_compiled_code() {
    for tier in ["interp", "t0", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", PROGRAM])
            .env("EGCL_FORCE_TIER", tier)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "{tier}: {stdout}\n{stderr}");
        assert!(
            stdout.contains("RESULT (60 63 -10 -11 63 62)"),
            "{tier}: {stdout}\n{stderr}"
        );
    }
}
