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
