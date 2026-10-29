// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A non-local exit out of a LET whose binding a closure captures must leave
//! that LET's heap EnvFrame. Every handler that resumes its activation — CATCH,
//! BLOCK, TAGBODY, an UNWIND-PROTECT cleanup, a HANDLER-CASE clause, a
//! RESTART-CASE return — resumes in the frame it was established in. Before
//! bliss-uhsp5 the bytecode tiers kept the inner child frame after the exit, so
//! a later read of the shadowed name saw the INNER binding and a later closure
//! captured the wrong frame. The tree-walker is the oracle.
use std::process::{Command, Output};

const PROGRAM: &str = r#"
(progn
  (defun nlx-shadow (x)
    (let ((f (lambda () x)) (g nil))
      (catch 'k (let ((x (* x 10))) (setq g (lambda () x)) (throw 'k nil)))
      (let ((h (lambda () x)))
        (list x (funcall f) (funcall g) (funcall h)
              (progn (setq x (+ x 1)) (list x (funcall f) (funcall g)))))))
  (defun nlx-uwp (x)
    (let ((f (lambda () x)) (log nil))
      (catch 'k
        (unwind-protect
             (let ((x (+ x 1))) (push (funcall (lambda () x)) log) (throw 'k nil))
          (push x log)))
      (list x (funcall f) log)))
  (defun nlx-block (x)
    (let ((f (lambda () x)) (g nil))
      (block b
        (let ((x (* x 10)))
          (setq g (lambda () x))
          (when (> x -1) (return-from b nil))))
      (list x (funcall f) (funcall g) (funcall (lambda () x)))))
  (defun nlx-go (x)
    (let ((f (lambda () x)) (g nil) (n 0))
      (tagbody
       top
         (let ((x (+ x 100 n)))
           (setq g (lambda () x))
           (incf n)
           (when (< n 3) (go top))
           (go out))
       out)
      (list x n (funcall f) (funcall g) (funcall (lambda () x)))))
  (defun nlx-closure-return (x)
    (let ((f (lambda () x)))
      (block b
        (let ((x (* x 2)))
          (mapc (lambda (e) (when e (return-from b x))) (list x))))
      (list x (funcall f) (funcall (lambda () x)))))
  (defun nlx-handler-case (x)
    (let ((f (lambda () x)))
      (list (handler-case (let ((x (* x 3))) (funcall (lambda () x)) (error "e"))
              (error () (list x (funcall (lambda () x)))))
            (funcall f))))
  (defun nlx-restart (x)
    (let ((f (lambda () x)))
      (list (restart-case (let ((x (* x 5))) (funcall (lambda () x)) (invoke-restart 'r x))
              (r (v) (list v x (funcall (lambda () x)))))
            (funcall f))))
  (defun nlx-thrower (v) (throw 'k v))
  (defun nlx-callee-throw (x)
    (let ((f (lambda () x)))
      (let ((r (catch 'k (let ((x (1+ x))) (funcall (lambda () x)) (nlx-thrower x)))))
        (list r x (funcall f) (funcall (lambda () x))))))
  (defun nlx-deep (x)
    (let ((f (lambda () x)))
      (catch 'k
        (let ((x (+ x 1)))
          (funcall (lambda () x))
          (let ((x (+ x 1))) (funcall (lambda () x)) (throw 'k nil))))
      (list x (funcall f) (funcall (lambda () x)))))
  (defun nlx-inner-handler (x)
    (let ((x (+ x 1)) (f nil))
      (setq f (lambda () x))
      (catch 'k (let ((x (* x 100))) (funcall (lambda () x)) (throw 'k nil)))
      (list x (funcall f) (funcall (lambda () x)))))
  (defun nlx-all (i)
    (list (nlx-shadow i) (nlx-uwp i) (nlx-block i) (nlx-go i) (nlx-closure-return i)
          (nlx-handler-case i) (nlx-restart i) (nlx-callee-throw i) (nlx-deep i)
          (nlx-inner-handler i)))
  (defparameter *fns* '(nlx-shadow nlx-uwp nlx-handler-case nlx-restart nlx-callee-throw
                        nlx-deep nlx-inner-handler))
  ;; Keep calling (bounded) until the native-capable functions are published at
  ;; T2, so the answers below come from the promoted code.
  (loop for i from 0 below 3000
        do (nlx-all i)
        until (every (lambda (f) (eql (egcl-ext:function-tier f) 2)) *fns*))
  (format t "RESULT ~s~%" (nlx-all 7))
  (format t "TIERS ~{~a~^ ~}~%" (mapcar #'egcl-ext:function-tier *fns*)))
"#;

/// The tree-walker's answer, pinned so an oracle drifting with the code cannot
/// hide a regression.
const EXPECTED: &str = "RESULT ((7 7 70 7 (8 8 70)) (7 7 (7 8)) (7 7 70 7) (7 3 7 109 7) \
                        (7 7 7) ((7 7) 7) ((35 7 7) 7) (8 7 7 7) (7 7 7) (8 8 8))";

const ALL_T2: &str = "2 2 2 2 2 2 2";

const MAPPED_LINUX: bool = cfg!(all(target_arch = "x86_64", target_os = "linux"));

fn egcl(program: &str, envs: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
    command.args(["--no-init", "--eval", program]);
    for name in [
        "EGCL_FORCE_TIER",
        "EGCL_BACKEND",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T1_T2_INVOKE_THRESHOLD",
        "EGCL_T1_T2_BACKEDGE_THRESHOLD",
        "EGCL_OSR_THRESHOLD",
        "EGCL_DISABLE_T2",
        "EGCL_T2",
        "EGCL_GC_STRESS",
        "EGCL_GC_POISON",
        "EGCL_GC_STRESS_AFTER_INIT",
    ] {
        command.env_remove(name);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    command.output().unwrap()
}

/// The result line and the tier line of one run.
fn run(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let output = egcl(program, envs);
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{envs:?}: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let line = |prefix: &str| {
        stdout
            .lines()
            .find(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("{envs:?}: no {prefix} line in {stdout}"))
            .to_string()
    };
    (line("RESULT "), line("TIERS "))
}

#[test]
fn nonlocal_exits_leave_captured_let_frames_at_every_tier() {
    for envs in [
        &[("EGCL_FORCE_TIER", "interp")][..],
        &[("EGCL_FORCE_TIER", "t0")][..],
        &[("EGCL_FORCE_TIER", "t1")][..],
        &[("EGCL_FORCE_TIER", "t2")][..],
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T1_T2_INVOKE_THRESHOLD", "5"),
        ][..],
    ] {
        let (result, tiers) = run(PROGRAM, envs);
        assert_eq!(result, EXPECTED, "{envs:?} ({tiers})");
        if MAPPED_LINUX && envs[0] == ("EGCL_FORCE_TIER", "t2") {
            assert_eq!(
                tiers,
                format!("TIERS {ALL_T2}"),
                "{envs:?} did not reach T2"
            );
        }
    }
}

/// The landing replaces the activation's frame with the handler's, and the
/// promoted code reconstructs handler frames from its scope map. Under a
/// collection on every allocation, with freed nursery poisoned, every tier
/// must still compute the oracle's answer.
#[test]
fn nonlocal_exit_frames_survive_gc_stress() {
    // A pinned T2 publishes before the first call returns, so a short warmup
    // loop suffices and keeps the stressed T0 run brief.
    let program = PROGRAM.replace("below 3000", "below 3");
    for tier in ["t0", "t2"] {
        let (result, tiers) = run(
            &program,
            &[
                ("EGCL_FORCE_TIER", tier),
                ("EGCL_GC_STRESS", "1"),
                ("EGCL_GC_POISON", "1"),
                ("EGCL_GC_STRESS_AFTER_INIT", "1"),
            ],
        );
        assert_eq!(result, EXPECTED, "{tier} under GC stress ({tiers})");
        if MAPPED_LINUX && tier == "t2" {
            assert_eq!(tiers, format!("TIERS {ALL_T2}"));
        }
    }
}
