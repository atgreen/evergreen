// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! FUNCALL of a closure whose compiled code never reads its receiver
//! (bliss-gg3i1.7.27). A closure that captures nothing and calls other
//! functions gets ordinary T2 code with no explicit-callable slot; its
//! instances' entry words were never armed, so every FUNCALL from T2 code
//! went through the runtime's named adapter and a fresh activation. Any
//! instance may enter such code, so the lean FUNCALL now enters it directly.
//!
//! The answers must not change at any tier: the receiver-free closure, a
//! capturing closure (which still needs its own environment), several
//! instances of one capturing template, and a call made after the global
//! function the closure calls is redefined. With `EGCL_BUILTIN_CENSUS`,
//! FUNCALLs from ordinary T2 code to the receiver-free closure no longer
//! reach the runtime veneer: 200,000 calls reached it before.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
(progn
  (defun rf-weight (a b) (+ (* 3 a) b))
  (defun rf-make-free () (lambda (a b) (rf-weight a b)))
  (defun rf-make-captured (k) (lambda (a b) (+ (rf-weight a b) k)))
  (defun rf-drive (f n)
    (let ((sum 0))
      (dotimes (i n sum)
        (setq sum (+ sum (funcall f i 1))))))
  (let ((free (rf-make-free))
        (captured (rf-make-captured 7))
        (instances (list (rf-make-captured 1) (rf-make-captured 2) (rf-make-captured 3))))
    (dotimes (i 20) (rf-drive free 10) (rf-drive captured 10))
    (let ((r1 (rf-drive free 200000))
          (r2 (rf-drive captured 1000))
          (r3 (mapcar (lambda (f) (rf-drive f 100)) instances)))
      (defun rf-weight (a b) (- a b))
      (let ((r4 (rf-drive free 100))
            (*print-pretty* nil))
        (format t "RESULT ~S ~S ~S ~S~%" r1 r2 r3 r4)))))
"#;

const EXPECTED: &str = "RESULT 59999900000 1506500 (15050 15150 15250) 4850";

fn run(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new("timeout");
    cmd.args(["--kill-after=5", "300", BIN, "--no-init", "--eval", PROGRAM]);
    for (key, _) in std::env::vars().filter(|(key, _)| key.starts_with("EGCL_")) {
        cmd.env_remove(key);
    }
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("spawn egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        out.status.success(),
        "{envs:?}: program failed\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

fn result_line(out: &str) -> &str {
    out.lines()
        .find(|line| line.starts_with("RESULT "))
        .unwrap_or_else(|| panic!("no RESULT line in:\n{out}"))
}

#[test]
fn receiver_free_closures_answer_alike_at_every_tier() {
    for tier in [None, Some("interp"), Some("t0"), Some("t1"), Some("t2")] {
        let envs: Vec<(&str, &str)> = tier.map(|t| ("EGCL_FORCE_TIER", t)).into_iter().collect();
        assert_eq!(result_line(&run(&envs)), EXPECTED, "tier {tier:?}");
    }
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn receiver_free_funcall_skips_the_runtime_veneer() {
    let dir = std::env::temp_dir().join(format!("egcl-receiver-free-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let report = dir.join("builtins");
    let out = run(&[("EGCL_BUILTIN_CENSUS", report.to_str().unwrap())]);
    assert_eq!(result_line(&out), EXPECTED);
    let text = std::fs::read_to_string(&report).expect("builtin census report");
    // `ordinary/3=N` on the FUNCALL row counts three-argument FUNCALLs that
    // reached the runtime from ordinary T2 code.
    let ordinary = text
        .lines()
        .find(|line| line.split_whitespace().nth(3) == Some("FUNCALL"))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|field| field.strip_prefix("ordinary/3="))
                .and_then(|count| count.parse::<u64>().ok())
        })
        .unwrap_or(0);
    assert!(
        ordinary < 20_000,
        "{ordinary} FUNCALLs from ordinary T2 code reached the runtime:\n{text}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
