// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::process::Command;

const PROGRAM: &str = r#"
(defgeneric gf801 (object))
(defmethod gf801 (object) object)
(defparameter *gf801* #'gf801)
(defun gf801-ordinary (object) object)
(defparameter *gf801-source* (eval '(lambda (object) object)))
(deftype gf801-standard-callable () '(and function standard-object (not compiled-function)))
(deftype gf801-compiled () 'compiled-function)
(defun gf801-native-probe (function)
  (list (typep function 'standard-object)
        (typep function 'compiled-function)
        (compiled-function-p function)
        (typep function (find-class 'standard-object))
        (typep function (find-class 'generic-function))
        (typep function (find-class 'standard-generic-function))))
(defun gf801-the-standard (function) (the standard-object function))
;; THE is a declaration and compiled code may trust it. CHECK-TYPE supplies
;; the explicit runtime error contract exercised in every evaluator tier.
(defun gf801-require-compiled (function)
  (check-type function compiled-function)
  function)
(assert (equal (gf801-native-probe *gf801*) '(t nil nil t t t)))
(assert (eq (class-of *gf801*) (find-class 'standard-generic-function)))
(assert (eq (type-of *gf801*) 'standard-generic-function))
(assert (equal (multiple-value-list (subtypep 'generic-function 'standard-object)) '(t t)))
(assert (equal (multiple-value-list (subtypep 'standard-generic-function 'standard-object)) '(t t)))
(assert (equal (multiple-value-list (subtypep (type-of *gf801*) 'compiled-function)) '(nil t)))
(assert (typep *gf801* 'gf801-standard-callable))
(assert (not (typep *gf801* 'gf801-compiled)))
(assert (typep *gf801* '(and function (not compiled-function))))
(assert (eq *gf801* (gf801-the-standard *gf801*)))
(assert (handler-case (progn (gf801-require-compiled *gf801*) nil)
          (type-error (condition)
            (and (eq (type-error-datum condition) *gf801*)
                 (eq (type-error-expected-type condition) 'compiled-function)))))
(dolist (type '(function standard-object generic-function standard-generic-function compiled-function))
  (multiple-value-bind (answer known) (subtypep (list 'member *gf801*) type)
    (assert known)
    (assert (eql answer (typep *gf801* type)))))
;; Native and source function controls retain their ordinary callable types.
(dolist (function (list #'gf801-ordinary *gf801-source* #'car))
  (assert (functionp function))
  (assert (typep function 'function))
  (assert (not (typep function 'standard-object)))
  (assert (not (typep function (find-class 'generic-function))))
  (assert (eql (compiled-function-p function) (typep function 'compiled-function)))
  (when (compiled-function-p function)
    (assert (eq function (gf801-require-compiled function)))
    (assert (equal (multiple-value-list (subtypep (type-of function) 'compiled-function)) '(t t)))))
;; A callable CLOS object keeps its real user class, instead of being refined
;; to FUNCTION just because it has an installed function.
(defclass gf801-callable () () (:metaclass egcl-ext:funcallable-standard-class))
(defparameter *gf801-callable* (make-instance 'gf801-callable))
(egcl-ext:set-funcallable-instance-function *gf801-callable* #'gf801-ordinary)
(assert (eq (class-of *gf801-callable*) (find-class 'gf801-callable)))
(assert (eq (type-of *gf801-callable*) 'gf801-callable))
(assert (typep *gf801-callable* 'gf801-callable))
(assert (typep *gf801-callable* (find-class 'gf801-callable)))
(assert (typep *gf801-callable* 'standard-object))
(assert (not (typep *gf801-callable* 'compiled-function)))
(assert (not (compiled-function-p *gf801-callable*)))
(assert (eq :ok (funcall *gf801-callable* :ok)))
(assert (not (functionp '(gf801))))
(assert (not (typep '(gf801) 'compiled-function)))
;; Classification follows the retained callable, not whatever its old public
;; function cell happens to contain after replacement.
(let ((replacement #'gf801-ordinary))
  (setf (fdefinition 'gf801) replacement)
  (assert (typep *gf801* 'standard-generic-function))
  (assert (not (compiled-function-p *gf801*)))
  (assert (not (typep (fdefinition 'gf801) 'generic-function)))
  (assert (compiled-function-p (fdefinition 'gf801)))
  (setf (fdefinition 'gf801) *gf801*))
"#;

const NATIVE: &str = r#"
(dotimes (i 120)
  (assert (equal (gf801-native-probe *gf801*) '(t nil nil t t t)))
  (gf801-ordinary i))
(loop repeat 200
  until (and (eql 2 (egcl-ext:function-tier 'gf801-native-probe))
             (eql 2 (egcl-ext:function-tier 'gf801-ordinary)))
  do (gf801-native-probe *gf801*) (gf801-ordinary 7) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'gf801-native-probe)))
(assert (= 2 (egcl-ext:function-tier 'gf801-ordinary)))
(assert (equal (gf801-native-probe *gf801*) '(t nil nil t t t)))
(assert (compiled-function-p #'gf801-ordinary))
"#;

#[test]
fn generic_function_type_membership_agrees_across_types_classes_and_native_calls() {
    let mut reference = None;
    for (tier, stress) in [
        ("interp", false),
        ("t0", false),
        ("t2", false),
        ("t2", true),
    ] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        let program = format!(
            "{PROGRAM}\n{}\n(format t \"GENERIC-FUNCTION-TYPES-OK~%\")",
            if tier == "t2" { NATIVE } else { "nil" }
        );
        command
            .args(["--no-init", "--eval", &program])
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LOG", "stderr")
            .env("EGCL_LAZY_COMPILE", "1")
            .env("EGCL_LAZY_THRESHOLD", "10")
            .env("EGCL_T1_T2_INVOKE_THRESHOLD", "10")
            .env("EGCL_T2_THREADS", "1");
        for key in [
            "EGCL_PROFILING_DISABLED",
            "EGCL_T2_NO_QUEUE",
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(key);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run generic function type regression");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier}, stress={stress}: {stdout}\n{stderr}"
        );
        assert!(
            stdout.contains("GENERIC-FUNCTION-TYPES-OK"),
            "{stdout}\n{stderr}"
        );
        if tier == "t2" {
            for name in ["GF801-NATIVE-PROBE", "GF801-ORDINARY"] {
                assert!(
                    stderr.lines().any(|line| line.contains(name)
                        && line.contains("optimized worker result installed with ")),
                    "actual native installation required for {name}: {stderr}"
                );
            }
        }
        if let Some(expected) = &reference {
            assert_eq!(&stdout, expected);
        } else {
            reference = Some(stdout);
        }
    }
}
