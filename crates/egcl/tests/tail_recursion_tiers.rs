// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A hot self-tail-recursive function must reach the optimising tier.
//!
//! `eliminate_self_tail_calls` rewrites `CallNamed{self} ; Return` into stores
//! to the parameter slots followed by `Br(entry)`. That is a win, but it means
//! such a function has NO CALLS and NO TAGBODY: it is a loop whose only
//! back-edge is a plain `Br`. Promotion therefore depends on that `Br` being
//! counted as loop heat — an emitter that treats it as an ordinary jump leaves
//! the function at T1 forever, however hot, because the invocation counter it
//! would otherwise have used is no longer bumped either.
//!
//! Measured on AArch64 before the fix: `(rec 200 0)` driven 2000 times —
//! 402,000 invocations — stayed at tier 1, while 5000 SHALLOW calls reached
//! tier 2. Promotion tracked the outer call count alone. At the same time a
//! self-recursive call cost 0.30 us against 0.019 us for the equivalent loop,
//! a 16x gap that closed completely once the function promoted.
//!
//! This test is architecture-agnostic on purpose, like `bitwise_shift_tiers`:
//! it drives the shape and asserts the tier, so it checks whichever emitter
//! the binary was built for. x86-64 reaches T2 here by another route (the
//! mapped ABI's own recursion handling), so it passed throughout; the checked
//! backends are the ones this pins.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

fn run(program: &str) -> String {
    let out = Command::new(BIN)
        .args(["--no-init", "--eval", program])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stdout}\n{stderr}");
    stdout.into_owned()
}

/// The regression itself: heat must come from the recursion, not only from
/// calls made by the caller. 100 outer calls is far below the 4096-invocation
/// threshold, so nothing but the back-edge can promote this.
#[test]
fn a_hot_tail_recursive_function_promotes_on_its_own_recursion() {
    let out = run("(progn \
           (defun trt-rec (n acc) (if (= n 0) acc (trt-rec (- n 1) (+ acc 1)))) \
           (dotimes (i 100) (trt-rec 400 0)) \
           (sleep 1) \
           (format t \"TIER ~s~%\" (egcl-ext:function-tier 'trt-rec)))");
    let tier: i32 = out
        .lines()
        .find_map(|l| l.trim().strip_prefix("TIER "))
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or_else(|| panic!("no TIER line in:\n{out}"));
    assert!(
        tier >= 2,
        "40,000 recursive invocations must promote past T1, got tier {tier}:\n{out}"
    );
}

/// Promotion must not change the answers, including at a depth that outruns
/// the native depth cap and across the base case.
#[test]
fn tail_recursion_answers_the_same_once_promoted() {
    let out = run("(progn \
           (defun trt-sum (n acc) (if (= n 0) acc (trt-sum (- n 1) (+ acc n)))) \
           (dotimes (i 200) (trt-sum 400 0)) \
           (format t \"SUM ~s ~s ~s~%\" (trt-sum 0 7) (trt-sum 10 0) (trt-sum 100000 0)))");
    assert!(
        out.lines().any(|l| l.trim() == "SUM 7 55 5000050000"),
        "promoted tail recursion changed its answers:\n{out}"
    );
}

/// The back-edge is also a signal and GC poll. A tail-recursive loop that
/// allocates must still collect — and must carry heap values across the poll
/// without losing them, which a dropped root would show as a wrong sum rather
/// than as a crash.
#[test]
fn an_allocating_tail_recursive_loop_collects_and_keeps_its_values() {
    let out = run("(progn \
           (defun trt-build (n acc) (if (= n 0) acc (trt-build (- n 1) (cons n acc)))) \
           (defun trt-total (l acc) (if (null l) acc (trt-total (cdr l) (+ acc (car l))))) \
           (let ((total 0)) \
             (dotimes (i 300) (setq total (+ total (trt-total (trt-build 200 nil) 0)))) \
             (format t \"TOTAL ~s~%\" total)))");
    // 300 * (1+..+200) = 300 * 20100
    assert!(
        out.lines().any(|l| l.trim() == "TOTAL 6030000"),
        "an allocating tail-recursive loop lost values:\n{out}"
    );
}
