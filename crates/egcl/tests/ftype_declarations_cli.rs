// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Global FTYPE metadata must survive the real bootstrap and compile/load phases.
use std::{fs, process::Command};

fn run(source: &str) -> String {
    run_with_env(source, &[])
}

fn run_with_env(source: &str, variables: &[(&str, &str)]) -> String {
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", source])
        .envs(variables.iter().copied())
        .output()
        .expect("run FTYPE declaration regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

#[test]
fn boot_retains_ftype_for_multiple_names_and_distinct_setf_functions() {
    let output = run(r#"
        (assert (equal
          (macroexpand-1 '(declaim (ftype (function () fixnum) retained-result-a)))
          '(progn (eval-when (:compile-toplevel :load-toplevel :execute)
                    (proclaim '(ftype (function () fixnum) retained-result-a))))))
        (declaim (ftype (function () fixnum) retained-result-a retained-result-b))
        (assert (equal (get 'retained-result-a 'egcl-internal::function-type)
                       '(function () fixnum)))
        (assert (equal (get 'retained-result-b 'egcl-internal::function-type)
                       '(function () fixnum)))
        (proclaim '(ftype (function (t t) t) (setf retained-result-a)))
        (assert (equal (get 'retained-result-a 'egcl-internal::setf-function-type)
                       '(function (t t) t)))
        (assert (equal (get 'retained-result-a 'egcl-internal::function-type)
                       '(function () fixnum)))
        (proclaim '(ftype (function () (values fixnum &optional)) retained-result-b))
        (assert (equal (get 'retained-result-b 'egcl-internal::function-type)
                       '(function () (values fixnum &optional))))
        (assert (null (proclaim '(ftype function retained-unrestricted))))
        (assert (eq (get 'retained-unrestricted 'egcl-internal::function-type)
                    'function))
        (defun retained-result-a () (values 7 8))
        (assert (equal (multiple-value-list (retained-result-a)) '(7 8)))
        (format t "FTYPE-RETAINED~%")
    "#);
    assert!(output.contains("FTYPE-RETAINED"));
}

#[test]
fn shadowed_primitive_result_types_do_not_supply_builtin_facts() {
    let output = run(r#"
        (defpackage :result-types (:use :cl) (:shadow :fixnum :single-float :integer))
        (in-package :result-types)
        (deftype fixnum () 'string)
        (deftype single-float () 'string)
        (deftype integer (low high) (declare (ignore low high)) 'string)
        (declaim (ftype (function () fixnum) shadow-fixnum))
        (declaim (ftype (function () single-float) shadow-float))
        (declaim (ftype (function () (integer 0 10)) shadow-range))
        (defun shadow-fixnum () "fixnum")
        (defun shadow-float () "float")
        (defun shadow-range () "range")
        (assert (equal (shadow-fixnum) "fixnum"))
        (assert (equal (shadow-float) "float"))
        (assert (equal (shadow-range) "range"))
        (compile 'shadow-fixnum)
        (compile 'shadow-float)
        (compile 'shadow-range)
        (assert (equal (shadow-fixnum) "fixnum"))
        (assert (equal (shadow-float) "float"))
        (assert (equal (shadow-range) "range"))
        (format t "SHADOWED-RESULT-TYPES~%")
    "#);
    assert!(output.contains("SHADOWED-RESULT-TYPES"));
}

#[test]
fn checked_results_preserve_deep_self_tail_calls_and_multiple_values() {
    let output = run(r#"
        (declaim (optimize (speed 3) (safety 0)))
        (declaim (ftype (function (t t) (integer 0 10)) checked-tail-result))
        (defun checked-tail-result (n value)
          (if (zerop n) (values value :secondary)
              (checked-tail-result (1- n) value)))
        (compile 'checked-tail-result)
        (dotimes (i 500) (checked-tail-result 3 7))
        (assert (equal (multiple-value-list (checked-tail-result 100000 7))
                       '(7 :secondary)))
        (assert (handler-case (progn (checked-tail-result 100000 11) nil)
                  (type-error (condition) (= (type-error-datum condition) 11))))
        (format t "CHECKED-TAIL-RESULT~%")
    "#);
    assert!(output.contains("CHECKED-TAIL-RESULT"));
}

#[test]
fn checked_tail_calls_cannot_skip_parameter_contracts() {
    let source = r#"
        (declaim (optimize (speed 3) (safety 0)))
        (declaim (ftype (function (t) fixnum) checked-tail-argument))
        (defun checked-tail-argument (x)
          (declare (type (integer 0 1) x))
          (if (= x 0) (checked-tail-argument 2) 7))
        (compile 'checked-tail-argument)
        (assert (= (checked-tail-argument 1) 7))
        (assert (handler-case (progn (checked-tail-argument 2) nil)
                  (type-error (condition) (= (type-error-datum condition) 2))))
        (assert (handler-case (progn (checked-tail-argument 0) nil)
                  (type-error (condition) (= (type-error-datum condition) 2))))
        (dotimes (i 500) (checked-tail-argument 1))
        (format t "CHECKED-TAIL-TIER=~D~%"
          (egcl-ext:function-tier 'checked-tail-argument))
        (assert (handler-case (progn (checked-tail-argument 2) nil)
                  (type-error (condition) (= (type-error-datum condition) 2))))
        (assert (handler-case (progn (checked-tail-argument 0) nil)
                  (type-error (condition) (= (type-error-datum condition) 2))))
        (format t "CHECKED-TAIL-ARGUMENT~%")
    "#;
    for variables in [
        vec![],
        vec![("EGCL_FORCE_TIER", "t1"), ("EGCL_TCO_DISABLE", "1")],
    ] {
        let output = run_with_env(source, &variables);
        assert!(output.contains("CHECKED-TAIL-ARGUMENT"));
        // The default rewrite may decline native compilation for its always-
        // failing constant argument guard. The separate deep typed-tail test
        // requires native frame reuse; this mode must test mapped recursion.
        if !variables.is_empty() {
            assert!(output.contains("CHECKED-TAIL-TIER=1"), "{output}");
        }
    }
}

#[test]
fn local_native_and_closure_fallback_preserve_parameter_contracts() {
    let output = run(r#"
        (defun local-checked-argument (x)
          (flet ((inner (value)
                   (declare (type (integer 0 1) value))
                   value))
            (inner x)))
        (defun closure-checked-argument (x)
          (funcall (lambda (value)
                     (declare (type (integer 0 1) value))
                     value) x))
        (dolist (function '(local-checked-argument closure-checked-argument))
          (assert (= (funcall function 1) 1))
          (assert (handler-case (progn (funcall function 2) nil)
                    (type-error (condition) (= (type-error-datum condition) 2))))
          (dotimes (i 500) (funcall function 1))
          (when (eq function 'local-checked-argument)
            (assert (> (egcl-ext:function-tier function) 0)))
          (assert (handler-case (progn (funcall function 2) nil)
                    (type-error (condition) (= (type-error-datum condition) 2)))))
        (format t "LOCAL-CLOSURE-ARGUMENT-CHECKS~%")
    "#);
    assert!(output.contains("LOCAL-CLOSURE-ARGUMENT-CHECKS"));
}

#[test]
fn typed_self_tail_calls_check_arguments_before_frame_reuse() {
    let output = run(r#"
        (declaim (ftype (function (fixnum fixnum) fixnum) typed-count))
        (defun typed-count (n acc)
          (declare (type (integer 0 100000) n acc))
          (if (zerop n) (values acc :secondary)
              (typed-count (1- n) (1+ acc))))
        (dotimes (i 500) (typed-count 3 0))
        (assert (> (egcl-ext:function-tier 'typed-count) 0))
        (assert (equal (multiple-value-list (typed-count 100000 0))
                       '(100000 :secondary)))
        (assert (handler-case (progn (typed-count 1 100000) nil)
                  (type-error (c) (= 100001 (type-error-datum c)))))
        (defun typed-underflow (n acc)
          (declare (type (integer 0 1) n) (type fixnum acc))
          (if (= n 1) acc (typed-underflow (1- n) (1+ acc))))
        (dotimes (i 500) (typed-underflow 1 0))
        (assert (handler-case (progn (typed-underflow 0 7) nil)
                  (type-error (c) (= -1 (type-error-datum c)))))
        (format t "TYPED-TAIL-DEPTH-OK~%")
    "#);
    assert!(output.contains("TYPED-TAIL-DEPTH-OK"));
}

#[test]
fn single_float_result_checks_survive_native_fallback() {
    let output = run(r#"
        (declaim (optimize (speed 3) (safety 0)))
        (declaim (ftype (function (t) single-float) checked-float-result))
        (defun checked-float-result (value) (values value :secondary))
        (compile 'checked-float-result)
        (dotimes (i 500) (checked-float-result 1.5f0))
        (assert (equal (multiple-value-list (checked-float-result 1.5f0))
                       '(1.5f0 :secondary)))
        (dolist (bad '(1 1.5d0 nil :wrong-tag))
          (assert (handler-case (progn (checked-float-result bad) nil)
                    (type-error (condition) (eql (type-error-datum condition) bad)))))
        (format t "SINGLE-FLOAT-RESULT-CHECKED~%")
    "#);
    assert!(output.contains("SINGLE-FLOAT-RESULT-CHECKED"));
}

#[test]
fn safety_zero_checks_declared_primary_results_without_losing_extra_values() {
    let output = run(r#"
        (declaim (optimize (speed 3) (safety 0)))
        (declaim (ftype (function (t) (integer 0 10)) checked-ftype-result))
        (defun checked-ftype-result (value) (values value :secondary))
        (compile 'checked-ftype-result)
        (assert (equal (multiple-value-list (checked-ftype-result 7))
                       '(7 :secondary)))
        (dolist (bad '(11 -1 :wrong-tag))
          (assert
            (handler-case (progn (checked-ftype-result bad) nil)
              (type-error (condition)
                (eql (type-error-datum condition) bad)))))
        (format t "FTYPE-RESULT-CHECKED-AT-SAFETY-ZERO~%")
    "#);
    assert!(output.contains("FTYPE-RESULT-CHECKED-AT-SAFETY-ZERO"));
}

#[test]
fn retained_result_contract_survives_redefinition_and_named_return() {
    let output = run(r#"
        (declaim (optimize (speed 3) (safety 0)))
        (declaim (ftype (function (t) (integer 0 10)) retained-checked-result))
        (defun retained-checked-result (value)
          "A docstring must not hide the retained result contract."
          (return-from retained-checked-result (values value :secondary)))
        (defparameter *old-checked-result* #'retained-checked-result)
        (remprop 'retained-checked-result 'egcl-internal::function-type)
        (defun retained-checked-result (value) (values value :replacement))
        (assert (equal (multiple-value-list (retained-checked-result 11)) '(11 :replacement)))
        (assert (equal (multiple-value-list (funcall *old-checked-result* 10)) '(10 :secondary)))
        (assert (handler-case (progn (funcall *old-checked-result* 11) nil)
                  (type-error (condition) (= (type-error-datum condition) 11))))
        (format t "FTYPE-RETAINED-RETURN-CHECKED~%")
    "#);
    assert!(output.contains("FTYPE-RETAINED-RETURN-CHECKED"));
}

#[test]
fn retained_unrestricted_result_does_not_adopt_a_replacement_contract() {
    let output = run(r#"
        (declaim (optimize (speed 3) (safety 0)))
        (defun retained-unrestricted-result (value)
          "The original definition has no result contract."
          (return-from retained-unrestricted-result (values value :original)))
        (defparameter *old-unrestricted-result* #'retained-unrestricted-result)
        (declaim (ftype (function (t) (integer 0 10)) retained-unrestricted-result))
        ;; Compilation must use the definition's snapshot, not a later proclamation.
        (compile 'retained-unrestricted-result)
        (assert (equal '(11 :original)
                       (multiple-value-list (retained-unrestricted-result 11))))
        (defun retained-unrestricted-result (value) (values value :replacement))
        (compile 'retained-unrestricted-result)
        (dotimes (i 40)
          (assert (equal '(11 :original)
                         (multiple-value-list (funcall *old-unrestricted-result* 11))))
          (assert (equal '(10 :replacement)
                         (multiple-value-list (retained-unrestricted-result 10)))))
        (assert (handler-case (progn (retained-unrestricted-result 11) nil)
                  (type-error (condition) (= (type-error-datum condition) 11))))
        (format t "FTYPE-UNRESTRICTED-SNAPSHOT-PRESERVED~%")
    "#);
    assert!(output.contains("FTYPE-UNRESTRICTED-SNAPSHOT-PRESERVED"));
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn native_result_checks_preserve_values_and_do_not_replay_effects() {
    let source = r#"
        (declaim (optimize (speed 3) (safety 0)))
        (defparameter *checked-result-calls* 0)
        (declaim (ftype (function (t) (integer 0 10)) native-checked-result))
        (defun native-checked-result (value)
          (incf *checked-result-calls*)
          (return-from native-checked-result (values value :secondary)))
        (dotimes (i 40)
          (assert (equal (multiple-value-list (native-checked-result 7))
                         '(7 :secondary))))
        (assert (= 2 (egcl-ext:function-tier 'native-checked-result)))
        (dolist (bad '(11 -1 :wrong-tag))
          (let ((before *checked-result-calls*))
            (assert (handler-case (progn (native-checked-result bad) nil)
                      (type-error (condition)
                        (eql bad (type-error-datum condition)))))
            (assert (= (1+ before) *checked-result-calls*))))
        (assert (equal (multiple-value-list (native-checked-result 10))
                       '(10 :secondary)))
        (format t "NATIVE-RESULT-CHECKED-ONCE~%")
    "#;
    let output = Command::new("timeout")
        .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", source])
        .env("EGCL_LAZY_COMPILE", "0")
        .env("EGCL_FORCE_TIER", "t2")
        .env("EGCL_T2_LOG", "1")
        .output()
        .expect("run native result regression");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("NATIVE-RESULT-CHECKED-ONCE"));
    assert!(
        stderr.contains("NATIVE-CHECKED-RESULT: optimized worker result installed with"),
        "checked function must install native T2 code: {stderr}"
    );
}

#[test]
fn global_ftype_properties_preserve_package_identity() {
    let output = run(r#"
        (let* ((left-package (make-package "FTYPE-LEFT" :use '()))
               (right-package (make-package "FTYPE-RIGHT" :use '()))
               (left (intern "RESULT" left-package))
               (right (intern "RESULT" right-package)))
          (proclaim (list 'ftype '(function () fixnum) left))
          (proclaim (list 'ftype '(function () single-float) right))
          (assert (equal (get left 'egcl-internal::function-type)
                         '(function () fixnum)))
          (assert (equal (get right 'egcl-internal::function-type)
                         '(function () single-float))))
        (format t "FTYPE-IDENTITY~%")
    "#);
    assert!(output.contains("FTYPE-IDENTITY"));
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn native_result_edges_cover_baseline_and_optimized_tiers() {
    let program = r#"
        (declaim (optimize (speed 3) (safety 0)))
        (defparameter *result-effects* 0)
        (declaim (ftype (function (t) fixnum) result-plain))
        (defun result-plain (x) x)
        (defun result-producer (x)
          (incf *result-effects*) (values x :secondary))
        (declaim (ftype (function (t) (integer 0 10)) result-wrapper))
        (defun result-wrapper (x) (result-producer x))
        (declaim (ftype (function (t) fixnum) result-zero))
        (defun result-zero (present)
          (if present (values 3 :secondary) (values)))
        (dotimes (i 40)
          (assert (= 7 (result-plain 7)))
          (assert (equal '(7 :secondary) (multiple-value-list (result-wrapper 7))))
          (assert (equal '(3 :secondary) (multiple-value-list (result-zero t)))))
        (dolist (name '(result-plain result-wrapper result-zero))
          (assert (= *expected-tier* (egcl-ext:function-tier name))))
        (assert (handler-case (progn (result-plain :bad) nil)
                  (type-error (c) (eq :bad (type-error-datum c)))))
        (dolist (bad '(11 -1 :bad))
          (let ((before *result-effects*))
            (assert (handler-case (progn (result-wrapper bad) nil)
                      (type-error (c) (eql bad (type-error-datum c)))))
            (assert (= (1+ before) *result-effects*))))
        (assert (handler-case (progn (result-zero nil) nil)
                  (type-error (c) (null (type-error-datum c)))))
        (assert (equal '(10 :secondary) (multiple-value-list (result-wrapper 10))))
        (format t "NATIVE-RESULT-EDGES-OK~%")
    "#;
    for tier in [1, 2] {
        let source = format!("(defparameter *expected-tier* {tier})\n{program}");
        let output = Command::new("timeout")
            .args(["--kill-after=5", "120", env!("CARGO_BIN_EXE_egcl")])
            .args(["--no-init", "--eval", &source])
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", format!("t{tier}"))
            .env("EGCL_T2_LOG", "1")
            .output()
            .expect("run native result edge regressions");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "tier {tier}: {stdout}\n{stderr}");
        assert!(stdout.contains("NATIVE-RESULT-EDGES-OK"));
        if tier == 2 {
            assert!(stderr.contains("RESULT-PLAIN: optimized worker result installed with ordinary x86-64 Lisp T2 ABI"), "{stderr}");
            assert!(
                stderr.contains("RESULT-WRAPPER: optimized worker result installed with"),
                "{stderr}"
            );
        }
    }
}

#[test]
fn source_free_bfasl_retains_ftype_at_compile_and_load_time() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!("egcl-ftype-{}-{nonce}", std::process::id()));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("declarations.lisp");
    let fasl = directory.join("declarations.bfasl");
    fs::write(
        &source,
        r#"
        (declaim (ftype (function () fixnum) persisted-ftype-result))
        (eval-when (:compile-toplevel)
          (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                         '(function () fixnum))))
        (defun persisted-ftype-result () (values 7 8))
        (declaim (ftype (function (t) (integer 0 10)) persisted-checked-result))
        (defun persisted-checked-result (value) (values value :secondary))
    "#,
    )
    .unwrap();
    run(&format!(
        r#"
        (multiple-value-bind (output warnings failure)
            (compile-file {source:?} :output-file {fasl:?})
          (declare (ignore warnings))
          (assert output)
          (assert (not failure)))
        (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                       '(function () fixnum)))
    "#
    ));
    let bytes = fs::read(&fasl).unwrap();
    let count = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
    let mut position = 24;
    let mut bytecode = false;
    for _ in 0..count {
        let kind = u16::from_le_bytes(bytes[position..position + 2].try_into().unwrap());
        let length =
            u32::from_le_bytes(bytes[position + 2..position + 6].try_into().unwrap()) as usize;
        assert_ne!(
            kind, 11,
            "FTYPE retention must not require embedded source fallback"
        );
        bytecode |= kind == 12;
        position += 6 + length;
    }
    assert!(bytecode, "artifact must contain a bytecode unit");
    fs::remove_file(&source).unwrap();
    let output = run(&format!(
        r#"
        (assert (null (get 'persisted-ftype-result 'egcl-internal::function-type)))
        (load {fasl:?})
        (assert (equal (get 'persisted-ftype-result 'egcl-internal::function-type)
                       '(function () fixnum)))
        (assert (equal (multiple-value-list (persisted-ftype-result)) '(7 8)))
        (assert (equal (multiple-value-list (persisted-checked-result 10))
                       '(10 :secondary)))
        (assert (handler-case (progn (persisted-checked-result 11) nil)
                  (type-error (condition) (= (type-error-datum condition) 11))))
        (format t "FTYPE-LOADED~%")
    "#
    ));
    assert!(output.contains("FTYPE-LOADED"));
    let old_version = directory.join("forged-old-version.bfasl");
    let image = egcl_rt::bfasl::load(&bytes).unwrap();
    let mut builder = egcl_rt::bfasl::BfaslBuilder::new();
    for (kind, section) in image.sections() {
        let mut section = section.to_vec();
        if kind == egcl_rt::bfasl::section::BYTECODE_UNIT {
            section[4..6].copy_from_slice(&0x0201u16.to_le_bytes());
        }
        builder = builder.section(kind, section);
    }
    fs::write(&old_version, builder.build()).unwrap();
    run(&format!(
        r#"
        (assert (handler-case (progn (load {old_version:?}) nil)
                  (error () t)))
        (assert (not (fboundp 'persisted-checked-result)))
    "#
    ));
    fs::remove_file(old_version).unwrap();
    fs::remove_file(fasl).unwrap();
    fs::remove_dir(directory).unwrap();
}

/// A declared integer RANGE must be enforced at the optimising tier, not just
/// its fixnum tag.
///
/// `CheckResult` on a declared `FTYPE` lowers to a T2 `Guard` carrying a
/// FIXNUM `TypeTag` whose `range` holds the declared bounds. An emitter that
/// matches on the tag and ignores the range does not lose an optimisation, it
/// loses a check the program can observe: the function returns an
/// out-of-range value where every other tier signals a `TYPE-ERROR`.
///
/// Forcing T2 is what makes this a real check — the shape is small enough
/// that it would otherwise sit at a lower tier and pass for the wrong reason.
/// It is written without recursion on purpose, so it pins the guard itself
/// rather than the tail-call path that first exposed the gap on AArch64.
#[test]
fn a_declared_integer_range_is_enforced_at_the_optimising_tier() {
    let output = run_with_env(
        r#"
        (declaim (ftype (function (fixnum) (integer 0 100)) ranged-result))
        (defun ranged-result (n) (+ n 0))
        (dotimes (i 50) (ranged-result 1))
        (assert (= 100 (ranged-result 100)))
        (assert (= 0 (ranged-result 0)))
        (assert (handler-case (progn (ranged-result 101) nil)
                  (type-error (c) (= 101 (type-error-datum c)))))
        (assert (handler-case (progn (ranged-result -1) nil)
                  (type-error (c) (= -1 (type-error-datum c)))))
        (format t "RANGE-ENFORCED~%")
    "#,
        &[("EGCL_FORCE_TIER", "t2")],
    );
    assert!(output.contains("RANGE-ENFORCED"));
}
