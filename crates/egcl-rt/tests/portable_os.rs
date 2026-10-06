// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exercise OS ABI boundaries on the host and under QEMU on foreign targets.
use egcl_rt::syscall as os;

#[test]
fn stack_guard_uses_the_kernel_page_size() {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as usize };
    assert_eq!(os::page_size(), page);
    let stack = egcl_rt::stack::EgclStack::new(4097);
    let base = stack.base() as usize;
    let guard = stack.guard_base().unwrap() as usize;
    assert_eq!(base % page, 0);
    assert_eq!(guard % page, 0);
    assert_eq!(guard - base, 4097_usize.div_ceil(page) * page);
}

#[test]
fn mapped_memory_and_errors() {
    unsafe {
        let p = os::mmap(
            std::ptr::null_mut(),
            65536,
            os::PROT_READ | os::PROT_WRITE,
            os::MAP_PRIVATE | os::MAP_ANONYMOUS,
            -1,
            0,
        )
        .unwrap();
        p.write(42);
        os::mprotect(p, 65536, os::PROT_READ).unwrap();
        assert_eq!(p.read(), 42);
        os::munmap(p, 65536).unwrap();
        assert!(os::mprotect(std::ptr::without_provenance_mut(1), 65536, os::PROT_READ).is_err());
    }
    assert_eq!(os::getpid() as u32, std::process::id());
    assert_eq!(os::gettid(), os::cached_tid());
    assert!(os::getrlimit(os::RLIMIT_STACK).unwrap().rlim_cur > 0);
    assert!(os::thread_cpu_time_ns().is_ok());
    assert!(os::current_sigaltstack().is_ok());
}

#[test]
fn image_tag_identifies_the_target() {
    let arch = if cfg!(target_arch = "x86_64") {
        1
    } else if cfg!(target_arch = "aarch64") {
        2
    } else if cfg!(target_arch = "powerpc64") {
        3
    } else if cfg!(target_arch = "s390x") {
        4
    } else {
        panic!("unlisted test architecture")
    };
    // The OS half is not always Linux: Android runs the same kernel with a
    // different libc, and an image saved under bionic must not load in a glibc or
    // musl EGCL, so it carries its own tag (Os::Android = 4; bliss-w2vp).
    let os = if cfg!(target_os = "android") {
        5
    } else if cfg!(target_os = "macos") {
        2
    } else {
        1
    };
    assert_eq!(egcl_rt::current_platform_tag(), (arch << 32) | os);
}

#[cfg(any(target_os = "linux", target_os = "android"))]
#[test]
fn epoll_preserves_a_full_width_cookie() {
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    let (read, mut write) = UnixStream::pair().unwrap();
    let epfd = os::epoll_create1(os::EPOLL_CLOEXEC).unwrap();
    let cookie = 0x1234_5678_9abc_def0;
    let mut event = os::EpollEvent {
        events: 1,
        data: cookie,
    };
    unsafe {
        os::epoll_ctl(epfd, os::EPOLL_CTL_ADD, read.as_raw_fd(), &mut event).unwrap();
    }
    write.write_all(b"x").unwrap();
    let mut ready = [os::EpollEvent { events: 0, data: 0 }; 2];
    let count = unsafe { os::epoll_wait(epfd, ready.as_mut_ptr(), 2, 1000).unwrap() };
    assert_eq!(count, 1);
    let received = ready[0].data;
    assert_eq!(received, cookie);
    let mut pollfd = os::PollFd {
        fd: read.as_raw_fd(),
        events: os::POLLIN,
        revents: 0,
    };
    assert_eq!(unsafe { os::poll(&mut pollfd, 1, 0).unwrap() }, 1);
    assert_ne!(pollfd.revents & os::POLLIN, 0);
    unsafe {
        libc::close(epfd);
    }
}

#[test]
fn signal_handler_can_return() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static SEEN: AtomicBool = AtomicBool::new(false);
    extern "C" fn handler(_: i32) {
        SEEN.store(true, Ordering::SeqCst);
    }
    unsafe {
        os::rt_sigaction(os::SIGUSR1, handler as *const () as usize, os::SA_RESTART).unwrap();
    }
    os::tgkill(os::getpid(), os::gettid(), os::SIGUSR1).unwrap();
    assert!(SEEN.load(Ordering::SeqCst));
}
