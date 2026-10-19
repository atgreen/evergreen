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
    let mut command = Command::new("timeout");
    command
        .args(["--kill-after=10", "300", env!("CARGO_BIN_EXE_egcl")])
        .args(["--no-init", "--eval", &format!("(progn {PROGRAM})")]);
    if let Some(size) = stack_size {
        command.env("EGCL_STACK_SIZE", size);
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
