// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A function argument dispatches to a FUNCTION method, never a LIST or CONS one.
//!
//! An interpreter closure is physically the cons `(EGCL::CLOSURE . id)`.
//! `builtin_type_specializer_distance` already keeps it out of CONS and LIST
//! (bliss-5ir), but it answers None both for a name it does not know and for
//! a name that does not apply, and the None sent `method_specificity_vector`
//! to its class-graph fallback, which asked `egcl_stdlib::class_of` -- the
//! representation, so CONS, whose precedence list holds LIST. A LIST method
//! then applied to every interpreted lambda and outranked the FUNCTION
//! method, while CLASS-OF, FUNCTIONP and TYPEP all said FUNCTION.
//!
//! cl-ppcre's BUILD-REPLACEMENT-TEMPLATE has string, function, symbol and
//! list methods, so its function method was skipped and the list method
//! returned the replacement function unwrapped: every REGEX-REPLACE with a
//! function replacement failed with "#<FUNCTION> is not of type LIST". That
//! stopped Gendl's web pages from rendering (QUOTE-SECTIONS, reached from a
//! base64 path encoding).
//!
//! Both evaluator tiers: direct forms and inside a DEFUN.

use std::process::Command;

const PROGRAM: &str = r#"
  (defgeneric brt (x))
  (defmethod brt ((x string)) :string)
  (defmethod brt ((x function)) :function)
  (defmethod brt ((x symbol)) :symbol)
  (defmethod brt ((x list)) :list)
  (defgeneric only-list (x))
  (defmethod only-list ((x list)) :list)
  (defgeneric only-cons (x))
  (defmethod only-cons ((x cons)) :cons)
  (defmacro report (label form)
    `(format t "~a ~s~%" ,label (handler-case ,form (error () :no-method))))

  (report "LAMBDA" (brt (lambda (y) y)))
  (report "CLOSURE" (let ((z 1)) (brt (lambda (y) (+ y z)))))
  (report "SHARP-QUOTE" (brt #'car))
  (report "LIST" (brt '(1 2)))
  (report "NIL" (brt nil))
  (report "STRING" (brt "s"))
  ;; A list-only or cons-only generic has no method for a function.
  (report "ONLY-LIST-LAMBDA" (only-list (lambda (y) y)))
  (report "ONLY-CONS-CLOSURE" (let ((z 1)) (only-cons (lambda (y) (+ y z)))))
  (report "ONLY-LIST-LIST" (only-list '(1)))
  ;; COMPUTE-APPLICABLE-METHODS agrees with the call.
  (report "CAM" (mapcar (lambda (m) (mapcar #'class-name (method-specializers m)))
                          (compute-applicable-methods #'brt (list (lambda (y) y)))))

  (defun tier2 (z)
    (list (brt (lambda (y) y))
          (brt (lambda (y) (+ y z)))
          (handler-case (only-list (lambda (y) y)) (error () :no-method))))
  (report "COMPILED" (tier2 1))

  ;; The shape cl-ppcre reaches: a function replacement for REGEX-REPLACE-ALL
  ;; goes through BUILD-REPLACEMENT-TEMPLATE's FUNCTION method, which wraps it.
  (defgeneric template (x))
  (defmethod template ((x string)) (list x))
  (defmethod template ((x function)) (list x))
  (defmethod template ((x list)) x)
  (report "TEMPLATE-WRAPS" (let ((f (lambda (m) m))) (eq (first (template f)) f)))
  (format t "CLOSURE-DISPATCH-OK~%")
"#;

#[test]
fn a_function_argument_dispatches_to_function_not_list() {
    let output = Command::new("timeout")
        .args(["--kill-after=10", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    for expected in [
        "LAMBDA :FUNCTION",
        "CLOSURE :FUNCTION",
        "SHARP-QUOTE :FUNCTION",
        "LIST :LIST",
        "NIL :SYMBOL",
        "STRING :STRING",
        "ONLY-LIST-LAMBDA :NO-METHOD",
        "ONLY-CONS-CLOSURE :NO-METHOD",
        "ONLY-LIST-LIST :LIST",
        "CAM ((FUNCTION))",
        "COMPILED (:FUNCTION :FUNCTION :NO-METHOD)",
        "TEMPLATE-WRAPS T",
        "CLOSURE-DISPATCH-OK",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in:\n{stdout}");
    }
}
