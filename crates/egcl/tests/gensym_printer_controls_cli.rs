// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

const PROGRAM: &str = r##"
(declaim (notinline native-gensym-default native-gensym-prefix native-gensym-number))
(defun native-gensym-default () (symbol-name (gensym)))
(defun native-gensym-prefix (prefix) (symbol-name (gensym prefix)))
(defun native-gensym-number (number) (symbol-name (gensym number)))
(defun check-gensym-counter (counter suffix next)
  (let ((*gensym-counter* counter))
    (assert (equal (native-gensym-default) (concatenate 'string "G" suffix)))
    (assert (= *gensym-counter* next)))
  (let ((*gensym-counter* counter))
    (assert (equal (native-gensym-prefix "PREFIX") (concatenate 'string "PREFIX" suffix)))
    (assert (= *gensym-counter* next)))
  (let ((*gensym-counter* counter))
    (assert (equal (native-gensym-number 71) "G71"))
    (assert (= *gensym-counter* counter))
    (assert (equal (native-gensym-number 1000000000000000000000000000000)
                   "G1000000000000000000000000000000"))
    (assert (= *gensym-counter* counter))))
(defun check-global-gensym-counter ()
  (let ((saved *gensym-counter*))
    (unwind-protect
        (progn (setq *gensym-counter* 71)
               (assert (equal (native-gensym-default) "G71"))
               (assert (= *gensym-counter* 72)))
      (setq *gensym-counter* saved))))
(defun gensym-type-error-p (thunk)
  (handler-case (progn (funcall thunk) nil) (type-error () t)))
(dotimes (i 80)
  (native-gensym-default) (native-gensym-prefix "WARM") (native-gensym-number 71))
(when *gensym-required-tier*
  (dolist (name '(native-gensym-default native-gensym-prefix native-gensym-number))
    (loop repeat 100 until (= *gensym-required-tier* (egcl-ext:function-tier name))
          do (native-gensym-default) (native-gensym-prefix "WARM")
             (native-gensym-number 71) (sleep 0.01))
    (assert (= *gensym-required-tier* (egcl-ext:function-tier name)))))
(let ((table (copy-pprint-dispatch nil)))
  ;; Internal integer-to-name conversion must never call this user printer.
  (set-pprint-dispatch 'integer
    (lambda (stream value) (declare (ignore stream value))
      (error "GENSYM entered an integer pretty printer")) 100 table)
  (dolist (base '(2 16 36))
    (dolist (radix '(nil t))
      (dolist (readably '(nil t))
        (dolist (pretty '(nil t))
          (let ((*print-base* base) (*print-radix* radix) (*print-readably* readably)
                (*print-pretty* pretty) (*print-pprint-dispatch* table))
            (check-gensym-counter 71 "71" 72)
            (check-gensym-counter 1000000000000000000000000000000
                                  "1000000000000000000000000000000"
                                  1000000000000000000000000000001)
            (check-global-gensym-counter)))))))
(assert (gensym-type-error-p (lambda () (gensym -1))))
(assert (gensym-type-error-p (lambda () (gensym '(bad)))))
(assert (gensym-type-error-p (lambda () (let ((*gensym-counter* -1)) (gensym)))))
(assert (gensym-type-error-p (lambda () (let ((*gensym-counter* -1)) (gensym "BAD")))))
;; Establish the handler before the invalid binding, and expand ASSERT outside
;; that binding: macro expansion may legitimately request its own gensyms.
;; An explicit integer suffix does not consult or alter the counter.
(assert (equal (let ((*gensym-counter* -1)) (list (native-gensym-number 71) *gensym-counter*))
               '("G71" -1)))
(write-line "GENSYM-PRINTER-CONTROLS-OK")
"##;

#[test]
fn gensym_decimal_names_do_not_enter_printer_callbacks() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_LAZY", "0")
            .env("EGCL_T2_THREADS", "1");
        for name in [
            "EGCL_GC_STRESS",
            "EGCL_GC_POISON",
            "EGCL_GC_STRESS_AFTER_INIT",
            "EGCL_GC_STRESS_SKIP",
            "EGCL_GC_STRESS_AT",
        ] {
            command.env_remove(name);
        }
        if stress {
            command
                .env("EGCL_GC_STRESS", "17")
                .env("EGCL_GC_POISON", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1");
        }
        let required = if tier == "t2" { "2" } else { "nil" };
        let program = format!("(defparameter *gensym-required-tier* {required}){PROGRAM}");
        let output = command
            .args(["--no-init", "--eval", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{tier} stress={stress}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "GENSYM-PRINTER-CONTROLS-OK")
                .count(),
            1
        );
    }
}
