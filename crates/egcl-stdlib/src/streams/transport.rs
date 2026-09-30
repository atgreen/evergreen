//! Owned OS transports behind the common stream buffers. A Windows SOCKET is
//! not a file HANDLE: keep TcpStream responsible for its reads, writes and close.
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

pub(super) enum StreamHandle {
    File(File),
    Pipe(PipeHandle),
    Socket(TcpStream),
    Closed,
}

pub(super) struct PipeHandle {
    file: File,
    closed: Arc<AtomicBool>,
}

impl StreamHandle {
    pub(super) fn socket(socket: TcpStream) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        Ok(Self::Socket(socket))
    }

    pub(super) fn pipe(file: File) -> Self {
        Self::Pipe(PipeHandle {
            file,
            closed: Arc::new(AtomicBool::new(false)),
        })
    }

    pub(super) fn close(&mut self) {
        if let Self::Socket(socket) = self {
            // Wake readiness waits holding a duplicate before releasing ours.
            let _ = socket.shutdown(Shutdown::Both);
        }
        if let Self::Pipe(pipe) = self {
            pipe.closed.store(true, Ordering::Release);
        }
        *self = Self::Closed;
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::File(file) => file.try_clone().map(Self::File),
            Self::Pipe(pipe) => Ok(Self::Pipe(PipeHandle {
                file: pipe.file.try_clone()?,
                closed: Arc::clone(&pipe.closed),
            })),
            Self::Socket(socket) => socket.try_clone().map(Self::Socket),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }

    pub(super) fn metadata(&self) -> io::Result<Metadata> {
        match self {
            Self::File(file) => file.metadata(),
            _ => Err(io::Error::new(io::ErrorKind::Unsupported, "not a file")),
        }
    }

    /// True when a read can complete, including EOF and connection errors.
    pub(super) fn wait_readable(&self, timeout_ms: Option<i32>) -> io::Result<bool> {
        if let Self::Socket(socket) = self {
            let timeout = timeout_ms
                .filter(|n| *n >= 0)
                .map(|n| std::time::Duration::from_millis(n as u64));
            return egcl_rt::sync::wait_socket(socket, egcl_rt::sync::IoInterest::Read, timeout)
                .map_err(|e| io::Error::other(e.to_string()));
        }
        if matches!(self, Self::Pipe(_)) && timeout_ms != Some(0) {
            let handle = self.try_clone()?;
            return pipe_io("pipe readiness", move || {
                handle.wait_readable_native(timeout_ms)
            });
        }
        self.wait_readable_native(timeout_ms)
    }

    fn wait_readable_native(&self, timeout_ms: Option<i32>) -> io::Result<bool> {
        if let Self::Pipe(pipe) = self {
            return pipe_wait_readable(pipe, timeout_ms);
        }
        #[cfg(unix)]
        {
            use egcl_rt::syscall::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd};
            let mut descriptor = PollFd {
                fd: self.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor is live and this handle owns the descriptor.
            let count =
                unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, timeout_ms.unwrap_or(-1)) }
                    .map_err(|errno| io::Error::from_raw_os_error(errno.abs()))?;
            if descriptor.revents & POLLNVAL != 0 {
                return Err(io::Error::other("invalid stream descriptor"));
            }
            Ok(count > 0 && descriptor.revents & (POLLIN | POLLHUP | POLLERR) != 0)
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows_sys::Win32::Networking::WinSock::*;
            let Self::Socket(socket) = self else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "readiness requires a socket",
                ));
            };
            let mut descriptor = WSAPOLLFD {
                fd: socket.as_raw_socket() as SOCKET,
                events: POLLRDNORM,
                revents: 0,
            };
            // SAFETY: Rust initialized Winsock when creating this owned socket;
            // descriptor is a live single-element array for the entire call.
            let count = unsafe { WSAPoll(&mut descriptor, 1, timeout_ms.unwrap_or(-1)) };
            if count == SOCKET_ERROR {
                return Err(io::Error::from_raw_os_error(unsafe { WSAGetLastError() }));
            }
            if descriptor.revents & POLLNVAL != 0 {
                return Err(io::Error::other("invalid socket"));
            }
            Ok(count > 0 && descriptor.revents & (POLLRDNORM | POLLHUP | POLLERR) != 0)
        }
    }

    /// LISTEN distinguishes readable data from EOF without consuming a byte.
    pub(super) fn socket_has_input(&self) -> io::Result<bool> {
        if let Self::Pipe(pipe) = self {
            let file = &pipe.file;
            #[cfg(windows)]
            return Ok(pipe_available(file)?.is_some_and(|count| count > 0));
            #[cfg(unix)]
            {
                use egcl_rt::syscall::{POLLIN, PollFd};
                let mut descriptor = PollFd {
                    fd: file.as_raw_fd(),
                    events: POLLIN,
                    revents: 0,
                };
                // SAFETY: this live pipe owns the descriptor for the call.
                unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, 0) }
                    .map_err(|errno| io::Error::from_raw_os_error(errno.abs()))?;
                return Ok(descriptor.revents & POLLIN != 0);
            }
        }
        let Self::Socket(socket) = self else {
            return Ok(false);
        };
        if !self.wait_readable(Some(0))? {
            return Ok(false);
        }
        match socket.peek(&mut [0]) {
            Ok(count) => Ok(count > 0),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
            Err(e) => Err(e),
        }
    }
}

impl Read for StreamHandle {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buffer),
            Self::Pipe(pipe) => {
                if buffer.is_empty() {
                    return Ok(0);
                }
                let mut file = pipe.file.try_clone()?;
                let mut owned = vec![0; buffer.len()];
                let (owned, count) = pipe_io("pipe read", move || {
                    let count = match file.read(&mut owned) {
                        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => 0,
                        result => result?,
                    };
                    Ok((owned, count))
                })?;
                buffer[..count].copy_from_slice(&owned[..count]);
                Ok(count)
            }
            Self::Socket(socket) => socket_io(
                socket,
                egcl_rt::sync::IoInterest::Read,
                socket.read_timeout()?,
                |mut socket| socket.read(buffer),
            ),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }
}

/// Retry non-blocking I/O against one timeout budget. Readiness is only a hint;
/// a competing reader or a spurious notification may require another wait.
fn socket_io<T>(
    socket: &TcpStream,
    interest: egcl_rt::sync::IoInterest,
    timeout: Option<std::time::Duration>,
    mut action: impl FnMut(&TcpStream) -> io::Result<T>,
) -> io::Result<T> {
    let started = std::time::Instant::now();
    loop {
        match action(socket) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let remaining = timeout.map(|t| t.saturating_sub(started.elapsed()));
                if remaining == Some(std::time::Duration::ZERO)
                    || !egcl_rt::sync::wait_socket(socket, interest, remaining)
                        .map_err(|e| io::Error::other(e.to_string()))?
                {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "socket I/O timed out",
                    ));
                }
            }
            result => return result,
        }
    }
}
impl Write for StreamHandle {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.write(buffer),
            Self::Pipe(pipe) => {
                if buffer.is_empty() {
                    return Ok(0);
                }
                let mut file = pipe.file.try_clone()?;
                let owned = buffer.to_vec();
                pipe_io("pipe write", move || file.write(&owned))
            }
            Self::Socket(socket) => socket_io(
                socket,
                egcl_rt::sync::IoInterest::Write,
                socket.write_timeout()?,
                |mut socket| socket.write(buffer),
            ),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            Self::Pipe(_) => Ok(()),
            Self::Socket(socket) => socket.flush(),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }
}

/// The worker owns only Rust data and a duplicate OS handle. No pointer into
/// Lisp storage or a caller's stack crosses this boundary. Stream ownership
/// remains with the parked caller, preserving whole-operation atomicity.
fn pipe_io<T: Send + 'static>(
    operation: &str,
    action: impl FnOnce() -> io::Result<T> + Send + 'static,
) -> io::Result<T> {
    use std::sync::{Arc, Mutex};
    use egcl_rt::sync::{BlockingMode, EgclSemaphore, blocking_mode};
    let as_io_error = |error: egcl_rt::error::EgclError| io::Error::other(error.to_string());
    match blocking_mode(operation).map_err(as_io_error)? {
        BlockingMode::Native => {
            // SAFETY: action owns only Rust data; it never accesses Lisp values
            // or roots while this thread is admitted to a moving collection.
            let _blocked = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
            action()
        }
        BlockingMode::Fiber => {
            let completed = Arc::new(EgclSemaphore::new(None, 0).map_err(as_io_error)?);
            let result = Arc::new(Mutex::new(None));
            let worker_completed = Arc::clone(&completed);
            let worker_result = Arc::clone(&result);
            std::thread::Builder::new()
                .name("egcl-pipe-io".into())
                .spawn(move || {
                    let output = std::panic::catch_unwind(std::panic::AssertUnwindSafe(action))
                        .unwrap_or_else(|_| Err(io::Error::other("pipe I/O worker panicked")));
                    *worker_result.lock().unwrap() = Some(output);
                    worker_completed
                        .signal(1)
                        .expect("single pipe I/O completion");
                })?;
            completed.wait(None).map_err(as_io_error)?;
            result
                .lock()
                .unwrap()
                .take()
                .ok_or_else(|| io::Error::other("missing pipe I/O result"))?
        }
    }
}
impl Seek for StreamHandle {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        match self {
            Self::File(file) => file.seek(position),
            _ => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "stream is not positionable",
            )),
        }
    }
}
#[cfg(unix)]
impl AsRawFd for StreamHandle {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        match self {
            Self::File(file) => file.as_raw_fd(),
            Self::Pipe(pipe) => pipe.file.as_raw_fd(),
            Self::Socket(socket) => socket.as_raw_fd(),
            Self::Closed => -1,
        }
    }
}

#[cfg(windows)]
fn pipe_available(file: &File) -> io::Result<Option<u32>> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{Foundation::ERROR_BROKEN_PIPE, System::Pipes::PeekNamedPipe};
    let mut available = 0;
    // SAFETY: this is an owned anonymous pipe's read handle; only the byte
    // count is requested, so no buffer pointers are supplied.
    let result = unsafe {
        PeekNamedPipe(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            &mut available,
            std::ptr::null_mut(),
        )
    };
    if result != 0 {
        return Ok(Some(available));
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_BROKEN_PIPE as i32) {
        Ok(None)
    } else {
        Err(error)
    }
}

// Readiness duplicates share cancellation with CLOSE. Finite poll slices also
// release the duplicate promptly when the peer stays alive but never writes.
fn pipe_wait_readable(pipe: &PipeHandle, timeout_ms: Option<i32>) -> io::Result<bool> {
    use std::time::{Duration, Instant};
    let start = Instant::now();
    let timeout = timeout_ms
        .filter(|n| *n >= 0)
        .map(|n| Duration::from_millis(n as u64));
    loop {
        if pipe.closed.load(Ordering::Acquire) {
            return Err(io::Error::other("pipe closed during readiness wait"));
        }
        let slice = timeout.map_or(Duration::from_millis(10), |limit| {
            limit
                .saturating_sub(start.elapsed())
                .min(Duration::from_millis(10))
        });
        #[cfg(unix)]
        let ready = {
            use egcl_rt::syscall::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd};
            let mut descriptor = PollFd {
                fd: pipe.file.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: pipe owns this descriptor for the entire poll.
            match unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, slice.as_millis() as i32) } {
                Ok(_) => {}
                Err(errno) => {
                    let error = io::Error::from_raw_os_error(errno.abs());
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(error);
                }
            }
            if descriptor.revents & POLLNVAL != 0 {
                return Err(io::Error::other("invalid pipe descriptor"));
            }
            descriptor.revents & (POLLIN | POLLHUP | POLLERR) != 0
        };
        #[cfg(windows)]
        let ready = pipe_available(&pipe.file)? != Some(0);
        if pipe.closed.load(Ordering::Acquire) {
            return Err(io::Error::other("pipe closed during readiness wait"));
        }
        if ready {
            return Ok(true);
        }
        if timeout.is_some_and(|limit| start.elapsed() >= limit) {
            return Ok(false);
        }
        #[cfg(windows)]
        std::thread::sleep(slice.min(Duration::from_millis(1)));
    }
}

#[cfg(test)]
mod socket_tests {
    use super::*;
    use std::net::TcpListener;
    use std::sync::{Mutex, OnceLock, mpsc};
    use std::time::Duration;
    use egcl_rt::value::{T, EgclVal};
    use egcl_rt::{SchedulerConfig, SchedulerGroup};

    static SOCKET: Mutex<Option<StreamHandle>> = Mutex::new(None);
    static WRITING: AtomicBool = AtomicBool::new(false);
    static RELEASE: OnceLock<mpsc::Sender<()>> = OnceLock::new();
    const SIZE: usize = 16 * 1024 * 1024;

    fn writer() -> EgclVal {
        let mut socket = SOCKET.lock().unwrap().take().unwrap();
        let bytes = vec![0x5a; SIZE];
        WRITING.store(true, Ordering::Release);
        socket.write_all(&bytes).unwrap();
        T
    }

    fn release_reader() -> EgclVal {
        while !WRITING.load(Ordering::Acquire) {
            egcl_rt::sync::fiber_sleep(Duration::from_millis(1)).unwrap();
        }
        RELEASE.get().unwrap().send(()).unwrap();
        T
    }

    #[test]
    fn socket_backpressure_parks_writer_and_preserves_every_byte() {
        const CHILD: &str = "EGCL_SOCKET_BACKPRESSURE_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "streams::transport::socket_tests::socket_backpressure_parks_writer_and_preserves_every_byte", "--nocapture"])
                .env(CHILD, "1").output().unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(30));
            std::process::exit(124);
        });
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let socket = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut peer, _) = listener.accept().unwrap();
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let (sender, receiver) = mpsc::channel();
        RELEASE.set(sender).unwrap();
        let reader = std::thread::spawn(move || {
            receiver.recv_timeout(Duration::from_secs(10)).unwrap();
            let mut received = 0;
            let mut buffer = [0; 65536];
            loop {
                let n = peer.read(&mut buffer).unwrap();
                if n == 0 {
                    break;
                }
                assert!(buffer[..n].iter().all(|b| *b == 0x5a));
                received += n;
            }
            assert_eq!(received, SIZE);
        });
        *SOCKET.lock().unwrap() = Some(StreamHandle::socket(socket).unwrap());
        let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
        for entry in [writer as fn() -> EgclVal, release_reader] {
            let function = unsafe { EgclVal::from_function_ptr(entry as *const () as *mut u8) };
            group
                .submit(egcl_rt::thread::make_fiber(function).unwrap())
                .unwrap();
        }
        assert_eq!(group.finish().unwrap(), vec![T, T]);
        reader.join().unwrap();
    }
}
