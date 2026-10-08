// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(unix)]

use egcl_rt::thread::NativeThreadState;
use egcl_stdlib::streams::*;
use std::process::Command;
use std::time::{Duration, Instant};

fn collect_and_close_waiting_listener(readiness: bool, test_name: &str) {
    const CHILD: &str = "EGCL_LISTENER_GC_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture"])
            .env(CHILD, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    std::thread::spawn(|| {
        std::thread::sleep(Duration::from_secs(10));
        eprintln!("listener wait prevented collection or did not wake on close");
        std::process::exit(124);
    });
    let waiting_thread = egcl_rt::thread::current_thread();
    egcl_rt::gc::ensure_heap_initialized();
    install_gc_hooks();
    let listener = socket_listen("127.0.0.1", 0, 5).unwrap();
    egcl_rt::rooted!(kept = egcl_rt::gc::alloc_double_float(41.0));
    let original = kept.to_raw();
    let collector = std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        while waiting_thread.state() != NativeThreadState::Blocked {
            assert!(
                Instant::now() < deadline,
                "listener never entered a GC-safe wait"
            );
            std::thread::yield_now();
        }
        egcl_rt::collect_t0_minor().unwrap();
        socket_close_listener(listener);
    });
    if readiness {
        assert!(socket_listener_ready(listener, -1).is_err());
    } else {
        assert!(socket_accept(listener).is_err());
    }
    collector.join().unwrap();
    assert_ne!(
        kept.to_raw(),
        original,
        "the blocked caller's root must move"
    );
    assert_eq!(unsafe { kept.as_ptr().add(8).cast::<f64>().read() }, 41.0);
    assert_eq!(socket_local_port(listener), None);
}

#[test]
fn native_accept_allows_gc_and_wakes_on_close() {
    collect_and_close_waiting_listener(false, "native_accept_allows_gc_and_wakes_on_close");
}

#[test]
fn native_readiness_allows_gc_and_wakes_on_close() {
    collect_and_close_waiting_listener(true, "native_readiness_allows_gc_and_wakes_on_close");
}
