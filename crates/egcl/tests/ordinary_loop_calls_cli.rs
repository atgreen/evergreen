// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Loops that call other functions from optimized code (bliss-6m6ca).
//!
//! Every DOTIMES, DOLIST, DO and LOOP body is a TAGBODY. Such a body now gets
//! the ordinary x86-64 Lisp T2 entry, so its calls to fixed-arity callees are
//! native register calls instead of a managed child activation per call, and
//! its idle loop polls stay in generated code. These programs run each shape
//! well past the tiering thresholds and check the tree-walker's answer under
//! every forced tier: speculation failures in the looping frame itself and in
//! a looping callee, GO in both directions, redefinition while the caller is
//! hot, THROW, conditions, restarts and UNWIND-PROTECT across the loop, zero
//! and multiple values, allocating callees, deep recursion and stack
//! exhaustion, wrong argument counts, and FUNCALL/APPLY of every designator.
//!
//! The expected lines were taken from the tree-walker.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [Option<&str>; 5] = [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None];

fn run(tier: Option<&str>, program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_FORCE_TIER",
        "EGCL_T2_LOG",
    ] {
        cmd.env_remove(key);
    }
    if let Some(tier) = tier {
        cmd.env("EGCL_FORCE_TIER", tier);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(
        out.status.success(),
        "tier {tier:?}: program failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    (stdout, stderr)
}

fn result_line(tier: Option<&str>, out: &str) -> String {
    out.lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("tier {tier:?}: no RESULT line in:\n{out}"))
        .to_string()
}

fn check_all_tiers(program: &str, expected: &str) {
    for tier in TIERS {
        let (out, _) = run(tier, program, &[]);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
}

#[test]
fn speculation_failures_in_looping_callers_and_callees() {
    check_all_tiers(
        r#"
(progn
  (defun ol-leaf (a b) (+ a b))
  (defun ol-big (n x) (let ((s x)) (dotimes (i n s) (setq s (ol-leaf s i)))))
  (defun ol-child (n x) (let ((r 0)) (dotimes (k 2 r) (setq r (ol-big n x)))))
  (defun ol-tb (n x)
    (let ((i 0) (s x))
      (block done
        (tagbody
         top
           (when (>= i n) (go out))
           (setq s (ol-leaf s i))
           (incf i)
           (if (evenp i) (go top) (go mid))
         mid
           (setq s (ol-leaf s 1))
           (go top)
         out
           (return-from done (list i s))))))
  (dotimes (j 40) (ol-big 100 0) (ol-child 50 0) (ol-tb 20 0))
  (format t "RESULT ~S~%"
          (list (ol-big 100 most-positive-fixnum) (ol-big 100 0.5)
                (ol-child 50 most-positive-fixnum) (ol-child 50 1.5)
                (ol-tb 20 0) (ol-tb 20 most-positive-fixnum) (ol-tb 7 0.25))))
"#,
        "RESULT (1152921504606851925 4950.5 1152921504606848200 1226.5 (20 200) \
         (20 1152921504606847175) (7 25.25))",
    );
}

#[test]
fn redefinition_and_non_local_exits_through_a_hot_loop() {
    check_all_tiers(
        r#"
(progn
  (defun ol-p1 (a) (+ a 1))
  (defun ol-calls-p1 (n)
    (let ((s 0))
      (dotimes (i n s)
        (setq s (ol-p1 s))
        (when (= i 500) (eval '(defun ol-p1 (a) (+ a 100)))))))
  (defun ol-thrower (a) (if (= a 77) (throw 'ol-tag (list :thrown a)) a))
  (defun ol-throw-loop (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (ol-thrower i))))))
  (defun ol-signaller (a) (if (= a 33) (error "boom ~a" a) a))
  (defun ol-signal-loop (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (ol-signaller i))))))
  (dotimes (j 40) (ol-calls-p1 100) (ol-throw-loop 50) (ol-signal-loop 20))
  (defun ol-p1 (a) (+ a 1))
  (let ((redefined (ol-calls-p1 1000)))
    (defun ol-p1 (a) (+ a 1))
    (format t "RESULT ~S~%"
            (list redefined (ol-calls-p1 400)
                  (catch 'ol-tag (ol-throw-loop 100))
                  (handler-case (ol-signal-loop 100) (error (e) (format nil "caught ~a" e)))
                  (restart-case
                      (handler-bind ((error (lambda (c) (declare (ignore c))
                                              (invoke-restart 'use 42))))
                        (ol-signal-loop 100))
                    (use (v) (list :restarted v)))
                  (let ((log nil))
                    (list (catch 'ol-tag (unwind-protect (ol-throw-loop 100) (push :cleaned log)))
                          log))))))
"#,
        "RESULT (50401 400 (:THROWN 77) \"caught boom 33\" (:RESTARTED 42) ((:THROWN 77) (:CLEANED)))",
    );
}

#[test]
fn values_allocation_depth_and_arity_through_loop_calls() {
    check_all_tiers(
        r#"
(progn
  (defun ol-mv (a) (values a (* 2 a) (* 3 a)))
  (defun ol-none (a) (declare (ignore a)) (values))
  (defun ol-mv-loop (n)
    (let ((s 0))
      (dotimes (i n)
        (multiple-value-bind (x y z) (ol-mv i) (setq s (+ s x y z))))
      (multiple-value-list (ol-mv s))))
  (defun ol-last-mv (n) (dotimes (i n) (ol-mv i)) (ol-mv n))
  (defun ol-none-loop (n) (let ((r :unset)) (dotimes (i n r) (setq r (ol-none i)))))
  (defun ol-cell (a) (list a (cons a a) (make-string 3 :initial-element #\x)))
  (defun ol-alloc-loop (n)
    (let ((acc nil)) (dotimes (i n) (push (ol-cell i) acc))
      (list (length acc) (first acc) (car (last acc)))))
  (defun ol-deep (n)
    (if (= n 0) 0 (let ((s 0)) (dotimes (i 1 s) (setq s (+ 1 (ol-deep (- n 1))))))))
  (defun ol-two (a b) (list a b))
  (defun ol-bad-arity (n f) (let ((r nil)) (dotimes (i n r) (setq r (funcall f i)))))
  (defun ol-bad-named (n) (let ((r nil)) (dotimes (i n r) (setq r (ol-two i)))))
  (dotimes (j 40) (ol-mv-loop 30) (ol-last-mv 5) (ol-none-loop 3) (ol-alloc-loop 100)
    (ol-deep 10))
  (format t "RESULT ~S~%"
          (list (ol-mv-loop 100) (multiple-value-list (ol-last-mv 9)) (ol-none-loop 4)
                (ol-alloc-loop 20000) (ol-deep 100)
                (handler-case (ol-bad-arity 3 #'ol-two)
                  (program-error () :program-error)
                  (error (e) (list :error (type-of e))))
                (handler-case (ol-bad-named 3)
                  (program-error () :program-error)
                  (error (e) (list :error (type-of e)))))))
"#,
        "RESULT ((29700 59400 89100) (9 18 27) NIL (20000 (19999 (19999 . 19999) \"xxx\") \
         (0 (0 . 0) \"xxx\")) 100 :PROGRAM-ERROR :PROGRAM-ERROR)",
    );
}

#[test]
fn funcall_and_apply_of_every_designator_from_a_loop() {
    check_all_tiers(
        r#"
(progn
  (defun ol-leaf (a b) (+ a b))
  (defun ol-opt (a &optional (b 10)) (+ a b))
  (defun ol-key (a &key (b 1)) (+ a b))
  (defun ol-adder (k) (lambda (a b) (+ a b k)))
  (defun ol-fl (f n) (let ((s 0)) (dotimes (i n s) (setq s (funcall f s 1)))))
  (defun ol-al (f n) (let ((s 0)) (dotimes (i n s) (setq s (apply f s '(1))))))
  (defun ol-named-opt (n) (let ((s 0)) (dotimes (i n s) (setq s (ol-opt s 1)))))
  (defun ol-named-key (n) (let ((s 0)) (dotimes (i n s) (setq s (ol-key s :b 1)))))
  (dotimes (j 40) (ol-fl #'ol-leaf 10) (ol-fl 'ol-leaf 10) (ol-al #'ol-leaf 10) (ol-al '+ 10)
    (ol-named-opt 10) (ol-named-key 10) (ol-fl (ol-adder 0) 10))
  (format t "RESULT ~S~%"
          (list (ol-fl #'ol-leaf 100) (ol-fl 'ol-leaf 100) (ol-fl #'+ 100)
                (ol-fl (lambda (a b) (* (+ a b) 1)) 100) (ol-fl (ol-adder 1) 100)
                (ol-al #'ol-leaf 100) (ol-al '+ 100) (ol-named-opt 100) (ol-named-key 100)
                (handler-case (ol-fl 'ol-no-such-function 3)
                  (undefined-function () :undefined-function)
                  (error (e) (list :error (type-of e))))
                (handler-case (ol-fl 42 3)
                  (type-error () :type-error) (undefined-function () :undefined-function)
                  (error (e) (list :error (type-of e)))))))
"#,
        "RESULT (100 100 100 100 200 100 100 100 100 :UNDEFINED-FUNCTION :TYPE-ERROR)",
    );
}

/// The looping caller gets the ordinary entry rather than the mapped one,
/// reaches T2 and stays there: the calls run without deoptimizing it.
#[test]
fn a_dotimes_caller_installs_the_ordinary_entry() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = r#"
(progn
  (defun ol-callee (a b) (declare (ignore b)) a)
  (defun ol-caller (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (ol-callee i 2))))))
  (dotimes (j 40) (ol-caller 1000))
  (let ((before (egcl-ext:deopt-count)))
    (format t "RESULT ~D TIER ~D DEOPTS ~D~%" (ol-caller 100000)
            (egcl-ext:function-tier 'ol-caller) (- (egcl-ext:deopt-count) before))))
"#;
    let (out, log) = run(None, program, &[("EGCL_T2_LOG", "stderr")]);
    assert_eq!(result_line(None, &out), "RESULT 4999950000 TIER 2 DEOPTS 0", "{out}\n{log}");
    assert!(
        log.lines().any(|line| line
            .contains("OL-CALLER: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
        "{log}"
    );
}

/// Deep recursion through a looping function stays native and exhausting the
/// stack signals a catchable STORAGE-CONDITION. The tree-walker cannot reach
/// this depth, so only the compiled tiers run it.
#[test]
fn deep_recursion_through_looping_frames_signals_storage_condition() {
    let program = r#"
(progn
  (defun ol-deep (n)
    (if (= n 0) 0 (let ((s 0)) (dotimes (i 1 s) (setq s (+ 1 (ol-deep (- n 1))))))))
  (dotimes (j 40) (ol-deep 10))
  (format t "RESULT ~S~%"
          (list (ol-deep 2000)
                (handler-case (ol-deep 10000000)
                  (storage-condition () :storage-condition)
                  (error (e) (list :error (type-of e)))))))
"#;
    for tier in [Some("t2"), None] {
        let (out, _) = run(tier, program, &[]);
        assert_eq!(result_line(tier, &out), "RESULT (2000 :STORAGE-CONDITION)", "tier {tier:?}:\n{out}");
    }
}
