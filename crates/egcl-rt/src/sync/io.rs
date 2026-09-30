// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::{BlockingMode, blocking_mode, timer};
use crate::error::EgclError;
#[cfg(unix)]
use std::os::fd::RawFd;
#[cfg(windows)]
type RawFd = i32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IoInterest {
    Read,
    Write,
    ReadWrite,
}

#[cfg(unix)]
fn poll_events(interest: IoInterest) -> i16 {
    use crate::syscall::{POLLIN, POLLOUT};
    match interest {
        IoInterest::Read => POLLIN,
        IoInterest::Write => POLLOUT,
        IoInterest::ReadWrite => POLLIN | POLLOUT,
    }
}

#[cfg(unix)]
fn native_poll(
    fd: RawFd,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    const EINTR: i32 = 4;
    let started = Instant::now();
    let mut descriptor = crate::syscall::PollFd {
        fd,
        events: poll_events(interest),
        revents: 0,
    };
    loop {
        let remaining = timeout.map(|t| t.saturating_sub(started.elapsed()));
        let timeout_ms = remaining
            .map(|t| t.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32)
            .unwrap_or(-1);
        // SAFETY: `descriptor` is a valid single-element PollFd for the call.
        match unsafe { crate::syscall::poll(&mut descriptor, 1, timeout_ms) } {
            Ok(n) if n > 0 => return Ok(true),
            Ok(_) | Err(EINTR) => {
                if timeout.is_some_and(|t| started.elapsed() >= t) {
                    return Ok(false);
                }
            }
            Err(e) => {
                return Err(EgclError::StreamError(format!(
                    "fd readiness wait failed: errno {e}"
                )));
            }
        }
    }
}

/// Wait for descriptor readiness.  Linux fibers register with a shared epoll
/// thread; other Unix targets use a poll helper fallback.  Native executions
/// call poll directly.
pub fn wait_fd(
    fd: RawFd,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    if fd < 0 {
        return Err(EgclError::StreamError(
            "cannot wait on a negative file descriptor".into(),
        ));
    }
    if timeout.is_some_and(|duration| duration.is_zero()) {
        return native_poll(fd, interest, timeout);
    }
    match blocking_mode("FD-WAIT")? {
        BlockingMode::Native => {
            // SAFETY: poll touches only the descriptor and native stack data.
            let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
            native_poll(fd, interest, timeout)
        }
        BlockingMode::Fiber => fiber_wait_fd(fd, interest, timeout),
    }
}

/// Wait on an owned TCP socket without occupying an unpinned fiber's carrier.
/// The caller must keep the socket alive until this call returns.
pub fn wait_socket(
    socket: &std::net::TcpStream,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        wait_fd(socket.as_raw_fd(), interest, timeout)
    }
    #[cfg(windows)]
    {
        super::socket_windows::wait(socket, interest, timeout)
    }
}

// Android is a Linux kernel with epoll; its target_os is "android", so a bare
// `target_os = "linux"` would drop it into the no-poller fallback below and
// silently degrade fiber I/O (bliss-w2vp).
#[cfg(any(target_os = "linux", target_os = "android"))]
fn fiber_wait_fd(
    fd: RawFd,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    let (fiber, token) =
        crate::thread::prepare_current_fiber_park(crate::thread::FiberState::Waiting)?;
    let ready = Arc::new(AtomicBool::new(false));
    let registration = match epoll::register(fd, interest, fiber, token, Arc::clone(&ready)) {
        Ok(registration) => registration,
        Err(error) => {
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(error);
        }
    };
    if let Some(duration) = timeout {
        if let Err(error) = timer::schedule(fiber, token, Instant::now() + duration) {
            epoll::cancel(registration);
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(error);
        }
    }
    let parked = crate::thread::park_prepared_current_fiber();
    epoll::cancel(registration);
    parked?;
    Ok(ready.load(Ordering::Acquire))
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
fn fiber_wait_fd(
    fd: RawFd,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    let (fiber, token) =
        crate::thread::prepare_current_fiber_park(crate::thread::FiberState::Waiting)?;
    let ready = Arc::new(AtomicBool::new(false));
    let registration = match kqueue::register(fd, interest, fiber, token, Arc::clone(&ready)) {
        Ok(registration) => registration,
        Err(error) => {
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(error);
        }
    };
    if let Some(duration) = timeout {
        if let Err(error) = timer::schedule(fiber, token, Instant::now() + duration) {
            kqueue::cancel(registration);
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(error);
        }
    }
    let parked = crate::thread::park_prepared_current_fiber();
    kqueue::cancel(registration);
    parked?;
    Ok(ready.load(Ordering::Acquire))
}

#[cfg(not(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
)))]
fn fiber_wait_fd(
    fd: RawFd,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    let (fiber, token) =
        crate::thread::prepare_current_fiber_park(crate::thread::FiberState::Waiting)?;
    let ready = Arc::new(AtomicBool::new(false));
    let worker_ready = Arc::clone(&ready);
    if std::thread::Builder::new()
        .name("egcl-poll-wait".into())
        .spawn(move || {
            if native_poll(fd, interest, timeout).unwrap_or(false) {
                worker_ready.store(true, Ordering::Release);
            }
            crate::thread::wake_fiber_wait(fiber, token);
        })
        .is_err()
    {
        crate::thread::cancel_prepared_current_fiber_park();
        return Err(EgclError::Internal(
            "failed to start fd poll fallback".into(),
        ));
    }
    if let Some(duration) = timeout {
        if let Err(error) = timer::schedule(fiber, token, Instant::now() + duration) {
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(error);
        }
    }
    crate::thread::park_prepared_current_fiber()?;
    Ok(ready.load(Ordering::Acquire))
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod epoll {
    use super::{IoInterest, RawFd};
    use crate::error::EgclError;
    use crate::lock_order::{LockLevel, OrderedMutex};
    use crate::thread::FiberId;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    struct Registration {
        fd: RawFd,
        fiber: FiberId,
        token: u64,
        ready: Arc<AtomicBool>,
    }

    struct Poller {
        epoll_fd: RawFd,
        registrations: Arc<OrderedMutex<HashMap<u64, Registration>>>,
        fds: Arc<OrderedMutex<HashMap<RawFd, u64>>>,
        started: bool,
    }

    static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

    fn poller() -> &'static Poller {
        static POLLER: OnceLock<Poller> = OnceLock::new();
        POLLER.get_or_init(|| {
            let epoll_fd =
                crate::syscall::epoll_create1(crate::syscall::EPOLL_CLOEXEC).unwrap_or(-1);
            let registrations = Arc::new(OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                101,
                "epoll fiber registrations",
                HashMap::new(),
            ));
            let fds = Arc::new(OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                100,
                "epoll descriptor registry",
                HashMap::new(),
            ));
            let started = if epoll_fd < 0 {
                false
            } else {
                let worker_registrations = Arc::clone(&registrations);
                let worker_fds = Arc::clone(&fds);
                std::thread::Builder::new()
                    .name("egcl-io-epoll".into())
                    .spawn(move || epoll_loop(epoll_fd, worker_registrations, worker_fds))
                    .is_ok()
            };
            Poller {
                epoll_fd,
                registrations,
                fds,
                started,
            }
        })
    }

    fn epoll_events(interest: IoInterest) -> u32 {
        use crate::syscall::{EPOLLIN, EPOLLONESHOT, EPOLLOUT};
        let events = match interest {
            IoInterest::Read => EPOLLIN,
            IoInterest::Write => EPOLLOUT,
            IoInterest::ReadWrite => EPOLLIN | EPOLLOUT,
        };
        events | EPOLLONESHOT
    }

    #[test]
    fn repeatedly_register_already_ready_descriptor() {
        use std::net::{TcpListener, TcpStream};
        use std::os::fd::AsRawFd;
        use std::time::{Duration, Instant};
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let _peer = listener.accept().unwrap();
        for iteration in 0..5000 {
            let ready = Arc::new(AtomicBool::new(false));
            let id = register(
                socket.as_raw_fd(),
                IoInterest::Write,
                FiberId(u64::MAX),
                1,
                Arc::clone(&ready),
            )
            .unwrap();
            let deadline = Instant::now() + Duration::from_secs(1);
            while !ready.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::yield_now();
            }
            cancel(id);
            assert!(
                ready.load(Ordering::Acquire),
                "lost readiness at iteration {iteration}"
            );
        }
    }

    pub(super) fn register(
        fd: RawFd,
        interest: IoInterest,
        fiber: FiberId,
        token: u64,
        ready: Arc<AtomicBool>,
    ) -> Result<u64, EgclError> {
        let poller = poller();
        if !poller.started {
            return Err(EgclError::StreamError(
                "failed to initialize epoll readiness service".into(),
            ));
        }
        let id = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        let mut fds = poller.fds.lock().unwrap();
        if fds.contains_key(&fd) {
            return Err(EgclError::ProgramError(format!(
                "file descriptor {fd} already has a fiber waiter"
            )));
        }
        let mut event = crate::syscall::EpollEvent {
            events: epoll_events(interest),
            data: id,
        };
        // SAFETY: `event` is a valid EpollEvent for the duration of the call.
        if unsafe {
            crate::syscall::epoll_ctl(
                poller.epoll_fd,
                crate::syscall::EPOLL_CTL_ADD,
                fd,
                &mut event,
            )
        }
        .is_err()
        {
            return Err(EgclError::StreamError("epoll registration failed".into()));
        }
        poller.registrations.lock().unwrap().insert(
            id,
            Registration {
                fd,
                fiber,
                token,
                ready,
            },
        );
        fds.insert(fd, id);
        Ok(id)
    }

    pub(super) fn cancel(id: u64) {
        let poller = poller();
        // Serialize publication, event dispatch and deletion under the same
        // descriptor lock. A consumed one-shot event must see its registration;
        // an old DEL must finish before a new ADD can reuse this descriptor.
        let mut fds = poller.fds.lock().unwrap();
        let registration = poller.registrations.lock().unwrap().remove(&id);
        if let Some(registration) = registration {
            // SAFETY: DEL takes no event pointer.
            unsafe {
                let _ = crate::syscall::epoll_ctl(
                    poller.epoll_fd,
                    crate::syscall::EPOLL_CTL_DEL,
                    registration.fd,
                    std::ptr::null_mut(),
                );
            }
            fds.remove(&registration.fd);
        }
    }

    fn epoll_loop(
        epoll_fd: RawFd,
        registrations: Arc<OrderedMutex<HashMap<u64, Registration>>>,
        fds: Arc<OrderedMutex<HashMap<RawFd, u64>>>,
    ) {
        const EINTR: i32 = 4;
        let mut events = [crate::syscall::EpollEvent { events: 0, data: 0 }; 64];
        loop {
            // SAFETY: `events` is a valid writable buffer of `len()` entries.
            let count = match unsafe {
                crate::syscall::epoll_wait(epoll_fd, events.as_mut_ptr(), events.len() as i32, -1)
            } {
                Ok(n) => n,
                Err(EINTR) => continue,
                Err(_) => {
                    std::thread::sleep(std::time::Duration::from_millis(1));
                    continue;
                }
            };
            for event in events.iter().take(count) {
                let id = unsafe { std::ptr::addr_of!(event.data).read_unaligned() };
                let mut descriptors = fds.lock().unwrap();
                let registration = registrations.lock().unwrap().remove(&id);
                let Some(registration) = registration else {
                    continue;
                };
                // SAFETY: DEL takes no event pointer.
                unsafe {
                    let _ = crate::syscall::epoll_ctl(
                        epoll_fd,
                        crate::syscall::EPOLL_CTL_DEL,
                        registration.fd,
                        std::ptr::null_mut(),
                    );
                }
                descriptors.remove(&registration.fd);
                drop(descriptors);
                registration.ready.store(true, Ordering::Release);
                crate::thread::wake_fiber_wait(registration.fiber, registration.token);
            }
        }
    }
}

#[cfg(any(
    target_os = "macos",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd",
    target_os = "dragonfly"
))]
mod kqueue {
    use super::{IoInterest, RawFd};
    use crate::error::EgclError;
    use crate::lock_order::{LockLevel, OrderedMutex};
    use crate::thread::FiberId;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Arc, OnceLock};

    struct Registration {
        fd: RawFd,
        fiber: FiberId,
        token: u64,
        ready: Arc<AtomicBool>,
    }

    struct Poller {
        queue_fd: RawFd,
        registrations: Arc<OrderedMutex<HashMap<u64, Registration>>>,
        fds: Arc<OrderedMutex<HashMap<RawFd, u64>>>,
        started: bool,
    }

    static NEXT_REGISTRATION: AtomicU64 = AtomicU64::new(1);

    fn poller() -> &'static Poller {
        static POLLER: OnceLock<Poller> = OnceLock::new();
        POLLER.get_or_init(|| {
            let queue_fd = unsafe { libc::kqueue() };
            let registrations = Arc::new(OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                101,
                "kqueue fiber registrations",
                HashMap::new(),
            ));
            let fds = Arc::new(OrderedMutex::new(
                LockLevel::ExecutionRegistry,
                100,
                "kqueue descriptor registry",
                HashMap::new(),
            ));
            let started = if queue_fd < 0 {
                false
            } else {
                let worker_registrations = Arc::clone(&registrations);
                let worker_fds = Arc::clone(&fds);
                std::thread::Builder::new()
                    .name("egcl-io-kqueue".into())
                    .spawn(move || kqueue_loop(queue_fd, worker_registrations, worker_fds))
                    .is_ok()
            };
            Poller {
                queue_fd,
                registrations,
                fds,
                started,
            }
        })
    }

    fn filters(interest: IoInterest) -> &'static [i16] {
        match interest {
            IoInterest::Read => &[libc::EVFILT_READ],
            IoInterest::Write => &[libc::EVFILT_WRITE],
            IoInterest::ReadWrite => &[libc::EVFILT_READ, libc::EVFILT_WRITE],
        }
    }

    unsafe fn change(queue_fd: RawFd, fd: RawFd, filter: i16, flags: u16, id: u64) -> i32 {
        let mut event: libc::kevent = unsafe { std::mem::zeroed() };
        event.ident = fd as libc::uintptr_t;
        event.filter = filter;
        event.flags = flags;
        event.udata = id as usize as *mut libc::c_void;
        unsafe {
            libc::kevent(
                queue_fd,
                &event,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        }
    }

    pub(super) fn register(
        fd: RawFd,
        interest: IoInterest,
        fiber: FiberId,
        token: u64,
        ready: Arc<AtomicBool>,
    ) -> Result<u64, EgclError> {
        let poller = poller();
        if !poller.started {
            return Err(EgclError::StreamError(
                "failed to initialize kqueue readiness service".into(),
            ));
        }
        let id = NEXT_REGISTRATION.fetch_add(1, Ordering::Relaxed);
        let mut fds = poller.fds.lock().unwrap();
        if fds.contains_key(&fd) {
            return Err(EgclError::ProgramError(format!(
                "file descriptor {fd} already has a fiber waiter"
            )));
        }
        for &filter in filters(interest) {
            if unsafe {
                change(
                    poller.queue_fd,
                    fd,
                    filter,
                    (libc::EV_ADD | libc::EV_ONESHOT) as u16,
                    id,
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                for &filter in filters(interest) {
                    unsafe {
                        change(poller.queue_fd, fd, filter, libc::EV_DELETE as u16, id);
                    }
                }
                return Err(EgclError::StreamError(format!(
                    "kqueue registration failed: {error}"
                )));
            }
        }
        poller.registrations.lock().unwrap().insert(
            id,
            Registration {
                fd,
                fiber,
                token,
                ready,
            },
        );
        fds.insert(fd, id);
        Ok(id)
    }

    pub(super) fn cancel(id: u64) {
        let poller = poller();
        let mut fds = poller.fds.lock().unwrap();
        let registration = poller.registrations.lock().unwrap().remove(&id);
        if let Some(registration) = registration {
            for &filter in &[libc::EVFILT_READ, libc::EVFILT_WRITE] {
                unsafe {
                    change(
                        poller.queue_fd,
                        registration.fd,
                        filter,
                        libc::EV_DELETE as u16,
                        id,
                    );
                }
            }
            fds.remove(&registration.fd);
        }
    }

    fn kqueue_loop(
        queue_fd: RawFd,
        registrations: Arc<OrderedMutex<HashMap<u64, Registration>>>,
        fds: Arc<OrderedMutex<HashMap<RawFd, u64>>>,
    ) {
        let mut events: [libc::kevent; 64] = unsafe { std::mem::zeroed() };
        loop {
            let count = unsafe {
                libc::kevent(
                    queue_fd,
                    std::ptr::null(),
                    0,
                    events.as_mut_ptr(),
                    events.len() as i32,
                    std::ptr::null(),
                )
            };
            if count < 0 {
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            for event in events.iter().take(count as usize) {
                let id = event.udata as usize as u64;
                let mut descriptors = fds.lock().unwrap();
                let registration = registrations.lock().unwrap().remove(&id);
                let Some(registration) = registration else {
                    continue;
                };
                // A ReadWrite registration may still have its other filter
                // installed after this one-shot event consumed the first.
                for &filter in &[libc::EVFILT_READ, libc::EVFILT_WRITE] {
                    unsafe {
                        change(
                            queue_fd,
                            registration.fd,
                            filter,
                            libc::EV_DELETE as u16,
                            id,
                        );
                    }
                }
                descriptors.remove(&registration.fd);
                drop(descriptors);
                registration.ready.store(true, Ordering::Release);
                crate::thread::wake_fiber_wait(registration.fiber, registration.token);
            }
        }
    }
}

#[cfg(windows)]
fn native_poll(
    _fd: RawFd,
    _interest: IoInterest,
    _timeout: Option<Duration>,
) -> Result<bool, EgclError> {
    Err(EgclError::StreamError(
        "Unix file-descriptor readiness is unavailable on Windows".into(),
    ))
}
