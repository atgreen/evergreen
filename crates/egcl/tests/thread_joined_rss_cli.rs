// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A joined native thread gives back the Rust-side memory it used, including
//! what promoting functions to T1 allocated on it (bliss-orayb, GitHub #591).
use std::process::Command;

/// The joined churn from #591. Prints the RSS growth per thread, in kB, after
/// a warm-up that lets allocator pools and the heap reach their working size.
const CHURN: &str = r#"
  (defun vm-rss-kb ()
    (with-open-file (in "/proc/self/status")
      (do ((line (read-line in nil) (read-line in nil)))
          ((null line) 0)
        (when (and (> (length line) 6) (string= "VmRSS:" line :end2 6))
          (return (parse-integer line :start 6 :junk-allowed t))))))
  (defvar *done* 0)
  (defvar *lock* (egcl-thread:make-mutex :name "done"))
  (defun work ()
    (length (make-list 1000))
    (egcl-thread:with-mutex (*lock*) (incf *done*)))
  (defun churn (n)
    (dotimes (i n)
      (egcl-thread:join-thread (egcl-thread:make-thread #'work :name "churn"))))
  (churn 600)
  (let ((before (vm-rss-kb)))
    (churn 1000)
    (format t "GROWTH-PER-THREAD ~,2F~%" (/ (- (vm-rss-kb) before) 1000.0)))
"#;

fn growth_per_thread(t1_threshold: Option<&str>) -> f64 {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=5",
        "400",
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
        "--eval",
        CHURN,
    ]);
    match t1_threshold {
        Some(threshold) => command.env("EGCL_T0_T1_THRESHOLD", threshold),
        None => command.env_remove("EGCL_T0_T1_THRESHOLD"),
    };
    let output = command.output().expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("GROWTH-PER-THREAD "))
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or_else(|| panic!("no growth line\n{stdout}\n{stderr}"))
}

/// Before the fix, on x86-64 musl, each joined thread whose work promoted a few
/// functions to T1 kept ~88 kB of RSS: the thread-cache allocator never handed
/// an exited thread's cached blocks back, and every execution's call cells
/// stayed pinned by dead weak references. With T1 disabled the same threads
/// kept ~8 kB. Compare both modes in the same test so per-thread costs common
/// to them cancel out; the bound sits far above the fixed difference (~1.5 kB)
/// and far below the regression.
#[test]
fn joined_threads_that_promote_to_t1_return_their_memory() {
    let promoted = growth_per_thread(None);
    let interpreted = growth_per_thread(Some("4000000000"));
    println!("RSS per joined thread: T1 enabled={promoted:.2} kB, T1 disabled={interpreted:.2} kB");
    assert!(
        promoted - interpreted < 20.0,
        "T1 enabled={promoted:.2} kB/thread, T1 disabled={interpreted:.2} kB/thread"
    );
    assert!(promoted < 30.0, "T1 enabled={promoted:.2} kB/thread");
}
