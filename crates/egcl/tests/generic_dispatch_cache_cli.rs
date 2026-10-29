// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
;; The two first-hit orders from GH901 must produce identical classification.
(defgeneric gf901-first-function (object)
  (:method ((object standard-generic-function)) :sgf)
  (:method ((object function)) :function)
  (:method ((object t)) :other))
(defgeneric gf901-first-generic (object)
  (:method ((object standard-generic-function)) :sgf)
  (:method ((object function)) :function)
  (:method ((object t)) :other))
(defgeneric gf901-compiled (object)
  (:method ((object generic-function)) :sgf)
  (:method ((object compiled-function)) :compiled)
  (:method ((object t)) :other))
;; A short combination recomputes applicability; it is an independent control
;; for the chapter-wide function-to-nonfunction contamination in the report.
(defgeneric gf901-list (object) (:method-combination list))
(defmethod gf901-list list ((object standard-generic-function)) :sgf)
(defmethod gf901-list list ((object function)) :function)
(defmethod gf901-list list ((object cons)) :cons)
(defmethod gf901-list list ((object string)) :string)
(defmethod gf901-list list ((object vector)) :vector)
(defmethod gf901-list list ((object hash-table)) :hash)
(defmethod gf901-list list ((object t)) :other)
(defun gf901-native-probe (object)
  (list (gf901-first-function object)
        (gf901-first-generic object)
        (gf901-compiled object)
        (gf901-list object)))
(defparameter *gf901-generic* #'make-instance)
(defparameter *gf901-function* #'cons)
(assert (eq (gf901-first-function *gf901-function*) :function))
(assert (eq (gf901-first-function *gf901-generic*) :sgf))
(assert (eq (gf901-first-function *gf901-function*) :function))
(assert (eq (gf901-first-generic *gf901-generic*) :sgf))
(assert (eq (gf901-first-generic *gf901-function*) :function))
(defparameter *gf901-cases*
  (list (list *gf901-function* '(:function :function :compiled (:function :other)))
        (list *gf901-generic* '(:sgf :sgf :sgf (:sgf :function :other)))
        (list nil '(:other :other :other (:other)))
        (list t '(:other :other :other (:other)))
        (list 17 '(:other :other :other (:other)))
        (list (cons 1 2) '(:other :other :other (:cons :other)))
        (list "text" '(:other :other :other (:string :vector :other)))
        (list (vector 1 2) '(:other :other :other (:vector :other)))
        (list (make-hash-table) '(:other :other :other (:hash :other)))))
(dotimes (pass 20)
  (dolist (row *gf901-cases*)
    (assert (equal (gf901-native-probe (first row)) (second row))))
;; Classification belongs to the retained object, including a public alias
;; whose binding is subsequently replaced with a different callable family.
(setf (fdefinition 'gf901-alias) *gf901-generic*)
(assert (eq (gf901-first-function #'gf901-alias) :sgf))
(setf (fdefinition 'gf901-alias) *gf901-function*)
(assert (eq (gf901-first-function #'gf901-alias) :function))
(assert (eq (gf901-first-function *gf901-generic*) :sgf))
"#;

const NATIVE: &str = r#"
(loop repeat 200
  until (eql 2 (egcl-ext:function-tier 'gf901-native-probe))
  do (dolist (row *gf901-cases*) (gf901-native-probe (first row)))
     (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'gf901-native-probe)))
(dotimes (pass 20)
  (dolist (row *gf901-cases*)
    (assert (equal (gf901-native-probe (first row)) (second row)))))
"#;

#[test]
fn callable_dispatch_cache_preserves_semantic_families_and_nonfunction_controls() {
    let mut reference = None;
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t2", false),
        ("t2", true),
    ] {
        let program = format!(
            "{PROGRAM}\n{}\n(write-line \"GENERIC-DISPATCH-CACHE-OK\")",
            if tier == "t2" { NATIVE } else { "nil" }
        );
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command.args(["--no-init", "--eval", &program]);
        for name in [
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
            "EGCL_GC_DISABLE",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(name);
        }
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_LAZY_COMPILE", "1")
            .env("EGCL_LAZY_THRESHOLD", "10")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1");
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run callable dispatch cache regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier}, stress={stress}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("GENERIC-DISPATCH-CACHE-OK"),
            "{stdout}\n{stderr}"
        );
        if tier == "t2" {
            assert!(
                stderr
                    .lines()
                    .any(|line| line.contains("GF901-NATIVE-PROBE")
                        && line.contains("optimized worker result installed with ")),
                "the actual caller must install native T2: {stderr}"
            );
        }
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}
