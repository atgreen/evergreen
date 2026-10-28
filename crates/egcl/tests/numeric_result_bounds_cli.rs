// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use std::process::Command;

const SOURCE: &str = r#"
(declaim (notinline bounded-result-add bounded-result-sub bounded-result-mul))
;; TOP-declared IF joins hold bounded fixnums, while every arithmetic result
;; is outside the signed61 fixnum range. Warm the bypass branch only, so no
;; lower-tier arithmetic samples can repair a doomed cold fixnum guess.
(defun bounded-result-add (flag arithmetic)
  (let ((n (if flag 1152921504606846975 1152921504606846974)))
    (if arithmetic (let ((result (+ n 2))) result) n)))
(defun bounded-result-sub (flag arithmetic)
  (let ((n (if flag -1152921504606846976 -1152921504606846975)))
    (if arithmetic (let ((result (- n 2))) result) n)))
(defun bounded-result-mul (flag arithmetic)
  (let ((n (if flag 1152921504606846975 1152921504606846974)))
    (if arithmetic (let ((result (* n 2))) result) n)))
(defun bounded-result-warm ()
  (dolist (flag '(nil t))
    (bounded-result-add flag nil)
    (bounded-result-sub flag nil)
    (bounded-result-mul flag nil)))
(dotimes (i 60) (bounded-result-warm))
"#;

const READY: &str = r#"
(loop repeat 100
      until (and (= 2 (egcl-ext:function-tier 'bounded-result-add))
                 (= 2 (egcl-ext:function-tier 'bounded-result-sub))
                 (= 2 (egcl-ext:function-tier 'bounded-result-mul)))
      do (bounded-result-warm) (sleep 0.01))
(assert (= 2 (egcl-ext:function-tier 'bounded-result-add)))
(assert (= 2 (egcl-ext:function-tier 'bounded-result-sub)))
(assert (= 2 (egcl-ext:function-tier 'bounded-result-mul)))
;; The window includes only the first successful arithmetic calls. Assertions
;; on their heap integer results run afterwards, avoiding unrelated counters.
(let* ((before (egcl-ext:deopt-count))
       (add (bounded-result-add t t))
       (sub (bounded-result-sub t t))
       (mul (bounded-result-mul t t))
       (after (egcl-ext:deopt-count)))
  (assert (= before after))
  (assert (= add 1152921504606846977))
  (assert (= sub -1152921504606846978))
  (assert (= mul 2305843009213693950)))
"#;

const CHECK: &str = r#"
(dolist (flag '(nil t))
  (let ((add (multiple-value-list (bounded-result-add flag t)))
        (sub (multiple-value-list (bounded-result-sub flag t)))
        (mul (multiple-value-list (bounded-result-mul flag t))))
    (assert (= (length add) 1))
    (assert (= (length sub) 1))
    (assert (= (length mul) 1))
    (assert (integerp (car add)))
    (assert (integerp (car sub)))
    (assert (integerp (car mul)))
    (assert (not (typep (car add) 'fixnum)))
    (assert (not (typep (car sub) 'fixnum)))
    (assert (not (typep (car mul) 'fixnum)))
    (assert (= (car add) (if flag 1152921504606846977 1152921504606846976)))
    (assert (= (car sub) (if flag -1152921504606846978 -1152921504606846977)))
    (assert (= (car mul) (if flag 2305843009213693950 2305843009213693948)))
    (format t "BOUNDED-RESULT ~S ~S ~S ~S~%" flag add sub mul)))
(format t "NUMERIC-RESULT-BOUNDS-OK~%")
"#;

#[test]
fn guaranteed_bignum_results_stay_generic_without_first_call_overflow_deopts() {
    let mut reference: Option<String> = None;
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let ready = if tier == "t2" { READY } else { "" };
        let program = format!("{SOURCE}\n{ready}\n{CHECK}");
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .env_remove("EGCL_PROFILING_DISABLED")
            .env_remove("EGCL_T2_NO_QUEUE")
            .env_remove("EGCL_GC_STRESS")
            .env_remove("EGCL_GC_STRESS_SKIP")
            .env_remove("EGCL_GC_STRESS_AT")
            .env_remove("EGCL_GC_STRESS_AFTER_INIT")
            .env_remove("EGCL_GC_POISON")
            .args(["--no-init", "--eval", &program]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command
            .output()
            .expect("run bounded numeric result regression");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "tier={tier} stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("NUMERIC-RESULT-BOUNDS-OK"));
        if tier == "t2" {
            for name in [
                "BOUNDED-RESULT-ADD",
                "BOUNDED-RESULT-SUB",
                "BOUNDED-RESULT-MUL",
            ] {
                assert!(
                    stderr.lines().any(|line| line
                        .contains(&format!("{name}: optimized worker result installed with "))
                        && (line.contains("ordinary x86-64 Lisp T2 ABI")
                            || line.contains("mapped T2 ABI"))),
                    "{name} must use an installed native T2 body before first arithmetic\n{stderr}"
                );
                assert!(
                    stderr
                        .lines()
                        .any(|line| line.contains(&format!("{name}: 0 speculatable sites"))),
                    "guaranteed outside-fixnum result must veto the cold family\n{stderr}"
                );
                assert!(
                    !stderr
                        .lines()
                        .any(|line| line.contains(&format!("{name}: speculated "))),
                    "no doomed guarded fixnum version may precede a generic recompile\n{stderr}"
                );
            }
        }
        if let Some(expected) = &reference {
            assert_eq!(stdout.as_ref(), expected.as_str());
        } else {
            reference = Some(stdout.into_owned());
        }
    }
}
