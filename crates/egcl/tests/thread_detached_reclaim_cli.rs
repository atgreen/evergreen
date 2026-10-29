// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A native thread that finishes without being joined is reclaimed (#591),
//! and a later JOIN-THREAD still observes it exactly as before.
use std::process::Command;

fn run(program: &str) -> (bool, String, String) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "240",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .expect("run egcl");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

/// A finished thread keeps its observable identity until it is joined, even
/// after later thread creations have reclaimed its OS thread and stacks.
#[test]
fn finished_unjoined_thread_still_joins_once_with_its_value() {
    let program = r#"
      (let ((worker (egcl-thread:make-thread (lambda () (list 1 2 (make-string 3 :initial-element #\z)))
                                             :name "reclaimed worker")))
        (loop while (egcl-thread:thread-alive-p worker) do (egcl-thread:thread-yield))
        ;; Each creation and each exit reclaims finished, unjoined threads.
        (dotimes (i 8)
          (egcl-thread:join-thread (egcl-thread:make-thread (lambda () (make-list 100)))))
        ;; Collections must keep the unconsumed result rooted.
        (dotimes (i 20) (make-list 10000))
        (format t "RECLAIM name=~S alive=~S listed=~S~%"
                (egcl-thread:thread-name worker)
                (egcl-thread:thread-alive-p worker)
                (and (member worker (egcl-thread:all-threads)) t))
        (assert (string= "reclaimed worker" (egcl-thread:thread-name worker)))
        (assert (null (egcl-thread:thread-alive-p worker)))
        (assert (null (member worker (egcl-thread:all-threads))))
        (let ((joined (multiple-value-list (egcl-thread:join-thread worker :timeout 10))))
          (format t "RECLAIM-JOINED ~S~%" joined)
          (assert (equal '((1 2 "zzz") t) joined)))
        (assert (eq :errored
                    (handler-case (progn (egcl-thread:join-thread worker) :joined)
                      (error () :errored))))
        (assert (null (egcl-thread:thread-alive-p worker)))
        (format t "RECLAIM-SEMANTICS-OK~%"))
    "#;
    let (ok, stdout, stderr) = run(program);
    assert!(ok, "{stdout}\n{stderr}");
    assert!(
        stdout.contains("RECLAIM-SEMANTICS-OK"),
        "{stdout}\n{stderr}"
    );
}

/// The issue's reproducer shape: batches of threads that finish and are never
/// joined. Before the fix each one kept its Lisp stack, TLS vector and
/// un-joined pthread stack: measured on x86-64 musl, 126 KB of RSS per thread
/// more than the same threads joined. Compare against a joined run in the same
/// process, so per-thread costs that joining does not avoid either cancel out.
#[test]
fn unjoined_threads_cost_no_more_rss_than_joined_ones() {
    let program = r#"
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
      (defun churn (n detached)
        (setq *done* 0)
        (do ((started 0)) ((>= started n))
          (if detached
              (progn
                (dotimes (k 4) (egcl-thread:make-thread #'work :name "churn"))
                (incf started 4)
                (do () ((>= (egcl-thread:with-mutex (*lock*) *done*) started))
                  (egcl-thread:thread-yield)))
              (progn
                (egcl-thread:join-thread (egcl-thread:make-thread #'work :name "churn"))
                (incf started)))))
      (defun growth-per-thread (n detached)
        (let ((before (vm-rss-kb)))
          (churn n detached)
          (/ (- (vm-rss-kb) before) (float n))))
      ;; Warm up allocator pools and the heap before measuring.
      (churn 200 nil)
      (churn 200 t)
      (let* ((joined (growth-per-thread 800 nil))
             (detached (growth-per-thread 800 t)))
        (format t "RSS per thread: joined=~,2F kB detached=~,2F kB~%" joined detached)
        (assert (< (- detached joined) 25.0))
        (format t "RECLAIM-RSS-OK~%"))
    "#;
    let (ok, stdout, stderr) = run(program);
    assert!(ok, "{stdout}\n{stderr}");
    assert!(stdout.contains("RECLAIM-RSS-OK"), "{stdout}\n{stderr}");
}
