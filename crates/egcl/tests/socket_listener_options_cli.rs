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

#[test]
fn socket_names_report_both_ends_of_a_connection() {
    // usocket's GET-LOCAL-ADDRESS / GET-PEER-ADDRESS / GET-PEER-PORT /
    // GET-LOCAL-NAME had no primitive behind them, so AllegroServe could not
    // log or print a socket (bliss-by1gf, GitHub #350).  Address and port come
    // back together as (octets port), the way SBCL's SOCKET-NAME returns them
    // as multiple values.
    //
    // The cross-checks are the point: the client's PEER port must equal the
    // listener's port, and the server's PEER port must equal the client's LOCAL
    // port.  An implementation that answered local_addr() for both ends would
    // satisfy neither.
    check(
        r#"
      (let* ((l (egcl::%socket-listen "127.0.0.1" 0 5))
             (port (egcl::%socket-local-port l))
             (srv nil))
        ;; A listener reports the address it is bound to, agreeing with
        ;; %SOCKET-LOCAL-PORT on the port.  A listener id is not a stream, so
        ;; it has its own primitive.  EQUALP, not EQUAL: EQUAL does not
        ;; descend into vectors (CLHS 5.3.3), so it is false for two distinct
        ;; #(127 0 0 1) objects however equal their contents.
        (assert (equalp (egcl::%socket-listener-local-name l) (list #(127 0 0 1) port)))
        (let ((acc (egcl-thread:make-thread
                    (lambda () (setq srv (egcl::%socket-accept l)) :ok)))
              (cli (egcl::%socket-connect "127.0.0.1" port nil)))
          (egcl-thread:join-thread acc)
          (let ((srv-local (egcl::%socket-local-name srv))
                (srv-peer  (egcl::%socket-peer-name srv))
                (cli-local (egcl::%socket-local-name cli))
                (cli-peer  (egcl::%socket-peer-name cli)))
            ;; Shape: a 4-element octet vector for IPv4, plus a port.
            (dolist (n (list srv-local srv-peer cli-local cli-peer))
              (assert (vectorp (first n)))
              (assert (= 4 (length (first n))))
              (assert (equalp #(127 0 0 1) (first n)))
              (assert (integerp (second n)))
              (assert (plusp (second n))))
            ;; The accepted socket is bound to the listener's port.
            (assert (= port (second srv-local)))
            ;; Cross-checks: the two primitives report opposite ends.
            (assert (= port (second cli-peer)))
            (assert (= (second cli-local) (second srv-peer)))
            ;; A listener has no peer, and is not a stream: neither the peer
            ;; primitive nor the stream-only local one accepts its id.
            (assert (handler-case (progn (egcl::%socket-peer-name l) nil)
                      (error () t)))
            (assert (handler-case (progn (egcl::%socket-local-name l) nil)
                      (error () t))))
          (close cli))
        ;; A closed listener answers NIL, as %SOCKET-LOCAL-PORT does.
        (egcl::%socket-close l)
        (assert (null (egcl::%socket-listener-local-name l)))
        (format t "SOCKET-NAMES-OK~%"))
    "#,
    );
}
