// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(defun check-complex-subtype (part1 part2 expected)
  (assert (equal (multiple-value-list
                  (subtypep `(complex ,part1) `(complex ,part2)))
                 (list expected t))))
;; Different upgraded representations require the ORIGINAL numeric containment.
(dolist (family '(short-float single-float double-float long-float))
  (check-complex-subtype family '(float 0.0 1.0) nil)
  (check-complex-subtype `(,family 0.0 2.0) '(float 0.0 1.0) nil)
  (check-complex-subtype `(,family 0.0 1.0) '(float 0.0 2.0) t)
  (check-complex-subtype `(,family 0.0 1.0) '(float (0.0) 2.0) nil)
  (check-complex-subtype `(,family (0.0) 1.0) '(float 0.0 1.0) t)
  (check-complex-subtype family 'float t)
  (check-complex-subtype 'float family nil)
  ;; Equal upgrades must retain the distinct CLHS representation-equivalence rule.
  (check-complex-subtype family `(,family 0.0 1.0) t))
(check-complex-subtype '(integer 0 1) '(rational 10 20) t)
(check-complex-subtype 'single-float 'short-float t)
(check-complex-subtype 'double-float 'long-float t)
(check-complex-subtype 'single-float 'double-float nil)
(check-complex-subtype 'double-float 'single-float nil)
(assert (equal (multiple-value-list (subtypep '(complex single-float) 'complex)) '(t t)))
(assert (equal (multiple-value-list (subtypep 'complex '(complex single-float))) '(nil t)))
(assert (null (typep #c(1.0 2.0) '(complex (float 0.0 1.0)))))
(assert (typep #c(1.0 2.0) '(complex single-float)))
(assert (typep #c(0.5 0.5) '(complex (float 0.0 1.0))))
(assert (null (typep #c(0.0 0.5) '(complex (float (0.0) 1.0)))))
(write-line "COMPLEX-SUBTYPE-BOUNDS-OK")
"#;

#[test]
fn complex_subtype_bounds_and_representation_equivalence() {
    let mut outputs = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", PROGRAM]);
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let result = command.output().expect("run complex subtype regression");
        assert!(
            result.status.success(),
            "stress={stress}: stdout={} stderr={}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("COMPLEX-SUBTYPE-BOUNDS-OK"));
        outputs.push(result.stdout);
    }
    assert_eq!(outputs[0], outputs[1]);
}
