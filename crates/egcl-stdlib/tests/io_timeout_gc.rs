// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::net::TcpListener;
use std::time::Duration;
use egcl_rt::EgclError;
use egcl_stdlib::streams::*;

#[test]
fn timeout_error_keeps_its_stream_live_and_relocates_it() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let stream = socket_connect(
        "127.0.0.1", listener.local_addr().unwrap().port(),
        Some(Duration::from_secs(1)),
    ).unwrap();
    let (_peer, _) = listener.accept().unwrap();
    socket_set_read_timeout(stream, Some(Duration::from_millis(20))).unwrap();
    egcl_rt::rooted!(error = stream_read_byte(stream).unwrap_err());
    let original = match &*error {
        EgclError::IoTimeout { stream, .. } => stream.to_raw(),
        other => panic!("expected typed timeout, got {other:?}"),
    };
    egcl_rt::gc::collect_t0_minor().unwrap();
    match &*error {
        EgclError::IoTimeout { stream, .. } => {
            assert_ne!(stream.to_raw(), original, "the probe must actually relocate");
            assert!(open_stream_p(*stream));
            assert_eq!(socket_read_timeout(*stream).unwrap(), Some(Duration::from_millis(20)));
            close(*stream, false).unwrap();
        }
        _ => unreachable!(),
    }
}
