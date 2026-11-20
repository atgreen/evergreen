// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Once-called creators stay interpreted; their escaped callbacks and canonical
//! local function identities must nevertheless compile against lexical bindings.
use std::process::Command;

const PROGRAM: &str = r#"
(defun ns-maker (seed)
  (let ((ns-step :variable))
    (flet ((ns-step (n) (incf seed n)))
      (list (lambda (n) (list ns-step (ns-step n) #'ns-step))
            #'ns-step (lambda () seed)))))
(defun ns-shadow-maker ()
  (flet ((ns-shadow () :outer))
    (flet ((ns-shadow () (ns-shadow)))
      (list (lambda () (ns-shadow)) #'ns-shadow))))
(defun ns-labels-maker (held)
  (labels ((ns-even (n) (if (= n 0) held (ns-odd (1- n))))
           (ns-odd (n) (if (= n 0) :odd (ns-even (1- n)))))
    (list (lambda (n) (ns-even n)) #'ns-even #'ns-odd)))
(defparameter *ns-basic* (ns-maker 0))
(defparameter *ns-shadow* (ns-shadow-maker))
(defparameter *ns-labels* (ns-labels-maker (list :held)))
(defparameter *ns-distinct*
  (let ((a (make-symbol "NS-SAME")) (b (make-symbol "NS-SAME")))
    (eval `(let ((held (list :identity)))
             (flet ((,a () (cons :a held)) (,b () (cons :b held)))
               (list (lambda () (list (,a) (,b) (function ,a) (function ,b)))
                     (function ,a) (function ,b)))))))
(dotimes (i 60)
  (let ((result (funcall (car *ns-basic*) 1)))
    (assert (eq :variable (car result)))
    (assert (= (1+ i) (cadr result)))
    (assert (eq (caddr result) (cadr *ns-basic*))))
  (assert (= (1+ i) (funcall (caddr *ns-basic*))))
  (assert (eq :outer (funcall (car *ns-shadow*))))
  (assert (eq (funcall (car *ns-labels*) 6) (funcall (cadr *ns-labels*) 0)))
  (assert (eq :odd (funcall (caddr *ns-labels*) 0)))
  (let ((result (funcall (car *ns-distinct*))))
    (assert (eq :a (caar result)))
    (assert (eq :b (caadr result)))
    (assert (eq (cdar result) (cdadr result)))
    (assert (eq (caddr result) (cadr *ns-distinct*)))
    (assert (eq (cadddr result) (caddr *ns-distinct*)))
    (assert (not (eq (caddr result) (cadddr result))))))
(assert (= 60 (funcall (caddr *ns-basic*))))
(format t "RESULT captured-namespace-ok~%")
(format t "TIERS ~{~A~^ ~}~%"
  (mapcar #'egcl-ext:function-tier
    (append *ns-basic* *ns-shadow* *ns-labels* *ns-distinct*)))
"#;

fn run(extra: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_egcl"));
    cmd.args(["--no-init", "--eval", PROGRAM]);
    for key in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_LAZY_COMPILE",
        "EGCL_LAZY_THRESHOLD",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_PROFILING_DISABLED",
        "EGCL_DISABLE_T2",
        "EGCL_DISABLE_CAPTURED_CLOSURE_FUNCTIONS",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        cmd.env_remove(key);
    }
    // Creators run once, below the lazy threshold; every escaped closure
    // crosses deterministic bytecode/native thresholds in the warm loop.
    cmd.env("EGCL_LAZY_THRESHOLD", "3")
        .env("EGCL_T0_T1_THRESHOLD", "1");
    cmd.envs(extra.iter().copied());
    let output = cmd.output().expect("run captured namespace fixture");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{extra:?}: {stdout}\n{stderr}");
    assert!(
        stdout.contains("RESULT captured-namespace-ok"),
        "{stdout}\n{stderr}"
    );
    stdout
}

#[test]
fn escaped_local_function_namespaces_compile_with_exact_identity() {
    let stdout = run(&[]);
    let tiers: Vec<_> = stdout
        .lines()
        .find_map(|line| line.strip_prefix("TIERS "))
        .expect("native admission evidence")
        .split_whitespace()
        .collect();
    assert_eq!(tiers.len(), 11);
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        assert!(
            tiers.iter().all(|tier| *tier == "1" || *tier == "2"),
            "{stdout}"
        );
    }
    run(&[("EGCL_FORCE_TIER", "interp")]);
    run(&[("EGCL_DISABLE_CAPTURED_CLOSURE_FUNCTIONS", "1")]);
}

#[test]
fn captured_function_namespace_compilation_survives_gc_stress() {
    for stride in ["1", "3", "7"] {
        run(&[
            ("EGCL_GC_STRESS", stride),
            ("EGCL_GC_POISON", "1"),
            ("EGCL_GC_STRESS_AFTER_INIT", "1"),
        ]);
    }
}
