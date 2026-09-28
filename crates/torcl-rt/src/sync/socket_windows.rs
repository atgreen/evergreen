//! One shared Winsock readiness thread; waiting fibers never own helper threads.
//! Registry snapshots own duplicate sockets until WSAPoll has finished with them.
use super::{BlockingMode, IoInterest, blocking_mode, timer};
use crate::error::TorclError;
use crate::thread::{FiberId, FiberState};
use std::collections::HashMap;
use std::net::TcpStream;
use std::os::windows::io::AsRawSocket;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};
use windows_sys::Win32::Networking::WinSock::*;

struct Registration {
    socket: TcpStream,
    interest: IoInterest,
    fiber: FiberId,
    token: u64,
    result: Mutex<Option<Result<(), i32>>>,
}

// This native-only queue never contains Lisp values. No scheduler, timer or
// wake call runs under its mutex (the same discipline as the timer service).
type Queue = (Mutex<HashMap<u64, Arc<Registration>>>, Condvar);
struct Service {
    queue: Arc<Queue>,
    started: bool,
}

fn service() -> &'static Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let queue = Arc::new((Mutex::new(HashMap::new()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let started = std::thread::Builder::new()
            .name("torcl-io-wsapoll".into())
            .spawn(move || poll_loop(worker_queue))
            .is_ok();
        Service { queue, started }
    })
}

fn descriptor(socket: &TcpStream, interest: IoInterest) -> WSAPOLLFD {
    WSAPOLLFD {
        fd: socket.as_raw_socket() as SOCKET,
        events: match interest {
            IoInterest::Read => POLLRDNORM,
            IoInterest::Write => POLLWRNORM,
            IoInterest::ReadWrite => POLLRDNORM | POLLWRNORM,
        },
        revents: 0,
    }
}

fn poll(descriptors: &mut [WSAPOLLFD], timeout_ms: i32) -> Result<bool, i32> {
    // SAFETY: descriptors refer to owned live sockets; Rust initialized Winsock
    // when it created them. The slice remains writable throughout this call.
    let count = unsafe {
        WSAPoll(
            descriptors.as_mut_ptr(),
            descriptors.len() as u32,
            timeout_ms,
        )
    };
    if count == SOCKET_ERROR {
        Err(unsafe { WSAGetLastError() })
    } else {
        Ok(count > 0)
    }
}

fn error(code: i32) -> TorclError {
    TorclError::StreamError(format!(
        "socket readiness wait: {}",
        std::io::Error::from_raw_os_error(code)
    ))
}

fn native_wait(
    socket: &TcpStream,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, TorclError> {
    let started = Instant::now();
    let mut descriptors = [descriptor(socket, interest)];
    loop {
        let remaining = timeout.map(|t| t.saturating_sub(started.elapsed()));
        let millis = remaining
            .map(|t| t.as_nanos().div_ceil(1_000_000).min(i32::MAX as u128) as i32)
            .unwrap_or(-1);
        match poll(&mut descriptors, millis) {
            Err(WSAEINTR) => {
                if remaining == Some(Duration::ZERO) {
                    return Ok(false);
                }
            }
            result => return result.map_err(error),
        }
    }
}

pub(super) fn wait(
    socket: &TcpStream,
    interest: IoInterest,
    timeout: Option<Duration>,
) -> Result<bool, TorclError> {
    if timeout == Some(Duration::ZERO) {
        return native_wait(socket, interest, timeout);
    }
    if matches!(blocking_mode("SOCKET-WAIT")?, BlockingMode::Native) {
        // SAFETY: WSAPoll only touches native descriptors, never Lisp storage.
        let _blocked = unsafe { crate::safepoint::NativeBlockingScope::enter() };
        return native_wait(socket, interest, timeout);
    }
    let service = service();
    if !service.started {
        return Err(TorclError::StreamError(
            "failed to start Winsock readiness service".into(),
        ));
    }
    let socket = socket
        .try_clone()
        .map_err(|e| TorclError::StreamError(e.to_string()))?;
    let (fiber, token) = crate::thread::prepare_current_fiber_park(FiberState::Waiting)?;
    let registration = Arc::new(Registration {
        socket,
        interest,
        fiber,
        token,
        result: Mutex::new(None),
    });
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let (queue, wake) = &*service.queue;
    queue.lock().unwrap().insert(id, Arc::clone(&registration));
    wake.notify_one();
    if let Some(duration) = timeout {
        if let Err(e) = timer::schedule(fiber, token, Instant::now() + duration) {
            queue.lock().unwrap().remove(&id);
            crate::thread::cancel_prepared_current_fiber_park();
            return Err(e);
        }
    }
    let parked = crate::thread::park_prepared_current_fiber();
    queue.lock().unwrap().remove(&id);
    parked?;
    match registration.result.lock().unwrap().take() {
        Some(Ok(())) => Ok(true),
        Some(Err(code)) => Err(error(code)),
        None => Ok(false),
    }
}

fn poll_loop(shared: Arc<Queue>) {
    let (queue, wake) = &*shared;
    loop {
        let snapshot: Vec<_> = {
            let mut guard = queue.lock().unwrap();
            while guard.is_empty() {
                guard = wake.wait(guard).unwrap();
            }
            guard
                .iter()
                .map(|(id, registration)| (*id, Arc::clone(registration)))
                .collect()
        };
        let mut descriptors: Vec<_> = snapshot
            .iter()
            .map(|(_, r)| descriptor(&r.socket, r.interest))
            .collect();
        // Bound admission/cancellation latency for registrations added while
        // this snapshot is in flight. An empty service sleeps on the condvar.
        let result = poll(&mut descriptors, 10);
        if result == Err(WSAEINTR) {
            continue;
        }
        for ((id, registration), descriptor) in snapshot.iter().zip(descriptors.iter()) {
            if result.is_ok() && descriptor.revents == 0 {
                continue;
            }
            let owned = {
                let mut guard = queue.lock().unwrap();
                if guard.remove(id).is_some() {
                    *registration.result.lock().unwrap() = Some(result.map(|_| ()));
                    true
                } else {
                    false
                }
            };
            if owned {
                crate::thread::wake_fiber_wait(registration.fiber, registration.token);
            }
        }
    }
}
