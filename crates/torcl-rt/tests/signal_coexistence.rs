#![cfg(unix)]
use std::sync::{Arc, Barrier};
use torcl_rt::runtime::install_signal_handlers;

// One test owns process dispositions; individual probes use fresh native threads.
#[test]
fn signal_reentry_preserves_foreign_handlers_and_thread_stack_ownership() {
    install_signal_handlers().unwrap();
    unsafe extern "C" fn foreign_handler(_: i32) {}
    unsafe {
        let mut prior: libc::sigaction = std::mem::zeroed();
        let mut foreign: libc::sigaction = std::mem::zeroed();
        foreign.sa_sigaction = foreign_handler as *const () as usize;
        assert_eq!(libc::sigaction(libc::SIGFPE, &foreign, &mut prior), 0);
        install_signal_handlers().unwrap();
        let mut actual: libc::sigaction = std::mem::zeroed();
        assert_eq!(
            libc::sigaction(libc::SIGFPE, std::ptr::null(), &mut actual),
            0
        );
        assert_eq!(
            actual.sa_sigaction, foreign.sa_sigaction,
            "reentry replaced a host handler"
        );
        assert_eq!(
            libc::sigaction(libc::SIGFPE, &prior, std::ptr::null_mut()),
            0
        );
    }
    let barrier = Arc::new(Barrier::new(2));
    let workers: Vec<_> = (0..2)
        .map(|_| {
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                install_signal_handlers().unwrap();
                let stack = torcl_rt::syscall::current_sigaltstack().unwrap();
                assert_eq!(stack.ss_flags & torcl_rt::syscall::SS_DISABLE, 0);
                barrier.wait(); // both allocations must remain live simultaneously
                stack.ss_sp as usize
            })
        })
        .collect();
    let addresses: Vec<_> = workers.into_iter().map(|t| t.join().unwrap()).collect();
    assert_ne!(
        addresses[0], addresses[1],
        "threads share alternate-stack storage"
    );
    std::thread::spawn(|| unsafe {
        let mut storage = vec![0u8; 128 * 1024];
        let host = libc::stack_t {
            ss_sp: storage.as_mut_ptr().cast(),
            ss_flags: 0,
            ss_size: storage.len(),
        };
        assert_eq!(libc::sigaltstack(&host, std::ptr::null_mut()), 0);
        install_signal_handlers().unwrap();
        let actual = torcl_rt::syscall::current_sigaltstack().unwrap();
        assert_eq!(
            actual.ss_sp,
            storage.as_mut_ptr(),
            "host alternate stack replaced"
        );
        let disable = libc::stack_t {
            ss_sp: std::ptr::null_mut(),
            ss_flags: libc::SS_DISABLE,
            ss_size: 0,
        };
        assert_eq!(libc::sigaltstack(&disable, std::ptr::null_mut()), 0);
    })
    .join()
    .unwrap();
}
