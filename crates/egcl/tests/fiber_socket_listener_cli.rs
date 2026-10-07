// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(unix, target_arch = "x86_64"))]

use std::process::Command;

fn listener_wait(readiness: bool, close: bool) {
    let wait = if readiness {
        "(egcl::%socket-listener-ready-p listener -1)"
    } else {
        "(egcl::%socket-accept listener)"
    };
    let consume = if close {
        format!("(assert (handler-case (progn {wait} nil) (file-error () t)))")
    } else if readiness {
        format!("(assert {wait}) (close (egcl::%socket-accept listener))")
    } else {
        format!("(close {wait})")
    };
    let wake = if close {
        "(egcl::%socket-close listener)"
    } else {
        "(close (egcl::%socket-connect \"127.0.0.1\" port))"
    };
    let program = format!(
        r#"
        (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5))
               (port (egcl::%socket-local-port listener))
               (waiter (egcl-fiber:make-fiber (lambda () {consume} :waited)))
               (waker (egcl-fiber:make-fiber
                       (lambda ()
                         (assert (egcl-fiber:fiber-park
                                  (lambda () (eq :suspended (egcl-fiber:fiber-state waiter)))
                                  :timeout 5))
                         {wake}
                         :woke))))
          (unwind-protect
              (assert (equal '(:waited :woke)
                             (egcl-fiber:run-fibers (list waiter waker) :carrier-count 1)))
            (egcl::%socket-close listener)))
        (format t "FIBER-LISTENER-PASS~%")
        "#
    );
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "20",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &program,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{stdout}\n{stderr}\nstatus: {}",
        output.status
    );
    assert!(stdout.contains("FIBER-LISTENER-PASS"), "{stdout}\n{stderr}");
}

#[test]
fn accept_releases_carrier_until_connection() {
    listener_wait(false, false);
}

#[test]
fn readiness_releases_carrier_until_connection() {
    listener_wait(true, false);
}

#[test]
fn close_wakes_pending_accept() {
    listener_wait(false, true);
}

#[test]
fn close_wakes_pending_readiness() {
    listener_wait(true, true);
}

fn concurrent_waiters(readiness: bool, close: bool) {
    let wait = if readiness {
        "(assert (egcl::%socket-listener-ready-p listener -1))"
    } else {
        "(close (egcl::%socket-accept listener))"
    };
    let serve = if close {
        format!("(handler-case (progn {wait} :unexpected) (file-error () :closed))")
    } else {
        format!("{wait} :served")
    };
    let wake = if close {
        "(egcl::%socket-close listener)"
    } else {
        "(dotimes (i 2) (close (egcl::%socket-connect \"127.0.0.1\" port)))"
    };
    let expected = if close { ":closed" } else { ":served" };
    let program = format!(
        r#"
      (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5))
             (port (egcl::%socket-local-port listener))
             (started 0))
        (flet ((serve ()
                 (incf started)
                 (handler-case (progn {serve})
                   (error (c) (format t "WAITER-ERROR: ~A~%" c) :failed))))
          (let ((first (egcl-fiber:make-fiber #'serve))
                (second (egcl-fiber:make-fiber #'serve))
                (waker (egcl-fiber:make-fiber
                        (lambda ()
                          (assert (egcl-fiber:fiber-park (lambda () (= started 2)) :timeout 5))
                          {wake}
                          :woke))))
            (unwind-protect
                (assert (equal '({expected} {expected} :woke)
                               (egcl-fiber:run-fibers (list first second waker) :carrier-count 1)))
              (egcl::%socket-close listener)))))
      (format t "CONCURRENT-LISTENERS-PASS~%")
    "#
    );
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "20",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            &program,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{stdout}\n{stderr}\n{:?}",
        output.status
    );
    assert!(stdout.contains("CONCURRENT-LISTENERS-PASS"));
}

#[test]
fn concurrent_accept_waiters_can_share_a_listener() {
    concurrent_waiters(false, false);
}

#[test]
fn concurrent_readiness_waiters_can_share_a_listener() {
    concurrent_waiters(true, false);
}

#[test]
fn concurrent_accept_waiters_all_wake_on_close() {
    concurrent_waiters(false, true);
}

#[test]
fn concurrent_readiness_waiters_all_wake_on_close() {
    concurrent_waiters(true, true);
}
