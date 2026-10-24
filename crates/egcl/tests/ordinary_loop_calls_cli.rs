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
//! A callee whose trailing parameters are `&optional` gets an ordinary entry
//! too, which binds each omitted parameter as the managed entry does.
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

#[test]
fn optional_callees_bind_omitted_parameters_through_the_ordinary_entry() {
    check_all_tiers(
        r#"
(progn
  (defun oo-1 (a &optional (b 10) (c (+ a b) c-p)) (list a b c c-p))
  (defun oo-0 (&optional x) (if x (* x 2) :none))
  (defun oo-2 (a &optional b) (+ a (or b 1)))
  (defun oo-alloc (a &optional (b (list a a))) (cons a b))
  (defun oo-rec (n &optional (acc 0)) (if (= n 0) acc (oo-rec (- n 1) (+ acc n))))
  (defun oo-drive-1 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (oo-1 i) (oo-1 i 2) (oo-1 i 2 3))))))
  (defun oo-drive-0 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (oo-0) (oo-0 i))))))
  (defun oo-drive-2 (n x) (let ((s x)) (dotimes (i n s) (setq s (oo-2 s)) (setq s (oo-2 s i)))))
  (defun oo-drive-alloc (n)
    (let ((acc nil)) (dotimes (i n) (push (oo-alloc i) acc) (push (oo-alloc i :x) acc))
      (list (length acc) (first acc) (second acc) (car (last acc)))))
  (defun oo-few (n) (let ((r nil)) (dotimes (i n r) (setq r (oo-2)))))
  (defun oo-many (n) (let ((r nil)) (dotimes (i n r) (setq r (oo-2 1 2 3)))))
  (defun oo-fo (f n) (let ((r nil)) (dotimes (i n r) (setq r (list (funcall f i) (funcall f i 5))))))
  (dotimes (j 40) (oo-drive-1 100) (oo-drive-0 100) (oo-drive-2 100 0) (oo-drive-alloc 50)
    (oo-rec 50) (oo-fo #'oo-2 10))
  (format t "RESULT ~S~%"
          (list (oo-drive-1 1000) (oo-drive-0 1000)
                (oo-drive-2 1000 0) (oo-drive-2 100 most-positive-fixnum) (oo-drive-2 100 0.5)
                (handler-case (oo-few 3) (program-error () :program-error)
                  (error (e) (list :error (type-of e))))
                (handler-case (oo-many 3) (program-error () :program-error)
                  (error (e) (list :error (type-of e))))
                (oo-fo #'oo-2 100) (oo-fo 'oo-2 100)
                (oo-drive-alloc 3000) (oo-rec 300) (oo-rec 100 most-positive-fixnum))))
"#,
        "RESULT (((999 10 1009 NIL) (999 2 1001 NIL) (999 2 3 T)) (:NONE 1998) 500500 \
         1152921504606852025 5050.5 :PROGRAM-ERROR :PROGRAM-ERROR (100 104) (100 104) \
         (6000 (2999 . :X) (2999 2999 2999) (0 0 0)) 45150 1152921504606852025)",
    );
}

#[test]
fn keyword_callees_bind_through_the_ordinary_entry() {
    check_all_tiers(
        r#"
(progn
  (defun ok-0 (&key value) value)
  (defun ok-1 (a &key (b 10) (c nil c-p)) (list a b c c-p))
  (defun ok-2 (&key x) (if x (* x 2) :none))
  (defun ok-3 (a &key b) (+ a (or b 1)))
  (defun ok-4 (a &optional (o 5) &key (z 7)) (list a o z))
  (defun ok-5 (a &key (b (list a))) (cons a b))
  (defun ok-6 (a &key b &allow-other-keys) (list a b))
  (defun ok-rec (n &key (acc 0)) (if (= n 0) acc (ok-rec (- n 1) :acc (+ acc n))))
  (defun ok-cell (a &key (tag :t)) (list a tag (make-string 2 :initial-element #\y)))
  (defun ok-drive-1 (n)
    (let ((r nil)) (dotimes (i n r)
      (setq r (list (ok-1 i) (ok-1 i :b 2) (ok-1 i :c 3) (ok-1 i :c 3 :b 4) (ok-1 i :b 1 :b 2))))))
  (defun ok-drive-2 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (ok-2) (ok-2 :x i))))))
  (defun ok-drive-3 (n x) (let ((s x)) (dotimes (i n s) (setq s (ok-3 s)) (setq s (ok-3 s :b i)))))
  (defun ok-drive-4 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (ok-4 i) (ok-4 i 1) (ok-4 i 1 :z 2))))))
  (defun ok-drive-5 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (ok-5 i) (ok-5 i :b 3))))))
  (defun ok-drive-6 (n) (let ((r nil)) (dotimes (i n r) (setq r (list (ok-6 i :q 1) (ok-6 i :b 2 :zz 3))))))
  (defun ok-alloc (n)
    (let ((acc nil)) (dotimes (i n) (push (ok-cell i) acc) (push (ok-cell i :tag :u) acc))
      (list (length acc) (first acc) (second acc))))
  (defun ok-odd (n) (let ((r nil)) (dotimes (i n r) (setq r (ok-3 1 :b)))))
  (defun ok-unknown (n) (let ((r nil)) (dotimes (i n r) (setq r (ok-3 1 :zz 2)))))
  (defun ok-few (n) (let ((r nil)) (dotimes (i n r) (setq r (ok-3)))))
  (defun ok-aok (n) (let ((r nil)) (dotimes (i n r) (setq r (ok-3 1 :zz 2 :allow-other-keys t)))))
  (defun ok-aok-nil (n) (let ((r nil)) (dotimes (i n r) (setq r (ok-3 1 :allow-other-keys nil :b 5)))))
  (defun ok-fk (f n) (let ((r nil)) (dotimes (i n r) (setq r (list (funcall f i) (funcall f i :b 5))))))
  (dotimes (j 40) (ok-drive-1 50) (ok-drive-2 50) (ok-drive-3 50 0) (ok-drive-4 50) (ok-drive-5 50)
    (ok-drive-6 50) (ok-rec 50) (ok-alloc 50) (ok-fk #'ok-3 10))
  ;; Called from the evaluator, before and after it tiers: the managed
  ;; activation binds and enters past the keyword entry, even for a leaf.
  (let ((direct (loop for i below 60 sum (ok-0 :value i)))
        (errors (mapcar (lambda (f) (handler-case (funcall f 3) (program-error () :program-error)
                                      (error (e) (list :error (type-of e)))))
                        (list #'ok-odd #'ok-unknown #'ok-few))))
    (format t "RESULT ~S~%"
            (list direct (ok-drive-1 1000) (ok-drive-2 1000)
                  (ok-drive-3 1000 0) (ok-drive-3 100 most-positive-fixnum) (ok-drive-3 100 0.5)
                  (ok-drive-4 1000) (ok-drive-5 1000) (ok-drive-6 1000) errors
                  (ok-aok 3) (ok-aok-nil 3) (ok-fk #'ok-3 100) (ok-fk 'ok-3 100)
                  (ok-rec 100) (ok-rec 100 :acc most-positive-fixnum) (ok-alloc 3000)))))
"#,
        "RESULT (1770 ((999 10 NIL NIL) (999 2 NIL NIL) (999 10 3 T) (999 4 3 T) (999 1 NIL NIL)) \
         (:NONE 1998) 500500 1152921504606852025 5050.5 ((999 5 7) (999 1 7) (999 1 2)) \
         ((999 999) (999 . 3)) ((999 NIL) (999 2)) (:PROGRAM-ERROR :PROGRAM-ERROR :PROGRAM-ERROR) \
         2 6 (100 104) (100 104) 5050 1152921504606852025 (6000 (2999 :U \"yy\") (2999 :T \"yy\")))",
    );
}

/// A warmed `&key` callee is entered natively: it installs the ordinary
/// entry rather than staying on the mapped ABI.
#[test]
fn a_keyword_callee_installs_the_ordinary_entry() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = r#"
(progn
  (defun ok-key (a &key (b 0)) (+ a b))
  (defun ok-key-caller (n) (let ((s 0)) (dotimes (i n s) (setq s (ok-key s :b 1)))))
  (dotimes (j 40) (ok-key-caller 1000))
  (format t "RESULT ~D TIER ~D~%" (ok-key-caller 100000) (egcl-ext:function-tier 'ok-key)))
"#;
    let (out, log) = run(None, program, &[("EGCL_T2_LOG", "stderr")]);
    assert_eq!(result_line(None, &out), "RESULT 100000 TIER 2", "{out}\n{log}");
    assert!(
        log.lines().any(|line| line
            .contains("OK-KEY: optimized worker result installed with ordinary x86-64 Lisp T2 ABI")),
        "{log}"
    );
}

/// Raw double-float bodies get the ordinary entry too (bliss-k4psf): a
/// guard failing in one, as caller or callee, reboxes its raw values for the
/// interpreter, which then finishes the call with the generic arithmetic.
#[test]
fn double_float_bodies_call_and_deoptimize_through_the_ordinary_entry() {
    check_all_tiers(
        r#"
(progn
  (defun od-g1 (x) (+ x 1d0))
  (defun od-g2 (x y) (let ((d (- x y))) (* d d)))
  (defun od-g4 (a b c d) (declare (double-float a b c d)) (+ (* a b) (* c d)))
  (defun od-loop-g1 (n x) (let ((s x)) (dotimes (i n s) (setq s (od-g1 s)))))
  (defun od-loop-mix (lst) (let ((s 0d0)) (dolist (e lst s) (setq s (+ s (od-g2 e 1d0))))))
  (defun od-loop-g4 (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s (od-g4 s 0.5d0 1d0 2d0))))))
  (defun od-boxing (n)
    (let ((acc nil)) (dotimes (i n) (push (od-g1 (float i 1d0)) acc))
      (list (length acc) (first acc) (car (last acc)))))
  (dotimes (j 300) (od-loop-g1 50 0d0) (od-loop-mix (list 1d0 2d0 3d0)) (od-loop-g4 50)
    (od-boxing 20))
  (format t "RESULT ~S~%"
          (list (od-loop-g1 1000 0d0) (od-loop-g1 10 5) (od-loop-g1 10 1/2) (od-loop-g1 10 2.5f0)
                (od-loop-mix (list 1d0 2d0 3 4/3 5.0f0 6d0))
                (handler-case (od-loop-g1 3 "x") (type-error () :type-error)
                  (error (e) (list :error (type-of e))))
                (od-boxing 3000) (od-loop-g4 1000)
                (handler-case (od-g4 1d0 2d0 3d0 4) (type-error () :type-error)
                  (error (e) (list :error (type-of e)))))))
"#,
        "RESULT (1000.0d0 15.0d0 10.5d0 12.5d0 46.111111111111114d0 :TYPE-ERROR \
         (3000 3000.0d0 1.0d0) 4.935362387624709d176 14.0d0)",
    );
}

/// A callee with declared parameter types gets a published ordinary entry,
/// which checks each declaration itself and signals the same TYPE-ERROR as
/// before when one fails, including from callers already at T2.
#[test]
fn typed_callees_check_their_declarations_in_the_ordinary_entry() {
    check_all_tiers(
        r#"
(progn
  (defun ot-1 (a b) (declare (fixnum a b)) (+ a b))
  (defun ot-2 (a b) (declare (type (integer 0 100) a) (fixnum b)) (+ a b))
  (defun ot-3 (x) (declare (single-float x)) (* x 2.0))
  (defun ot-5 (a b c d e) (declare (fixnum a e)) (list a b c d e))
  (defun ot-l1 (n) (let ((s 0)) (dotimes (i n s) (setq s (ot-1 s 1)))))
  (defun ot-l2 (n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (ot-2 (mod i 100) 1))))))
  (defun ot-l3 (n) (let ((s 0.0)) (dotimes (i n s) (setq s (+ s (ot-3 1.5))))))
  (defun ot-l5 (n) (let ((r nil)) (dotimes (i n r) (setq r (ot-5 i 2 3 4 i)))))
  (defun ot-bad1 (x) (let ((r nil)) (dotimes (i 3 r) (setq r (ot-1 x 1)))))
  (defun ot-bad2 (x) (let ((r nil)) (dotimes (i 3 r) (setq r (ot-2 x 1)))))
  (defun ot-bad3 (x) (let ((r nil)) (dotimes (i 3 r) (setq r (ot-3 x)))))
  (defun ot-bad5 (x) (let ((r nil)) (dotimes (i 3 r) (setq r (ot-5 1 2 3 4 x)))))
  (defun ot-badf (f x) (let ((r nil)) (dotimes (i 3 r) (setq r (funcall f x 1)))))
  (dotimes (j 60) (ot-l1 100) (ot-l2 100) (ot-l3 100) (ot-l5 10))
  (loop repeat 20000
        until (every (lambda (f) (eql 2 (egcl-ext:function-tier f)))
                     '(ot-bad1 ot-bad2 ot-bad3 ot-bad5 ot-badf))
        do (ot-bad1 1) (ot-bad2 5) (ot-bad3 1.0) (ot-bad5 1) (ot-badf #'ot-1 2))
  (format t "RESULT ~S~%"
          (list (ot-l1 1000) (ot-l2 1000) (ot-l3 100) (ot-l5 3)
                (mapcar (lambda (thunk)
                          (handler-case (funcall thunk)
                            (type-error (e) (list :type-error (type-error-datum e)
                                                  (type-error-expected-type e)))
                            (error (e) (list :error (type-of e)))))
                        (list (lambda () (ot-bad1 1.5)) (lambda () (ot-bad1 "x"))
                              (lambda () (ot-bad2 101)) (lambda () (ot-bad2 -1))
                              (lambda () (ot-bad3 1)) (lambda () (ot-bad3 1d0))
                              (lambda () (ot-bad5 'q)) (lambda () (ot-badf #'ot-1 2.5)))))))
"#,
        "RESULT (1000 50500 300.0 (2 2 3 4 2) ((:TYPE-ERROR 1.5 FIXNUM) (:TYPE-ERROR \"x\" FIXNUM) \
         (:TYPE-ERROR 101 (INTEGER 0 100)) (:TYPE-ERROR -1 (INTEGER 0 100)) \
         (:TYPE-ERROR 1 SINGLE-FLOAT) (:TYPE-ERROR 1.0d0 SINGLE-FLOAT) (:TYPE-ERROR Q FIXNUM) \
         (:TYPE-ERROR 2.5 FIXNUM)))",
    );
}

/// FUNCALL of the current definition of a name, from an ordinary caller,
/// enters that name's ordinary entry directly once recorded. Redefinition,
/// guard failure, non-local exits, allocation, arity errors and the other
/// designators keep their meaning.
#[test]
fn funcall_of_a_named_function_enters_its_ordinary_entry() {
    check_all_tiers(
        r#"
(progn
  (defun of-a (a b) (+ a b))
  (defun of-o (a &optional (b 10)) (+ a b))
  (defun of-z () 7)
  (defun of-3 (a b c) (list a b c))
  (defun of-add (a b) (+ a b))
  (defun of-cons (a b) (cons b a))
  (defun of-throw (a b) (if (= a 50) (throw 'of-out (list :thrown a b)) (+ a b)))
  (defun of-rec (f n) (if (= n 0) 0 (+ 1 (funcall f f (- n 1)))))
  (defun of-l (f n) (let ((s 0)) (dotimes (i n s) (setq s (funcall f s 1)))))
  (defun of-l1 (f n) (let ((s 0)) (dotimes (i n s) (setq s (funcall f s)))))
  (defun of-l0 (f n) (let ((s 0)) (dotimes (i n s) (setq s (+ s (funcall f))))))
  (defun of-l3 (f n) (let ((r nil)) (dotimes (i n r) (setq r (funcall f i r 3)))))
  (defun of-big (f n) (let ((s most-positive-fixnum)) (dotimes (i n s) (setq s (funcall f s 1)))))
  (defun of-la (f n) (let ((s nil)) (dotimes (i n s) (setq s (funcall f s i)))))
  (dotimes (j 60) (of-l #'of-a 100) (of-l1 #'of-o 100) (of-l0 #'of-z 100) (of-l3 #'of-3 3)
    (of-l #'of-add 100) (of-big #'of-add 1) (of-la #'of-cons 100)
    (catch 'of-out (of-l #'of-throw 10)) (of-rec #'of-rec 10))
  (let ((old #'of-a)
        (first (list (of-l #'of-a 1000) (of-l1 #'of-o 100) (of-l0 #'of-z 100) (length (of-l3 #'of-3 5)))))
    (defun of-a (a b) (- a b))
    (format t "RESULT ~S~%"
            (list first (of-l old 100) (of-l #'of-a 100)
                  (handler-case (of-l1 #'of-add 3) (program-error () :program-error)
                    (error (e) (list :error (type-of e))))
                  (of-l #'of-add 100)
                  (let ((s 0.5)) (dotimes (i 10 s) (setq s (funcall #'of-add s 1))))
                  (of-big #'of-add 10)
                  (let ((l (of-la #'of-cons 20000))) (list (car l) (loop for x = l then (cdr x) while (consp x) count t)))
                  (catch 'of-out (of-l #'of-throw 100))
                  (of-rec #'of-rec 300)
                  (of-l 'of-add 100) (of-l (let ((k 2)) (lambda (a b) (+ a b k))) 100) (of-l #'+ 100)))))
"#,
        "RESULT ((1000 1000 700 3) 100 -100 :PROGRAM-ERROR 100 10.5 1152921504606846985 \
         (19999 20000) (:THROWN 50 1) 300 100 300 100)",
    );
}

/// A callee whose ordinary T2 entry is installed while a child activation of
/// it is still looping, so that loop's OSR selects code with an ordinary
/// entry. The transfer must enter the mapped companion that keeps the loop
/// headers, as the root activation's OSR already did; it panicked with
/// "ordinary entry is not an OSR destination" (bliss-3n3eh).
#[test]
fn a_child_loop_osr_enters_the_mapped_companion_of_ordinary_code() {
    let program = "(progn
      (defun build-ordinary (n)
        (let ((acc nil) (x (list :a :b)))
          (dotimes (i n) (setq acc (cons (cons i x) acc)))
          acc))
      (defun check-ordinary (n)
        (let ((l (build-ordinary n)) (k (1- n)) (ok t))
          (dolist (e l)
            (unless (and (eql (car e) k) (equal (cdr e) '(:a :b))) (setq ok nil))
            (decf k))
          (list ok (length l))))
      (dotimes (k 40) (check-ordinary 50))
      (format t \"RESULT ~s~%\" (check-ordinary 50000)))";
    check_all_tiers(program, "RESULT (T 50000)");
}
