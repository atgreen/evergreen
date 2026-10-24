// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Double-float arithmetic and comparisons compiled natively and unboxed
//! (bliss-thcbb, GitHub #594). Doubles are heap-boxed, so a generic `+` on
//! them cost a call and an allocation per operation at every tier. T2 now
//! speculates double-float sites, computes them on raw `f64` values kept in
//! non-root homes across arithmetic chains and loop iterations, and boxes only
//! where a value escapes. These tests pin:
//!
//! * the answers are the interpreter's at every tier, including contagion with
//!   fixnum constants and variables and with single-floats, signed zero,
//!   overflow to infinity, NaN comparisons, and the interpreter's own
//!   `(+ -0d0 -0d0)` = `0.0d0`;
//! * on x86-64 Linux the raw native code really runs: functions install a raw
//!   T2 candidate, reach tier 2 and take no deopt in steady state, and a
//!   non-double operand fails a native guard, which retires that speculation;
//! * the escape boxes are safe under a moving collector.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

/// Definitions shared by every tier, each warmed with doubles (and, where
/// contagion is the point, fixnum variables).
const DEFINITIONS: &str = "\
    (defun dadd1 (x) (+ x 1d0)) \
    (defun dadd (x y) (+ x y)) \
    (defun dsub (x y) (- x y)) \
    (defun dmul (x y) (* x y)) \
    (defun dmix (x) (* x 3)) \
    (defun dbig (x) (+ x 1152921504606846975)) \
    (defun dsing (x) (+ x 1.5)) \
    (defun dscale (i) (* i 0.001d0)) \
    (defun dmixv (x y) (+ x y)) \
    (defun dlt (x y) (< x y)) \
    (defun dle (x y) (<= x y)) \
    (defun dgt (x y) (> x y)) \
    (defun dge (x y) (>= x y)) \
    (defun deq (x y) (= x y)) \
    (defun dltk (x) (if (< x 2d0) 'small 'large)) \
    (defun dltbig (x) (< x 9007199254740993)) \
    (defun dfib (n) (if (< n 2d0) n (+ (dfib (- n 1d0)) (dfib (- n 2d0))))) \
    (defun dsum (n x) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s x))))) \
    (defun darg (n x) (let ((s x)) (dotimes (i n s) (setq s (+ s x))))) \
    (defun dpoly (n x) (let ((s 0d0)) (dotimes (i n s) (setq s (+ (* s x) 1d0))))) \
    (defun dwhile (limit) (let ((s 0d0) (k 0)) \
      (loop while (< s limit) do (setq s (+ s 0.5d0) k (1+ k))) (list s k))) \
    (defun ddist (x y z) (+ (* x x) (+ (* y y) (* z z)))) \
    (defun dvec (v) (+ (* (svref v 0) (svref v 0)) (* (svref v 1) (svref v 1)))) \
    (defun dlerp (a b u) (+ a (* (- b a) u))) \
    (dotimes (i 60) \
      (dadd1 0.5d0) (dadd 1d0 2d0) (dsub 2.5d0 1d0) (dmul 1.5d0 2d0) (dmix 1.5d0) \
      (dbig 1d0) (dsing 1d0) (dscale i) (dmixv 1.5d0 i) \
      (dlt 1d0 2d0) (dle 1d0 2d0) (dgt 1d0 2d0) (dge 1d0 2d0) (deq 1d0 2d0) \
      (dltk 1d0) (dltk 3d0) (dltbig 1d0) (dsum 10 1d0) (darg 10 1d0) (dpoly 10 0.5d0) \
      (dwhile 3d0) (ddist 1d0 2d0 3d0) (dvec (vector 3d0 4d0)) (dlerp 1d0 3d0 0.25d0)) \
    (dfib 12d0)";

/// Every result, printed readably. NaN comparisons print whatever the
/// interpreter answers; native code deopts on an unordered compare instead.
const PROBES: &str = "\
    (let* ((inf (dmul 1d300 1d300)) (nan (dsub inf inf))) \
      (format t \"RESULT ~S~%\" \
        (list (dadd1 0.5d0) (dadd1 -1d0) (dadd1 2) (dadd1 1/2) (dadd1 1.0) \
              (dadd1 most-positive-fixnum) \
              (dadd -0d0 -0d0) (dadd -0d0 0d0) (dadd 0d0 -0d0) (dadd 1d0 -1d0) (dadd 2 3) \
              (dadd 1d0 2) (dsub 0.1d0 0.3d0) (dsub -0d0 0d0) (dsub 0d0 0d0) \
              (dmul 1.1d0 1.1d0) (dmul -2d0 0d0) \
              inf (dmul -1d300 1d300) (dadd1 inf) (dsub inf inf) (dmul 1d-300 1d-300) \
              (dmul nan 2d0) (dmix 2.5d0) (dmix 7) (dbig 1d0) (dbig 0.5d0) \
              (dsing 1d0) (dsing 0.1d0) (dsing 2) \
              (dscale 7) (dscale 3000) (dscale most-positive-fixnum) (dscale 1/2) (dscale 0.5) \
              (dmixv 1.5d0 2) (dmixv 2 1.5d0) (dmixv 2 3) (dmixv 1.5d0 1/2) \
              (dmixv 0.5d0 -7) (dmixv 1d0 most-positive-fixnum) (dmixv 1.25d0 0.5) \
              (dlt 1d0 2d0) (dlt 2d0 1d0) (dlt 1d0 1d0) (dlt 1 2d0) (dlt 1d0 2.0) \
              (dle 1d0 1d0) (dle 2d0 1d0) (dgt 2d0 1d0) (dgt 1d0 1d0) \
              (dge 1d0 1d0) (dge 1d0 2d0) (deq 1d0 1d0) (deq 1d0 2d0) \
              (deq -0d0 0d0) (deq 2 2d0) (dlt -0d0 0d0) (dle inf inf) (dlt 1d0 inf) \
              (dlt nan 1d0) (dle nan 1d0) (dgt nan 1d0) (dge 1d0 nan) (deq nan nan) \
              (dltk 1.5d0) (dltk 2d0) (dltk 1) (dltk 1/2) \
              (dltbig 9.007199254740992d15) (dltbig 1d0) \
              (dfib 15d0) (dfib 10.5d0) (dfib 5) (dfib 4.0) \
              (dsum 1000 0.1d0) (dsum 3 1) (dsum 0 'x) (dsum 4 1.5) \
              (darg 1000 0.1d0) (darg 3 1) (darg 0 'x) (darg 2 -0d0) \
              (dpoly 50 0.5d0) (dpoly 5 2) (dwhile 10d0) (dwhile 2.25d0) \
              (ddist 1d0 2d0 3d0) (ddist 1 2 3) (ddist 1d0 2 3.5d0) (ddist -0d0 -0d0 -0d0) \
              (dvec (vector 3d0 4d0)) (dvec (vector 3 4)) (dvec (vector 0.5d0 1.5d0)) \
              (dlerp 1d0 3d0 0.25d0) (dlerp 1d0 3d0 0.75d0) (dlerp 2 4 1/2))))";

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
fn double_float_results_match_the_interpreter_at_every_tier() {
    let interpreted = run(&[("EGCL_BACKEND", "tree-walker")]);
    // Pin the interpreter's own answers, so a regression there is not silently
    // copied into every tier. `(+ -0d0 -0d0)` is 0.0d0 because the interpreter
    // folds onto a +0.0 seed; NaN comparisons are the interpreter's, not IEEE's.
    let expected = "RESULT (1.5d0 0.0d0 3.0d0 1.5d0 2.0d0 1.152921504606847d18 \
        0.0d0 0.0d0 0.0d0 0.0d0 5 3.0d0 -0.19999999999999998d0 -0.0d0 0.0d0 \
        1.2100000000000002d0 -0.0d0 inf -inf inf NaN 0.0d0 NaN 7.5d0 21 \
        1.152921504606847d18 1.152921504606847d18 2.5d0 1.6d0 3.5 \
        0.007d0 3.0d0 1.152921504606847d15 5.0d-4 5.0d-4 \
        3.5d0 3.5d0 5 2.0d0 -6.5d0 1.152921504606847d18 1.75d0 \
        T NIL NIL T T T NIL T NIL T NIL T NIL T T NIL T T \
        NIL T NIL T T \
        SMALL LARGE SMALL SMALL T T \
        610.0d0 99.5d0 5.0d0 3.0d0 \
        99.9999999999986d0 3.0d0 0.0d0 6.0d0 \
        100.09999999999859d0 4 X 0.0d0 \
        1.9999999999999982d0 31.0d0 (10.0d0 20) (2.5d0 5) \
        14.0d0 14 17.25d0 0.0d0 25.0d0 25 2.5d0 1.5d0 2.5d0 3)";
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
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
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

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
const PROMOTE: [(&str, &str); 4] = [
    ("EGCL_T0_T1_THRESHOLD", "2"),
    ("EGCL_T2_THRESHOLD", "20"),
    ("EGCL_T2_THREADS", "1"),
    ("EGCL_T2_LOG", "stderr"),
];

/// The #594 accumulation loops and a double recursion install raw-double T2
/// code: each logs a raw candidate whose emitted code holds raw f64 values,
/// reaches tier 2, and computes its steady state without a deopt.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn double_float_loops_and_recursion_run_raw_at_t2() {
    let program = "(progn \
        (defun loop-double-literal (n) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s 1d0))))) \
        (defun loop-double-arg (n x) (let ((s x)) (dotimes (i n s) (setq s (+ s x))))) \
        (defun loop-double-declared (n x) \
          (declare (fixnum n) (double-float x) (optimize (speed 3) (safety 0))) \
          (let ((s x)) (declare (double-float s)) (dotimes (i n s) (setq s (+ s x))))) \
        (defun dfib (n) (if (< n 2d0) n (+ (dfib (- n 1d0)) (dfib (- n 2d0))))) \
        (loop repeat 20000 \
              until (every (lambda (f) (eql 2 (egcl-ext:function-tier f))) \
                           '(loop-double-literal loop-double-arg loop-double-declared dfib)) \
              do (loop-double-literal 50) (loop-double-arg 50 1d0) \
                 (loop-double-declared 50 1d0) (dfib 6d0)) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"T2 ~S ~S ~S ~S ~S ~S~%\" \
            (loop-double-literal 100000) (loop-double-arg 100000 0.5d0) \
            (loop-double-declared 1000 0.25d0) (dfib 20d0) \
            (- (egcl-ext:deopt-count) before) \
            (mapcar #'egcl-ext:function-tier \
              '(loop-double-literal loop-double-arg loop-double-declared dfib)))))";
    let (stdout, stderr) = run_logged(program, &PROMOTE);
    assert!(
        stdout.contains("T2 100000.0d0 50000.5d0 250.25d0 6765.0d0 0 (2 2 2 2)"),
        "{stdout}\n{stderr}"
    );
    for name in ["LOOP-DOUBLE-LITERAL", "LOOP-DOUBLE-ARG", "LOOP-DOUBLE-DECLARED", "DFIB"] {
        assert!(stderr.contains(&format!("{name}: raw double candidate: speculated")), "{name}\n{stderr}");
        // Either ABI holds raw values: a raw body now usually gets the
        // ordinary x86-64 Lisp entry (bliss-k4psf).
        let raw_emitted = stderr.lines().any(|line| {
            (line.starts_with(&format!("[T2] {name}: mapped T2 emitted"))
                || line.starts_with(&format!("[T2] {name}: ordinary x86-64 Lisp T2 emitted")))
                && !line.ends_with("raw_f64_values=0")
        });
        assert!(raw_emitted, "{name} must install raw f64 code\n{stderr}");
        assert!(!stderr.contains(&format!("{name}: raw double T2 emission declined")), "{stderr}");
    }
}

/// A ratio reaching a raw double site fails its native unboxing guard: the
/// call deopts once, answers as the interpreter does, and the site's raw
/// speculation is retired, so the replacement takes later fixnum and ratio
/// operands without deopting again.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn failed_double_guard_retires_the_raw_speculation() {
    let program = "(progn \
        (defun dadd (x y) (+ x y)) \
        (loop repeat 20000 until (eql 2 (egcl-ext:function-tier 'dadd)) do (dadd 1d0 2d0)) \
        (let ((before (egcl-ext:deopt-count))) \
          (dotimes (i 1000) (dadd 1d0 2d0)) \
          (format t \"STEADY ~S~%\" (- (egcl-ext:deopt-count) before))) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"GUARD ~S ~S~%\" (dadd 1/2 1d0) (- (egcl-ext:deopt-count) before))) \
        (loop repeat 20000 until (eql 2 (egcl-ext:function-tier 'dadd)) do (dadd 1d0 2d0)) \
        (let ((before (egcl-ext:deopt-count))) \
          (format t \"AFTER ~S ~S ~S~%\" \
            (list (dadd 2 3) (dadd 1/2 1/2) (dadd 1d0 2d0)) \
            (- (egcl-ext:deopt-count) before) (egcl-ext:function-tier 'dadd))))";
    let (stdout, stderr) = run_logged(program, &PROMOTE);
    assert!(stdout.contains("STEADY 0"), "{stdout}\n{stderr}");
    assert!(stdout.contains("GUARD 1.5d0 1"), "the native guard must fail once: {stdout}\n{stderr}");
    assert!(stderr.contains("DADD: native guard deopt #1 => resuming interpreted (raw double speculation failed"),
        "{stderr}");
    assert!(stdout.contains("AFTER (5 1 3.0d0) 0 2"), "{stdout}\n{stderr}");
}

/// The escape box allocates, so a collection can run while raw and tagged
/// values are live in native homes. Under forced collections and nursery
/// poisoning the answers must equal an unstressed run.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn escape_boxes_survive_gc_stress() {
    let program = "(progn \
        (defun keep (v x) (let ((r (+ x 1d0))) (cons v r))) \
        (defun lerp3 (a b u) (list (+ a (* (- b a) u)) (- a u) (* a b))) \
        (defun acc (n x) (let ((s 0d0)) (dotimes (i n s) (setq s (+ s (* x 0.5d0)))))) \
        (loop repeat 200 \
              until (every (lambda (f) (eql 2 (egcl-ext:function-tier f))) '(keep lerp3 acc)) \
              do (keep (cons 1 2) 1d0) (lerp3 1d0 3d0 0.5d0) (acc 5 1d0)) \
        (format t \"TIERS ~S~%\" (mapcar #'egcl-ext:function-tier '(keep lerp3 acc))) \
        (format t \"RESULT ~S~%\" \
          (list (loop for i below 20 collect (keep (list i) (* i 1d0))) \
                (loop for i below 10 collect (lerp3 (* i 1d0) 7d0 0.25d0)) \
                (acc 200 0.1d0))))";
    let plain = &[("EGCL_FORCE_TIER", "t2")];
    let (expected, stderr) = run_logged(program, plain);
    assert!(expected.contains("TIERS (2 2 2)"), "{expected}\n{stderr}");
    let (stressed, stderr) = run_logged(program, &[
        ("EGCL_FORCE_TIER", "t2"),
        ("EGCL_GC_STRESS", "1"),
        ("EGCL_GC_POISON", "1"),
        ("EGCL_GC_STRESS_AFTER_INIT", "1"),
    ]);
    assert!(stressed.contains("TIERS (2 2 2)"), "{stressed}
{stderr}");
    let result = |out: &str| out.lines().find(|line| line.starts_with("RESULT")).map(str::to_owned);
    assert!(result(&expected).is_some(), "{expected}");
    assert_eq!(result(&stressed), result(&expected), "{stressed}\n{stderr}");
}

/// GH #594's STEP-PLAIN (bliss-thcbb.1): a declared fixnum times a double
/// literal, products of a propagated double constant, and a three-operand
/// `+` all compute raw. The fixnum's contagion guard survives guard
/// elimination, and `+`, `-` and `*` of three or more operands split into
/// binary raw links. Answers, including signed zeros and mixed operands that
/// deopt, are the interpreter's.
#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn fixnum_contagion_and_variadic_arithmetic_run_raw_at_t2() {
    let program = "(progn \
        (defun step-plain (i) (declare (fixnum i) (optimize (speed 3) (safety 0))) \
          (let ((px (* i 1d-3)) (py (* i 2d-3)) (pz 1.5d0)) (declare (double-float px py pz)) \
            (+ (* px px) (* py py) (* pz pz)))) \
        (defun a3 (a b c) (+ a b c)) \
        (defun s3 (a b c) (- a b c)) \
        (defun m3 (a b c) (* a b c)) \
        (loop for j from 0 repeat 5000 \
              until (every (lambda (f) (eql 2 (egcl-ext:function-tier f))) '(step-plain a3 s3 m3)) \
              do (step-plain j) (a3 1d0 2d0 3d0) (s3 1d0 2d0 3d0) (m3 1d0 2d0 3d0)) \
        (format t \"RESULT ~S~%\" (list (step-plain 7) \
          (a3 1d0 2d0 3d0) (a3 -0d0 -0d0 -0d0) (a3 1 2 3d0) (a3 1 2 3) (a3 1/2 1/3 1d0) (a3 0.1d0 0.2d0 0.3d0) \
          (s3 1d0 2d0 3d0) (s3 -0d0 0d0 0d0) (s3 3 1 0.5d0) \
          (m3 1d0 2d0 3d0) (m3 -0d0 1d0 1d0) (m3 2 3 0.5d0) (m3 2 3 4))))";
    let expected = "RESULT (2.250245d0 6.0d0 0.0d0 6.0d0 6 1.8333333333333335d0 \
        0.6000000000000001d0 -4.0d0 -0.0d0 1.5d0 6.0d0 -0.0d0 3.0d0 24)";
    let (interpreted, _) = run_logged(program, &[("EGCL_BACKEND", "tree-walker")]);
    assert!(interpreted.contains(expected), "{interpreted}");
    let (stdout, stderr) = run_logged(program, &PROMOTE);
    assert!(stdout.contains(expected), "{stdout}\n{stderr}");
    let raw_emitted = stderr.lines().any(|line| {
        (line.starts_with("[T2] STEP-PLAIN: mapped T2 emitted")
            || line.starts_with("[T2] STEP-PLAIN: ordinary x86-64 Lisp T2 emitted"))
            && !line.ends_with("raw_f64_values=0")
    });
    assert!(raw_emitted, "STEP-PLAIN must install raw f64 code\n{stderr}");
    assert!(!stderr.contains("STEP-PLAIN: raw double T2 emission declined"), "{stderr}");
}
