// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Linux ABI adaptation for non-x86-64 hosts. Keep the runtime's negative-errno
//! convention even though libc syscall wrappers return -1 and set errno.
use super::*;

/// Base page size of the running kernel, including 64 KiB POWER/ARM kernels.
pub fn page_size() -> usize {
    static PAGE: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *PAGE.get_or_init(|| {
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(
            page > 0 && (page as usize).is_power_of_two(),
            "invalid kernel page size"
        );
        page as usize
    })
}

pub mod nr {
    pub const WRITE: usize = libc::SYS_write as usize;
    pub const MUNMAP: usize = libc::SYS_munmap as usize;
    pub const MPROTECT: usize = libc::SYS_mprotect as usize;
    pub const PRLIMIT64: usize = libc::SYS_prlimit64 as usize;
    pub const GETTID: usize = libc::SYS_gettid as usize;
    pub const GETPID: usize = libc::SYS_getpid as usize;
    pub const TGKILL: usize = libc::SYS_tgkill as usize;
    pub const EXIT_GROUP: usize = libc::SYS_exit_group as usize;
    pub const SCHED_YIELD: usize = libc::SYS_sched_yield as usize;
    pub const SIGALTSTACK: usize = libc::SYS_sigaltstack as usize;
    pub const CLOCK_GETTIME: usize = libc::SYS_clock_gettime as usize;
    pub const EPOLL_CREATE1: usize = libc::SYS_epoll_create1 as usize;
    pub const EPOLL_CTL: usize = libc::SYS_epoll_ctl as usize;
}

/// The thread-local errno, whatever the platform calls its accessor. bionic
/// spells it `__errno`, glibc and musl `__errno_location`, and the libc crate
/// exposes each only where it exists — so this is the single place that has to
/// know (bliss-w2vp).
///
/// # Safety
/// Reads the calling thread's errno; only meaningful immediately after a failed
/// libc call on that same thread.
unsafe fn errno() -> i32 {
    #[cfg(target_os = "android")]
    let value = *libc::__errno();
    #[cfg(not(target_os = "android"))]
    let value = *libc::__errno_location();
    value
}

unsafe fn negative_errno(ret: libc::c_long) -> isize {
    if ret == -1 {
        -(errno() as isize)
    } else {
        ret as isize
    }
}

macro_rules! raw_syscall {
    ($name:ident $(, $arg:ident)*) => {
        /// Invoke a Linux syscall, returning a negative errno on failure.
        ///
        /// # Safety
        /// The number and arguments must obey the target kernel's syscall ABI.
        #[inline]
        pub unsafe fn $name(n: usize $(, $arg: usize)*) -> isize {
            negative_errno(libc::syscall(n as libc::c_long $(, $arg)*))
        }
    };
}
raw_syscall!(syscall0);
raw_syscall!(syscall1, a1);
raw_syscall!(syscall2, a1, a2);
raw_syscall!(syscall3, a1, a2, a3);
raw_syscall!(syscall4, a1, a2, a3, a4);
raw_syscall!(syscall6, a1, a2, a3, a4, a5, a6);

/// Map memory, including s390x's indirect mmap syscall convention.
///
/// # Safety
/// Arguments must form a valid mmap request; the caller owns the mapping.
pub unsafe fn mmap(
    addr: *mut u8,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> Result<*mut u8, i32> {
    let ptr = libc::mmap(addr.cast(), len, prot, flags, fd, offset);
    if ptr == libc::MAP_FAILED {
        Err(errno())
    } else {
        Ok(ptr.cast())
    }
}

/// Schedule or cancel SIGALRM (libc uses setitimer where alarm is absent).
pub fn alarm(seconds: u32) -> u32 {
    unsafe { libc::alarm(seconds) }
}

/// Install a signal handler using the target's sigaction layout and restorer.
///
/// # Safety
/// Handler must have the signal-handler ABI appropriate to extra_flags.
pub unsafe fn rt_sigaction(sig: i32, handler: usize, extra_flags: u64) -> Result<(), i32> {
    let mut act: libc::sigaction = std::mem::zeroed();
    act.sa_sigaction = handler;
    act.sa_flags = extra_flags as libc::c_int;
    libc::sigemptyset(&mut act.sa_mask);
    check(negative_errno(
        libc::sigaction(sig, &act, std::ptr::null_mut()) as _,
    ))
    .map(|_| ())
}

/// Install a three-argument SA_SIGINFO handler.
///
/// # Safety
/// Handler must accept the signal number, siginfo pointer and context pointer.
pub unsafe fn rt_sigaction_siginfo(sig: i32, handler: usize, flags: u64) -> Result<(), i32> {
    rt_sigaction(sig, handler, flags | SA_SIGINFO)
}

/// Wait for file descriptors.
///
/// # Safety
/// fds must describe nfds writable PollFd records.
pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout_ms: i32) -> Result<usize, i32> {
    check(negative_errno(
        libc::poll(fds.cast(), nfds as _, timeout_ms) as _,
    ))
}

/// Wait for epoll events (libc uses epoll_pwait where epoll_wait is absent).
///
/// # Safety
/// events must point to storage for maxevents records.
pub unsafe fn epoll_wait(
    epfd: i32,
    events: *mut EpollEvent,
    maxevents: i32,
    timeout_ms: i32,
) -> Result<usize, i32> {
    check(negative_errno(
        libc::epoll_wait(epfd, events.cast(), maxevents, timeout_ms) as _,
    ))
}

// Shared structs are passed across the kernel/libc boundary. Check their
// complete layouts, not just size, particularly on big-endian s390x.
const _: () = {
    assert!(size_of::<EpollEvent>() == size_of::<libc::epoll_event>());
    assert!(std::mem::offset_of!(EpollEvent, data) == std::mem::offset_of!(libc::epoll_event, u64));
    assert!(size_of::<StackT>() == size_of::<libc::stack_t>());
    assert!(
        std::mem::offset_of!(StackT, ss_flags) == std::mem::offset_of!(libc::stack_t, ss_flags)
    );
    assert!(std::mem::offset_of!(StackT, ss_size) == std::mem::offset_of!(libc::stack_t, ss_size));
    assert!(size_of::<PollFd>() == size_of::<libc::pollfd>());
};
