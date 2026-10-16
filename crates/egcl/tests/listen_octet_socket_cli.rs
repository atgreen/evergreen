// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! LISTEN on an octet socket stream leaves the pending byte for READ-BYTE.
//!
//! LISTEN was READ-CHAR-NO-HANG followed by UNREAD-CHAR for every stream. On
//! an octet socket that took the pending byte as a character and parked it in
//! the character unread slot, where READ-BYTE never looks: LISTEN answered T
//! and the READ-BYTE after it hung. usocket's WAIT-FOR-INPUT calls LISTEN on
//! every stream socket before its backend runs, so AllegroServe hung on its
//! first read after every wait. LISTEN now asks the stream's own readiness for
//! streams whose element type is not CHARACTER.
//!
//! The read after LISTEN carries a receive timeout, so a regression fails the
//! test with a timeout error instead of hanging it.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (let* ((l (egcl::%socket-listen "127.0.0.1" 0 5))
         (port (egcl::%socket-local-port l))
         (c (egcl::%socket-connect "127.0.0.1" port))
         (p (egcl::%socket-accept l)))
    (egcl::%socket-read-timeout p 2000)
    (format t "IDLE ~A~%" (listen p))
    (write-byte 7 c) (finish-output c)
    (sleep 0.2)
    (format t "PENDING ~A~%" (listen p))
    (format t "BYTE ~A~%" (read-byte p))
    (format t "DRAINED ~A~%" (listen p))
    (write-byte 8 c) (finish-output c)
    (sleep 0.2)
    (format t "AGAIN ~A ~A~%" (listen p) (read-byte p))
    (close c)
    (sleep 0.2)
    (format t "EOF ~A~%" (listen p))
    (close p)
    (egcl::%socket-close l))
"#;

#[test]
fn listen_on_an_octet_socket_leaves_the_byte_for_read_byte() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "LISTEN then READ-BYTE on a socket failed: {stdout}\n{stderr}"
    );
    for expected in [
        "IDLE NIL",
        "PENDING T",
        "BYTE 7",
        "DRAINED NIL",
        "AGAIN T 8",
        "EOF NIL",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in: {stdout}");
    }
}
