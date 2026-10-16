// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Readiness is multiplexed independently of the number of waiting fibers.
#![cfg(egcl_fibers)]
use egcl_rt::sync::{IoInterest, wait_socket};
use egcl_rt::thread::{FiberState, fiber_state, make_fiber};
use egcl_rt::value::EgclVal;
use egcl_rt::{SchedulerConfig, SchedulerGroup};
use std::io::Write;
use std::net::{TcpListener, TcpStream};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

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
        let entry = native_entry::entry(wait_for_peer);
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

#[path = "support/native_entry.rs"]
mod native_entry;

/// A directed interrupt must end a wait that is parked in the kernel.
///
/// `post_interrupt` reaches a thread that is RUNNING Lisp, because every tier
/// polls. A thread parked in `poll` touches no polling page, so R13.14's
/// flag-only SIGUSR1 is what ends the syscall -- but `native_poll` used to
/// treat the resulting `EINTR` as "recompute the remaining timeout and poll
/// again", so the signal achieved nothing and a wait with no deadline parked
/// for ever (bliss-5uklt).
///
/// The wait is given a long but finite deadline so a regression FAILS on the
/// elapsed assertion instead of hanging the suite. Without the fix this returns
/// `Ok(false)` after the full 30s; with it, `Err(Interrupt)` almost at once.
/// `native_timeout_keeps_its_deadline_when_signals_interrupt_poll` above is the
/// other half of the contract: an UNRELATED signal must still not shorten a
/// deadline, and only a pending interrupt may.
#[test]
fn a_directed_interrupt_ends_a_wait_parked_in_the_kernel() {
    use egcl_rt::thread::{current_thread_id, interrupt_thread};
    use egcl_rt::value::T;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    // Accepted but silent: the waiter below can only be released by the
    // interrupt, never by data arriving.
    let _peer = listener.accept().unwrap();

    let (tx, rx) = std::sync::mpsc::channel();
    let waiter = std::thread::spawn(move || {
        // Registers this thread, which is what gives the interrupt an address
        // and publishes the kernel TID the directed signal needs.
        tx.send(current_thread_id()).unwrap();
        let started = Instant::now();
        let result = wait_socket(
            &socket,
            IoInterest::Read,
            Some(Duration::from_secs(30)),
        );
        (result, started.elapsed())
    });

    let target = rx.recv().unwrap();
    // Let the waiter actually reach the syscall before signalling it.
    std::thread::sleep(Duration::from_millis(250));
    interrupt_thread(target, T).expect("interrupt must be accepted");

    let (result, elapsed) = waiter.join().unwrap();
    assert!(
        matches!(result, Err(egcl_rt::EgclError::Interrupt)),
        "parked wait did not report the interrupt: {result:?} after {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the interrupt did not end the parked wait: {elapsed:?}"
    );
}

/// A posted interrupt must land on the channel the evaluators actually poll.
///
/// `interrupt_pending` on its own is read only after the entry returns, so an
/// interrupt posted to running code could not reach it. `PendingSignal::Interrupt`
/// is what every tier takes at a safepoint -- the tree-walker's backward GO, the
/// bytecode run_loop, the native back-edge poll -- and is how SIGINT already
/// reaches a hot loop. This asserts the posting side raises it; the tiers'
/// consumption is already covered by their own suites.
#[test]
fn a_posted_interrupt_raises_the_signal_the_evaluators_poll() {
    use egcl_rt::thread::{
        PendingSignal, current_thread_id, interrupt_thread, take_current_pending_signal,
    };
    use egcl_rt::value::T;

    assert_eq!(
        take_current_pending_signal(),
        None,
        "a previous test left a signal posted"
    );
    interrupt_thread(current_thread_id(), T).expect("interrupt must be accepted");
    assert_eq!(
        take_current_pending_signal(),
        Some(PendingSignal::Interrupt),
        "a posted interrupt must be visible on the polled channel"
    );
    assert_eq!(take_current_pending_signal(), None, "consumed exactly once");
}
