// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Definition-time expansion must retain binding syntax across moving GC.
use std::process::Command;

#[test]
fn expanded_binding_lists_survive_gc_stress() {
    // POSITION expands LOOP, whose ASSIGN helper uses PUSH. A stale MVB
    // variable list in its installed expansion produced unbound #:NEW-SETF-28
    // under stress (bliss-ohwle.2), even without callbacks or fibers.
    let program = r#"
      (let ((names '("EGCL-DEBUG::LIST-BACKTRACE" "COMMON-LISP-USER::BOUNDARY-LEAF"
                     "trace_inner_c" "EGCL-FFI::FOREIGN-CALL"
                     "COMMON-LISP-USER::BOUNDARY-MIDDLE" "trace_outer_c"
                     "EGCL-FFI::FOREIGN-CALL" "COMMON-LISP-USER::BOUNDARY-OUTER")))
        (assert (= 1 (position "COMMON-LISP-USER::BOUNDARY-LEAF" names :test #'equal))))
      (dotimes (i 30)
        ;; Fresh syntax exercises reconstruction of both binding forms.
        (eval (copy-tree
                '(defun binding-source (value)
                   (multiple-value-bind (head tail) (values value (list :tail))
                     (destructuring-bind ((a b) c) (list (list head tail) :last)
                       (list a b c))))))
        (assert (equal (list i '(:tail) :last) (binding-source i))))
      (format t "BINDING-SOURCE-OK~%")
    "#;
    for tier in ["interp", "t0", "t1", "t2"] {
        let mut baseline = None;
        for stress in ["0", "100"] {
            let output = Command::new("timeout")
                .args([
                    "--kill-after=5",
                    "90",
                    env!("CARGO_BIN_EXE_egcl"),
                    "--no-init",
                    "--eval",
                    program,
                ])
                .env("EGCL_FORCE_TIER", tier)
                .env("EGCL_GC_STRESS", stress)
                .env("EGCL_GC_POISON", "1")
                .env_remove("EGCL_GC_STRESS_SKIP")
                .env_remove("EGCL_GC_STRESS_AT")
                .output()
                .expect("run binding-source regression");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                output.status.success(),
                "{tier} stress={stress}: {stdout}\n{stderr}"
            );
            assert!(
                stdout.contains("BINDING-SOURCE-OK"),
                "{tier}: {stdout}\n{stderr}"
            );
            if let Some(expected) = &baseline {
                assert_eq!(
                    &output.stdout, expected,
                    "{tier}: stress changed the result"
                );
            } else {
                baseline = Some(output.stdout);
            }
        }
    }
}
