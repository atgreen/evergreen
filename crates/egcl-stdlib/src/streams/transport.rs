// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

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
    /// The contents of an image-embedded file: a copy taken when the stream
    /// was opened, so the stream never holds a pointer into the moving heap.
    Memory(io::Cursor<Vec<u8>>),
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
            Self::Memory(cursor) => Ok(Self::Memory(cursor.clone())),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }

    /// The length of an in-memory transport; `None` for every OS-backed one.
    pub(super) fn memory_len(&self) -> Option<u64> {
        match self {
            Self::Memory(cursor) => Some(cursor.get_ref().len() as u64),
            _ => None,
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
        if matches!(self, Self::Memory(_)) {
            return Ok(true);
        }
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
    ///
    /// Asked only when the stream's own buffer is empty, so for a regular
    /// file the handle's position is the stream's: the file has input while
    /// that position is short of its length. The same goes for an
    /// image-embedded file. A descriptor that is not a regular file (a
    /// device, a FIFO opened by name) is polled like a pipe.
    pub(super) fn socket_has_input(&self) -> io::Result<bool> {
        match self {
            Self::File(file) => {
                let metadata = file.metadata()?;
                if metadata.file_type().is_file() {
                    let mut handle = file;
                    return Ok(handle.stream_position()? < metadata.len());
                }
                descriptor_has_input(file)
            }
            Self::Memory(cursor) => Ok(cursor.position() < cursor.get_ref().len() as u64),
            Self::Pipe(pipe) => descriptor_has_input(&pipe.file),
            Self::Socket(socket) => {
                if !self.wait_readable(Some(0))? {
                    return Ok(false);
                }
                match socket.peek(&mut [0]) {
                    Ok(count) => Ok(count > 0),
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(false),
                    Err(e) => Err(e),
                }
            }
            Self::Closed => Ok(false),
        }
    }
}

/// Whether a pipe or device descriptor has a byte to read right now: a
/// zero-timeout poll for input, which does not consume anything.
fn descriptor_has_input(file: &File) -> io::Result<bool> {
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
        // SAFETY: the caller's live handle owns the descriptor for the call.
        unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, 0) }
            .map_err(|errno| io::Error::from_raw_os_error(errno.abs()))?;
        Ok(descriptor.revents & POLLIN != 0)
    }
}

impl Read for StreamHandle {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.read(buffer),
            Self::Memory(cursor) => cursor.read(buffer),
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

impl StreamHandle {
    /// Read without waiting, reporting `WouldBlock` rather than parking.
    ///
    /// `Read::read` below parks inside `socket_io` until the socket is ready.
    /// That is correct in isolation but its caller holds the stream's state
    /// mutex for the whole wait, which locks every other thread out of the
    /// stream -- CLOSE, a write on the other half, even INPUT-STREAM-P
    /// (bliss-vupo0). This lets the caller attempt the read under the mutex,
    /// release it on `WouldBlock`, and park outside via `wait_readable`.
    ///
    /// Sockets report it directly: `StreamHandle::socket` puts them in
    /// non-blocking mode. A PIPE -- a child's stdout or stderr, the only thing
    /// that builds `StreamHandle::Pipe` -- is asked for readiness first and
    /// reports `WouldBlock` when it has none, so a pipe whose peer is slow
    /// parks outside the mutex just as a socket does. Measured both ways in
    /// `process_pipes::a_parked_pipe_read_does_not_lock_other_threads_out_of_the_stream`:
    /// without this arm, `stream_element_type` on another thread waited 2.6s
    /// behind a reader parked on a child that withheld its output, and it
    /// returns at once with it.
    ///
    /// Files and embedded memory complete without parking, and a readiness
    /// poll on a regular file is always ready, so there is nothing to hand
    /// back -- note a FIFO opened with `OPEN` is a `File` here, not a `Pipe`.
    pub(super) fn read_without_waiting(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Socket(socket) => {
                let mut socket = &*socket;
                loop {
                    match socket.read(buffer) {
                        Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                        result => return result,
                    }
                }
            }
            Self::Pipe(_) => {
                // Zero timeout: a readiness question, not a wait. Readers are
                // serialized by the stream's `input` mutex, so nothing can
                // consume the data between this answer and the read below.
                if !self.wait_readable(Some(0))? {
                    return Err(io::Error::from(io::ErrorKind::WouldBlock));
                }
                self.read(buffer)
            }
            _ => self.read(buffer),
        }
    }

    /// Whether this handle can report `WouldBlock` from `read_without_waiting`.
    pub(super) fn parks_outside_mutex(&self) -> bool {
        matches!(self, Self::Socket(_) | Self::Pipe(_))
    }

    /// The read timeout budget a socket handle carries, if any.
    pub(super) fn read_timeout_budget(&self) -> Option<std::time::Duration> {
        match self {
            Self::Socket(socket) => socket.read_timeout().ok().flatten(),
            _ => None,
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
            Self::Memory(_) => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "embedded files are read-only",
            )),
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
            Self::Pipe(_) | Self::Memory(_) => Ok(()),
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
            Self::Memory(_) | Self::Closed => -1,
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
