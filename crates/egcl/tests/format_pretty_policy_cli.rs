// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline policy-format policy-formatter))
(defun policy-format (control args) (apply #'format nil control args))
(defun policy-formatter ()
  (with-output-to-string (s) (funcall (formatter "A ~:@_A ~:@_A") s)))
(defun policy-column (stream object colon at &rest params)
  (declare (ignore object colon at params))
  (format stream "~3,0T!"))
(dotimes (i 80) (policy-format "A ~:@_A" nil) (policy-formatter))
(when *policy-required-tier*
  (dolist (name '(policy-format policy-formatter))
    (loop repeat 100 until (= *policy-required-tier* (egcl-ext:function-tier name))
          do (if (eq name 'policy-format) (policy-format "A" nil) (policy-formatter)) (sleep 0.01))
    (assert (= *policy-required-tier* (egcl-ext:function-tier name)))))
(with-standard-io-syntax
  (let ((*print-readably* nil) (*print-escape* nil) (*print-right-margin* 100)
        (*print-miser-width* nil))
    (dolist (pretty '(nil t))
      (let ((*print-pretty* pretty))
        (assert (equal (policy-format "A ~:@_A ~:@_A" nil) "A A A"))
        (assert (equal (policy-formatter) "A A A"))
        (assert (equal (policy-format "A ~_A ~:_A ~@_A" nil) "A A A A"))
        (assert (equal (policy-format "XX~10:TYY" nil) "XXYY"))
        (assert (equal (policy-format "XX~10:@TYY" nil) "XXYY"))
        (assert (equal (policy-format "XX~v,v:T~A" '(10 0 "YY")) "XXYY"))
        (assert (equal (policy-format "~?" '("λ~v,v:T~A" (10 0 "終"))) "λ終"))
        (assert (equal (policy-format "XX~5,0TYY" nil) "XX   YY"))
        (assert (equal (policy-format "XX~3,0@TYY" nil) "XX   YY"))
        (assert (equal (policy-format "A~%B" nil) (format nil "A~CB" #\Newline)))))
    (let ((*print-pretty* t))
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil)
                         (let ((*print-pretty* nil))
                           (format s "XX~v,v:T~A~3,0:@T~:@_Z" 10 0 "YY")))) "XXYYZ"))
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil)
                         (let ((*print-pretty* nil))
                           (write-string "XX" s) (pprint-tab :section 10 0 s)
                           (pprint-newline :mandatory s) (write-string "YY" s)))) "XXYY"))
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil) (format s "XX~10,0:TYY")))
                     "XX        YY"))
      ;; Callback stream origin must observe the mandatory newline while the
      ;; direct logical body has not yet been wrapped in its final Block event.
      (assert (equal (policy-format "~<A~:@_~/policy-column/~:>" '((nil)))
                     (format nil "A~C   !" #\Newline)))
      ;; Bare FORMAT controls on an existing Lisp pretty stream need the
      ;; destination's logical context for callbacks as well.
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil)
                         (format s "A~:@_~/policy-column/" nil)))
                     (format nil "A~C   !" #\Newline)))
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil)
                         (funcall (formatter "A~:@_~/policy-column/") s nil)))
                     (format nil "A~C   !" #\Newline)))
      (assert (equal (policy-format "~<~(A~:@_~/policy-column/~)~:>" '((nil)))
                     (format nil "a~C   !" #\Newline)))
      (assert (equal (policy-format "~<A ~:@_A~:>" '(nil))
                     (format nil "A~CA" #\Newline)))
      (assert (equal (with-output-to-string (s)
                       (pprint-logical-block (s nil)
                         (let ((*print-pretty* nil)) (format s "A~%B"))))
                     (format nil "A~CB" #\Newline))))))
(write-line "FORMAT-PRETTY-POLICY-OK")
"##;

#[test]
fn pretty_directives_observe_dynamic_policy_and_stream_context() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_STRESS_AT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_POISON",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let required = if tier == "t2" { "2" } else { "nil" };
        let output = command
            .args([
                "--no-init",
                "--eval",
                &format!("(defparameter *policy-required-tier* {required}){PROGRAM}"),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line == "FORMAT-PRETTY-POLICY-OK")
        );
    }
}
