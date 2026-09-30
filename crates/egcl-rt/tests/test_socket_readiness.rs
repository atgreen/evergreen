// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Readiness is multiplexed independently of the number of waiting fibers.
#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use egcl_rt::sync::{IoInterest, wait_socket};
use egcl_rt::thread::{FiberState, fiber_state, make_fiber};
use egcl_rt::value::EgclVal;
use egcl_rt::{SchedulerConfig, SchedulerGroup};

static SOCKETS: OnceLock<Vec<TcpStream>> = OnceLock::new();
static NEXT: AtomicUsize = AtomicUsize::new(0);

fn wait_for_peer() -> EgclVal {
    let index = NEXT.fetch_add(1, Ordering::AcqRel);
    let ready = wait_socket(
        &SOCKETS.get().unwrap()[index],
        IoInterest::Read,
        Some(Duration::from_secs(10)),
    )
    .unwrap();
    EgclVal::from_fixnum(i64::from(ready))
}

#[test]
fn many_waiting_sockets_share_one_carrier_and_bounded_service_threads() {
    const COUNT: usize = 128;
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let mut sockets = Vec::new();
    let mut peers = Vec::new();
    for _ in 0..COUNT {
        sockets.push(TcpStream::connect(address).unwrap());
        peers.push(listener.accept().unwrap().0);
    }
    SOCKETS.set(sockets).unwrap();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    #[cfg(target_os = "linux")]
    let baseline_threads = std::fs::read_dir("/proc/self/task").unwrap().count();
    let mut fibers = Vec::new();
    for _ in 0..COUNT {
        let entry = unsafe { EgclVal::from_function_ptr(wait_for_peer as *const () as *mut u8) };
        let fiber = make_fiber(entry).unwrap();
        fibers.push(fiber);
        group.submit(fiber).unwrap();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fibers
        .iter()
        .all(|id| fiber_state(*id) == Some(FiberState::Waiting))
    {
        assert!(
            Instant::now() < deadline,
            "not all fibers reached their socket wait"
        );
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    #[cfg(target_os = "linux")]
    assert!(
        std::fs::read_dir("/proc/self/task").unwrap().count() <= baseline_threads + 3,
        "socket waiters created native threads proportional to fiber count"
    );
    for peer in &mut peers {
        peer.write_all(&[42]).unwrap();
    }
    let results = group.finish().unwrap();
    assert_eq!(results, vec![EgclVal::from_fixnum(1); COUNT]);
}

#[cfg(unix)]
#[test]
fn native_timeout_keeps_its_deadline_when_signals_interrupt_poll() {
    // This integration-test process owns the temporary signal disposition.
    unsafe extern "C" fn ignore(_: libc::c_int) {}
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    let mut previous: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = ignore as *const () as usize;
    unsafe {
        libc::sigemptyset(&mut action.sa_mask);
        assert_eq!(libc::sigaction(libc::SIGWINCH, &action, &mut previous), 0);
    }
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    let _peer = listener.accept().unwrap();
    let owner = unsafe { libc::pthread_self() } as usize;
    let signals = std::thread::spawn(move || {
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(5));
            assert_eq!(
                unsafe { libc::pthread_kill(owner as libc::pthread_t, libc::SIGWINCH) },
                0
            );
        }
    });
    let started = Instant::now();
    let ready = wait_socket(&socket, IoInterest::Read, Some(Duration::from_millis(50))).unwrap();
    let elapsed = started.elapsed();
    signals.join().unwrap();
    unsafe {
        libc::sigaction(libc::SIGWINCH, &previous, std::ptr::null_mut());
    }
    assert!(!ready);
    assert!(
        elapsed >= Duration::from_millis(50),
        "timeout returned early: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_millis(250),
        "signals restarted timeout: {elapsed:?}"
    );
}
