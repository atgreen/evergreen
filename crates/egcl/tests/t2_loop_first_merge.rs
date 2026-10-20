// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Arithmetic `LOOP FOR i ...` clauses reach T2 (bliss-fva34).
//!
//! LOOP steps its index as `(setq i (if first i (+ i 1)))`, so the index flows
//! through an if-merge block parameter declared TOP. Guard elimination proved
//! the guard on that merge value by inference and forwarded the guard's FIXNUM
//! result back to the TOP merge parameter, which then fed the back edge into
//! the FIXNUM loop-header parameter. The post-mid-end verifier rejected the
//! function ("V4 block-param type") and every such loop stayed at T1.
//!
//! The hand-written `G-MERGE` tagbody has the same shape without LOOP. After
//! warm-up, each function is fed inputs that fail its fixnum guards (a float
//! bound) and overflow the index past MOST-POSITIVE-FIXNUM; every result must
//! match the interpreter.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const SHAPES: &[&str] = &[
    "S-LOOP-COLLECT",
    "L-APPEND",
    "L-NCONC",
    "L-COLLECT-INTO",
    "L-FROM-TO",
    "L-SUM",
    "L-COUNT",
    "G-MERGE",
    "L-OVERFLOW",
];

const PROGRAM: &str = r#"
(defun s-loop-collect (n) (loop for i below n collect (* i i)))
(defun l-append (n) (loop for i below n append (list i)))
(defun l-nconc (n) (loop for i below n nconc (list i i)))
(defun l-collect-into (n)
  (loop for i below n collect i into acc finally (return (reverse acc))))
(defun l-from-to (n) (loop for i from 1 to n collect i))
(defun l-sum (n) (loop for i below n sum i))
(defun l-count (n) (loop for i below n count (evenp i)))
(defun l-overflow (lo)
  (loop for i from lo below (+ lo 5) collect (- i most-positive-fixnum)))
(defun g-merge (n)
  (let ((i 0) (s 0) (first t))
    (tagbody
     top
       (setq i (if first i (+ i 1)))
       (when (>= i n) (go end))
       (setq s (+ s 1))
       (setq first nil)
       (go top)
     end)
    s))
(defun run-all ()
  (list (s-loop-collect 5) (l-append 4) (l-nconc 3) (l-collect-into 4)
        (l-from-to 5) (l-sum 10) (l-count 7) (g-merge 9)
        (l-overflow (- most-positive-fixnum 30))))
(dotimes (k 40) (run-all))
;; T2 compiles in the background; give the installs a bounded moment to land.
(loop repeat 100
      until (every (lambda (f) (= 2 (egcl-ext:function-tier f)))
                   '(s-loop-collect l-append l-nconc l-collect-into
                     l-from-to l-sum l-count g-merge))
      do (run-all) (sleep 0.02))
(format t "WARM ~S~%" (run-all))
(format t "TIERS ~S~%"
        (mapcar #'egcl-ext:function-tier
                '(s-loop-collect l-append l-nconc l-collect-into
                  l-from-to l-sum l-count g-merge)))
(format t "FLOAT ~S~%"
        (list (s-loop-collect 2.5) (l-sum 3.5) (l-count 4.5) (g-merge 4.5)
              (l-from-to 2.0)))
(format t "OVERFLOW ~S~%" (l-overflow (- most-positive-fixnum 2)))
(format t "AFTER ~S~%" (run-all))
"#;

fn run(envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", PROGRAM]);
    for key in [
        "EGCL_T2_LOG",
        "EGCL_FORCE_TIER",
        "EGCL_DISABLE_T2",
        "EGCL_T0_T1_THRESHOLD",
        "EGCL_T2_THRESHOLD",
        "EGCL_T2_NO_QUEUE",
    ] {
        command.env_remove(key);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    assert!(
        out.status.success(),
        "egcl failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Drop the TIERS line, which legitimately differs between tiers.
fn results(stdout: &str) -> String {
    stdout
        .lines()
        .filter(|line| !line.starts_with("TIERS "))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn loop_first_merge_shapes_reach_t2_and_match_interpreter() {
    let (tiered, log) = run(&[
        ("EGCL_T2_LOG", "stderr"),
        ("EGCL_T0_T1_THRESHOLD", "5"),
        ("EGCL_T2_THRESHOLD", "20"),
    ]);
    assert!(
        !log.contains("post-mid-end verify failed"),
        "guard elimination must leave verifiable IR:\n{log}"
    );
    if cfg!(all(target_arch = "x86_64", target_os = "linux")) {
        for name in SHAPES {
            let installed = format!("[T2] {name}: optimized worker result installed");
            assert!(
                log.lines().any(|line| line.contains(&installed)),
                "{name} must install at T2:\n{log}"
            );
        }
        assert!(
            tiered.contains("TIERS (2 2 2 2 2 2 2 2)"),
            "every shape must report T2:\n{tiered}"
        );
    }

    let (interp, _) = run(&[("EGCL_FORCE_TIER", "interp")]);
    assert!(
        interp.contains("AFTER "),
        "interpreter run finished: {interp}"
    );
    assert_eq!(
        results(&tiered),
        results(&interp),
        "T2 must match interpreter"
    );
}
