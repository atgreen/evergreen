// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Native client streams must expose bytes before the peer finishes its reply.
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output};
use std::time::{Duration, Instant};

fn run(form: &str, extra: &[&str], stress: bool) -> Output {
    let mut command = Command::new("timeout");
    command.args([
        "--kill-after=5",
        if stress { "150" } else { "30" },
        env!("CARGO_BIN_EXE_egcl"),
        "--no-init",
    ]);
    command.args(extra).args(["--eval", form]);
    if stress {
        command
            .env("EGCL_GC_STRESS", "1")
            .env("EGCL_GC_POISON", "1")
            .env("EGCL_GC_VERIFY", "1");
    }
    command.output().expect("run bounded EGCL")
}

fn successful(output: Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    stdout
}

#[test]
fn native_client_reads_incrementally_and_writes_before_eof() {
    for stress in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        listener.set_nonblocking(true).unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(if stress { 150 } else { 20 });
            let (mut peer, _) = loop {
                match listener.accept() {
                    Ok(pair) => break pair,
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            && Instant::now() < deadline =>
                    {
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => return Err(e),
                }
            };
            peer.set_read_timeout(Some(Duration::from_secs(10)))?;
            peer.set_write_timeout(Some(Duration::from_secs(10)))?;
            peer.write_all(&[65, 200])?;
            // Deliberately withhold the final byte until the Lisp client reads
            // the first bytes and acknowledges them. Buffer-until-EOF deadlocks.
            let mut ack = [0];
            peer.read_exact(&mut ack)?;
            assert_eq!(ack, [42]);
            peer.write_all(&[255])?;
            Ok::<_, std::io::Error>(())
        });
        let output = run(
            &format!(
                r#"
          (let ((s (egcl::%socket-connect "127.0.0.1" {port} 2000)))
            (unwind-protect
                (progn
                  (assert (input-stream-p s))
                  (assert (output-stream-p s))
                  (assert (equal (stream-element-type s) '(unsigned-byte 8)))
                  (assert (= 65 (read-byte s)))
                  (assert (= 200 (read-byte s)))
                  (write-byte 42 s)
                  (finish-output s)
                  (assert (= 255 (read-byte s)))
                  (assert (eq :end (read-byte s nil :end))))
              (close s))
            (assert (not (open-stream-p s)))
            (close s)
            (format t "NATIVE-TCP-STREAM-OK~%"))
        "#
            ),
            &[],
            stress,
        );
        let server_result = server.join().expect("server thread");
        let stdout = successful(output);
        server_result.expect("incremental byte exchange");
        assert!(stdout.contains("NATIVE-TCP-STREAM-OK"), "{stdout}");
    }
}

#[test]
fn native_client_rejects_invalid_arguments_before_connecting() {
    let output = run(
        r#"
      (dolist (args '(("127.0.0.1" -1) ("127.0.0.1" 65536)
                     ("127.0.0.1" 1 -1) ("127.0.0.1" 1 0)
                     ("127.0.0.1" 1 "bad") (42 80)))
        (assert (handler-case (progn (eval (cons 'egcl::%socket-connect args)) nil)
                  (type-error () t))))
      (format t "INVALID-TCP-ARGS-OK~%")
    "#,
        &[],
        false,
    );
    assert!(successful(output).contains("INVALID-TCP-ARGS-OK"));
}

#[test]
fn native_client_is_denied_in_sandbox() {
    let output = run(
        r#"(egcl::%socket-connect "127.0.0.1" 80 100)"#,
        &["--sandbox"],
        false,
    );
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("network access denied"), "{stderr}");
}

#[test]
fn native_client_reports_connection_refusal() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let output = run(
        &format!(
            r#"(assert (handler-case
            (progn (egcl::%socket-connect "127.0.0.1" {port} 500) nil)
            (file-error () t))) (format t "TCP-REFUSAL-OK~%")"#
        ),
        &[],
        false,
    );
    assert!(successful(output).contains("TCP-REFUSAL-OK"));
}

#[test]
fn native_client_receive_timeout_is_enforced_and_can_be_disabled() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let server = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let (mut peer, _) = loop {
            match listener.accept() {
                Ok(pair) => break pair,
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(e),
            }
        };
        peer.set_read_timeout(Some(Duration::from_secs(3)))?;
        let mut ack = [0];
        peer.read_exact(&mut ack)?;
        assert_eq!(ack, [42]);
        peer.write_all(&[65])?;
        Ok::<_, std::io::Error>(())
    });
    let output = run(
        &format!(
            r#"
      (let ((s (egcl::%socket-connect "localhost" {port})))
        (unwind-protect
            (progn
              (assert (null (egcl::%socket-read-timeout s)))
              (egcl::%socket-read-timeout s 50)
              (assert (>= (egcl::%socket-read-timeout s) 50))
              (assert (handler-case (progn (read-byte s) nil) (stream-error () t)))
              (egcl::%socket-read-timeout s nil)
              (assert (null (egcl::%socket-read-timeout s)))
              (write-byte 42 s)
              (finish-output s)
              (assert (= 65 (read-byte s)))
              (format t "TCP-TIMEOUT-OK~%"))
          (close s)))
    "#
        ),
        &[],
        false,
    );
    let server_result = server.join().unwrap();
    assert!(successful(output).contains("TCP-TIMEOUT-OK"));
    server_result.expect("timeout acknowledgement");
}
