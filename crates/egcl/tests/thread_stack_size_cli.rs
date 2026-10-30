// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A thread from `egcl-thread:make-thread` can recurse as deep as the main
//! thread, and `EGCL_STACK_SIZE` sizes its stack.
//!
//! The thread took Rust's default 2 MiB stack, which neither `EGCL_STACK_SIZE`
//! nor `--stack-size` reached: it overflowed at about a quarter of the main
//! thread's depth, and a server's worker threads failed on work the main
//! thread finished.

use std::process::Command;

const PROGRAM: &str = r#"
(defun deep (n) (if (zerop n) 0 (1+ (deep (1- n)))))
(defun max-depth ()
  ;; the largest N for which (deep N) returns, by doubling then bisection
  (flet ((ok? (n) (handler-case (progn (deep n) t) (storage-condition () nil))))
    (let ((lo 0) (hi 1000))
      (loop while (ok? hi) do (setq lo hi hi (* 2 hi)) (when (> hi 100000000) (return)))
      (loop while (> (- hi lo) (max 1 (floor lo 100)))
            do (let ((mid (floor (+ lo hi) 2))) (if (ok? mid) (setq lo mid) (setq hi mid))))
      lo)))
(format t "~&MAIN ~d~%" (max-depth))
(format t "~&THREAD ~d~%" (egcl-thread:join-thread (egcl-thread:make-thread #'max-depth)))
"#;

fn depths(stack_size: Option<&str>) -> (u64, u64) {
    depths_with(stack_size.map(|size| ("EGCL_STACK_SIZE", size)))
}

fn depths_with(env: Option<(&str, &str)>) -> (u64, u64) {
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=10", "300", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")]);
    if let Some((name, value)) = env {
        command.env(name, value);
    }
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    let depth = |label: &str| -> u64 {
        stdout
            .lines()
            .find_map(|line| line.strip_prefix(label))
            .and_then(|rest| rest.trim().parse().ok())
            .unwrap_or_else(|| panic!("no {label} line in:\n{stdout}"))
    };
    (depth("MAIN "), depth("THREAD "))
}

#[test]
fn a_thread_recurses_as_deep_as_the_main_thread() {
    let (main, thread) = depths(None);
    // The two stacks are the same size; the thread's frames differ a little
    // from the main thread's, so allow it a quarter less, not the three
    // quarters less it used to get.
    assert!(
        thread * 4 >= main * 3,
        "a thread recursed {thread} deep where the main thread recursed {main}"
    );
}

#[test]
fn egcl_stack_size_sizes_a_threads_stack() {
    let (_, default) = depths(None);
    let (_, large) = depths(Some("64m"));
    assert!(
        large >= default * 2,
        "EGCL_STACK_SIZE=64m left a thread at {large} deep (default {default})"
    );
}

/// An explicit thread stack size overrides `RUST_MIN_STACK`, so it is kept
/// as a floor: a deployment that raised it still gets the larger stack.
#[test]
fn rust_min_stack_still_raises_a_threads_stack() {
    let (_, default) = depths(None);
    let (_, large) = depths_with(Some(("RUST_MIN_STACK", "67108864")));
    assert!(
        large >= default * 2,
        "RUST_MIN_STACK=64 MiB left a thread at {large} deep (default {default})"
    );
}

// A deterministic small numeric limit exercises the real failure branch without
// depending on the host frame size or exhausting its emergency stack reserve.
const DIAGNOSTIC_PROGRAM: &str = r#"
(defun origin-deep (n) (if (zerop n) 0 (1+ (origin-deep (1- n)))))
(handler-case (progn (origin-deep 100000) (error "overflow did not fire"))
  (storage-condition () (write-line "STACK-ORIGIN-CAUGHT")))
"#;

fn overflow_diagnostic(enabled: Option<&str>, managed: bool) -> String {
    let mut command = Command::new("timeout");
    command.args(["--kill-after=5", "60", env!("CARGO_BIN_EXE_egcl")]);
    command.args(["--no-init", "--eval", DIAGNOSTIC_PROGRAM]);
    command.env_remove("EGCL_STACK_OVERFLOW_DIAGNOSTICS");
    command.env_remove("EGCL_MAX_EVAL_STACK_BYTES");
    command.env_remove("EGCL_NATIVE_DEPTH_CAP");
    command.env("EGCL_FORCE_TIER", if managed { "t0" } else { "interp" });
    if managed {
        command.env("EGCL_STACK_SIZE", "64k");
        command.env_remove("EGCL_MAX_CALL_DEPTH");
    } else {
        command.env("EGCL_MAX_CALL_DEPTH", "32");
        command.env_remove("EGCL_STACK_SIZE");
    }
    if let Some(value) = enabled {
        command.env("EGCL_STACK_OVERFLOW_DIAGNOSTICS", value);
    }
    let output = command.output().expect("run actual overflow diagnostic");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(stdout.contains("STACK-ORIGIN-CAUGHT"), "{stdout}\n{stderr}");
    stderr.into_owned()
}

#[test]
fn stack_origin_diagnostic_reports_real_numeric_failure_only_when_enabled() {
    let stderr = overflow_diagnostic(Some("1"), false);
    assert!(
        stderr.contains("[stack-overflow] origin=numeric-call-depth"),
        "{stderr}"
    );
    assert!(stderr.contains("depth=Some(33)"), "{stderr}");
    assert!(stderr.contains("managed_capacity="), "{stderr}");
    assert!(stderr.contains("host_budget="), "{stderr}");
}

#[test]
fn stack_origin_diagnostic_is_silent_unless_explicitly_enabled() {
    for flag in [None, Some("true"), Some("0")] {
        let stderr = overflow_diagnostic(flag, false);
        assert!(!stderr.contains("[stack-overflow]"), "{flag:?}: {stderr}");
    }
}

#[test]
fn stack_origin_diagnostic_reports_real_managed_frame_exhaustion() {
    let stderr = overflow_diagnostic(Some("1"), true);
    assert!(
        stderr.contains("[stack-overflow] origin=managed-frame"),
        "{stderr}"
    );
    assert!(stderr.contains("managed_capacity=Some(65536)"), "{stderr}");
}
