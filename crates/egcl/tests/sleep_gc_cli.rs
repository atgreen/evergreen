// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A thread in SLEEP does not hold up a collection.
//!
//! SLEEP called `std::thread::sleep` while the thread stayed Running. A
//! sleeping thread reaches no poll point, and the collector's SIGUSR1 nudge
//! only interrupts `nanosleep`, which `std::thread::sleep` resumes; so a
//! stop-the-world handshake waited for it, gave up after its timeouts, and
//! the allocation that needed the collection failed. With a cons that is
//! "memory allocation of 16 bytes failed" and an abort; in a loop that keeps
//! retrying it is a hang. An AllegroServe whose main thread waits in SLEEP --
//! the usual shape of a server's top level -- lost its worker threads that
//! way on the first collection they needed.
//!
//! Condition waits, joins, mutexes and socket I/O already enter a
//! `NativeBlockingScope`; SLEEP now does too.

use std::process::Command;
use std::time::{Duration, Instant};

fn run(program: &str, limit: u64) -> (String, String, bool, Duration) {
    let started = Instant::now();
    let output = Command::new("timeout")
        .args([
            "--kill-after=10",
            &limit.to_string(),
            env!("CARGO_BIN_EXE_egcl"),
        ])
        .args(["--no-init", "--eval", program])
        .env("EGCL_HEAP_SIZE", "256m")
        .output()
        .expect("run egcl");
    (
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
        output.status.success(),
        started.elapsed(),
    )
}

/// Allocate well past the nursery, so minor collections must run, and then
/// ask for a full one, while another thread sleeps.
const WITH_SLEEPER: &str = r#"
  (progn
    (egcl-thread:make-thread (lambda () (sleep 3600)) :name "sleeper")
    (sleep 0.3)
    (let ((keep nil))
      (dotimes (round 20)
        (let ((junk nil))
          (dotimes (i 100000) (push (cons i i) junk))
          (setq keep (subseq junk 0 10))))
      (format t "ALLOCATED ~a~%" (length keep)))
    (egcl-ext:gc :full t)
    (format t "COLLECTED~%")
    (finish-output)
    (egcl::%exit 0))
"#;

#[test]
fn collections_run_while_another_thread_sleeps() {
    let (stdout, stderr, ok, elapsed) = run(WITH_SLEEPER, 120);
    assert!(ok, "exited badly after {elapsed:?}:\n{stdout}\n{stderr}");
    assert!(stdout.contains("ALLOCATED 10"), "{stdout}\n{stderr}");
    assert!(stdout.contains("COLLECTED"), "{stdout}\n{stderr}");
}

/// SLEEP still sleeps for the time asked, and returns NIL, on the calling
/// thread and on another one.
#[test]
fn sleep_still_sleeps_and_returns_nil() {
    let (stdout, stderr, ok, _) = run(
        r#"(progn
             (let ((t0 (get-internal-real-time)))
               (format t "MAIN ~s ~a~%" (sleep 0.2)
                       (>= (- (get-internal-real-time) t0) (* 0.15 internal-time-units-per-second))))
             (format t "THREAD ~s~%"
                     (multiple-value-list
                      (egcl-thread:join-thread
                       (egcl-thread:make-thread (lambda () (sleep 0.1) :woke)) :timeout 10)))
             (finish-output)
             (egcl::%exit 0))"#,
        60,
    );
    assert!(ok, "{stdout}\n{stderr}");
    assert!(stdout.contains("MAIN NIL T"), "{stdout}");
    assert!(stdout.contains("THREAD (:WOKE T)"), "{stdout}");
}
