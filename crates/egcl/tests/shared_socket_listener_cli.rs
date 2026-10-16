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

/// A composite must not hold its own guard across a component's park.
///
/// #353 moved direct reads off the state mutex, but the composite arms reached
/// their component through the OUTER guard, so a TWO-WAY / ECHO /
/// CONCATENATED stream reproduced the same bug one level up: a parked read on
/// the inner socket locked every other thread out of the composite
/// (bliss-ifed5). Reachable from Lisp through MAKE-TWO-WAY-STREAM and friends,
/// and *TERMINAL-IO* is bidirectional.
///
/// Same discriminator as the direct case: INPUT-STREAM-P performs no I/O, the
/// control assertion is on a different stream, and the reader is checked to be
/// still parked so the test cannot pass by the park becoming a spin.
#[test]
fn a_parked_component_read_does_not_lock_the_composite() {
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
                ;; Input half is the idle socket, so the read parks inside the
                ;; component while the composite is what other threads touch.
                (let ((two-way (make-two-way-stream server client)))
                  (egcl-thread:make-thread
                   (lambda ()
                     (setq reader-state :parked)
                     (handler-case (read-byte two-way) (error () :errored))
                     (setq reader-state :returned)))
                  (sleep 1)
                  (assert (eq :parked reader-state))
                  ;; Control: a different stream answers.
                  (assert (input-stream-p client))
                  ;; The point: the COMPOSITE answers while its component is parked.
                  (assert (input-stream-p two-way))
                  (assert (output-stream-p two-way))
                  (assert (eq :parked reader-state)))
                (close client))
              (egcl::%socket-close listener)
              (format t "PARKED-COMPONENT-DOES-NOT-LOCK-COMPOSITE~%"))
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
        stdout.contains("PARKED-COMPONENT-DOES-NOT-LOCK-COMPOSITE"),
        "{stdout}"
    );
}

/// A thread parked in READ-SEQUENCE must not lock other threads out of the
/// stream, and must resume where it left off.
///
/// #353 moved READ-BYTE off the state mutex and #365 moved READ-CHAR, but the
/// sequence forms kept the old `with_stream` path, so READ-SEQUENCE still
/// waited with the guard held (bliss-mclc6). That is the reachable shape of
/// bliss-vupo0: socket streams are `(unsigned-byte 8)`, so READ-SEQUENCE is how
/// usocket and AllegroServe actually read a socket. It DEADLOCKED rather than
/// merely stalling, because the thread that would supply the bytes is the one
/// blocked on the parked stream.
///
/// Two things are asserted that a naive fix would break:
///
///   - RESUMPTION. The peer sends 2 octets, pauses, then sends 2 more, so the
///     read parks with the request half filled. READ-SEQUENCE must answer 4
///     with the octets in order -- the bytes already transferred neither lost
///     nor read twice -- not a short count, which CLHS reserves for EOF.
///   - A GENUINE SHORT COUNT still happens at end of file: 2 octets then a
///     close must answer 2, not hang waiting for the balance.
#[test]
fn a_parked_read_sequence_does_not_lock_the_stream_and_resumes() {
    let output = Command::new("timeout")
        .args([
            "--kill-after=5",
            "90",
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
                (let* ((buffer (make-array 4 :element-type '(unsigned-byte 8)))
                       (reader (egcl-thread:make-thread
                                (lambda ()
                                  (setq reader-state :parked)
                                  (let ((n (read-sequence buffer server)))
                                    (setq reader-state :returned)
                                    n)))))
                  ;; Nothing sent yet, so the read parks.
                  (sleep 1)
                  (assert (eq :parked reader-state))
                  ;; Control: a different stream answers while the reader parks.
                  (assert (input-stream-p client))
                  ;; The point: lock-only accessors on the PARKED stream must
                  ;; answer rather than block behind the park. Before the fix
                  ;; this never returned, and the whole program deadlocked
                  ;; because the bytes below are written by THIS thread.
                  (assert (input-stream-p server))
                  (assert (eq 'unsigned-byte (first (stream-element-type server))))
                  ;; Still parked -- the park is real, not a spin.
                  (assert (eq :parked reader-state))
                  ;; Half the request, then a pause, then the rest: the read
                  ;; must resume rather than restart or give up short.
                  (write-byte 10 client) (write-byte 20 client)
                  (finish-output client)
                  (sleep 0.5)
                  (write-byte 30 client) (write-byte 40 client)
                  (finish-output client)
                  (assert (= 4 (egcl-thread:join-thread reader)))
                  (assert (eq :returned reader-state))
                  (assert (equalp #(10 20 30 40) buffer)))
                (close client))
              (egcl::%socket-close listener))
            ;; A short count still means end of file, not "not yet".
            (let* ((listener (egcl::%socket-listen "127.0.0.1" 0 5))
                   (port (egcl::%socket-local-port listener))
                   (server nil))
              (let ((acceptor (egcl-thread:make-thread
                               (lambda () (setq server (egcl::%socket-accept listener)) :ok)))
                    (client (egcl::%socket-connect "127.0.0.1" port)))
                (egcl-thread:join-thread acceptor)
                (let* ((buffer (make-array 4 :element-type '(unsigned-byte 8)))
                       (reader (egcl-thread:make-thread
                                (lambda () (read-sequence buffer server)))))
                  (sleep 0.3)
                  (write-byte 7 client) (write-byte 8 client)
                  (finish-output client)
                  (sleep 0.3)
                  (close client)
                  (assert (= 2 (egcl-thread:join-thread reader)))
                  (assert (equalp #(7 8 0 0) buffer))))
              (close server)
              (egcl::%socket-close listener))
            (format t "PARKED-READ-SEQUENCE-RESUMES~%")
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
    assert!(stdout.contains("PARKED-READ-SEQUENCE-RESUMES"), "{stdout}");
}
