// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Single-float arithmetic and comparisons compiled natively (bliss-9bw59,
//! bliss-w67f1). The Linux mapped emitter used to refuse every `ConstFloat`, so
//! `(+ x 1.0)` never left the interpreter, and a single-float recursion ran
//! ~100x slower than its fixnum twin. Now constants, `+ - *` and `< <= > >= =`
//! on single-floats compile at T1 and T2. These tests pin two things:
//!
//! * the answers are the interpreter's at every tier, including contagion with
//!   fixnums and doubles, overflow to infinity, NaN and signed zero; and
//! * on x86-64 Linux the speculated native code really runs: the function
//!   reaches its tier, and a non-float argument fails a native guard (the deopt
//!   counter moves), which only compiled float code can do.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Definitions shared by every tier. Each function is warmed with
/// single-floats so the profile at each site says single-float.
const DEFINITIONS: &str = "\
    (defun fadd1 (x) (+ x 1.0)) \
    (defun fsub (x y) (- x y)) \
    (defun fmul (x y) (* x y)) \
    (defun fmix (x) (* x 3)) \
    (defun fbig (x) (+ x 16777217)) \
    (defun flt (x y) (< x y)) \
    (defun fle (x y) (<= x y)) \
    (defun fgt (x y) (> x y)) \
    (defun fge (x y) (>= x y)) \
    (defun feq (x y) (= x y)) \
    (defun fltk (x) (if (< x 2.0) 'small 'large)) \
    (defun ffib (n) (if (< n 2.0) n (+ (ffib (- n 1.0)) (ffib (- n 2.0))))) \
    (dotimes (i 60) \
      (fadd1 0.5) (fsub 2.5 1.0) (fmul 1.5 2.0) (fmix 1.5) (fbig 1.0) \
      (flt 1.0 2.0) (fle 1.0 2.0) (fgt 1.0 2.0) (fge 1.0 2.0) (feq 1.0 2.0) \
      (fltk 1.0) (fltk 3.0)) \
    (ffib 12.0)";

/// Every result, printed readably. Comparisons with a NaN print whatever the
/// interpreter answers; a native comparison must defer to it, not to IEEE.
const PROBES: &str = "\
    (let* ((inf (fmul 3e38 10.0)) (nan (fsub inf inf))) \
      (format t \"RESULT ~S~%\" \
        (list (fadd1 0.5) (fadd1 -1.0) (fadd1 2) (fadd1 1/2) (fadd1 1d0) \
              (fsub 0.1 0.3) (fsub -0.0 0.0) (fmul 1.1 1.1) (fmul -2.0 0.0) \
              inf (fmul -3e38 10.0) (fadd1 inf) (fsub inf inf) (fmul 1e-30 1e-30) \
              (fmix 2.5) (fmix 7) (fbig 1.0) (fbig 0.5) \
              (flt 1.0 2.0) (flt 2.0 1.0) (flt 1.0 1.0) (flt 1 2.0) (flt 1.0 2d0) \
              (fle 1.0 1.0) (fle 2.0 1.0) (fgt 2.0 1.0) (fgt 1.0 1.0) \
              (fge 1.0 1.0) (fge 1.0 2.0) (feq 1.0 1.0) (feq 1.0 2.0) \
              (feq -0.0 0.0) (feq 2 2.0) (flt -0.0 0.0) (fle inf inf) (flt 1.0 inf) \
              (flt nan 1.0) (fle nan 1.0) (fgt nan 1.0) (fge 1.0 nan) (feq nan nan) \
              (fltk 1.5) (fltk 2.0) (fltk 1) (fltk 1/2) \
              (ffib 15.0) (ffib 10.5) (ffib 5) (ffib 4d0))))";

fn run(envs: &[(&str, &str)]) -> String {
    let program = format!("(progn {DEFINITIONS} {PROBES})");
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", &program]).env("EGCL_LAZY_COMPILE", "0");
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "egcl failed with {envs:?}\nstdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
        .lines()
        .find(|line| line.starts_with("RESULT"))
        .unwrap_or_else(|| panic!("no RESULT line with {envs:?}: {stdout}"))
        .to_string()
}

#[test]
fn single_float_results_match_the_interpreter_at_every_tier() {
    let interpreted = run(&[("EGCL_BACKEND", "tree-walker")]);
    // Pin the interpreter's own answers, so a regression there is not silently
    // copied into every tier. The NaN comparisons are the interpreter's, not
    // IEEE's; native code deopts on an unordered compare to keep them.
    let expected = "RESULT (1.5 0.0 3.0 1.5 2.0d0 \
        -0.20000002 -0.0 1.21 -0.0 inf -inf inf NaN 0.0 \
        7.5 21 1.6777218e7 1.6777218e7 \
        T NIL NIL T T T NIL T NIL T NIL T NIL T T NIL T T \
        NIL T NIL T T \
        SMALL LARGE SMALL SMALL \
        610.0 99.5 5.0 3.0d0)";
    assert_eq!(interpreted, expected);
    for tier in ["t0", "t1", "t2"] {
        let compiled = run(&[("EGCL_FORCE_TIER", tier)]);
        assert_eq!(compiled, interpreted, "tier {tier} differs from the interpreter");
    }
    // The ordinary promotion path, with a warm profile rather than a forced tier.
    let promoted = run(&[
        ("EGCL_T0_T1_THRESHOLD", "2"),
        ("EGCL_T2_THRESHOLD", "20"),
        ("EGCL_T2_THREADS", "1"),
    ]);
    assert_eq!(promoted, interpreted, "promoted tiers differ from the interpreter");
}

/// Run `program` and return (stdout, stderr).
fn run_logged(program: &str, envs: &[(&str, &str)]) -> (String, String) {
    let mut command = Command::new(BIN);
    command.args(["--no-init", "--eval", program]).env("EGCL_LAZY_COMPILE", "0");
    for (key, value) in envs {
        command.env(key, value);
    }
    let out = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "{stdout}\n{stderr}");
    (stdout, stderr)
}

/// T1 alone (T2 disabled): the literal-float function installs at T1, runs a
/// warm batch with no deopt, and a ratio argument fails its native
/// single-float guard -- proof the guarded float code is what executes.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn single_float_t1_runs_native_and_guards() {
    let program = "(progn \
        (defun fadd1 (x) (+ x 1.0)) \
        (defun flt (x y) (< x y)) \
        (dotimes (i 40) (fadd1 0.5) (flt 1.0 2.0)) \
        (let ((before (egcl-ext:deopt-count)) (sum 0.0)) \
          (dotimes (i 1000) (setq sum (fadd1 sum)) (flt sum 2.0)) \
          (let ((steady (- (egcl-ext:deopt-count) before))) \
            (format t \"T1 ~S ~S ~S ~S~%\" sum steady \
              (egcl-ext:function-tier 'fadd1) (egcl-ext:function-tier 'flt)) \
            (let ((before (egcl-ext:deopt-count))) \
              (format t \"GUARD ~S ~S ~S~%\" (fadd1 1/2) (flt 1/3 1/2) \
                (- (egcl-ext:deopt-count) before))))))";
    let (stdout, stderr) = run_logged(
        program,
        &[("EGCL_T0_T1_THRESHOLD", "2"), ("EGCL_DISABLE_T2", "1"), ("EGCL_T2_LOG", "stderr")],
    );
    assert!(stdout.contains("T1 1000.0 0 1 1"), "{stdout}\n{stderr}");
    assert!(!stderr.contains("FADD1: T1 declined"), "{stderr}");
    assert!(stdout.contains("GUARD 1.5 T 2"), "both native guards must fail once: {stdout}\n{stderr}");
}

/// T2: the single-float Fibonacci compiles every arithmetic and comparison
/// site as single-float, installs native T2 code (the background compile is
/// awaited by polling its tier), and its steady state takes no deopt.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn single_float_recursion_reaches_t2_with_float_speculation() {
    let program = "(progn \
        (defun ffib (n) (if (< n 2.0) n (+ (ffib (- n 1.0)) (ffib (- n 2.0))))) \
        (loop repeat 20000 until (eql 2 (egcl-ext:function-tier 'ffib)) do (ffib 8.0)) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"T2 ~S ~S ~S~%\" (ffib 20.0) \
            (- (egcl-ext:deopt-count) before) (egcl-ext:function-tier 'ffib))))";
    let (stdout, stderr) = run_logged(
        program,
        &[
            ("EGCL_T0_T1_THRESHOLD", "2"),
            ("EGCL_T2_THRESHOLD", "20"),
            ("EGCL_T2_THREADS", "1"),
            ("EGCL_T2_LOG", "stderr"),
        ],
    );
    assert!(stdout.contains("T2 6765.0 0 2"), "{stdout}\n{stderr}");
    assert!(stderr.contains("FFIB: speculated 4 site(s)"), "{stderr}");
    assert!(
        stderr.contains("FFIB: optimized worker result installed")
            || stderr.contains("FFIB: T2 installed"),
        "{stderr}"
    );
    assert!(!stderr.contains("FFIB: blacklisted"), "{stderr}");
}
