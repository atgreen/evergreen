// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

fn check(program: &str) {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "30",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            program,
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{:?}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        output.status
    );
}

#[test]
fn invalid_listener_options_signal_before_binding() {
    check(
        r#"
      (assert (handler-case (progn (egcl::%socket-listen "127.0.0.1" 0 -1) nil)
                (type-error () t)))
      (assert (handler-case (progn (egcl::%socket-listen "127.0.0.1" -1) nil)
                (type-error () t)))
      (assert (handler-case (progn (egcl::%socket-listen "127.0.0.1" 65536) nil)
                (type-error () t)))
      (assert (handler-case (progn (egcl::%socket-listen "127.0.0.1" 0 2147483648) nil)
                (type-error () t)))
      (assert (handler-case (progn (egcl::%socket-listen nil 0) nil)
                (type-error () t)))
      (assert (handler-case (progn (egcl::%socket-listen "127.0.0.1" 0 1 nil :extra) nil)
                (program-error () t)))
    "#,
    );
}

#[test]
#[cfg(target_os = "linux")]
fn reuse_address_allows_restart_after_active_close() {
    check(
        r#"
      (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5 t))
             (port (egcl::%socket-local-port listener))
             (client (egcl::%socket-connect "127.0.0.1" port))
             (peer (egcl::%socket-accept listener)))
        (close peer)
        (assert (eq :eof (read-byte client nil :eof)))
        (close client)
        (egcl::%socket-close listener)
        (let ((replacement (egcl::%socket-listen "127.0.0.1" port 5 t)))
          (assert (= port (egcl::%socket-local-port replacement)))
          (egcl::%socket-close replacement)))
    "#,
    );
}

#[test]
#[cfg(target_os = "linux")]
fn disabling_reuse_address_preserves_time_wait_protection() {
    check(
        r#"
      (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5 nil))
             (port (egcl::%socket-local-port listener))
             (client (egcl::%socket-connect "127.0.0.1" port))
             (peer (egcl::%socket-accept listener)))
        (close peer)
        (assert (eq :eof (read-byte client nil :eof)))
        (close client)
        (egcl::%socket-close listener)
        (assert (handler-case
                    (progn (egcl::%socket-listen "127.0.0.1" port 5 t) nil)
                  (file-error () t))))
    "#,
    );
}

#[test]
#[cfg(target_os = "linux")]
fn zero_backlog_limits_the_pending_connection_queue() {
    check(
        r#"
      (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 0))
             (port (egcl::%socket-local-port listener))
             (client (egcl::%socket-connect "127.0.0.1" port 100)))
        (unwind-protect
            (assert (handler-case
                        (progn (close (egcl::%socket-connect "127.0.0.1" port 100)) nil)
                      (file-error () t)))
          (close client)
          (egcl::%socket-close listener)))
    "#,
    );
}
