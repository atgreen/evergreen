// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use std::process::Command;

#[test]
fn listener_can_be_accepted_and_closed_from_another_thread() {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "30",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            r#"
            (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5))
                   (port (egcl::%socket-local-port listener))
                   (client (egcl::%socket-connect "127.0.0.1" port)))
              (unwind-protect
                  (let* ((worker
                           (egcl-thread:make-thread
                            (lambda ()
                              (handler-case
                                  (let ((other (egcl::%socket-listen "127.0.0.1" 0 5)))
                                    (unwind-protect
                                        (progn
                                          (assert (/= listener other))
                                          (assert (= port (egcl::%socket-local-port listener)))
                                          (assert (egcl::%socket-listener-ready-p listener 100))
                                          (let ((peer (egcl::%socket-accept listener)))
                                            (unwind-protect
                                                (progn (write-byte 201 peer)
                                                       (finish-output peer))
                                              (close peer)))
                                          (egcl::%socket-close listener)
                                          :served)
                                      (egcl::%socket-close other)))
                                (error (condition) (list :error (format nil "~A" condition)))))))
                         (result (egcl-thread:join-thread worker)))
                    (assert (eq :served result) () "Server worker failed: ~S" result)
                    (assert (null (egcl::%socket-local-port listener)))
                    (assert (= 201 (read-byte client)))
                    (assert (eq :eof (read-byte client nil :eof)))
                    (format t "SHARED-LISTENER-PASS~%"))
                (close client)
                (egcl::%socket-close listener)))
            "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stdout}\n{stderr}");
    assert!(
        stdout.contains("SHARED-LISTENER-PASS"),
        "{stdout}\n{stderr}"
    );
}

/// A thread parked in READ-BYTE must not lock other threads out of the stream.
///
/// Input used to park inside the stream's state mutex, so while one thread
/// waited for a peer every other operation on that stream blocked -- CLOSE, a
/// write on the other half, even INPUT-STREAM-P, which performs no I/O at all
/// (bliss-vupo0). A server could not shut a worker's idle keep-alive
/// connection, which is where NET.ASERVE:SHUTDOWN ended up (GitHub #350).
///
/// The discriminator is INPUT-STREAM-P: it only takes the lock. The control
/// assertion on a *different* stream rules out a general scheduler problem, and
/// the reader is checked to be genuinely parked rather than finished, so the
/// test cannot pass by the park having silently become a spin.
#[test]
fn a_parked_reader_does_not_lock_other_threads_out_of_the_stream() {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "60",
            env!("CARGO_BIN_EXE_egcl"),
            "--no-init",
            "--eval",
            r#"
            (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5))
                   (port (egcl::%socket-local-port listener))
                   (server nil)
                   (reader-state :not-started))
              (let ((acceptor (egcl-thread:make-thread
                               (lambda () (setq server (egcl::%socket-accept listener)) :ok)))
                    (client (egcl::%socket-connect "127.0.0.1" port)))
                (egcl-thread:join-thread acceptor)
                (let ((reader (egcl-thread:make-thread
                               (lambda ()
                                 (setq reader-state :parked)
                                 (handler-case (read-byte server)
                                   (error () :errored))
                                 (setq reader-state :returned)))))
                  ;; Nothing is sent, so the reader parks.
                  (sleep 1)
                  (assert (eq :parked reader-state))
                  ;; Control: another stream answers while the reader is parked.
                  (assert (input-stream-p client))
                  ;; The point: a lock-only accessor on the PARKED stream must
                  ;; answer rather than block behind the park.
                  (assert (input-stream-p server))
                  (assert (output-stream-p server))
                  ;; Still parked -- the park is real, not a spin.
                  (assert (eq :parked reader-state))
                  ;; And CLOSE from this thread must complete, waking the reader.
                  (close server)
                  (egcl-thread:join-thread reader)
                  (assert (eq :returned reader-state)))
                (close client))
              (egcl::%socket-close listener)
              (format t "PARKED-READER-DOES-NOT-BLOCK-STREAM~%"))
            "#,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}\n{:?}",
        String::from_utf8_lossy(&output.stderr),
        output.status
    );
    assert!(
        stdout.contains("PARKED-READER-DOES-NOT-BLOCK-STREAM"),
        "{stdout}"
    );
}
