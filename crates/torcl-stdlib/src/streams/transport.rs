//! Owned OS transports behind the common stream buffers. A Windows SOCKET is
//! not a file HANDLE: keep TcpStream responsible for its reads, writes and close.
use std::fs::{File, Metadata};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd;

pub(super) enum StreamHandle {
    File(File),
    Socket(TcpStream),
    Closed,
}

impl StreamHandle {
    pub(super) fn close(&mut self) {
        if let Self::Socket(socket) = self {
            // Wake readiness waits holding a duplicate before releasing ours.
            let _ = socket.shutdown(Shutdown::Both);
        }
        *self = Self::Closed;
    }

    pub(super) fn try_clone(&self) -> io::Result<Self> {
        match self {
            Self::File(file) => file.try_clone().map(Self::File),
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
        #[cfg(unix)]
        {
            use torcl_rt::syscall::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd};
            let mut descriptor = PollFd {
                fd: self.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: descriptor is live and this handle owns the descriptor.
            let count =
                unsafe { torcl_rt::syscall::poll(&mut descriptor, 1, timeout_ms.unwrap_or(-1)) }
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
            Self::Socket(socket) => socket.read(buffer),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }
}
impl Write for StreamHandle {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        match self {
            Self::File(file) => file.write(buffer),
            Self::Socket(socket) => socket.write(buffer),
            Self::Closed => Err(io::Error::other("closed stream")),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::File(file) => file.flush(),
            Self::Socket(socket) => socket.flush(),
            Self::Closed => Err(io::Error::other("closed stream")),
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
            Self::Socket(socket) => socket.as_raw_fd(),
            Self::Closed => -1,
        }
    }
}
