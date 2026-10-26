// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! WITH forces the general arithmetic LOOP lowering. Expected records were
//! checked with SBCL: initial arithmetic forms see the outer I, run once in
//! source order, and precede INITIALLY even for a zero-trip loop.
use std::process::Command;

const PROGRAM: &str = r#"
    (defvar *arithmetic-loop-events* nil)
    (declaim (notinline arithmetic-loop-event))
    (defun arithmetic-loop-event (tag value)
      (push tag *arithmetic-loop-events*) value)
    (defun arithmetic-loop-up (start bound stride)
      (let ((i 20))
        (setq *arithmetic-loop-events* nil)
        (loop with seed = (arithmetic-loop-event :with 7)
              for i from (arithmetic-loop-event :start start)
                    below (arithmetic-loop-event (list :bound i seed) bound)
                    by (arithmetic-loop-event (list :step i seed) stride)
              initially (arithmetic-loop-event (list :initially i seed) nil)
                        (setq bound 1000 stride 1000)
              do (arithmetic-loop-event (list :body i) nil)
                 (setq bound -1000 stride 2000)
              finally (arithmetic-loop-event (list :finally i) nil)
                      (return (list i (reverse *arithmetic-loop-events*))))))
    (defun arithmetic-loop-down (start bound stride)
      (let ((i 20))
        (setq *arithmetic-loop-events* nil)
        (loop with seed = (arithmetic-loop-event :with 7)
              for i downfrom (arithmetic-loop-event :start start)
                    above (arithmetic-loop-event (list :bound i seed) bound)
                    by (arithmetic-loop-event (list :step i seed) stride)
              initially (arithmetic-loop-event (list :initially i seed) nil)
                        (setq bound -1000 stride 1000)
              do (arithmetic-loop-event (list :body i) nil)
                 (setq bound 1000 stride 2000)
              finally (arithmetic-loop-event (list :finally i) nil)
                      (return (list i (reverse *arithmetic-loop-events*))))))
    (defun check-arithmetic-loops ()
      (assert (equal (arithmetic-loop-up 1 4 1)
        '(4 (:with :start (:bound 20 7) (:step 20 7) (:initially 1 7)
             (:body 1) (:body 2) (:body 3) (:finally 4)))))
      (assert (equal (arithmetic-loop-down 5 0 2)
        '(-1 (:with :start (:bound 20 7) (:step 20 7) (:initially 5 7)
              (:body 5) (:body 3) (:body 1) (:finally -1)))))
      (assert (equal (arithmetic-loop-up 5 0 2)
        '(5 (:with :start (:bound 20 7) (:step 20 7) (:initially 5 7) (:finally 5)))))
      (assert (equal (arithmetic-loop-down 0 5 2)
        '(0 (:with :start (:bound 20 7) (:step 20 7) (:initially 0 7) (:finally 0))))))
    (dotimes (i 120) (check-arithmetic-loops))
    (check-arithmetic-loops)
    (format t "ARITHMETIC-LOOP-ONCE-OK~%")
"#;

#[test]
fn general_arithmetic_loop_initial_forms_run_once_across_tiers() {
    let mut results = Vec::new();
    for tier in ["t0", "t1", "t2"] {
        let output = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", tier)
            .env("EGCL_T2_THREADS", "1")
            .env("EGCL_T2_LOG", "stderr")
            .args(["--no-init", "--eval", PROGRAM])
            .output()
            .expect("run arithmetic LOOP case");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success(), "tier={tier}\n{stdout}\n{stderr}");
        assert!(stdout.contains("ARITHMETIC-LOOP-ONCE-OK"), "{stdout}");
        assert!(
            !stderr
                .lines()
                .any(|line| line.contains("ARITHMETIC-LOOP-UP:")
                    && line.contains("not compiled to bytecode")),
            "general LOOP fell back:\n{stderr}"
        );
        results.push(stdout);
    }
    assert!(results.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn allocating_arithmetic_loop_expansion_preserves_initial_forms_under_gc_stress() {
    // Fewer repetitions keep stress focused on the allocating expansion and
    // each initializer's fresh list; output is compared to the ordinary run.
    let program = PROGRAM.replace("(dotimes (i 120)", "(dotimes (i 3)");
    let mut results = Vec::new();
    for stress in [false, true] {
        let mut command = Command::new(env!("CARGO_BIN_EXE_egcl"));
        command
            .env("EGCL_LAZY_COMPILE", "0")
            .env("EGCL_FORCE_TIER", "t0")
            .args(["--no-init", "--eval", &program]);
        if stress {
            command
                .env("EGCL_GC_STRESS", "1")
                .env("EGCL_GC_STRESS_AFTER_INIT", "1")
                .env("EGCL_GC_POISON", "1");
        }
        let output = command.output().expect("run arithmetic LOOP expansion");
        let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "stress={stress}\n{stdout}\n{stderr}"
        );
        assert!(stdout.contains("ARITHMETIC-LOOP-ONCE-OK"));
        results.push(stdout);
    }
    assert_eq!(results[0], results[1]);
}
