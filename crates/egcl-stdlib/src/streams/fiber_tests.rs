// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Stream ownership must survive a parked operation without blocking its carrier.
use super::*;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;
use egcl_rt::sync::EgclSemaphore;
use egcl_rt::thread::{FiberState, fiber_state, make_fiber};
use egcl_rt::{SchedulerConfig, SchedulerGroup};

static FORMAT: AtomicBool = AtomicBool::new(false);
static STREAM: AtomicU64 = AtomicU64::new(0);
static WRITE_STREAM: AtomicU64 = AtomicU64::new(0);
static RELEASE: OnceLock<EgclSemaphore> = OnceLock::new();

fn owner() -> EgclVal {
    let stream = EgclVal::from_raw(STREAM.load(Ordering::Acquire));
    let mut state = lock_stream(stream).unwrap();
    state.stream_write_char(EgclVal::from_char('A')).unwrap();
    RELEASE.get().unwrap().wait(None).unwrap();
    state.stream_write_char(EgclVal::from_char('B')).unwrap();
    NIL
}

fn contender() -> EgclVal {
    egcl_rt::rooted!(stream = EgclVal::from_raw(WRITE_STREAM.load(Ordering::Acquire)));
    if FORMAT.load(Ordering::Acquire) {
        let text = make_lisp_string("CD");
        crate::format::format(*stream, "~A", &[text]).unwrap();
    } else {
        let text = make_lisp_string("C");
        stream_write_string(*stream, text, 0, None).unwrap();
    }
    NIL
}

fn release() -> EgclVal {
    RELEASE.get().unwrap().signal(1).unwrap();
    NIL
}

fn entry(f: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(f as *const () as *mut u8) }
}

#[test]
fn parked_stream_owner_keeps_operations_atomic_without_blocking_carrier() {
    run_case(
        "parked_stream_owner_keeps_operations_atomic_without_blocking_carrier",
        false,
    );
}

#[test]
fn format_retains_destination_and_arguments_across_stream_wait() {
    run_case(
        "format_retains_destination_and_arguments_across_stream_wait",
        true,
    );
}

fn run_case(name: &str, format: bool) {
    const CHILD: &str = "EGCL_STREAM_LOCK_TEST_CHILD";
    if std::env::var_os(CHILD).is_none() {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("streams::fiber_tests::{name}"),
                "--nocapture",
            ])
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
        eprintln!("stream ownership blocked its carrier");
        std::process::exit(124);
    });
    egcl_rt::thread::current_thread_id();
    egcl_rt::gc::ensure_heap_initialized();
    install_gc_hooks();
    assert!(RELEASE.set(EgclSemaphore::new(None, 0).unwrap()).is_ok());
    egcl_rt::rooted!(stream = make_string_output_stream(NIL).unwrap());
    egcl_rt::rooted!(second_output = make_string_output_stream(NIL).unwrap());
    egcl_rt::rooted!(broadcast = make_broadcast_stream(&[*stream, *second_output]).unwrap());
    FORMAT.store(format, Ordering::Release);
    STREAM.store(
        if format {
            broadcast.to_raw()
        } else {
            stream.to_raw()
        },
        Ordering::Release,
    );
    WRITE_STREAM.store(broadcast.to_raw(), Ordering::Release);
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let first = make_fiber(entry(owner)).unwrap();
    group.submit(first).unwrap();
    while fiber_state(first) != Some(FiberState::Blocked) {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    let second = make_fiber(entry(contender)).unwrap();
    group.submit(second).unwrap();
    // Queue the releasing fiber only once the contender has actually parked
    // on the same stream. A native mutex strands this one-carrier group here.
    while fiber_state(second) != Some(FiberState::Blocked) {
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    // Relocate the blocked broadcast, its children, and its string argument.
    let original = broadcast.to_raw();
    egcl_rt::collect_t0_minor().unwrap();
    assert_ne!(
        broadcast.to_raw(),
        original,
        "must relocate the blocked stream"
    );
    group.submit(make_fiber(entry(release)).unwrap()).unwrap();
    group.finish().unwrap();
    let text = get_output_stream_string(*stream).unwrap();
    assert_eq!(
        extract_string_str(text).unwrap(),
        if format { "ABCD" } else { "ABC" }
    );
    let text = get_output_stream_string(*second_output).unwrap();
    assert_eq!(
        extract_string_str(text).unwrap(),
        if format { "ABCD" } else { "C" }
    );
}
