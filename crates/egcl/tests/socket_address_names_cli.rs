// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A socket's local and peer names, and a listener's local name.
//!
//! Nothing answered a stream's local or peer address and port, nor a
//! listener's address, beyond `%SOCKET-LOCAL-PORT` on a listener. usocket's
//! `get-local-address`, `get-peer-address`, `get-peer-port` and `get-local-name`
//! had nothing to build on, and AllegroServe asks for the local host of every
//! accepted connection and logs the peer, so it dropped every request after the
//! accept. The three new primitives answer `(address-string port)`.
//!
//! The test connects a socket to itself, so the accepted socket's peer name is
//! the client's local name and the client's peer name is the listener's name,
//! which pins every value without knowing the ephemeral ports in advance.

use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_egcl");

const PROGRAM: &str = r#"
  (let* ((l (egcl::%socket-listen "127.0.0.1" 0 5))
         (listener-name (egcl::%socket-listener-local-name l))
         (port (egcl::%socket-local-port l))
         (c (egcl::%socket-connect "127.0.0.1" (second listener-name)))
         (p (egcl::%socket-accept l))
         (client-local (egcl::%socket-local-name c))
         (client-peer (egcl::%socket-peer-name c))
         (served-local (egcl::%socket-local-name p))
         (served-peer (egcl::%socket-peer-name p)))
    (format t "LISTENER ~A ~A~%" (first listener-name) (eql (second listener-name) port))
    (format t "CLIENT-PEER-IS-LISTENER ~A~%" (equal client-peer listener-name))
    (format t "SERVED-LOCAL-IS-LISTENER ~A~%" (equal served-local listener-name))
    (format t "SERVED-PEER-IS-CLIENT-LOCAL ~A~%" (equal served-peer client-local))
    (format t "EPHEMERAL ~A~%" (and (integerp (second client-local)) (> (second client-local) 0)
                                    (/= (second client-local) port)))
    (close c) (close p)
    (egcl::%socket-close l))
"#;

#[test]
fn socket_names_describe_both_ends_of_a_connection() {
    let output = Command::new(BIN)
        .args(["--no-init", "--eval", PROGRAM])
        .output()
        .expect("run egcl");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "socket names failed: {stdout}\n{stderr}"
    );
    for expected in [
        "LISTENER 127.0.0.1 T",
        "CLIENT-PEER-IS-LISTENER T",
        "SERVED-LOCAL-IS-LISTENER T",
        "SERVED-PEER-IS-CLIENT-LOCAL T",
        "EPHEMERAL T",
    ] {
        assert!(stdout.contains(expected), "missing {expected:?} in: {stdout}");
    }
}
