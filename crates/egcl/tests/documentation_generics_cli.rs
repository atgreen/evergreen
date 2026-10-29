// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::{
    fs,
    path::Path,
    process::{Command, Output},
};

const LIBRARY: &str = r##"
(defclass documentation-target () ())
(defmethod documentation ((object documentation-target) (kind (eql 'function)))
  (let ((raw (call-next-method))) (and raw (concatenate 'string "read:" raw))))
(defmethod (setf documentation) (new (object documentation-target) (kind (eql 'function)))
  (call-next-method (and new (concatenate 'string "write:" new)) object kind))
(defclass documentation-metaclass (standard-class) ())
(defmethod egcl-mop:validate-superclass ((class documentation-metaclass) (super standard-class)) t)
(defclass documented-class () () (:metaclass documentation-metaclass))
(defmethod documentation ((object documentation-metaclass) (kind (eql t)))
  (let ((raw (call-next-method))) (and raw (concatenate 'string "meta:" raw))))
(defmethod (setf documentation) (new (object documentation-metaclass) (kind (eql t)))
  (call-next-method new object kind))
(defvar *documentation-order* nil)
(defvar *record-documentation-order* nil)
(defmethod documentation ((object documentation-target) (kind t))
  (when *record-documentation-order* (push :object *documentation-order*))
  (call-next-method))
(defmethod documentation ((object t) (kind (eql 'precedence-probe)))
  (when *record-documentation-order* (push :type *documentation-order*))
  (call-next-method))
(defmethod (setf documentation) ((new string) (object t) (kind t))
  (when *record-documentation-order* (push :new *documentation-order*))
  (call-next-method))
(defmethod (setf documentation) (new (object documentation-target) (kind t))
  (when *record-documentation-order* (push :object *documentation-order*))
  (call-next-method))
(defmethod (setf documentation) (new (object t) (kind (eql 'precedence-probe)))
  (when *record-documentation-order* (push :type *documentation-order*))
  (call-next-method))
(declaim (notinline native-documentation-read native-documentation-write))
(defun native-documentation-read (object kind) (documentation object kind))
(defun native-documentation-write (new object kind) (setf (documentation object kind) new))
(defun documentation-with-docstring () "Existing definition documentation." :ok)
;; Explicit load-time documentation tests persisted GF storage independently
;; of the compiler's existing choice to discard native DEFUN docstrings.
(setf (documentation 'documentation-with-docstring 'function)
      "Existing definition documentation.")
(defvar *documented-variable* 1 "Existing variable documentation.")
(defgeneric documentation-generic-identity-probe (object))
(defmethod documentation-generic-identity-probe ((object t)) :retained)
(defun documentation-program-error-p (thunk)
  (handler-case (progn (funcall thunk) nil) (program-error () t)))
(defun documentation-type-error-p (thunk)
  (handler-case (progn (funcall thunk) nil) (type-error () t)))
"##;

const CHECKS: &str = r##"
(defun source-documentation-with-docstring () "Fresh source documentation." :source)
(assert (equal (documentation 'source-documentation-with-docstring 'function)
               "Fresh source documentation."))
(assert (typep #'documentation 'generic-function))
(assert (typep #'documentation 'standard-generic-function))
(assert (typep (fdefinition '(setf documentation)) 'generic-function))
(assert (typep (fdefinition '(setf documentation)) 'standard-generic-function))
(dolist (signature '((function t) (function function) (list function) (list compiler-macro)
                     (symbol function) (symbol compiler-macro) (symbol setf)
                     (method-combination t) (method-combination method-combination)
                     (symbol method-combination) (standard-method t) (package t)
                     (standard-class t) (standard-class type) (structure-class t)
                     (structure-class type) (symbol type) (symbol structure) (symbol variable)))
  (let ((specializers (list (find-class (first signature))
                            (egcl-mop:intern-eql-specializer (second signature)))))
    (assert (find-method #'documentation nil specializers nil))
    (assert (find-method (fdefinition '(setf documentation)) nil
                         (cons (find-class t) specializers) nil))))
(assert (equal (documentation 'documentation-with-docstring 'function)
               "Existing definition documentation."))
(assert (equal (documentation '*documented-variable* 'variable)
               "Existing variable documentation."))
(assert (stringp (documentation 'egcl-ext:macroexpand-all 'function)))
(let ((getter #'documentation)
      (replacement #'list))
  (unwind-protect
       (progn
         (setf (fdefinition 'documentation) replacement)
         (assert (eq #'documentation replacement))
         (assert (eq (fdefinition 'documentation) replacement))
         (assert (not (typep #'documentation 'generic-function)))
         (assert (equal (documentation 'anything 'variable) '(anything variable)))
         (assert (equal (funcall #'documentation 'anything 'variable) '(anything variable)))
         (assert (typep getter 'generic-function))
         (assert (stringp (funcall getter 'documentation-with-docstring 'function)))
         (fmakunbound 'documentation)
         (assert (not (fboundp 'documentation)))
         (assert (handler-case (progn (fdefinition 'documentation) nil)
                   (undefined-function () t)))
         (assert (handler-case (progn (funcall 'documentation 'anything 'variable) nil)
                   (undefined-function () t)))
         (assert (typep getter 'generic-function))
         (assert (stringp (funcall getter 'documentation-with-docstring 'function))))
    (setf (fdefinition 'documentation) getter))
  (assert (eq #'documentation getter))
  (assert (typep #'documentation 'generic-function))
  (assert (find-method getter nil
                      (list (find-class 'symbol)
                            (egcl-mop:intern-eql-specializer 'function)) nil)))
(let ((saved #'documentation-generic-identity-probe))
  (assert (eq saved #'documentation-generic-identity-probe))
  (setf (fdefinition 'documentation-generic-identity-probe) saved)
  (assert (eq saved #'documentation-generic-identity-probe))
  (assert (eq (funcall saved nil) :retained))
  (assert (find-method saved nil (list (find-class t)) nil)))
(let ((getter #'documentation) (setter (fdefinition '(setf documentation))))
  (assert (equal (funcall setter "namespace" 'documentation-namespace 'variable) "namespace"))
  (assert (equal (funcall getter 'documentation-namespace 'variable) "namespace"))
  (assert (null (funcall setter nil 'documentation-namespace 'variable)))
  (assert (equal (multiple-value-list (funcall getter 'documentation-namespace 'variable)) '(nil))))
(let ((function #'documentation-with-docstring))
  (assert (equal (setf (documentation function t) "Function object documentation.")
                 "Function object documentation."))
  (assert (equal (documentation function 'function) "Function object documentation."))
  (setf (documentation function 'function) nil)
  (assert (null (documentation function t))))
(assert (documentation-program-error-p (lambda () (funcall #'documentation 'anything))))
(assert (documentation-program-error-p (lambda () (funcall #'documentation 'anything 'variable :extra))))
(assert (documentation-program-error-p (lambda () (funcall (fdefinition '(setf documentation)) "bad" 'anything))))
(assert (documentation-program-error-p (lambda () (funcall (fdefinition '(setf documentation)) "bad" 'anything 'variable :extra))))
(dolist (bad (list 42 #\a (list :not-a-string)))
  (assert (documentation-type-error-p
            (lambda () (funcall (fdefinition '(setf documentation)) bad 'documentation-bad 'variable)))))
(let ((target (make-instance 'documentation-target)) (class (find-class 'documented-class)))
  (assert (equal (native-documentation-write "value" target 'function) "value"))
  (assert (equal (native-documentation-read target 'function) "read:write:value"))
  (setf (documentation target 'function) nil)
  (assert (null (documentation target 'function)))
  (assert (equal (setf (documentation class t) "class documentation") "class documentation"))
  (assert (equal (documentation class t) "meta:class documentation"))
  (dotimes (i 80)
    (native-documentation-write "warm" target 'function)
    (native-documentation-read target 'function))
  (when *documentation-required-tier*
    (dolist (name '(native-documentation-read native-documentation-write))
      (loop repeat 100 until (= *documentation-required-tier* (egcl-ext:function-tier name))
            do (native-documentation-write "warm" target 'function)
               (native-documentation-read target 'function) (sleep 0.01))
      (assert (= *documentation-required-tier* (egcl-ext:function-tier name)))))
  (let ((text (make-string 200 :initial-element #\a)))
    (assert (eq (native-documentation-write text 'documentation-allocating 'variable) text))
    (dotimes (i 100) (make-list 20 :initial-element i))
    (assert (eq (native-documentation-read 'documentation-allocating 'variable) text))
    (setf (char text 0) #\z)
    (assert (char= (char (documentation 'documentation-allocating 'variable) 0) #\z)))
  (let ((text (make-string 200 :initial-element #\b)))
    (native-documentation-write text target 'function)
    (dotimes (i 100) (make-list 20 :initial-element i))
    (assert (equal (native-documentation-read target 'function)
                   (concatenate 'string "read:write:" text))))
  ;; DOC-TYPE is most significant, OBJECT next, and the setter NEW last.
  (let ((*record-documentation-order* t) (*documentation-order* nil))
    (native-documentation-write "precedence" target 'precedence-probe)
    (assert (equal (reverse *documentation-order*) '(:type :object :new)))
    (setq *documentation-order* nil)
    (assert (equal (native-documentation-read target 'precedence-probe) "precedence"))
    (assert (equal (reverse *documentation-order*) '(:type :object)))))
(write-line "DOCUMENTATION-GENERICS-OK")
"##;

fn invoke(program: &str, tier: &str, stress: bool, image: Option<&Path>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.arg("--no-init");
    if let Some(image) = image {
        command.arg("--image").arg(image);
    }
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
    command.args(["--eval", program]).output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn documentation_generic_methods_extend_storage_in_interpreted_and_native_calls() {
    for (tier, stress) in [("interp", false), ("t2", false), ("t2", true)] {
        let required = if tier == "t2" { "2" } else { "nil" };
        let program =
            format!("(defparameter *documentation-required-tier* {required}){LIBRARY}{CHECKS}");
        let output = invoke(&program, tier, stress, None);
        success(&output);
        assert_eq!(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| line.trim() == "DOCUMENTATION-GENERICS-OK")
                .count(),
            1
        );
    }
}

#[test]
fn documentation_generic_methods_survive_source_free_load_and_fresh_image() {
    let dir = std::env::temp_dir().join(format!(
        "egcl-documentation-generics-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).unwrap();
    let source = dir.join("documentation.lisp");
    let fasl = dir.join("documentation.bfasl");
    let image = dir.join("documentation.core");
    fs::write(&source, LIBRARY).unwrap();
    success(&invoke(
        &format!("(compile-file {source:?} :output-file {fasl:?})"),
        "interp",
        false,
        None,
    ));
    let bytes = fs::read(&fasl).unwrap();
    let unit = egcl_rt::bfasl::load(&bytes).expect("valid compiled documentation library");
    assert!(
        unit.section(egcl_rt::bfasl::section::TOPLEVEL_FORMS).is_none(),
        "source-form fallback must not mask portable method loading"
    );
    assert!(
        unit.section(egcl_rt::bfasl::section::BYTECODE_UNIT)
            .is_some_and(|body| body.starts_with(b"BBU\0")),
        "expected portable bytecode definitions"
    );
    fs::remove_file(&source).unwrap();
    for stress in [false, true] {
        success(&invoke(
            &format!("(load {fasl:?})(defparameter *documentation-required-tier* 2){CHECKS}"),
            "t2",
            stress,
            None,
        ));
    }
    success(&invoke(
        &format!("(load {fasl:?})(egcl-ext:save-lisp-and-die {image:?})"),
        "interp",
        false,
        None,
    ));
    for stress in [false, true] {
        success(&invoke(
            &format!("(defparameter *documentation-required-tier* 2){CHECKS}"),
            "t2",
            stress,
            Some(&image),
        ));
    }
    fs::remove_dir_all(dir).unwrap();
}
