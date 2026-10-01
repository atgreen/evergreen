// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A waiting TCP stream must release the sole carrier to other Lisp fibers.
#![cfg(all(
    target_pointer_width = "64",
    any(
        all(unix, any(target_arch = "x86_64", target_arch = "aarch64")),
        all(
            target_vendor = "unknown",
            target_os = "linux",
            target_env = "gnu",
            any(
                all(target_arch = "powerpc64", target_endian = "little"),
                target_arch = "s390x"
            )
        ),
        all(windows, target_arch = "x86_64")
    )
))]
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

enum WaitKind {
    Read,
    Readiness,
    Timeout,
    Close,
}

fn roundtrip(kind: WaitKind) {
    for backend in ["tree-walker", "bytecode"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let close = matches!(kind, WaitKind::Close);
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(90);
            let accept = || loop {
                match listener.accept() {
                    Ok((stream, _)) => {
                        // Winsock inherits the listener's non-blocking mode.
                        stream.set_nonblocking(false).unwrap();
                        break stream;
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(Instant::now() < deadline, "client did not connect");
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    Err(e) => panic!("accept: {e}"),
                }
            };
            let mut data = accept();
            let mut control = accept();
            control
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            let mut byte = [0];
            // Drop both sockets on failure so the child cannot hang on read.
            control.read_exact(&mut byte)?;
            assert_eq!(byte, [42]);
            if !close {
                data.write_all(&[65, 66])?;
            }
            Ok::<_, std::io::Error>(())
        });
        let wait = match kind {
            WaitKind::Read => "",
            WaitKind::Readiness | WaitKind::Close => {
                "(assert (egcl::%socket-wait-for-input data 5000))"
            }
            WaitKind::Timeout => {
                r#"
                (assert (not (egcl::%socket-wait-for-input data 20)))
                (egcl::%socket-read-timeout data 100)
                (assert (handler-case (progn (read-byte data) nil) (stream-error () t)))
                (assert (> ticks 0))
                (setq timed-out t)
                (egcl::%socket-read-timeout data nil)
            "#
            }
        };
        let delay_write = if matches!(kind, WaitKind::Timeout) {
            "(loop until timed-out do (incf ticks) (egcl-fiber:fiber-sleep 0.001))"
        } else if close {
            "(assert (egcl-fiber:fiber-park
                        (lambda () (eq :suspended (egcl-fiber:fiber-state reader)))
                        :timeout 5))
             (close data)"
        } else {
            ""
        };
        let consume = if close {
            "(assert (not (open-stream-p data)))"
        } else {
            "(assert (= 65 (read-byte data)))
             (assert (= 66 (read-byte data)))
             (assert (eq :eof (read-byte data nil :eof)))"
        };
        let program = format!(
            r#"
          (let ((data (egcl::%socket-connect "127.0.0.1" {port}))
                (control (egcl::%socket-connect "127.0.0.1" {port}))
                (started nil) (timed-out nil) (ticks 0))
            (unwind-protect
                (let* ((reader (egcl-fiber:make-fiber
                                (lambda ()
                                  (setq started t)
                                  {wait}
                                  {consume}
                                  :read)))
                      (writer (egcl-fiber:make-fiber
                                (lambda ()
                                  (assert (egcl-fiber:fiber-park
                                            (lambda () started) :timeout 5))
                                  {delay_write}
                                  (write-byte 42 control)
                                  (finish-output control)
                                  :wrote))))
                  (assert (equal '(:read :wrote)
                                 (handler-case
                                     (egcl-fiber:run-fibers (list reader writer) :carrier-count 1)
                                   (egcl-fiber:fiber-error (e)
                                     (format t "Fiber failure: ~A~%" (egcl-fiber:fiber-error-cause e))
                                     (error e))))))
              (close data) (close control)))
          (format t "FIBER-SOCKET-OK~%")
        "#
        );
        let mut child = Command::new(env!("CARGO_BIN_EXE_egcl"))
            .args(["--no-init", "--eval", &program])
            .env("EGCL_BACKEND", backend)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(90);
        while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        let timed_out = child.try_wait().unwrap().is_none();
        if timed_out {
            child.kill().unwrap();
        }
        let output = child.wait_with_output().unwrap();
        let server_result = server.join().unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            !timed_out && output.status.success(),
            "{backend}: {stdout}\n{stderr}\nserver: {server_result:?}"
        );
        assert!(
            stdout.contains("FIBER-SOCKET-OK"),
            "{backend}: {stdout}\n{stderr}"
        );
        server_result.unwrap();
    }
}

#[test]
fn read_and_eof_release_the_carrier() {
    roundtrip(WaitKind::Read);
}

#[test]
fn explicit_readiness_releases_the_carrier() {
    roundtrip(WaitKind::Readiness);
}

#[test]
fn receive_timeout_releases_carrier_and_can_be_disabled() {
    roundtrip(WaitKind::Timeout);
}

#[test]
fn close_wakes_explicit_readiness_without_reusing_a_stale_handle() {
    roundtrip(WaitKind::Close);
}
