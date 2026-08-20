use bliss_rt::safepoint::*;

#[cfg(unix)]
use bliss_rt::value::T;
#[cfg(unix)]
use bliss_rt::{install_signal_handlers, join_thread, make_thread, BlissVal};
#[cfg(unix)]
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

#[cfg(unix)]
static BLOCKED_READ_FD: AtomicI32 = AtomicI32::new(-1);
#[cfg(unix)]
static BLOCKED_READER_ENTERED: AtomicBool = AtomicBool::new(false);
#[cfg(unix)]
static BLOCKED_READ_INTERRUPTED: AtomicBool = AtomicBool::new(false);

#[cfg(unix)]
fn blocked_reader() -> BlissVal {
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
    assert!(SafepointPage::init().is_ok());
}

#[test]
fn safepoint_page_address_non_null_and_aligned() {
    let page = SafepointPage::init().unwrap();
    let addr = page.address();
    assert!(!addr.is_null());
    assert_eq!(addr as usize % 4096, 0, "must be page-aligned");
}

#[test]
fn safepoint_initially_not_requested() {
    let page = SafepointPage::init().unwrap();
    assert!(!page.is_requested());
}

#[test]
fn request_then_resume_cycle() {
    let page = SafepointPage::init().unwrap();
    for _ in 0..3 {
        page.request_safepoint().unwrap();
        assert!(page.is_requested());
        page.resume().unwrap();
        assert!(!page.is_requested());
    }
}

#[test]
fn resume_without_request_is_harmless() {
    let page = SafepointPage::init().unwrap();
    assert!(page.resume().is_ok());
}

#[test]
fn wait_and_resume_all_threads() {
    assert!(wait_for_all_threads().is_ok());
    assert!(resume_all_threads().is_ok());
}

#[test]
fn poll_safepoint_does_not_panic() {
    poll_safepoint();
}

#[test]
fn enter_safepoint_does_not_panic() {
    enter_safepoint();
}

#[test]
#[cfg(unix)]
fn sigusr1_fallback_interrupts_a_blocked_syscall_and_reaches_safepoint() {
    install_signal_handlers().unwrap();
    BLOCKED_READER_ENTERED.store(false, Ordering::Release);
    BLOCKED_READ_INTERRUPTED.store(false, Ordering::Release);
    let mut pipe_fds = [-1_i32; 2];
    assert_eq!(unsafe { libc::pipe(pipe_fds.as_mut_ptr()) }, 0);
    BLOCKED_READ_FD.store(pipe_fds[0], Ordering::Release);
    let thread =
        make_thread(unsafe { BlissVal::from_function_ptr(blocked_reader as *const () as *mut u8) })
            .unwrap();

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
