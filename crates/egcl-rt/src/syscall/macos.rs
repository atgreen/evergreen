// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Darwin implementations of the OS operations used by the EGCL runtime.

use std::io;
use std::sync::OnceLock;

fn errno() -> i32 {
    io::Error::last_os_error()
        .raw_os_error()
        .unwrap_or(libc::EIO)
}

pub fn page_size() -> usize {
    static PAGE: OnceLock<usize> = OnceLock::new();
    *PAGE.get_or_init(|| {
        let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        assert!(size > 0 && (size as usize).is_power_of_two());
        size as usize
    })
}

pub const PROT_NONE: i32 = libc::PROT_NONE;
pub const PROT_READ: i32 = libc::PROT_READ;
pub const PROT_WRITE: i32 = libc::PROT_WRITE;
pub const PROT_EXEC: i32 = libc::PROT_EXEC;
pub const MAP_PRIVATE: i32 = libc::MAP_PRIVATE;
pub const MAP_ANONYMOUS: i32 = libc::MAP_ANONYMOUS;
pub const MAP_FIXED: i32 = libc::MAP_FIXED;

/// # Safety
/// The caller supplies valid mapping arguments and owns the returned mapping.
pub unsafe fn mmap(
    addr: *mut u8,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> Result<*mut u8, i32> {
    let ptr = unsafe { libc::mmap(addr.cast(), len, prot, flags, fd, offset) };
    if ptr == libc::MAP_FAILED {
        Err(errno())
    } else {
        Ok(ptr.cast())
    }
}

/// # Safety
/// `addr` and `len` name a live mapping.
pub unsafe fn munmap(addr: *mut u8, len: usize) -> Result<(), i32> {
    if unsafe { libc::munmap(addr.cast(), len) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

/// # Safety
/// `addr` and `len` name a live mapping.
pub unsafe fn mprotect(addr: *mut u8, len: usize, prot: i32) -> Result<(), i32> {
    if unsafe { libc::mprotect(addr.cast(), len, prot) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

pub const RLIMIT_STACK: u32 = libc::RLIMIT_STACK as u32;
pub const RLIM_INFINITY: u64 = libc::RLIM_INFINITY;
pub type Rlimit = libc::rlimit;

pub fn getrlimit(resource: u32) -> Result<Rlimit, i32> {
    let mut limit = std::mem::MaybeUninit::<Rlimit>::uninit();
    if unsafe { libc::getrlimit(resource as _, limit.as_mut_ptr()) } == 0 {
        Ok(unsafe { limit.assume_init() })
    } else {
        Err(errno())
    }
}

pub fn gettid() -> i32 {
    unsafe { libc::pthread_mach_thread_np(libc::pthread_self()) as i32 }
}

pub fn cached_tid() -> i32 {
    thread_local! {
        static TID: i32 = gettid();
    }
    TID.with(|id| *id)
}

pub fn getpid() -> i32 {
    unsafe { libc::getpid() }
}

pub fn tgkill(pid: i32, tid: i32, sig: i32) -> Result<(), i32> {
    if pid != getpid() {
        return Err(libc::ESRCH);
    }
    let thread = unsafe { libc::pthread_from_mach_thread_np(tid as libc::mach_port_t) };
    if thread == 0 {
        return Err(libc::ESRCH);
    }
    match unsafe { libc::pthread_kill(thread, sig) } {
        0 => Ok(()),
        error => Err(error),
    }
}

pub fn exit_group(code: i32) -> ! {
    unsafe { libc::_exit(code) }
}

pub fn abort() -> ! {
    unsafe { libc::abort() }
}

pub fn dbg_write(bytes: &[u8]) {
    unsafe { libc::write(2, bytes.as_ptr().cast(), bytes.len()) };
}

pub fn sched_yield() {
    unsafe { libc::sched_yield() };
}

pub const SIGINT: i32 = libc::SIGINT;
pub const SIGFPE: i32 = libc::SIGFPE;
pub const SIGABRT: i32 = libc::SIGABRT;
pub const SIGSEGV: i32 = libc::SIGSEGV;
pub const SIGBUS: i32 = libc::SIGBUS;
pub const SIGUSR1: i32 = libc::SIGUSR1;
pub const SIGPIPE: i32 = libc::SIGPIPE;
pub const SIGALRM: i32 = libc::SIGALRM;
pub const SIGTERM: i32 = libc::SIGTERM;
pub const SA_RESTART: u64 = libc::SA_RESTART as u64;
pub const SA_ONSTACK: u64 = libc::SA_ONSTACK as u64;
pub const SA_SIGINFO: u64 = libc::SA_SIGINFO as u64;
pub const SS_DISABLE: i32 = libc::SS_DISABLE;
pub const MIN_SIGSTKSZ: usize = libc::MINSIGSTKSZ;

pub fn alarm(seconds: u32) -> u32 {
    unsafe { libc::alarm(seconds) }
}

/// # Safety
/// `handler` must have the ABI required by `flags`.
pub unsafe fn rt_sigaction(sig: i32, handler: usize, flags: u64) -> Result<(), i32> {
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = handler;
    action.sa_flags = flags as i32;
    unsafe { libc::sigemptyset(&mut action.sa_mask) };
    if unsafe { libc::sigaction(sig, &action, std::ptr::null_mut()) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

/// # Safety
/// `handler` must have the three-argument signal handler ABI.
pub unsafe fn rt_sigaction_siginfo(sig: i32, handler: usize, flags: u64) -> Result<(), i32> {
    unsafe { rt_sigaction(sig, handler, flags | SA_SIGINFO) }
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StackT {
    pub ss_sp: *mut u8,
    pub ss_size: usize,
    pub ss_flags: i32,
}

const _: () = {
    assert!(std::mem::size_of::<StackT>() == std::mem::size_of::<libc::stack_t>());
    assert!(std::mem::offset_of!(StackT, ss_sp) == std::mem::offset_of!(libc::stack_t, ss_sp));
    assert!(std::mem::offset_of!(StackT, ss_size) == std::mem::offset_of!(libc::stack_t, ss_size));
    assert!(
        std::mem::offset_of!(StackT, ss_flags) == std::mem::offset_of!(libc::stack_t, ss_flags)
    );
};

/// # Safety
/// `new` and `old` must be null or point to valid stack records.
pub unsafe fn sigaltstack(new: *const StackT, old: *mut StackT) -> Result<(), i32> {
    if unsafe { libc::sigaltstack(new.cast(), old.cast()) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

pub fn current_sigaltstack() -> Result<StackT, i32> {
    let mut stack = StackT {
        ss_sp: std::ptr::null_mut(),
        ss_size: 0,
        ss_flags: 0,
    };
    unsafe { sigaltstack(std::ptr::null(), &mut stack) }?;
    Ok(stack)
}

pub const CLOCK_PROCESS_CPUTIME_ID: i32 = libc::CLOCK_PROCESS_CPUTIME_ID as i32;
pub const CLOCK_THREAD_CPUTIME_ID: i32 = libc::CLOCK_THREAD_CPUTIME_ID as i32;
pub type TimeSpec = libc::timespec;

/// # Safety
/// `ts` points at writable timespec storage.
pub unsafe fn clock_gettime(clock_id: i32, ts: *mut TimeSpec) -> Result<(), i32> {
    if unsafe { libc::clock_gettime(clock_id as libc::clockid_t, ts) } == 0 {
        Ok(())
    } else {
        Err(errno())
    }
}

fn cpu_time_ns(clock_id: i32) -> Result<u64, i32> {
    let mut ts = TimeSpec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { clock_gettime(clock_id, &mut ts) }?;
    let seconds = u64::try_from(ts.tv_sec).map_err(|_| libc::EINVAL)?;
    let nanos = u64::try_from(ts.tv_nsec).map_err(|_| libc::EINVAL)?;
    Ok(seconds.saturating_mul(1_000_000_000).saturating_add(nanos))
}

pub fn thread_cpu_time_ns() -> Result<u64, i32> {
    cpu_time_ns(CLOCK_THREAD_CPUTIME_ID)
}

pub fn process_cpu_time_ns() -> Result<u64, i32> {
    cpu_time_ns(CLOCK_PROCESS_CPUTIME_ID)
}

pub const POLLIN: i16 = libc::POLLIN;
pub const POLLERR: i16 = libc::POLLERR;
pub const POLLHUP: i16 = libc::POLLHUP;
pub const POLLNVAL: i16 = libc::POLLNVAL;
pub const POLLOUT: i16 = libc::POLLOUT;
pub type PollFd = libc::pollfd;

/// # Safety
/// `fds` points to `nfds` writable poll records.
pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout_ms: i32) -> Result<usize, i32> {
    let result = unsafe { libc::poll(fds, nfds as _, timeout_ms) };
    if result < 0 {
        Err(errno())
    } else {
        Ok(result as usize)
    }
}
