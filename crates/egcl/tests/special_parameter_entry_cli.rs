// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Lambda-list parameters named by special variables compile (Backpack stage
//! 6b, bliss-gg3i1.7.15.7). The lowerer renames such a parameter and binds the
//! special in a LET around the body, so its dynamic binding is an ordinary
//! special binding that every tier restores on every exit. SBCL's
//! `(defun valid-fun-use (call type &key ((:lossage-fun *lossage-fun*)) ...))`
//! is the hot shape.
//!
//! Each program is run tree-walked, in T0, T1 and T2 and under the default
//! policy, and the answers must agree. The tests also check that the functions
//! really compiled, that non-local exits restore the outer binding, that a
//! thread sees only its own binding, and that a later default form which could
//! observe the binding keeps the original (tree-walked) path.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const TIERS: [Option<&str>; 5] = [Some("interp"), Some("t0"), Some("t1"), Some("t2"), None];

fn run(tier: Option<&str>, program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "600", BIN, "--no-init", "--eval", program]);
    for key in [
        "EGCL_DISABLE_T2",
        "EGCL_BACKEND",
        "EGCL_PROFILING_DISABLED",
        "EGCL_T2_NO_QUEUE",
        "EGCL_T2_DISCARD",
        "EGCL_FORCE_TIER",
        "EGCL_T2_LOG",
        "EGCL_T2_THRESHOLD",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
        "EGCL_LOG",
        "EGCL_BYTECODE_TRACE_NAMES",
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

const DEFINITIONS: &str = r#"
  (defvar *sp-a* :a0)
  (defvar *sp-b* :b0)
  (defvar *sp-c* :c0)
  (declaim (notinline sp-read sp-boom))
  (defun sp-read () (list *sp-a* *sp-b* *sp-c*))
  (defun sp-boom (v) (if (eql v :boom) (error "boom ~a" *sp-a*) v))
  ;; Required, &optional with a default reading the OUTER value, keyword
  ;; renamed and plain, and &rest.
  (defun sp-req (x *sp-a*) (list x (sp-read)))
  (defun sp-opt (x &optional (*sp-b* (list :outer *sp-b*))) (list x (sp-read)))
  (defun sp-key (&key ((:lossage *sp-a*) :la) (*sp-c* :lc) (other 7 other-p))
    (list other other-p (sp-read)))
  (defun sp-rest (&rest *sp-b*) (sp-read))
  (defun sp-doc (*sp-a*) "docstring" (declare (optimize speed)) (sp-read))
  (defun sp-values (*sp-a*) (values *sp-a* (sp-read)))
  (defun sp-setq (*sp-a*) (setq *sp-a* (list :set *sp-a*)) (sp-read))
  ;; Non-local exits through the binding.
  (defun sp-error (*sp-a*) (sp-boom *sp-a*))
  (defun sp-throw (*sp-a*) (throw 'sp-tag (sp-read)))
  (defun sp-escape (k *sp-a*) (funcall k (sp-read)))
  (defun sp-deep (n *sp-c*) (if (= n 0) (sp-read) (list *sp-c* (sp-deep (1- n) n))))
  ;; A later default that could read the binding keeps the original path; it
  ;; must still see the new binding, as CLHS 3.4.1 requires.
  (defun sp-late (*sp-a* &optional (y (sp-read))) (list y (sp-read)))
  (defun sp-round (k)
    (list (sp-req k :ra) (sp-opt k) (sp-opt k :ob) (sp-key) (sp-key :lossage k :other 1 :*sp-c* :kc)
          (sp-rest 1 2) (sp-doc k) (multiple-value-list (sp-values k)) (sp-setq k)
          (handler-case (sp-error :boom) (error (e) (list :caught (sp-read) (princ-to-string e))))
          (catch 'sp-tag (sp-throw k))
          (block out (sp-escape (lambda (v) (return-from out (list :escaped v (sp-read)))) k))
          (block out (handler-bind ((error (lambda (c) (declare (ignore c))
                                              (return-from out (list :in-handler (sp-read))))))
                       (sp-error :boom)))
          (sp-deep 3 :d) (sp-late :late)
          (sp-read)))
"#;

const EXPECTED_ROUND: &str = "(5 (:RA :B0 :C0)) (5 (:A0 (:OUTER :B0) :C0)) (5 (:A0 :OB :C0)) \
(7 NIL (:LA :B0 :LC)) (1 T (5 :B0 :KC)) (:A0 (1 2) :C0) (5 :B0 :C0) (5 (5 :B0 :C0)) \
((:SET 5) :B0 :C0) (:CAUGHT (:A0 :B0 :C0) \"boom BOOM\") (5 :B0 :C0) \
(:ESCAPED (5 :B0 :C0) (5 :B0 :C0)) (:IN-HANDLER (:BOOM :B0 :C0)) \
(:D (3 (2 (:A0 :B0 1)))) ((:LATE :B0 :C0) (:LATE :B0 :C0)) (:A0 :B0 :C0)";

#[test]
fn special_parameters_agree_in_every_tier_and_restore_on_every_exit() {
    let program = format!(
        r#"(progn {DEFINITIONS}
  (let ((last nil)) (dotimes (i 300) (setq last (sp-round 5)))
    (format t "RESULT ~S ~S~%" last (sp-read))))"#
    );
    let expected = format!("RESULT ({EXPECTED_ROUND}) (:A0 :B0 :C0)");
    for tier in TIERS {
        let (out, _) = run(tier, &program, &[("EGCL_T2_THRESHOLD", "50")]);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
}

/// The functions with special parameters compile (none is declined for its
/// lambda list), except the one whose later default could observe the binding.
#[test]
fn special_parameter_functions_compile() {
    let program = format!(
        r#"(progn {DEFINITIONS}
  (dotimes (i 50) (sp-round 5))
  (format t "RESULT ~S~%" (sp-round 5)))"#
    );
    let (out, log) = run(
        Some("t0"),
        &program,
        &[("EGCL_LOG", "bail=trace,compile=trace")],
    );
    assert!(result_line(None, &out).starts_with("RESULT "), "{out}");
    let declined: Vec<&str> = log
        .lines()
        .filter(|line| line.contains("special-parameter-name"))
        .collect();
    assert!(
        declined.iter().all(|line| line.contains("SP-LATE:")),
        "a function was declined for a special parameter:\n{declined:#?}"
    );
    assert!(
        declined.iter().any(|line| line.contains("SP-LATE:")),
        "SP-LATE should keep the original binder:\n{log}"
    );
}

/// Warm, they reach T2 under the default policy; SP-LATE stays tree-walked.
#[test]
fn special_parameter_functions_reach_t2() {
    if !cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        return;
    }
    let program = format!(
        r#"(progn {DEFINITIONS}
  (dotimes (i 300) (sp-round 5))
  ;; T2 compiles on a shared background worker; wait (bounded) for it.
  (loop repeat 200 until (every (lambda (f) (eql 2 (egcl-ext:function-tier f)))
                                '(sp-req sp-key sp-rest sp-doc sp-error sp-deep))
        do (sp-round 5) (sleep 0.01))
  (format t "RESULT ~S~%"
          (mapcar #'egcl-ext:function-tier
                  '(sp-req sp-key sp-rest sp-doc sp-error sp-deep sp-late))))"#
    );
    let (out, _) = run(None, &program, &[("EGCL_T2_THRESHOLD", "50")]);
    assert_eq!(result_line(None, &out), "RESULT (2 2 2 2 2 2 0)", "{out}");
}

/// Each thread sees only its own binding of a special parameter, and the
/// global value is untouched afterwards.
#[test]
fn threads_see_their_own_special_parameter_binding() {
    let program = r#"(progn
  (defvar *sp-t* :global)
  (declaim (notinline sp-t-read))
  (defun sp-t-read () *sp-t*)
  (defun sp-t-bind (*sp-t* tag n)
    (let ((ok t))
      (dotimes (i n ok)
        (unless (eq (sp-t-read) tag) (setq ok nil))
        (when (zerop (mod i 64)) (egcl-thread:thread-yield)))))
  (defun sp-t-worker (tag) (lambda () (list tag (sp-t-bind tag tag 5000) (sp-t-read))))
  (let ((threads (loop for tag in '(:one :two :three :four)
                       collect (egcl-thread:make-thread (sp-t-worker tag)))))
    (format t "RESULT ~S ~S ~S~%"
            (mapcar #'egcl-thread:join-thread threads)
            (sp-t-bind :main :main 5000) *sp-t*)))"#;
    let expected =
        "RESULT ((:ONE T :GLOBAL) (:TWO T :GLOBAL) (:THREE T :GLOBAL) (:FOUR T :GLOBAL)) T :GLOBAL";
    for tier in TIERS {
        let (out, _) = run(tier, program, &[("EGCL_T2_THRESHOLD", "50")]);
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
}

/// The rewritten entry is GC-safe: a collection on every allocation, with
/// freed nursery poisoned, gives the same answers.
#[test]
fn special_parameters_survive_gc_stress() {
    let program = format!(
        r#"(progn {DEFINITIONS}
  (let ((last nil)) (dotimes (i 3) (setq last (sp-round 5)))
    (format t "RESULT ~S ~S~%" last (sp-read))))"#
    );
    let expected = format!("RESULT ({EXPECTED_ROUND}) (:A0 :B0 :C0)");
    for tier in [Some("t0"), Some("t2")] {
        let (out, _) = run(
            tier,
            &program,
            &[
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ],
        );
        assert_eq!(result_line(tier, &out), expected, "tier {tier:?}:\n{out}");
    }
}
