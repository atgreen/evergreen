// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! HANDLER-CASE and HANDLER-BIND clauses accept any type specifier (CLHS 9.2),
//! so a compound one such as `(or error storage-condition)` must match exactly
//! when TYPEP would (GH #532). A nested HANDLER-CASE that does not handle the
//! condition must also leave it intact for the enclosing one, which used to
//! receive NIL instead.
use std::process::{Command, Output};

fn egcl(dir: &std::path::Path, program: &str) -> Output {
    Command::new("timeout")
        .current_dir(dir)
        .args([
            "--kill-after=5",
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .expect("run bounded EGCL")
}

fn assert_ok(output: &Output, marker: &str) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains(marker),
        "stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("compound-handler-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// `(check FORM EXPECTED)` evaluates FORM under a guard that reports any
/// condition escaping it as `(:ESCAPED type)`, and exits 1 on a mismatch.
const CHECK_MACRO: &str = r#"
(defmacro check (form expected)
  `(let ((got (block guard
                (handler-bind ((condition (lambda (c) (return-from guard (list :escaped (type-of c))))))
                  ,form))))
     (unless (equal got ',expected)
       (format t "~&MISMATCH ~s => ~s, expected ~s~%" ',form got ',expected)
       (egcl::%exit 1))))
"#;

#[test]
fn compound_handler_types_match_in_toplevel_forms() {
    let dir = scratch("toplevel");
    let program = format!(
        r#"(progn {CHECK_MACRO}
  (check (handler-case (error "x") ((or error storage-condition) (e) (list :caught (type-of e))))
         (:caught simple-error))
  (check (handler-case (error "x") ((and error (not warning)) (e) (list :caught (type-of e))))
         (:caught simple-error))
  (check (handler-case (warn "w") ((or error warning) (e) (list :caught (type-of e))))
         (:caught simple-warning))
  (check (handler-case (car (identity 5)) ((or type-error storage-condition) (e) (list :caught (type-of e))))
         (:caught type-error))
  (check (block hb (handler-bind (((or error storage-condition)
                                   (lambda (e) (return-from hb (list :caught (type-of e))))))
                     (error "x")))
         (:caught simple-error))
  (check (block hb (handler-bind (((or type-error warning)
                                   (lambda (e) (return-from hb (list :caught (type-of e))))))
                     (car (identity 5))))
         (:caught type-error))
  (check (block hb (handler-bind (((and error (not simple-error))
                                   (lambda (e) (return-from hb (list :wrong (type-of e))))))
                     (error "x")))
         (:escaped simple-error))
  ;; The enclosing HANDLER-CASE receives the condition, not NIL.
  (check (handler-case (handler-case (error "x")
                         ((or error storage-condition) (e) (list :inner (type-of e))))
           (error (e) (list :outer (type-of e) e)))
         (:inner simple-error))
  (check (handler-case (handler-case (error "x") ((or warning storage-condition) () :wrong))
           (error (e) (list :outer (type-of e))))
         (:outer simple-error))
  (check (handler-case (multiple-value-list
                        (handler-case (error "x") ((or warning storage-condition) () :wrong)))
           (error (e) (list :outer (type-of e))))
         (:outer simple-error))
  (check (funcall (compile nil '(lambda ()
                                  (handler-case (car (identity 5))
                                    ((and error (not warning)) (e) (list :caught (type-of e)))))))
         (:caught type-error))
  (check (funcall (compile nil '(lambda ()
                                  (handler-case (handler-case (error "x") ((or warning storage-condition) () :wrong))
                                    (error (e) (list :outer (type-of e) (null e)))))))
         (:outer simple-error nil))
  (format t "COMPOUND-HANDLERS-OK~%"))"#
    );
    assert_ok(&egcl(&dir, &program), "COMPOUND-HANDLERS-OK");
}

/// Function definitions whose compound clause types use no macro operator, so
/// they reach the handler machinery as written in DEFUN, COMPILE and
/// COMPILE-FILE alike.
const DEFINITIONS: &str = r#"
(defun error-but-not-warning-p (c) (and (typep c 'error) (not (typep c 'warning))))
(defun hc-not () (handler-case (error "x") ((not warning) (e) (list :caught (type-of e)))))
(defun hc-satisfies () (handler-case (car (identity 5))
                         ((satisfies error-but-not-warning-p) (e) (list :caught (type-of e)))))
(defun hc-nested-miss () (handler-case (handler-case (error "x") ((not error) () :wrong))
                           (error (e) (list :outer (type-of e) (null e)))))
(defun hb-not () (block hb (handler-bind (((not warning) (lambda (e) (return-from hb (list :caught (type-of e))))))
                             (error "x"))))
(defun hb-miss () (block hb (handler-bind (((not error) (lambda (e) (return-from hb (list :wrong (type-of e))))))
                              (error "x"))))
"#;

const FUNCTION_CHECKS: &str = r#"
(check (hc-not) (:caught simple-error))
(check (hc-satisfies) (:caught type-error))
(check (hc-nested-miss) (:outer simple-error nil))
(check (hb-not) (:caught simple-error))
(check (hb-miss) (:escaped simple-error))
(format t "COMPOUND-HANDLER-FUNCTIONS-OK~%")
"#;

#[test]
fn compound_handler_types_match_in_defined_and_compiled_functions() {
    let dir = scratch("compiled");
    let interpreted = format!("(progn {CHECK_MACRO} {DEFINITIONS} {FUNCTION_CHECKS})");
    assert_ok(&egcl(&dir, &interpreted), "COMPOUND-HANDLER-FUNCTIONS-OK");
    let compiled = format!(
        "(progn {CHECK_MACRO} {DEFINITIONS}
           (dolist (f '(hc-not hc-satisfies hc-nested-miss hb-not hb-miss)) (compile f))
           {FUNCTION_CHECKS})"
    );
    assert_ok(&egcl(&dir, &compiled), "COMPOUND-HANDLER-FUNCTIONS-OK");
}

#[test]
fn compound_handler_types_match_after_compile_file() {
    let dir = scratch("fasl");
    std::fs::write(dir.join("handlers.lisp"), DEFINITIONS).unwrap();
    let compiled = egcl(
        &dir,
        r#"(progn (compile-file "handlers.lisp") (format t "COMPILED~%"))"#,
    );
    assert_ok(&compiled, "COMPILED");
    let program = format!("(progn {CHECK_MACRO} (load \"handlers.fasl\") {FUNCTION_CHECKS})");
    assert_ok(&egcl(&dir, &program), "COMPOUND-HANDLER-FUNCTIONS-OK");
}
