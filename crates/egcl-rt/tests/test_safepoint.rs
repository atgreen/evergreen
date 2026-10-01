// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::safepoint::*;

#[cfg(unix)]
use egcl_rt::value::T;
#[cfg(unix)]
use egcl_rt::{EgclVal, install_signal_handlers, join_thread, make_thread};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

/// Serializes these tests (bliss-z11t). The safepoint page is ONE mprotected
/// page per process and `wait_for_all_threads`/`resume_all_threads` drive one
/// global handshake, so run concurrently these tests contradict each other:
/// `safepoint_initially_not_requested` fails while `request_then_resume_cycle`
/// holds the page requested, and a rendezvous fails whenever another test has
/// threads running that are not polling. Every test here touches that shared
/// page, so every one takes the lock. Poisoning is recovered rather than
/// propagated, so a single panic does not cascade into the rest.
fn safepoint_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

macro_rules! serialize_safepoint {
    () => {
        let _safepoint_guard = safepoint_lock().lock().unwrap_or_else(|e| e.into_inner());
    };
}

#[cfg(unix)]
static BLOCKED_READ_FD: AtomicI32 = AtomicI32::new(-1);
#[cfg(unix)]
static BLOCKED_READER_ENTERED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static BLOCKED_READ_INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn blocked_reader() -> EgclVal {
    BLOCKED_READER_ENTERED.store(true, Ordering::Release);
    let mut byte = 0_u8;
    let result = unsafe {
        libc::read(
            BLOCKED_READ_FD.load(Ordering::Acquire),
            (&mut byte as *mut u8).cast(),
            1,
        )
    };
    if result < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
        BLOCKED_READ_INTERRUPTED.store(true, Ordering::Release);
    }
    poll_safepoint();
    T
}

#[test]
fn safepoint_page_init_succeeds() {
    serialize_safepoint!();
    assert!(SafepointPage::init().is_ok());
}

#[test]
fn safepoint_page_address_non_null_and_aligned() {
    serialize_safepoint!();
    let page = SafepointPage::init().unwrap();
    let addr = page.address();
    assert!(!addr.is_null());
    assert_eq!(addr as usize % 4096, 0, "must be page-aligned");
}

#[test]
fn safepoint_initially_not_requested() {
    serialize_safepoint!();
    let page = SafepointPage::init().unwrap();
    assert!(!page.is_requested());
}

#[test]
fn request_then_resume_cycle() {
    serialize_safepoint!();
    let page = SafepointPage::init().unwrap();
    for _ in 0..3 {
        page.request_safepoint().unwrap();
        assert!(page.is_requested());
        page.resume().unwrap();
        assert!(!page.is_requested());
    }
}

#[test]
fn request_protects_poll_page_and_resume_restores_readability() {
    serialize_safepoint!();
    let page = SafepointPage::init().unwrap();

    page.request_safepoint().unwrap();
    assert!(page.poll_page_is_protected());

    page.resume().unwrap();
    assert!(!page.poll_page_is_protected());
    unsafe {
        std::ptr::read_volatile(page.address());
    }
}

#[test]
fn resume_without_request_is_harmless() {
    serialize_safepoint!();
    let page = SafepointPage::init().unwrap();
    assert!(page.resume().is_ok());
}

#[test]
fn wait_and_resume_all_threads() {
    serialize_safepoint!();
    assert!(wait_for_all_threads().is_ok());
    assert!(resume_all_threads().is_ok());
}

#[test]
fn poll_safepoint_does_not_panic() {
    serialize_safepoint!();
    poll_safepoint();
}

#[test]
fn enter_safepoint_does_not_panic() {
    serialize_safepoint!();
    enter_safepoint();
}

#[test]
#[cfg(unix)]
fn sigusr1_fallback_interrupts_a_blocked_syscall_and_reaches_safepoint() {
    serialize_safepoint!();
    install_signal_handlers().unwrap();
    BLOCKED_READER_ENTERED.store(false, Ordering::Release);
    BLOCKED_READ_INTERRUPTED.store(false, Ordering::Release);
    let mut pipe_fds = [-1_i32; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    BLOCKED_READ_FD.store(pipe_fds[0], Ordering::Release);
    let thread = make_thread(native_entry::entry(blocked_reader)).unwrap();

    let entered_deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while !BLOCKED_READER_ENTERED.load(Ordering::Acquire)
        && std::time::Instant::now() < entered_deadline
    {
        std::thread::yield_now();
    }
    assert!(BLOCKED_READER_ENTERED.load(Ordering::Acquire));

    let started = std::time::Instant::now();
    wait_for_all_threads().unwrap();
    let elapsed = started.elapsed();
    resume_all_threads().unwrap();

    // Always release a restarted read before joining, so a failed assertion
    // cannot strand the helper thread.
    let byte = [1_u8];
    unsafe {
        libc::write(pipe_fds[1], byte.as_ptr().cast(), 1);
    }
    assert_eq!(join_thread(thread).unwrap(), T);
    unsafe {
        libc::close(pipe_fds[0]);
        libc::close(pipe_fds[1]);
    }

    assert!(BLOCKED_READ_INTERRUPTED.load(Ordering::Acquire));
    assert!(
        elapsed < std::time::Duration::from_millis(300),
        "SIGUSR1 fallback took {elapsed:?}"
    );
}

#[path = "support/native_entry.rs"]
mod native_entry;
