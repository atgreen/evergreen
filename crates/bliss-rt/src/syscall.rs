//! Direct Linux syscalls, no libc (bliss-bca.5).
//!
//! To produce fully static Bliss executables without a mandatory libc, the
//! runtime's internal OS services issue Linux syscalls directly rather than
//! going through `libc` wrappers. This module is the single place that knows the
//! syscall ABI; everything else calls the safe-ish typed helpers below.
//!
//! Scope: Linux only. Each `raw::N` argument arm matches the x86-64 / aarch64
//! kernel calling convention. Callers that need another platform must add an
//! arch arm here; there is deliberately no libc fallback (that is the point).
#![allow(dead_code)]
// Every function here is inherently unsafe (raw syscalls / inline asm); the
// `unsafe fn` marker is the safety boundary and re-nesting `unsafe {}` inside
// each body adds noise without adding checking. Allow the edition-2024 lint for
// this module only.
#![allow(unsafe_op_in_unsafe_fn)]

use core::arch::asm;

// ── Syscall numbers (Linux) ──────────────────────────────────────────────────
// x86-64 numbers. aarch64 differs and is added when that target is built.
#[cfg(target_arch = "x86_64")]
pub mod nr {
    pub const READ: usize = 0;
    pub const WRITE: usize = 1;
    pub const CLOSE: usize = 3;
    pub const MMAP: usize = 9;
    pub const MPROTECT: usize = 10;
    pub const MUNMAP: usize = 11;
    pub const RT_SIGACTION: usize = 13;
    pub const RT_SIGPROCMASK: usize = 14;
    pub const SCHED_YIELD: usize = 24;
    pub const NANOSLEEP: usize = 35;
    pub const GETPID: usize = 39;
    pub const EXIT: usize = 60;
    pub const EXIT_GROUP: usize = 231;
    pub const GETTID: usize = 186;
    pub const TGKILL: usize = 234;
    pub const ALARM: usize = 37;
    pub const FUTEX: usize = 202;
    pub const POLL: usize = 7;
    pub const EPOLL_CREATE1: usize = 291;
    pub const EPOLL_CTL: usize = 233;
    pub const EPOLL_WAIT: usize = 232;
    pub const PRLIMIT64: usize = 302;
    pub const CLOCK_GETTIME: usize = 228;
    pub const SIGALTSTACK: usize = 131;
}

// ── Raw syscall entry (x86-64) ───────────────────────────────────────────────
// The kernel returns -errno in [-4095, -1]; callers convert that to Err.

/// # Safety
///
/// caller must pass a valid syscall number and arguments whose pointer
/// operands (if any) are valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall0(n: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw 1-argument syscall.
///
/// # Safety
///
/// Same contract as [`syscall0`]: the syscall number must be valid and any
/// pointer operands must be valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall1(n: usize, a1: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw 2-argument syscall.
///
/// # Safety
///
/// Same contract as [`syscall0`]: the syscall number must be valid and any
/// pointer operands must be valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall2(n: usize, a1: usize, a2: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1, in("rsi") a2,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw 3-argument syscall.
///
/// # Safety
///
/// Same contract as [`syscall0`]: the syscall number must be valid and any
/// pointer operands must be valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall3(n: usize, a1: usize, a2: usize, a3: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1, in("rsi") a2, in("rdx") a3,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw 4-argument syscall.
///
/// # Safety
///
/// Same contract as [`syscall0`]: the syscall number must be valid and any
/// pointer operands must be valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall4(n: usize, a1: usize, a2: usize, a3: usize, a4: usize) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1, in("rsi") a2, in("rdx") a3, in("r10") a4,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Raw 6-argument syscall.
///
/// # Safety
///
/// Same contract as [`syscall0`]: the syscall number must be valid and any
/// pointer operands must be valid for the syscall's duration.
#[cfg(target_arch = "x86_64")]
#[inline]
pub unsafe fn syscall6(
    n: usize,
    a1: usize,
    a2: usize,
    a3: usize,
    a4: usize,
    a5: usize,
    a6: usize,
) -> isize {
    let ret: isize;
    asm!(
        "syscall",
        inlateout("rax") n as isize => ret,
        in("rdi") a1, in("rsi") a2, in("rdx") a3,
        in("r10") a4, in("r8") a5, in("r9") a6,
        lateout("rcx") _, lateout("r11") _,
        options(nostack, preserves_flags)
    );
    ret
}

/// Convert a raw syscall return into `Result`, mapping the `-errno` range to
/// `Err(errno)`.
#[inline]
pub fn check(ret: isize) -> Result<usize, i32> {
    if (-4095..0).contains(&ret) {
        Err(-ret as i32)
    } else {
        Ok(ret as usize)
    }
}

// ── Typed wrappers ───────────────────────────────────────────────────────────
// Only Linux is supported; these mirror the same constants libc exposed so the
// call sites change minimally. Values are the Linux/x86-64 ABI (identical across
// the common Linux architectures for these particular flags).

// mmap/mprotect protection flags.
pub const PROT_NONE: i32 = 0x0;
pub const PROT_READ: i32 = 0x1;
pub const PROT_WRITE: i32 = 0x2;
pub const PROT_EXEC: i32 = 0x4;

// mmap flags.
pub const MAP_PRIVATE: i32 = 0x2;
pub const MAP_ANONYMOUS: i32 = 0x20;
pub const MAP_FIXED: i32 = 0x10;

/// `mmap(2)`. Returns the mapped address or `Err(errno)`. Unlike libc there is no
/// `MAP_FAILED` sentinel: the kernel returns `-errno`, which `check` converts.
///
/// # Safety
///
/// `addr`/`len`/`prot`/`flags`/`fd`/`offset` must form a valid mmap
/// request; the returned mapping is unmanaged (free with [`munmap`]).
#[inline]
pub unsafe fn mmap(
    addr: *mut u8,
    len: usize,
    prot: i32,
    flags: i32,
    fd: i32,
    offset: i64,
) -> Result<*mut u8, i32> {
    let r = syscall6(
        nr::MMAP,
        addr as usize,
        len,
        prot as usize,
        flags as usize,
        fd as isize as usize,
        offset as usize,
    );
    check(r).map(|a| a as *mut u8)
}

/// `munmap(2)`.
///
/// # Safety
///
/// `addr`/`len` must name a mapping previously returned by [`mmap`].
#[inline]
pub unsafe fn munmap(addr: *mut u8, len: usize) -> Result<(), i32> {
    check(syscall2(nr::MUNMAP, addr as usize, len)).map(|_| ())
}

/// `mprotect(2)`.
///
/// # Safety
///
/// `addr`/`len` must name a valid mapping.
#[inline]
pub unsafe fn mprotect(addr: *mut u8, len: usize, prot: i32) -> Result<(), i32> {
    check(syscall3(nr::MPROTECT, addr as usize, len, prot as usize)).map(|_| ())
}

// getrlimit / prlimit64.
pub const RLIMIT_STACK: u32 = 3;
pub const RLIM_INFINITY: u64 = !0;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Rlimit {
    pub rlim_cur: u64,
    pub rlim_max: u64,
}

/// `getrlimit(2)` via `prlimit64(pid=0, resource, new=NULL, old=&out)`.
#[inline]
pub fn getrlimit(resource: u32) -> Result<Rlimit, i32> {
    let mut out = Rlimit::default();
    // SAFETY: `&mut out` is a valid writable Rlimit for the syscall's duration.
    let r = unsafe {
        syscall4(
            nr::PRLIMIT64,
            0,
            resource as usize,
            0,
            &mut out as *mut Rlimit as usize,
        )
    };
    check(r).map(|_| out)
}

/// `gettid(2)` — the calling thread's kernel thread id (never fails).
#[inline]
pub fn gettid() -> i32 {
    // SAFETY: gettid takes no arguments and cannot fail.
    unsafe { syscall0(nr::GETTID) as i32 }
}

/// `getpid(2)`.
#[inline]
pub fn getpid() -> i32 {
    // SAFETY: getpid takes no arguments and cannot fail.
    unsafe { syscall0(nr::GETPID) as i32 }
}

/// `tgkill(2)` — send `sig` to thread `tid` in process `tgid`.
#[inline]
pub fn tgkill(tgid: i32, tid: i32, sig: i32) -> Result<(), i32> {
    // SAFETY: no pointer arguments.
    let r = unsafe { syscall3(nr::TGKILL, tgid as usize, tid as usize, sig as usize) };
    check(r).map(|_| ())
}

/// `exit_group(2)` — terminate every thread in the process. Never returns.
#[inline]
pub fn exit_group(code: i32) -> ! {
    // SAFETY: no pointer arguments; the process is torn down.
    unsafe {
        syscall1(nr::EXIT_GROUP, code as usize);
        // exit_group never returns; loop as a belt-and-braces fallback.
        core::hint::unreachable_unchecked()
    }
}

/// Abort the process like libc `abort()`: raise `SIGABRT` to this thread (which
/// core-dumps / terminates under the default disposition), then fall back to
/// `exit_group(134)` (128 + SIGABRT) if the signal is somehow caught and returns.
#[inline]
pub fn abort() -> ! {
    let _ = tgkill(getpid(), gettid(), SIGABRT);
    exit_group(134)
}

/// Debug: write raw bytes to stderr (fd 2), ignoring errors. Used for
/// signal/fiber-context bring-up where the normal I/O stack is unavailable.
#[inline]
pub fn dbg_write(bytes: &[u8]) {
    // SAFETY: bytes is a valid readable slice.
    unsafe {
        syscall3(nr::WRITE, 2, bytes.as_ptr() as usize, bytes.len());
    }
}

/// `sched_yield(2)`.
#[inline]
pub fn sched_yield() {
    // SAFETY: no arguments.
    unsafe {
        syscall0(nr::SCHED_YIELD);
    }
}

// Signal numbers (Linux; identical across the common architectures except a
// few real-time-signal shifts that do not affect these).
pub const SIGINT: i32 = 2;
pub const SIGFPE: i32 = 8;
pub const SIGABRT: i32 = 6;
pub const SIGSEGV: i32 = 11;
pub const SIGUSR1: i32 = 10;
pub const SIGPIPE: i32 = 13;
pub const SIGALRM: i32 = 14;
pub const SIGTERM: i32 = 15;

/// `alarm(2)` — deliver SIGALRM to this process after `seconds` (0 cancels).
/// Async-signal-safe (a plain syscall); returns the previous alarm's remaining
/// seconds.
#[inline]
pub fn alarm(seconds: u32) -> u32 {
    // SAFETY: no pointer arguments.
    unsafe { syscall1(nr::ALARM, seconds as usize) as u32 }
}

// ── Signal installation via rt_sigaction ─────────────────────────────────────
// The kernel's sigaction differs from glibc's: on x86-64 the field order is
// {handler, flags, restorer, mask} and SA_RESTORER + a restorer that invokes
// rt_sigreturn are mandatory (libc normally supplies the restorer; we can't, so
// we provide our own naked stub).

pub const SA_RESTORER: u64 = 0x0400_0000;
pub const SA_RESTART: u64 = 0x1000_0000;
pub const SA_ONSTACK: u64 = 0x0800_0000;
pub const SA_SIGINFO: u64 = 0x0000_0004;
pub const SS_DISABLE: i32 = 2;
pub const MIN_SIGSTKSZ: usize = 2048;
pub const CLOCK_THREAD_CPUTIME_ID: i32 = 3;

#[repr(C)]
struct KernelSigaction {
    handler: usize,
    flags: u64,
    restorer: usize,
    mask: u64, // 8-byte sigset (64 signals) on x86-64
}

/// Signal-return trampoline. The kernel transfers control here (not via a normal
/// call) when a handler returns, so it must be naked and do nothing but issue
/// `rt_sigreturn` (syscall 15 on x86-64).
#[cfg(target_arch = "x86_64")]
#[unsafe(naked)]
unsafe extern "C" fn restore_rt() {
    core::arch::naked_asm!("mov rax, 15", "syscall")
}

/// Install `handler` for signal `sig` via `rt_sigaction`. `extra_flags` is OR'd
/// into the action flags (e.g. `SA_RESTART`); `SA_RESTORER` and the restorer are
/// always supplied. The signal mask is empty. Returns `Err(errno)` on failure.
///
/// # Safety
///
/// `handler` must be a valid `extern "C" fn(i32)` (or `SIG_DFL`/`SIG_IGN`
/// sentinel) appropriate for `sig`.
#[cfg(target_arch = "x86_64")]
pub unsafe fn rt_sigaction(sig: i32, handler: usize, extra_flags: u64) -> Result<(), i32> {
    let act = KernelSigaction {
        handler,
        flags: SA_RESTORER | extra_flags,
        restorer: restore_rt as *const () as usize,
        mask: 0,
    };
    let r = syscall4(
        nr::RT_SIGACTION,
        sig as usize,
        &act as *const KernelSigaction as usize,
        0,
        8, // sigsetsize = _NSIG/8
    );
    check(r).map(|_| ())
}

/// Install a three-argument SA_SIGINFO handler for `sig`.
///
/// # Safety
///
/// `handler` must be a valid
/// `extern "C" fn(i32, *mut SigInfo, *mut core::ffi::c_void)` appropriate for
/// `sig`.
#[cfg(target_arch = "x86_64")]
pub unsafe fn rt_sigaction_siginfo(sig: i32, handler: usize, extra_flags: u64) -> Result<(), i32> {
    rt_sigaction(sig, handler, extra_flags | SA_SIGINFO)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct StackT {
    pub ss_sp: *mut u8,
    pub ss_flags: i32,
    pub ss_size: usize,
}

/// `sigaltstack(2)`.
///
/// # Safety
///
/// `new` and `old` must be null or valid pointers for the syscall's
/// duration.
#[inline]
pub unsafe fn sigaltstack(new: *const StackT, old: *mut StackT) -> Result<(), i32> {
    check(syscall2(nr::SIGALTSTACK, new as usize, old as usize)).map(|_| ())
}

pub fn current_sigaltstack() -> Result<StackT, i32> {
    let mut out = StackT {
        ss_sp: core::ptr::null_mut(),
        ss_flags: 0,
        ss_size: 0,
    };
    // SAFETY: `old` points at writable stack_t storage; `new=NULL` only queries.
    unsafe { sigaltstack(core::ptr::null(), &mut out as *mut StackT) }.map(|_| out)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct TimeSpec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

/// `clock_gettime(2)`.
///
/// # Safety
///
/// `ts` must point at writable `TimeSpec` storage.
#[inline]
pub unsafe fn clock_gettime(clock_id: i32, ts: *mut TimeSpec) -> Result<(), i32> {
    check(syscall2(
        nr::CLOCK_GETTIME,
        clock_id as isize as usize,
        ts as usize,
    ))
    .map(|_| ())
}

pub fn thread_cpu_time_ns() -> Result<u64, i32> {
    let mut ts = TimeSpec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &mut ts as *mut TimeSpec) }?;
    let secs = u64::try_from(ts.tv_sec).map_err(|_| -1_i32)?;
    let nanos = u64::try_from(ts.tv_nsec).map_err(|_| -1_i32)?;
    Ok(secs.saturating_mul(1_000_000_000).saturating_add(nanos))
}

// poll.
pub const POLLIN: i16 = 0x001;
pub const POLLPRI: i16 = 0x002;
pub const POLLOUT: i16 = 0x004;
pub const POLLERR: i16 = 0x008;
pub const POLLHUP: i16 = 0x010;
pub const POLLNVAL: i16 = 0x020;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PollFd {
    pub fd: i32,
    pub events: i16,
    pub revents: i16,
}

/// `poll(2)`. `timeout` is milliseconds (-1 = block). Returns the number of ready
/// descriptors (0 on timeout).
///
/// # Safety
///
/// `fds`/`nfds` must describe a valid, writable slice of `PollFd`.
#[inline]
pub unsafe fn poll(fds: *mut PollFd, nfds: usize, timeout_ms: i32) -> Result<usize, i32> {
    let r = syscall3(nr::POLL, fds as usize, nfds, timeout_ms as isize as usize);
    check(r)
}

// epoll.
pub const EPOLLIN: u32 = 0x001;
pub const EPOLLOUT: u32 = 0x004;
pub const EPOLLONESHOT: u32 = 1 << 30;
pub const EPOLL_CTL_ADD: i32 = 1;
pub const EPOLL_CTL_DEL: i32 = 2;
pub const EPOLL_CTL_MOD: i32 = 3;
pub const EPOLL_CLOEXEC: i32 = 0x8_0000; // == O_CLOEXEC

/// `struct epoll_event`. On x86-64 the kernel layout is **packed** (12 bytes: a
/// u32 `events` immediately followed by a u64 `data`, no padding). Getting this
/// wrong silently misreads the data cookie, so keep `repr(C, packed)`.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct EpollEvent {
    pub events: u32,
    pub data: u64,
}

/// `epoll_create1(2)`.
#[inline]
pub fn epoll_create1(flags: i32) -> Result<i32, i32> {
    // SAFETY: no pointer arguments.
    let r = unsafe { syscall1(nr::EPOLL_CREATE1, flags as usize) };
    check(r).map(|fd| fd as i32)
}

/// `epoll_ctl(2)`. `event` may be null for `EPOLL_CTL_DEL`.
///
/// # Safety
///
/// `event` (if non-null) must point to a valid `EpollEvent`.
#[inline]
pub unsafe fn epoll_ctl(epfd: i32, op: i32, fd: i32, event: *mut EpollEvent) -> Result<(), i32> {
    let r = syscall4(
        nr::EPOLL_CTL,
        epfd as usize,
        op as usize,
        fd as usize,
        event as usize,
    );
    check(r).map(|_| ())
}

/// `epoll_wait(2)`. Returns the number of ready events written to `events`.
///
/// # Safety
///
/// `events`/`maxevents` must describe a valid, writable buffer.
#[inline]
pub unsafe fn epoll_wait(
    epfd: i32,
    events: *mut EpollEvent,
    maxevents: i32,
    timeout_ms: i32,
) -> Result<usize, i32> {
    let r = syscall4(
        nr::EPOLL_WAIT,
        epfd as usize,
        events as usize,
        maxevents as usize,
        timeout_ms as isize as usize,
    );
    check(r)
}
