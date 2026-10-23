// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Streams — Gray streams protocol and built-in stream types.
//!
//! See spec §5.5.

use std::alloc::Layout;
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::sync::OnceLock;

use egcl_rt::error::EgclError;
use egcl_rt::lock_order::{
    LockLevel, OrderedExecutionMutex, OrderedExecutionMutexGuard, OrderedMutex,
};

#[cfg(all(test, target_arch = "x86_64", any(unix, windows)))]
mod fiber_tests;
use egcl_rt::object::{ObjectHeader, type_id};
use egcl_rt::value::{EOF, NIL, T, EgclVal};

mod transport;
use transport::StreamHandle;

// ── Gray streams protocol ──────────────────────────────────────────

/// Read a standard character literal from just after its sharp-backslash prefix.
/// Keep the parser shared with the compiler reader and leave its delimiter unread.
pub fn read_character_literal(stream: EgclVal, suppress: bool) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let first = stream_read_char(*stream)?;
    if first == EOF {
        return Err(EgclError::StreamError("unexpected end after #\\".into()));
    }
    let mut token = vec![first.as_char()];
    if token[0].is_ascii_alphabetic() {
        loop {
            let next = stream_read_char(*stream)?;
            if next == EOF {
                break;
            }
            if egcl_compiler::reader::character_literal_delimiter(next.as_char()) {
                stream_unread_char(*stream, next)?;
                break;
            }
            token.push(next.as_char());
        }
    }
    if suppress {
        Ok(NIL)
    } else {
        egcl_compiler::reader::read_char_literal(&token, 0).map(|(value, _)| value)
    }
}

/// Gray stream operations trait. R5.23 / R5.113.
pub trait GrayStream {
    fn stream_read_char(&mut self) -> Result<EgclVal, EgclError>;
    fn stream_unread_char(&mut self, ch: EgclVal) -> Result<(), EgclError>;
    fn stream_read_byte(&mut self) -> Result<EgclVal, EgclError>;
    fn stream_write_char(&mut self, ch: EgclVal) -> Result<(), EgclError>;
    fn stream_write_byte(&mut self, byte: EgclVal) -> Result<(), EgclError>;
    fn stream_write_string(
        &mut self,
        string: EgclVal,
        start: usize,
        end: Option<usize>,
    ) -> Result<(), EgclError>;
    fn stream_force_output(&mut self) -> Result<(), EgclError>;
    fn stream_finish_output(&mut self) -> Result<(), EgclError>;
    fn stream_clear_input(&mut self) -> Result<(), EgclError>;
    fn stream_listen(&self) -> Result<bool, EgclError>;
    fn stream_line_number(&self) -> Option<u64>;
    fn stream_line_column(&self) -> Option<u64>;
    // R5.113 additional Gray protocol methods:
    fn stream_read_char_no_hang(&mut self) -> Result<EgclVal, EgclError>;
    fn stream_peek_char(&mut self) -> Result<EgclVal, EgclError>;
    fn stream_read_line(&mut self) -> Result<(EgclVal, bool), EgclError>;
    fn stream_terpri(&mut self) -> Result<(), EgclError>;
    fn stream_fresh_line(&mut self) -> Result<bool, EgclError>;
    fn stream_clear_output(&mut self) -> Result<(), EgclError>;
    fn stream_advance_to_column(&mut self, col: u64) -> Result<bool, EgclError>;
    fn stream_start_line_p(&self) -> bool;
    fn stream_read_sequence(&mut self, count: usize) -> Result<Vec<EgclVal>, EgclError>;
    fn stream_write_sequence(&mut self, elements: &[EgclVal]) -> Result<(), EgclError>;
    fn interactive_stream_p(&self) -> bool;
    fn stream_external_format(&self) -> ExternalFormat;
}

// ── Stream direction ───────────────────────────────────────────────

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamDirection {
    Input,
    Output,
    Io,
    Probe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExternalFormat {
    Utf8,
    Ascii,
    Latin1,
    Utf16,
    Utf32,
}

/// Element type for a stream — character vs binary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamElementType {
    Character,
    UnsignedByte8,
    /// `(unsigned-byte BITS)` / `(signed-byte BITS)` for any other width.
    /// Each element occupies `BITS.div_ceil(8)` octets, little-endian, two's
    /// complement when signed (ansi streams open.*, file-length.*).
    Integer { bits: u16, signed: bool },
}

impl StreamElementType {
    /// Octets one element occupies in a file.
    pub fn octets_per_element(self) -> usize {
        match self {
            StreamElementType::Integer { bits, .. } => usize::from(bits).div_ceil(8),
            _ => 1,
        }
    }

    /// A binary (integer-element) stream, of any width.
    pub fn is_binary(self) -> bool {
        !matches!(self, StreamElementType::Character)
    }

    /// The code `open()` and `stream_element_type()` exchange with the
    /// interpreter: the bit width, negated for a signed type; 8 is the
    /// `(unsigned-byte 8)` fast path.
    pub fn from_code(code: EgclVal) -> StreamElementType {
        if !code.is_fixnum() {
            return StreamElementType::Character;
        }
        match code.as_fixnum() {
            8 => StreamElementType::UnsignedByte8,
            n if (1..=i64::from(u16::MAX)).contains(&n) => StreamElementType::Integer {
                bits: n as u16,
                signed: false,
            },
            n if (-i64::from(u16::MAX)..=-1).contains(&n) => StreamElementType::Integer {
                bits: (-n) as u16,
                signed: true,
            },
            _ => StreamElementType::Character,
        }
    }

    pub fn to_code(self) -> EgclVal {
        match self {
            StreamElementType::Character => T,
            StreamElementType::UnsignedByte8 => EgclVal::from_fixnum(8),
            StreamElementType::Integer { bits, signed } => {
                let bits = i64::from(bits);
                EgclVal::from_fixnum(if signed { -bits } else { bits })
            }
        }
    }

    fn type_name(self) -> String {
        match self {
            StreamElementType::Character => "CHARACTER".into(),
            StreamElementType::UnsignedByte8 => "(UNSIGNED-BYTE 8)".into(),
            StreamElementType::Integer { bits, signed } => {
                format!("({} {})", if signed { "SIGNED-BYTE" } else { "UNSIGNED-BYTE" }, bits)
            }
        }
    }
}

/// Assemble a little-endian element of `bits` bits from its octets.
fn integer_from_octets(octets: &[u8], bits: u16, signed: bool) -> EgclVal {
    use egcl_rt::bignum::{BigInt, mag_bit, mag_set_bit, mag_sub, mag_trim};
    let bits = usize::from(bits);
    let mut limbs = vec![0u64; octets.len().div_ceil(8).max(1)];
    for (i, b) in octets.iter().enumerate() {
        limbs[i / 8] |= u64::from(*b) << (8 * (i % 8));
    }
    // Drop any bits above the element width.
    let (full, rem) = (bits / 64, bits % 64);
    for (i, limb) in limbs.iter_mut().enumerate() {
        if i > full || (i == full && rem == 0) {
            *limb = 0;
        } else if i == full {
            *limb &= (1u64 << rem) - 1;
        }
    }
    let mag = mag_trim(limbs);
    if signed && bits > 0 && (bits - 1) / 64 < mag.len() && mag_bit(&mag, bits - 1) == 1 {
        // Sign bit set: the value is RAW - 2^BITS.
        let mut pow = Vec::new();
        mag_set_bit(&mut pow, bits);
        return BigInt::from_mag(-1, mag_sub(&pow, &mag)).to_val();
    }
    BigInt::from_mag(1, mag).to_val()
}

/// The little-endian octets of `value` as an element of `bits` bits, or None
/// when it does not fit the element type.
fn integer_to_octets(value: &egcl_rt::bignum::BigInt, bits: u16, signed: bool) -> Option<Vec<u8>> {
    use egcl_rt::bignum::{mag_bitlen, twos_limbs};
    let bits = usize::from(bits);
    let n_octets = bits.div_ceil(8);
    let n_limbs = n_octets.div_ceil(8).max(1);
    let magnitude_bits = mag_bitlen(&value.mag);
    let in_range = if signed {
        if value.sign >= 0 {
            magnitude_bits < bits
        } else {
            // -2^(BITS-1) is representable: a magnitude of exactly one bit at BITS-1.
            magnitude_bits < bits
                || (magnitude_bits == bits
                    && value.mag.iter().map(|l| l.count_ones()).sum::<u32>() == 1)
        }
    } else {
        value.sign >= 0 && magnitude_bits <= bits
    };
    if !in_range {
        return None;
    }
    let limbs = twos_limbs(value, n_limbs.max(value.mag.len()));
    Some((0..n_octets).map(|i| (limbs[i / 8] >> (8 * (i % 8))) as u8).collect())
}

// ── if_exists keyword constants ───────────────────────────────────
// Callers pass these as EgclVal to open(). NIL and T are also valid.
// NIL = return nil if exists; T = default (:supersede).
pub const IF_EXISTS_ERROR_VAL: EgclVal = EgclVal(1 << 3); // fixnum 1
pub const IF_EXISTS_SUPERSEDE_VAL: EgclVal = EgclVal(2 << 3); // fixnum 2
pub const IF_EXISTS_APPEND_VAL: EgclVal = EgclVal(3 << 3); // fixnum 3
pub const IF_EXISTS_OVERWRITE_VAL: EgclVal = EgclVal(4 << 3); // fixnum 4

/// Missing-file creation policy; NIL returns NIL and T signals a file error.
pub const IF_DOES_NOT_EXIST_CREATE_VAL: EgclVal = EgclVal(5 << 3);

// ── Internal stream state ──────────────────────────────────────────

/// Off-heap Rust-owned stream state (egcl-jtc.7a). This is NOT the Lisp-visible
/// stream value; it is a `Box`-allocated block kept off the moving GC heap
/// (the `Mutex<File>`/buffers can't be relocated by an evacuating collector).
/// The Lisp value is a small GC-heap *handle* (`alloc_typed`, tag STREAM) whose
/// first body word holds a raw pointer to this block (see `alloc_stream` /
/// `get_stream_alloc`). A GC finalizer registered on the handle drops this block
/// when the stream becomes unreachable, releasing the fd (via `File`'s Drop) and
/// buffers, and warning for an unclosed file stream (R5.121).
struct StreamAlloc {
    /// Opening namestring, retained after CLOSE; absent for non-file streams.
    file_namestring: Option<String>,
    /// Immutable component references of a composite stream (broadcast /
    /// concatenated components, two-way/echo input+output, synonym symbol), or
    /// empty for non-composite streams. Stored OUTSIDE the per-stream mutex so
    /// the GC can trace and forward them lock-free during a stop-the-world pause
    /// (a mutator may be parked at a safepoint holding the mutex, so the tracer
    /// must never need it). Set once at construction; only the GC rewrites its
    /// entries, to forward evacuated components (egcl-jtc.7a / jtc.7f).
    components: Box<[EgclVal]>,
    /// Per-stream mutex — every operation spanning multiple elements is atomic
    /// under this lock (R5.120).
    state: OrderedExecutionMutex<StreamMutableState>,
    /// Serializes INPUT operations so a reader can park for data with `state`
    /// RELEASED (bliss-vupo0).
    ///
    /// Reads used to park inside `state`, which locked every other thread out
    /// of the stream for the duration -- CLOSE, a write on the other half of a
    /// socket, even INPUT-STREAM-P. The park cannot simply move outside
    /// `state`, because a read consumes bytes and two readers would interleave;
    /// the serialization has to live somewhere, so it lives here. Readers hold
    /// `input` across the whole operation and take `state` only for work that
    /// does not park. Everything else takes `state` alone and no longer waits
    /// behind a parked reader.
    ///
    /// Lock order is `input` then `state`, enforced by giving `input` the lower
    /// order of the pair (locks within a level must be acquired in increasing
    /// order).
    input: OrderedExecutionMutex<()>,
}

/// Mutable portion of stream state, protected by the per-stream mutex.
struct StreamMutableState {
    open: bool,
    element_type: StreamElementType,
    inner: StreamInner,
    /// Raw pointer to the sibling `StreamAlloc::components` slice, so composite
    /// op arms can read their component references without re-entering
    /// `get_stream_alloc`. Points into the same (stable, off-heap) `Box`, so it
    /// stays valid for the block's lifetime. Read via `self.components()`.
    components_ptr: *const [EgclVal],
}

impl StreamMutableState {
    /// One octet of a binary file stream, or EOF.
    fn read_raw_octet(&mut self) -> Result<EgclVal, EgclError> {
        match &mut self.inner {
            StreamInner::FileInput {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            }
            | StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } => file_read_byte_raw(file, read_buf, buf_pos, buf_fill),
            _ => Err(EgclError::StreamError(
                "binary element type on a non-file stream".into(),
            )),
        }
    }

    fn write_raw_octet(&mut self, b: u8) -> Result<(), EgclError> {
        match &mut self.inner {
            StreamInner::FileOutput {
                file, write_buf, ..
            }
            | StreamInner::FileIo {
                file, write_buf, ..
            } => {
                write_buf.push(b);
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            _ => Err(EgclError::StreamError(
                "binary element type on a non-file stream".into(),
            )),
        }
    }

    /// READ-BYTE on an `Integer` element type: the element's octets, assembled.
    fn read_integer_element(&mut self, bits: u16, signed: bool) -> Result<EgclVal, EgclError> {
        let n = usize::from(bits).div_ceil(8);
        let mut octets = Vec::with_capacity(n);
        for i in 0..n {
            let b = self.read_raw_octet()?;
            if b == EOF {
                if i == 0 {
                    return Ok(EOF);
                }
                return Err(EgclError::StreamError(
                    "end of file inside a binary element".into(),
                ));
            }
            octets.push(b.as_fixnum() as u8);
        }
        Ok(integer_from_octets(&octets, bits, signed))
    }

    /// WRITE-BYTE on an `Integer` element type.
    fn write_integer_element(
        &mut self,
        byte: EgclVal,
        bits: u16,
        signed: bool,
    ) -> Result<(), EgclError> {
        let type_error = || EgclError::TypeError {
            datum: byte,
            expected: StreamElementType::Integer { bits, signed }.type_name(),
        };
        let value = egcl_rt::bignum::bigint_from_val(byte).ok_or_else(type_error)?;
        let octets = integer_to_octets(&value, bits, signed).ok_or_else(type_error)?;
        for b in octets {
            self.write_raw_octet(b)?;
        }
        Ok(())
    }

    /// The stream's immutable component references (see `StreamAlloc::components`).
    /// Read the raw back-pointer without borrowing `self`, so callers may hold
    /// this alongside a `&mut self.inner` match.
    #[inline]
    fn components(&self) -> &'static [EgclVal] {
        // SAFETY: `components_ptr` was set at construction to the sibling
        // `components` slice in the same stable Box, which outlives every access.
        unsafe { &*self.components_ptr }
    }
}

enum StreamInner {
    StringInput {
        chars: Vec<char>,
        position: usize,
        end: usize,
        line: u64,
        col: u64,
        unread: Option<char>,
    },
    StringOutput {
        buffer: Vec<u8>,
        line: u64,
        col: u64,
    },
    FileInput {
        file: StreamHandle,
        read_buf: Vec<u8>,
        buf_pos: usize,
        buf_fill: usize,
        line: u64,
        col: u64,
        unread: Option<char>,
        external_format: ExternalFormat,
        element_type: StreamElementType,
    },
    FileOutput {
        file: StreamHandle,
        write_buf: Vec<u8>,
        external_format: ExternalFormat,
        line: u64,
        col: u64,
    },
    FileIo {
        file: StreamHandle,
        read_buf: Vec<u8>,
        buf_pos: usize,
        buf_fill: usize,
        write_buf: Vec<u8>,
        line: u64,
        col: u64,
        unread: Option<char>,
        external_format: ExternalFormat,
    },
    // ── Composite streams (egcl-jtc.7a) ──────────────────────────────
    // Component references live in `StreamAlloc::components` (immutable, traced
    // lock-free by the GC), indexed as documented per variant. These variants
    // hold only non-reference mutable state.
    /// Components: all broadcast targets (fan-out on write).
    Broadcast,
    /// Components: all source streams; `cursor` is the index of the current one.
    Concatenated { cursor: usize },
    /// Components: `[input, output]`.
    TwoWay,
    /// Components: `[input, output]`.
    Echo,
    /// Components: `[symbol]`.
    Synonym,
    /// The process standard input, connected to the real terminal / pipe.
    Stdin {
        unread: Option<char>,
        line: u64,
        col: u64,
    },
    /// The process standard output.
    Stdout { line: u64, col: u64 },
    /// The process error output.
    Stderr { line: u64, col: u64 },
}

impl StreamMutableState {
    /// Write OCTETS, in bulk where the stream is a buffered byte stream and one
    /// at a time otherwise.
    ///
    /// `stream_write_byte` is the wrong shape for a sequence: besides taking the
    /// stream lock per call, it does `self.components().to_vec()` every time --
    /// a heap allocation per octet. Writing the 6.8 MB of an APK through it cost
    /// 3.97 s, ~590 ns a byte, which was the single slowest phase of a build.
    fn write_octets(&mut self, octets: &[u8]) -> Result<(), EgclError> {
        self.check_output()?;
        if octets.is_empty() {
            return Ok(());
        }
        match &mut self.inner {
            StreamInner::StringOutput { buffer, line, col } => {
                buffer.extend_from_slice(octets);
                advance_line_col(octets, line, col);
                Ok(())
            }
            StreamInner::FileOutput {
                file,
                write_buf,
                line,
                col,
                ..
            }
            | StreamInner::FileIo {
                file,
                write_buf,
                line,
                col,
                ..
            } => {
                // A transfer larger than the buffer goes straight to the fd, but
                // only after the buffer drains -- otherwise it would overtake
                // bytes written earlier and the file would be out of order.
                if octets.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                    write_all_octets(file, octets)?;
                } else {
                    write_buf.extend_from_slice(octets);
                    if write_buf.len() >= FILE_BUF_SIZE {
                        file_flush_write_buf(file, write_buf)?;
                    }
                }
                advance_line_col(octets, line, col);
                Ok(())
            }
            // Broadcast and the rest fan out or re-encode per element; keep
            // their existing behaviour rather than duplicate it here.
            _ => {
                for &byte in octets {
                    self.stream_write_byte(EgclVal::from_fixnum(i64::from(byte)))?;
                }
                Ok(())
            }
        }
    }

    fn direction(&self) -> StreamDirection {
        match &self.inner {
            StreamInner::StringInput { .. } => StreamDirection::Input,
            StreamInner::StringOutput { .. } => StreamDirection::Output,
            StreamInner::FileInput { .. } => StreamDirection::Input,
            StreamInner::FileOutput { .. } => StreamDirection::Output,
            StreamInner::FileIo { .. } => StreamDirection::Io,
            StreamInner::Broadcast => StreamDirection::Output,
            StreamInner::Concatenated { .. } => StreamDirection::Input,
            StreamInner::TwoWay => StreamDirection::Io,
            StreamInner::Echo => StreamDirection::Io,
            // A synonym stream has the direction of the stream its symbol
            // currently names (ansi make-synonym-stream.1/.3).
            StreamInner::Synonym => match resolve_synonym(self.components()[0]) {
                Ok(target) => {
                    let input = crate::streams::input_stream_p(target);
                    let output = crate::streams::output_stream_p(target);
                    match (input, output) {
                        (true, false) => StreamDirection::Input,
                        (false, true) => StreamDirection::Output,
                        _ => StreamDirection::Io,
                    }
                }
                Err(_) => StreamDirection::Io,
            },
            StreamInner::Stdin { .. } => StreamDirection::Input,
            StreamInner::Stdout { .. } => StreamDirection::Output,
            StreamInner::Stderr { .. } => StreamDirection::Output,
        }
    }

    fn is_input(&self) -> bool {
        matches!(
            self.direction(),
            StreamDirection::Input | StreamDirection::Io
        )
    }

    fn is_output(&self) -> bool {
        matches!(
            self.direction(),
            StreamDirection::Output | StreamDirection::Io
        )
    }

    fn check_open(&self) -> Result<(), EgclError> {
        if !self.open {
            return Err(EgclError::StreamError("operation on closed stream".into()));
        }
        Ok(())
    }

    fn check_input(&self) -> Result<(), EgclError> {
        self.check_open()?;
        if !self.is_input() {
            return Err(EgclError::StreamError("not an input stream".into()));
        }
        Ok(())
    }

    fn check_output(&self) -> Result<(), EgclError> {
        self.check_open()?;
        if !self.is_output() {
            return Err(EgclError::StreamError("not an output stream".into()));
        }
        Ok(())
    }
}

// ── File I/O helpers ──────────────────────────────────────────────

/// Whether a read of the process standard input would return without waiting.
///
/// `StreamInner::Stdin` holds no `StreamHandle`, so it cannot use
/// `socket_has_input`; it asks descriptor 0 directly. `POLLHUP`/`POLLERR` count
/// as "would not block" because a read then returns end of file immediately,
/// and end of file is reported through the EOF conventions rather than as
/// "nothing available yet". So this answers false only while the descriptor is
/// genuinely open with no data, which is when READ-CHAR-NO-HANG must give NIL.
#[cfg(unix)]
fn stdin_read_would_not_block() -> Result<bool, EgclError> {
    use egcl_rt::syscall::{POLLERR, POLLHUP, POLLIN, PollFd};
    let mut descriptor = PollFd {
        fd: 0,
        events: POLLIN,
        revents: 0,
    };
    // SAFETY: one descriptor, owned by the process for the duration of the call.
    unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, 0) }
        .map_err(|errno| EgclError::StreamError(format!("read-char-no-hang: errno {}", errno.abs())))?;
    Ok(descriptor.revents & (POLLIN | POLLHUP | POLLERR) != 0)
}

/// Off Unix the descriptor cannot be polled portably yet, so keep the previous
/// behaviour (attempt the read) rather than reporting a character absent.
#[cfg(not(unix))]
fn stdin_read_would_not_block() -> Result<bool, EgclError> {
    Ok(true)
}

const FILE_BUF_SIZE: usize = 8192;

/// Decode one character without parking, reporting `WouldBlock` to the caller.
///
/// The non-parking twin of `file_read_char_buffered`, so a character read can
/// park with the stream's state mutex released (bliss-o61rm, the character half
/// of bliss-vupo0).
///
/// Returning mid-character is safe because no partial state is local: the
/// decoder consumes nothing until it has a whole character ("decode without
/// consuming an incomplete prefix" below), and the refill compacts the partial
/// prefix to the front of `read_buf` before reading. buf_pos/buf_fill therefore
/// describe a valid, resumable position at every point a `WouldBlock` can be
/// returned, including after compaction, and the retry re-attempts the decode
/// from there.
#[allow(clippy::too_many_arguments)]
fn file_read_char_without_parking(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    line: &mut u64,
    col: &mut u64,
    unread: &mut Option<char>,
    format: ExternalFormat,
) -> Result<InputStep<EgclVal>, EgclError> {
    if let Some(c) = unread.take() {
        return Ok(InputStep::Done(EgclVal::from_char(c)));
    }
    loop {
        match decode_char(format, &read_buf[*buf_pos..*buf_fill]) {
            Decoded::Char(ch, length) => {
                *buf_pos += length;
                track_col(ch, line, col);
                return Ok(InputStep::Done(EgclVal::from_char(ch)));
            }
            Decoded::Invalid => {
                *buf_pos += 1;
                return Err(invalid_octets_error(format));
            }
            Decoded::Incomplete => {}
        }
        // Compact the incomplete prefix to the front, exactly as the parking
        // version does, so the retry resumes from a valid position.
        let remaining = *buf_fill - *buf_pos;
        read_buf.copy_within(*buf_pos..*buf_fill, 0);
        read_buf.resize(FILE_BUF_SIZE, 0);
        *buf_pos = 0;
        *buf_fill = remaining;
        let count = match file.read_without_waiting(&mut read_buf[remaining..]) {
            Ok(count) => count,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(InputStep::WouldBlock);
            }
            Err(e) => {
                return Err(EgclError::StreamError(format!("file read error: {e}")));
            }
        };
        if count == 0 {
            if remaining != 0 {
                return Err(EgclError::StreamError(
                    "incomplete UTF-8 sequence at EOF".into(),
                ));
            }
            return Ok(InputStep::Done(EOF));
        }
        *buf_fill += count;
    }
}

/// Read a single UTF-8 char from a buffered file input, refilling buffer as needed.
// These are the disjoint mutable fields of a file stream plus its wait policy;
// borrowing the whole stream here would conflict with the caller's field borrows.
#[allow(clippy::too_many_arguments)]
fn file_read_char_buffered(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    line: &mut u64,
    col: &mut u64,
    unread: &mut Option<char>,
    wait: bool,
    format: ExternalFormat,
) -> Result<EgclVal, EgclError> {
    if let Some(c) = unread.take() {
        return Ok(EgclVal::from_char(c));
    }
    loop {
        // Decode without consuming an incomplete prefix. NO-HANG can return
        // NIL between any two bytes of a character and resume on its next call.
        match decode_char(format, &read_buf[*buf_pos..*buf_fill]) {
            Decoded::Char(ch, length) => {
                *buf_pos += length;
                track_col(ch, line, col);
                return Ok(EgclVal::from_char(ch));
            }
            Decoded::Invalid => {
                *buf_pos += 1;
                return Err(invalid_octets_error(format));
            }
            Decoded::Incomplete => {}
        }
        if !wait
            && !file
                .wait_readable(Some(0))
                .map_err(|e| EgclError::StreamError(format!("read-char-no-hang: {e}")))?
        {
            return Ok(NIL);
        }
        let remaining = *buf_fill - *buf_pos;
        read_buf.copy_within(*buf_pos..*buf_fill, 0);
        read_buf.resize(FILE_BUF_SIZE, 0);
        *buf_pos = 0;
        *buf_fill = remaining;
        let count = file
            .read(&mut read_buf[remaining..])
            .map_err(|e| EgclError::StreamError(format!("file read error: {e}")))?;
        if count == 0 {
            if remaining != 0 {
                return Err(EgclError::StreamError(
                    "incomplete UTF-8 sequence at EOF".into(),
                ));
            }
            return Ok(EOF);
        }
        *buf_fill += count;
    }
}

/// What one decoding attempt at the front of a buffer found.
enum Decoded {
    /// A character, and the octets it occupied.
    Char(char, usize),
    /// The buffer ends inside a character; more input is needed.
    Incomplete,
    /// The octets cannot begin a character of this format.
    Invalid,
}

/// Decode the character at the front of `bytes` under `format`: UTF-8 is
/// tried at every length up to four, LATIN-1 is one octet per character,
/// ASCII one octet below 128. A format this layer does not decode is
/// refused at OPEN, so none reaches here; the UTF-8 arm is the default.
fn decode_char(format: ExternalFormat, bytes: &[u8]) -> Decoded {
    match format {
        ExternalFormat::Latin1 => match bytes.first() {
            Some(&b) => Decoded::Char(b as char, 1),
            None => Decoded::Incomplete,
        },
        ExternalFormat::Ascii => match bytes.first() {
            Some(&b) if b < 0x80 => Decoded::Char(b as char, 1),
            Some(_) => Decoded::Invalid,
            None => Decoded::Incomplete,
        },
        _ => {
            for length in 1..=bytes.len().min(4) {
                match std::str::from_utf8(&bytes[..length]) {
                    Ok(text) => return Decoded::Char(text.chars().next().unwrap(), length),
                    Err(error) if error.error_len().is_some() || length == 4 => {
                        return Decoded::Invalid;
                    }
                    Err(_) => {}
                }
            }
            Decoded::Incomplete
        }
    }
}

/// The STREAM-ERROR for octets that are not a character of `format`.
fn invalid_octets_error(format: ExternalFormat) -> EgclError {
    EgclError::StreamError(match format {
        ExternalFormat::Ascii => "octet above 127 on an ASCII file stream".into(),
        _ => "invalid UTF-8 in file stream".into(),
    })
}

/// Decode a run of octets under `format`, for the bulk line reader.
///
/// The line reader hands this the octets between two `0x0A`s, which is a
/// line only because none of UTF-8, LATIN-1 and ASCII can put `0x0A` inside
/// a character. `OPEN` refuses UTF-16 and UTF-32 for that reason among
/// others: a format that can is not safe to give the line reader, and
/// whoever adds one must route it past the scan, not through it.
fn decode_run(format: ExternalFormat, bytes: Vec<u8>) -> Result<String, EgclError> {
    match format {
        ExternalFormat::Latin1 => Ok(bytes.iter().map(|&b| b as char).collect()),
        ExternalFormat::Ascii => {
            if bytes.iter().all(|&b| b < 0x80) {
                Ok(bytes.iter().map(|&b| b as char).collect())
            } else {
                Err(invalid_octets_error(format))
            }
        }
        _ => String::from_utf8(bytes).map_err(|_| invalid_octets_error(format)),
    }
}

/// Append the octets of `c` under `format` to `out`: one octet for LATIN-1
/// and ASCII, which refuse a character outside their range with a
/// STREAM-ERROR, else UTF-8.
fn encode_char(format: ExternalFormat, c: char, out: &mut Vec<u8>) -> Result<(), EgclError> {
    let (limit, name) = match format {
        ExternalFormat::Latin1 => (0x100, "LATIN-1"),
        ExternalFormat::Ascii => (0x80, "ASCII"),
        _ => {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            return Ok(());
        }
    };
    let code = c as u32;
    if code < limit {
        out.push(code as u8);
        Ok(())
    } else {
        Err(EgclError::StreamError(format!(
            "character U+{code:04X} cannot be encoded as {name}"
        )))
    }
}

/// Read a whole line (up to and consuming a `\n`, or to EOF) from a buffered
/// UTF-8, ASCII or LATIN-1 file input in bulk, instead of decoding and pushing one char at a
/// time (the per-char loop paid a UTF-8 decode + a `Vec<char>` push per byte).
/// The newline byte 0x0A never occurs inside a UTF-8 multi-byte sequence (those
/// are 0x80–0xBF continuation / 0xC0+ leading bytes), nor inside a LATIN-1 or
/// ASCII character, so scanning the raw buffer for 0x0A splits exactly at line
/// boundaries; the bytes between boundaries are therefore complete characters
/// and decode as one `String`. That holds ONLY for those three formats: a
/// UTF-16 or UTF-32 stream carries 0x0A inside other characters, and today
/// `OPEN` refuses both, which is what keeps this scan correct. A future format
/// with that property must not reach this function (see `stream_read_line`'s
/// guard and `decode_run`). Returns `(line, missing-newline-p)`; `(EOF, true)`
/// at end of input with nothing buffered.
#[allow(clippy::too_many_arguments)]
fn file_read_line_buffered(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    line: &mut u64,
    col: &mut u64,
    unread: &mut Option<char>,
    format: ExternalFormat,
) -> Result<(EgclVal, bool), EgclError> {
    let mut line_bytes: Vec<u8> = Vec::new();
    // A pushed-back (unread) char is logically the first char of the line.
    if let Some(c) = unread.take() {
        if c == '\n' {
            *line += 1;
            *col = 0;
            return Ok((make_lisp_string_fresh(""), false));
        }
        encode_char(format, c, &mut line_bytes)?;
    }
    let decode = |bytes: Vec<u8>| -> Result<String, EgclError> { decode_run(format, bytes) };
    loop {
        if *buf_pos >= *buf_fill {
            read_buf.resize(FILE_BUF_SIZE, 0);
            let n = file
                .read(&mut read_buf[..])
                .map_err(|e| EgclError::StreamError(format!("file read error: {}", e)))?;
            if n == 0 {
                if line_bytes.is_empty() {
                    return Ok((EOF, true));
                }
                let s = decode(line_bytes)?;
                *col += s.chars().count() as u64;
                return Ok((make_lisp_string_fresh(&s), true));
            }
            *buf_pos = 0;
            *buf_fill = n;
        }
        let slice = &read_buf[*buf_pos..*buf_fill];
        match slice.iter().position(|&b| b == b'\n') {
            Some(idx) => {
                line_bytes.extend_from_slice(&slice[..idx]);
                *buf_pos += idx + 1; // consume through the newline
                *line += 1;
                *col = 0;
                let s = decode(line_bytes)?;
                return Ok((make_lisp_string_fresh(&s), false));
            }
            None => {
                line_bytes.extend_from_slice(slice);
                *buf_pos = *buf_fill;
            }
        }
    }
}

fn stream_io_error(operation: &str, error: std::io::Error) -> EgclError {
    let message = format!("{operation}: {error}");
    if error.kind() == std::io::ErrorKind::TimedOut {
        EgclError::IoTimeout { stream: NIL, message }
    } else {
        EgclError::StreamError(message)
    }
}

fn attach_timeout_stream(error: &mut EgclError, stream: EgclVal) {
    if let EgclError::IoTimeout { stream: origin, .. } = error {
        if origin.is_nil() {
            *origin = stream;
        }
    }
}

/// Read a single raw byte from a buffered file input (for binary streams). Issue #10.
impl StreamMutableState {
    /// A duplicate of the descriptor an input park would wait on, with the read
    /// timeout budget to wait against.
    ///
    /// Duplicated so a concurrent CLOSE cannot recycle the descriptor while the
    /// park is running (`stream_wait_for_input` duplicates it for the same
    /// reason). `None` when this stream cannot park outside the mutex, in which
    /// case the caller must not retry.
    fn parking_handle(&self) -> Option<(StreamHandle, Option<std::time::Duration>)> {
        let file = match &self.inner {
            StreamInner::FileIo { file, .. } | StreamInner::FileInput { file, .. } => file,
            _ => return None,
        };
        if !file.parks_outside_mutex() {
            return None;
        }
        let budget = file.read_timeout_budget();
        file.try_clone().ok().map(|handle| (handle, budget))
    }

    /// `stream_read_char` as one non-parking attempt.
    ///
    /// Mirrors `stream_read_byte_step`: the direct descriptor arms use the
    /// decoder that reports `WouldBlock`, every other arm keeps its existing
    /// behaviour and answers `Done`. The FileIo arm flushes pending output
    /// first, as the parking version does, so an interactive stream still sees
    /// its prompt before the read waits.
    fn stream_read_char_step(&mut self) -> Result<InputStep<EgclVal>, EgclError> {
        self.check_input()?;
        match &mut self.inner {
            StreamInner::FileInput {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                line,
                col,
                unread,
                external_format,
                ..
            } => file_read_char_without_parking(
                file, read_buf, buf_pos, buf_fill, line, col, unread, *external_format,
            ),
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                write_buf,
                line,
                col,
                unread,
                external_format,
                ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                file_read_char_without_parking(
                    file, read_buf, buf_pos, buf_fill, line, col, unread, *external_format,
                )
            }
            _ => self.stream_read_char().map(InputStep::Done),
        }
    }

    /// `stream_read_byte` as one non-parking attempt.
    ///
    /// The direct descriptor arms use the refill that reports `WouldBlock`, so
    /// the caller can park with the state mutex released. Every other arm --
    /// composites, string streams, Gray streams, wide element types -- keeps
    /// its existing behaviour and answers `Done`, so this is a no-op for them.
    fn stream_read_byte_step(&mut self) -> Result<InputStep<EgclVal>, EgclError> {
        self.check_input()?;
        if let StreamElementType::Integer { bits, signed } = self.element_type {
            return self.read_integer_element(bits, signed).map(InputStep::Done);
        }
        let byte_stream = self.element_type == StreamElementType::UnsignedByte8;
        match &mut self.inner {
            StreamInner::FileInput {
                element_type,
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if *element_type == StreamElementType::UnsignedByte8 => {
                file_read_byte_without_parking(file, read_buf, buf_pos, buf_fill)
            }
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if byte_stream => {
                file_read_byte_without_parking(file, read_buf, buf_pos, buf_fill)
            }
            _ => self.stream_read_byte().map(InputStep::Done),
        }
    }

    /// `read_octets` as one non-parking attempt, appending into `out`.
    ///
    /// Mirrors `read_octets`: the direct-descriptor byte arms go through the
    /// bulk helper that reports `WouldBlock`, everything else keeps the
    /// per-element semantics of `stream_read_byte` -- via its `_step` twin, so
    /// a composite or string stream behind this path parks outside the mutex
    /// too.
    fn read_octets_step(
        &mut self,
        out: &mut Vec<u8>,
        count: usize,
    ) -> Result<InputStep<()>, EgclError> {
        self.check_input()?;
        let byte_stream = self.element_type == StreamElementType::UnsignedByte8;
        match &mut self.inner {
            StreamInner::FileInput {
                element_type,
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if *element_type == StreamElementType::UnsignedByte8 => {
                bulk_read_octets_without_parking(file, read_buf, buf_pos, buf_fill, out, count)
            }
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if byte_stream => {
                bulk_read_octets_without_parking(file, read_buf, buf_pos, buf_fill, out, count)
            }
            _ => {
                while out.len() < count {
                    let byte = match self.stream_read_byte_step()? {
                        InputStep::Done(byte) => byte,
                        InputStep::WouldBlock => return Ok(InputStep::WouldBlock),
                    };
                    if byte == EOF {
                        break;
                    }
                    if !byte.is_fixnum() {
                        return Err(EgclError::TypeError {
                            datum: byte,
                            expected: "an octet from a byte stream".into(),
                        });
                    }
                    let value = byte.as_fixnum();
                    if !(0..=255).contains(&value) {
                        return Err(EgclError::TypeError {
                            datum: byte,
                            expected: "an (unsigned-byte 8) element".into(),
                        });
                    }
                    out.push(value as u8);
                }
                Ok(InputStep::Done(()))
            }
        }
    }
}

/// Refill a byte buffer without parking, reporting `WouldBlock` to the caller.
///
/// The waiting twin of `file_read_byte_raw`: identical bookkeeping, but it asks
/// the transport for a read that will not park, so `with_stream_input` can drop
/// the stream's state mutex and park outside it (bliss-vupo0).
fn file_read_byte_without_parking(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
) -> Result<InputStep<EgclVal>, EgclError> {
    if *buf_pos >= *buf_fill {
        read_buf.resize(FILE_BUF_SIZE, 0);
        let n = match file.read_without_waiting(&mut read_buf[..]) {
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                return Ok(InputStep::WouldBlock);
            }
            Err(e) => return Err(stream_io_error("file read error", e)),
        };
        if n == 0 {
            return Ok(InputStep::Done(EOF));
        }
        *buf_pos = 0;
        *buf_fill = n;
    }
    let byte = read_buf[*buf_pos];
    *buf_pos += 1;
    Ok(InputStep::Done(EgclVal::from_fixnum(byte as i64)))
}

fn file_read_byte_raw(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
) -> Result<EgclVal, EgclError> {
    if *buf_pos >= *buf_fill {
        read_buf.resize(FILE_BUF_SIZE, 0);
        let n = file
            .read(&mut read_buf[..])
            .map_err(|e| stream_io_error("file read error", e))?;
        if n == 0 {
            return Ok(EOF);
        }
        *buf_pos = 0;
        *buf_fill = n;
    }
    let byte = read_buf[*buf_pos];
    *buf_pos += 1;
    Ok(EgclVal::from_fixnum(byte as i64))
}

/// Read up to `count` octets from a buffered byte stream in one go, without
/// parking: `WouldBlock` is reported to the caller instead.
///
/// `file_read_byte_raw` above is the right shape for READ-BYTE and the wrong
/// shape for READ-SEQUENCE: per octet it costs a buffer-bookkeeping branch, and
/// the stream's buffer is only 8 KiB, so a 6.76 MB read also made ~826 refill
/// syscalls. This drains whatever the per-byte path left buffered and then reads
/// straight into the destination in 64 KiB chunks, which is what makes a bulk
/// transfer run at memory speed rather than per-element speed.
///
/// It appends into `out` rather than returning a fresh Vec so the caller can own
/// the accumulator across a park: a partially filled READ-SEQUENCE must not lose
/// what it already transferred, and must not re-read it either.
///
/// `Done` means the request is finished -- `out.len() == count`, or the stream
/// reached end of file and READ-SEQUENCE reports the short count. CLHS requires
/// READ-SEQUENCE to keep going until one of those holds, so a transport with no
/// data yet is `WouldBlock` (park and resume), never a short return.
fn bulk_read_octets_without_parking(
    file: &mut StreamHandle,
    read_buf: &mut [u8],
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    out: &mut Vec<u8>,
    count: usize,
) -> Result<InputStep<()>, EgclError> {
    let buffered = (*buf_fill - *buf_pos).min(count - out.len());
    out.extend_from_slice(&read_buf[*buf_pos..*buf_pos + buffered]);
    *buf_pos += buffered;
    while out.len() < count {
        let base = out.len();
        let chunk = (count - base).min(1 << 16);
        out.resize(base + chunk, 0);
        match file.read_without_waiting(&mut out[base..]) {
            // End of file: READ-SEQUENCE reports how much it transferred and
            // the caller compares that against what it asked for.
            Ok(0) => {
                out.truncate(base);
                return Ok(InputStep::Done(()));
            }
            Ok(n) => out.truncate(base + n),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                out.truncate(base);
                return Ok(InputStep::WouldBlock);
            }
            Err(e) => {
                out.truncate(base);
                return Err(stream_io_error("file read error", e));
            }
        }
    }
    Ok(InputStep::Done(()))
}

/// Write a whole slice to a file, retrying short writes and EINTR.
///
/// The counterpart to `file_flush_write_buf` for a bulk transfer: a 6.8 MB
/// WRITE-SEQUENCE has no reason to be copied through an 8 KiB buffer.
fn write_all_octets(file: &mut impl Write, octets: &[u8]) -> Result<(), EgclError> {
    let mut written = 0;
    loop {
        if written == octets.len() {
            return Ok(());
        }
        match file.write(&octets[written..]) {
            Ok(0) => {
                return Err(EgclError::StreamError(
                    "file write error: wrote zero bytes".into(),
                ));
            }
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(EgclError::StreamError(format!("file write error: {error}"))),
        }
    }
}

/// Advance a stream's line/column counters past OCTETS.
fn advance_line_col(octets: &[u8], line: &mut u64, col: &mut u64) {
    match octets.iter().rposition(|&b| b == b'\n') {
        Some(last) => {
            *line += octets.iter().filter(|&&b| b == b'\n').count() as u64;
            *col = (octets.len() - last - 1) as u64;
        }
        None => *col += octets.len() as u64,
    }
}

/// Flush a write buffer to file.
fn file_flush_write_buf(file: &mut impl Write, write_buf: &mut Vec<u8>) -> Result<(), EgclError> {
    let mut written = 0;
    let result = loop {
        if written == write_buf.len() {
            break Ok(());
        }
        match file.write(&write_buf[written..]) {
            Ok(0) => break Err(std::io::Error::from(std::io::ErrorKind::WriteZero)),
            Ok(count) => written += count,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => break Err(error),
        }
    };
    // A retry must not duplicate the prefix already accepted by the OS.
    // Keep the unwritten suffix even when the error aborts CLOSE or FINISH-OUTPUT.
    write_buf.drain(..written);
    result.map_err(|e| EgclError::StreamError(format!("file write error: {e}")))
}

// ── Helper: extract string as &str for character-index slicing ────

/// The content of a simple string as an owned Rust `String` (decoded from the
/// fixed-width layout). Owned because the on-heap elements are the type's
/// fixed-width chars, not UTF-8 bytes.
fn extract_string_str(val: EgclVal) -> Result<String, EgclError> {
    if !val.is_heap_object() {
        return Err(EgclError::TypeError {
            datum: val,
            expected: "string".into(),
        });
    }
    unsafe {
        let ptr = val.as_ptr();
        let tid = (*(ptr as *const ObjectHeader)).type_id();
        if tid != type_id::SIMPLE_BASE_STRING && tid != type_id::SIMPLE_CHARACTER_STRING {
            return Err(EgclError::TypeError {
                datum: val,
                expected: "string".into(),
            });
        }
        Ok(egcl_rt::object::read_simple_string(ptr))
    }
}

// ── GrayStream implementation on StreamMutableState ────────────────

impl GrayStream for StreamMutableState {
    fn stream_read_char(&mut self) -> Result<EgclVal, EgclError> {
        self.check_input()?;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::StringInput {
                chars,
                position,
                end,
                line,
                col,
                unread,
            } => {
                if let Some(c) = unread.take() {
                    return Ok(EgclVal::from_char(c));
                }
                if *position >= *end {
                    return Ok(EOF);
                }
                let c = chars[*position];
                *position += 1;
                if c == '\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                Ok(EgclVal::from_char(c))
            }
            StreamInner::FileInput {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                line,
                col,
                unread,
                external_format,
                ..
            } => file_read_char_buffered(
                file, read_buf, buf_pos, buf_fill, line, col, unread, true, *external_format,
            ),
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                write_buf,
                line,
                col,
                unread,
                external_format,
                ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                file_read_char_buffered(
                    file, read_buf, buf_pos, buf_fill, line, col, unread, true, *external_format,
                )
            }
            // Issue #1: Handle TwoWay and Echo as separate arms to avoid
            // borrow-checker conflict when reading from input then writing to output.
            StreamInner::TwoWay => {
                let inp = comps[0];
                crate::streams::stream_read_char(inp)
            }
            StreamInner::Echo => {
                let inp = comps[0];
                let result = crate::streams::stream_read_char(inp)?;
                if result != EOF {
                    crate::streams::stream_write_char(comps[1], result)?;
                }
                Ok(result)
            }
            StreamInner::Concatenated { cursor } => {
                while *cursor < comps.len() {
                    let s = comps[*cursor];
                    let result = crate::streams::stream_read_char(s)?;
                    if result != EOF {
                        return Ok(result);
                    }
                    *cursor += 1;
                }
                Ok(EOF)
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_read_char(target)
            }
            StreamInner::Stdin {
                unread, line, col, ..
            } => {
                if let Some(c) = unread.take() {
                    return Ok(EgclVal::from_char(c));
                }
                let ch = read_char_from_stdin()?;
                if ch != EOF {
                    if ch.as_char() == '\n' {
                        *line += 1;
                        *col = 0;
                    } else {
                        *col += 1;
                    }
                }
                Ok(ch)
            }
            _ => Err(EgclError::StreamError("not an input stream".into())),
        }
    }

    fn stream_unread_char(&mut self, ch: EgclVal) -> Result<(), EgclError> {
        self.check_input()?;
        let c = ch.as_char();
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::StringInput {
                unread, line, col, ..
            } => {
                *unread = Some(c);
                if c == '\n' {
                    // Can't perfectly restore col after newline unread, but decrement line
                    if *line > 0 {
                        *line -= 1;
                    }
                    // col is unknown after unreading a newline; leave as-is (best effort)
                } else {
                    if *col > 0 {
                        *col -= 1;
                    }
                }
                Ok(())
            }
            StreamInner::FileInput {
                unread, line, col, ..
            }
            | StreamInner::FileIo {
                unread, line, col, ..
            } => {
                *unread = Some(c);
                if c == '\n' {
                    if *line > 0 {
                        *line -= 1;
                    }
                } else {
                    if *col > 0 {
                        *col -= 1;
                    }
                }
                Ok(())
            }
            StreamInner::TwoWay | StreamInner::Echo => {
                crate::streams::stream_unread_char(comps[0], ch)
            }
            StreamInner::Concatenated { cursor } => {
                if let Some(&s) = comps.get(*cursor) {
                    crate::streams::stream_unread_char(s, ch)
                } else {
                    Err(EgclError::StreamError(
                        "no streams in concatenated stream".into(),
                    ))
                }
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_unread_char(target, ch)
            }
            StreamInner::Stdin {
                unread, line, col, ..
            } => {
                *unread = Some(c);
                if c == '\n' {
                    if *line > 0 {
                        *line -= 1;
                    }
                } else if *col > 0 {
                    *col -= 1;
                }
                Ok(())
            }
            _ => Err(EgclError::StreamError("not an input stream".into())),
        }
    }

    // Issue #10: For binary file streams, read a single octet (0-255).
    // For character-based streams, read a character and return its codepoint.
    fn stream_read_byte(&mut self) -> Result<EgclVal, EgclError> {
        self.check_input()?;
        if let StreamElementType::Integer { bits, signed } = self.element_type {
            return self.read_integer_element(bits, signed);
        }
        let byte_stream = self.element_type == StreamElementType::UnsignedByte8;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::FileInput {
                element_type,
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if *element_type == StreamElementType::UnsignedByte8 => {
                file_read_byte_raw(file, read_buf, buf_pos, buf_fill)
            }
            // Bidirectional byte stream (e.g. a TCP socket connection): read raw
            // octets from the buffered fd rather than decoding characters.
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                ..
            } if byte_stream => file_read_byte_raw(file, read_buf, buf_pos, buf_fill),
            // Composite streams read bytes from their binary components (ansi
            // make-two-way-stream.12/.13, make-concatenated-stream.22).
            StreamInner::TwoWay => crate::streams::stream_read_byte(comps[0]),
            StreamInner::Echo => {
                let mut byte = crate::streams::stream_read_byte(comps[0])?;
                // Root BYTE across the echo write. On a wide element type the
                // value is a heap bignum, and the write allocates (buffering,
                // and the component's own element encoding), so a relocating GC
                // would leave this copy stale -- and it is the value returned to
                // the caller. stream_write_byte roots its OWN copy, not this one.
                egcl_rt::rooted_ref!(_byte_root = &mut byte);
                if byte != EOF {
                    crate::streams::stream_write_byte(comps[1], byte)?;
                }
                Ok(byte)
            }
            StreamInner::Concatenated { cursor } => {
                while *cursor < comps.len() {
                    let result = crate::streams::stream_read_byte(comps[*cursor])?;
                    if result != EOF {
                        return Ok(result);
                    }
                    *cursor += 1;
                }
                Ok(EOF)
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_read_byte(target)
            }
            _ => {
                // Character stream fallback: read char, return codepoint.
                let ch = self.stream_read_char()?;
                if ch == EOF {
                    Ok(EOF)
                } else {
                    Ok(EgclVal::from_fixnum(ch.as_char() as i64))
                }
            }
        }
    }

    fn stream_write_char(&mut self, ch: EgclVal) -> Result<(), EgclError> {
        self.check_output()?;
        let c = ch.as_char();
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::StringOutput { buffer, line, col } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                buffer.extend_from_slice(encoded.as_bytes());
                if c == '\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                Ok(())
            }
            StreamInner::FileOutput {
                file,
                write_buf,
                line,
                col,
                external_format,
                ..
            } => {
                encode_char(*external_format, c, write_buf)?;
                if c == '\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::FileIo {
                file,
                write_buf,
                line,
                col,
                external_format,
                ..
            } => {
                encode_char(*external_format, c, write_buf)?;
                if c == '\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::Broadcast => {
                // GC can update the rooted vector during a child call; do not
                // retain an iterator borrow across that call.
                #[allow(clippy::needless_range_loop)]
                for index in 0..comps.len() {
                    let s = comps[index];
                    crate::streams::stream_write_char(s, ch)?;
                }
                Ok(())
            }
            StreamInner::TwoWay | StreamInner::Echo => {
                crate::streams::stream_write_char(comps[1], ch)
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_write_char(target, ch)
            }
            StreamInner::Stdout { line, col } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_to_std(false, encoded.as_bytes())?;
                track_col(c, line, col);
                Ok(())
            }
            StreamInner::Stderr { line, col } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_to_std(true, encoded.as_bytes())?;
                track_col(c, line, col);
                Ok(())
            }
            _ => Err(EgclError::StreamError("not an output stream".into())),
        }
    }

    fn stream_write_byte(&mut self, byte: EgclVal) -> Result<(), EgclError> {
        self.check_output()?;
        if let StreamElementType::Integer { bits, signed } = self.element_type {
            return self.write_integer_element(byte, bits, signed);
        }
        // A composite stream has no element type of its own, so it must hand the
        // value to its components and let each validate against ITS element
        // type. Checking for an octet here first rejected
        // (write-byte 256 (make-broadcast-stream s)) even when S was an
        // (unsigned-byte 16) stream that accepts 256 -- SBCL writes it.
        {
            let mut comps = self.components().to_vec();
            egcl_rt::rooted_ref!(_components_root = &mut comps);
            match &self.inner {
                StreamInner::Broadcast => {
                    // GC can update the rooted vector during a child call, so do
                    // not hold an iterator borrow across one.
                    #[allow(clippy::needless_range_loop)]
                    for index in 0..comps.len() {
                        let s = comps[index];
                        crate::streams::stream_write_byte(s, byte)?;
                    }
                    return Ok(());
                }
                StreamInner::TwoWay | StreamInner::Echo => {
                    return crate::streams::stream_write_byte(comps[1], byte);
                }
                StreamInner::Synonym => {
                    let target = resolve_synonym(comps[0])?;
                    return crate::streams::stream_write_byte(target, byte);
                }
                _ => {}
            }
        }
        // CLHS WRITE-BYTE: a non-integer (or one outside the element type) is a
        // TYPE-ERROR the program can handle — never a host panic.
        if !byte.is_fixnum() {
            return Err(EgclError::TypeError {
                datum: byte,
                expected: "(UNSIGNED-BYTE 8)".into(),
            });
        }
        let value = byte.as_fixnum();
        if !(0..=255).contains(&value) {
            return Err(EgclError::TypeError {
                datum: byte,
                expected: "(UNSIGNED-BYTE 8)".into(),
            });
        }
        let b = value as u8;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::StringOutput { buffer, line, col } => {
                buffer.push(b);
                if b == b'\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                Ok(())
            }
            StreamInner::FileOutput {
                file,
                write_buf,
                line,
                col,
                ..
            } => {
                write_buf.push(b);
                if b == b'\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::FileIo {
                file,
                write_buf,
                line,
                col,
                ..
            } => {
                write_buf.push(b);
                if b == b'\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            // Broadcast / TwoWay / Echo / Synonym are handled above, before the
            // octet check, so that each component validates for itself.
            StreamInner::Stdout { line, col } => {
                write_to_std(false, &[b])?;
                track_col(b as char, line, col);
                Ok(())
            }
            StreamInner::Stderr { line, col } => {
                write_to_std(true, &[b])?;
                track_col(b as char, line, col);
                Ok(())
            }
            _ => Err(EgclError::StreamError("not an output stream".into())),
        }
    }

    // Issue #11: start/end are character indices, not byte indices.
    fn stream_write_string(
        &mut self,
        string: EgclVal,
        start: usize,
        end: Option<usize>,
    ) -> Result<(), EgclError> {
        let mut string = string;
        egcl_rt::rooted_ref!(_string_root = &mut string);
        self.check_output()?;
        // Extract string as &str so we can index by character position.
        let s = extract_string_str(string)?;
        let char_count = s.chars().count();
        let actual_end = end.unwrap_or(char_count);
        // Compute byte range from character indices.
        let byte_start = s
            .char_indices()
            .nth(start)
            .map(|(i, _)| i)
            .unwrap_or(s.len());
        let byte_end = if actual_end >= char_count {
            s.len()
        } else {
            s.char_indices()
                .nth(actual_end)
                .map(|(i, _)| i)
                .unwrap_or(s.len())
        };
        let slice = &s.as_bytes()[byte_start..byte_end];
        let str_slice = &s[byte_start..byte_end];
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::StringOutput { buffer, line, col } => {
                buffer.extend_from_slice(slice);
                for c in str_slice.chars() {
                    if c == '\n' {
                        *line += 1;
                        *col = 0;
                    } else {
                        *col += 1;
                    }
                }
                Ok(())
            }
            StreamInner::FileOutput {
                file,
                write_buf,
                line,
                col,
                external_format,
                ..
            } => {
                if *external_format == ExternalFormat::Utf8 {
                    write_buf.extend_from_slice(slice);
                } else {
                    for c in str_slice.chars() {
                        encode_char(*external_format, c, write_buf)?;
                    }
                }
                for c in str_slice.chars() {
                    if c == '\n' {
                        *line += 1;
                        *col = 0;
                    } else {
                        *col += 1;
                    }
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::FileIo {
                file,
                write_buf,
                line,
                col,
                external_format,
                ..
            } => {
                if *external_format == ExternalFormat::Utf8 {
                    write_buf.extend_from_slice(slice);
                } else {
                    for c in str_slice.chars() {
                        encode_char(*external_format, c, write_buf)?;
                    }
                }
                for c in str_slice.chars() {
                    if c == '\n' {
                        *line += 1;
                        *col = 0;
                    } else {
                        *col += 1;
                    }
                }
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::Broadcast => {
                // GC can update the rooted vector during a child call; do not
                // retain an iterator borrow across that call.
                #[allow(clippy::needless_range_loop)]
                for index in 0..comps.len() {
                    let s = comps[index];
                    crate::streams::stream_write_string(s, string, start, Some(actual_end))?;
                }
                Ok(())
            }
            StreamInner::TwoWay | StreamInner::Echo => {
                crate::streams::stream_write_string(comps[1], string, start, Some(actual_end))
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_write_string(target, string, start, Some(actual_end))
            }
            StreamInner::Stdout { line, col } => {
                write_to_std(false, slice)?;
                for c in str_slice.chars() {
                    track_col(c, line, col);
                }
                Ok(())
            }
            StreamInner::Stderr { line, col } => {
                write_to_std(true, slice)?;
                for c in str_slice.chars() {
                    track_col(c, line, col);
                }
                Ok(())
            }
            _ => Err(EgclError::StreamError("not an output stream".into())),
        }
    }

    fn stream_force_output(&mut self) -> Result<(), EgclError> {
        self.check_open()?;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::FileOutput {
                file, write_buf, ..
            }
            | StreamInner::FileIo {
                file, write_buf, ..
            } => file_flush_write_buf(file, write_buf),
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_force_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_finish_output(&mut self) -> Result<(), EgclError> {
        self.check_open()?;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::FileOutput {
                file, write_buf, ..
            }
            | StreamInner::FileIo {
                file, write_buf, ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                file.flush()
                    .map_err(|e| EgclError::StreamError(format!("flush error: {}", e)))
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_finish_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_clear_input(&mut self) -> Result<(), EgclError> {
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::FileInput {
                buf_pos,
                buf_fill,
                unread,
                ..
            }
            | StreamInner::FileIo {
                buf_pos,
                buf_fill,
                unread,
                ..
            } => {
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                Ok(())
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_clear_input(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_listen(&self) -> Result<bool, EgclError> {
        self.check_open()?;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &self.inner {
            StreamInner::StringInput {
                position,
                end,
                unread,
                ..
            } => Ok(unread.is_some() || *position < *end),
            StreamInner::FileInput {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            }
            | StreamInner::FileIo {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            } => {
                if unread.is_some() || *buf_pos < *buf_fill {
                    return Ok(true);
                }
                file.socket_has_input()
                    .map_err(|e| EgclError::StreamError(format!("listen: {e}")))
            }
            StreamInner::Concatenated { cursor } => Ok(*cursor < comps.len()),
            StreamInner::TwoWay | StreamInner::Echo => crate::streams::stream_listen(comps[0]),
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_listen(target)
            }
            StreamInner::Stdin { unread, .. } => Ok(unread.is_some()),
            _ => Ok(false),
        }
    }

    fn stream_line_number(&self) -> Option<u64> {
        match &self.inner {
            StreamInner::StringInput { line, .. } => Some(*line),
            StreamInner::StringOutput { line, .. } => Some(*line),
            StreamInner::FileInput { line, .. } | StreamInner::FileIo { line, .. } => Some(*line),
            StreamInner::FileOutput { line, .. } => Some(*line),
            StreamInner::Stdin { line, .. }
            | StreamInner::Stdout { line, .. }
            | StreamInner::Stderr { line, .. } => Some(*line),
            _ => None,
        }
    }

    fn stream_line_column(&self) -> Option<u64> {
        match &self.inner {
            StreamInner::StringInput { col, .. } => Some(*col),
            StreamInner::StringOutput { col, .. } => Some(*col),
            StreamInner::FileInput { col, .. } | StreamInner::FileIo { col, .. } => Some(*col),
            StreamInner::FileOutput { col, .. } => Some(*col),
            StreamInner::Stdin { col, .. }
            | StreamInner::Stdout { col, .. }
            | StreamInner::Stderr { col, .. } => Some(*col),
            _ => None,
        }
    }

    // ── R5.113 additional Gray protocol methods ──────────────────────

    fn stream_read_char_no_hang(&mut self) -> Result<EgclVal, EgclError> {
        self.check_input()?;
        if let StreamInner::FileIo {
            file,
            read_buf,
            buf_pos,
            buf_fill,
            line,
            col,
            unread,
            external_format,
            ..
        }
        | StreamInner::FileInput {
            file,
            read_buf,
            buf_pos,
            buf_fill,
            line,
            col,
            unread,
            external_format,
            ..
        } = &mut self.inner
        {
            if matches!(file, StreamHandle::Socket(_) | StreamHandle::Pipe(_)) {
                // Do not flush pending writes: a NO-HANG input call must not
                // block on a peer that has stopped reading our output.
                return file_read_char_buffered(
                    file, read_buf, buf_pos, buf_fill, line, col, unread, false, *external_format,
                );
            }
        }
        // For string streams, if exhausted return NIL -- but a character pushed
        // back with UNREAD-CHAR is available even at the end of the string, so
        // it must be consulted first. Ignoring it made NO-HANG (and so LISTEN)
        // answer "nothing available" for a character the caller had just
        // unread, which is ansi streams listen.6.
        if let StreamInner::StringInput {
            position,
            end,
            unread,
            ..
        } = &self.inner
        {
            if unread.is_none() && *position >= *end {
                return Ok(NIL);
            }
        }
        // The process standard input carries no handle of its own, so ask the
        // descriptor whether a read would block. When stdin is a pipe, socket or
        // terminal with nothing buffered yet, NO-HANG must answer NIL instead of
        // waiting for a character that may never come.
        if let StreamInner::Stdin { unread, .. } = &self.inner {
            if unread.is_none() && !stdin_read_would_not_block()? {
                return Ok(NIL);
            }
        }
        // For file streams (FileInput, FileIo) and all others, read_char never blocks
        // on regular files — attempt the read directly.
        self.stream_read_char()
    }

    fn stream_peek_char(&mut self) -> Result<EgclVal, EgclError> {
        self.check_input()?;
        let ch = self.stream_read_char()?;
        if ch != EOF {
            self.stream_unread_char(ch)?;
        }
        Ok(ch)
    }

    fn stream_read_line(&mut self) -> Result<(EgclVal, bool), EgclError> {
        self.check_input()?;
        // Bulk path for buffered UTF-8/ASCII character file input: scan the read
        // buffer for the newline byte and decode the run in one shot, rather than
        // decoding + pushing one char at a time. Other variants (string streams,
        // byte/other-encoding, composites) keep the generic per-char loop below.
        if let StreamInner::FileInput {
            file,
            read_buf,
            buf_pos,
            buf_fill,
            line,
            col,
            unread,
            external_format,
            element_type,
        } = &mut self.inner
        {
            if *element_type == StreamElementType::Character
                && matches!(
                    external_format,
                    ExternalFormat::Utf8 | ExternalFormat::Ascii | ExternalFormat::Latin1
                )
            {
                return file_read_line_buffered(
                    file, read_buf, buf_pos, buf_fill, line, col, unread, *external_format,
                );
            }
        }
        let mut chars = Vec::new();
        loop {
            let ch = self.stream_read_char()?;
            if ch == EOF {
                if chars.is_empty() {
                    return Ok((EOF, true));
                }
                let s: String = chars.into_iter().collect();
                return Ok((make_lisp_string_fresh(&s), true));
            }
            let c = ch.as_char();
            if c == '\n' {
                let s: String = chars.into_iter().collect();
                return Ok((make_lisp_string_fresh(&s), false));
            }
            chars.push(c);
        }
    }

    fn stream_terpri(&mut self) -> Result<(), EgclError> {
        self.stream_write_char(EgclVal::from_char('\n'))
    }

    fn stream_fresh_line(&mut self) -> Result<bool, EgclError> {
        self.check_output()?;
        // Nothing has been written to a broadcast stream with no components,
        // so FRESH-LINE has nothing to do (ansi make-broadcast-stream.4).
        if matches!(self.inner, StreamInner::Broadcast) && self.components().is_empty() {
            return Ok(false);
        }
        if !self.stream_start_line_p() {
            self.stream_terpri()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn stream_clear_output(&mut self) -> Result<(), EgclError> {
        self.check_open()?;
        let mut comps = self.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut self.inner {
            StreamInner::FileOutput { write_buf, .. } | StreamInner::FileIo { write_buf, .. } => {
                write_buf.clear();
                Ok(())
            }
            StreamInner::StringOutput { buffer, .. } => {
                buffer.clear();
                Ok(())
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_clear_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_advance_to_column(&mut self, target_col: u64) -> Result<bool, EgclError> {
        self.check_output()?;
        let current = self.stream_line_column().unwrap_or(0);
        if current >= target_col {
            return Ok(false);
        }
        for _ in current..target_col {
            self.stream_write_char(EgclVal::from_char(' '))?;
        }
        Ok(true)
    }

    fn stream_start_line_p(&self) -> bool {
        self.stream_line_column() == Some(0)
    }

    fn stream_read_sequence(&mut self, count: usize) -> Result<Vec<EgclVal>, EgclError> {
        self.check_input()?;
        let mut result = Vec::with_capacity(count);
        egcl_rt::rooted_ref!(_result_root = &mut result);
        for _ in 0..count {
            let ch = self.stream_read_char()?;
            if ch == EOF {
                break;
            }
            result.push(ch);
        }
        drop(_result_root);
        Ok(result)
    }

    fn stream_write_sequence(&mut self, elements: &[EgclVal]) -> Result<(), EgclError> {
        self.check_output()?;
        for &elem in elements {
            if elem.is_character() {
                self.stream_write_char(elem)?;
            } else if elem.is_fixnum() || egcl_rt::bignum::bigint_from_val(elem).is_some() {
                self.stream_write_byte(elem)?;
            } else {
                return Err(EgclError::StreamError(
                    "invalid element in write-sequence".into(),
                ));
            }
        }
        Ok(())
    }

    fn interactive_stream_p(&self) -> bool {
        // File streams connected to a terminal could be interactive;
        // string/composite streams are never interactive. The process
        // standard streams are treated as interactive.
        matches!(
            self.inner,
            StreamInner::Stdin { .. } | StreamInner::Stdout { .. } | StreamInner::Stderr { .. }
        )
    }

    fn stream_external_format(&self) -> ExternalFormat {
        match &self.inner {
            StreamInner::FileInput {
                external_format, ..
            } => *external_format,
            StreamInner::FileOutput {
                external_format, ..
            } => *external_format,
            StreamInner::FileIo {
                external_format, ..
            } => *external_format,
            _ => ExternalFormat::Utf8,
        }
    }
}

// ── String allocation ─────────────────────────────────────────────

// The interned-string table is a main-thread hot map during a large load: every
// make_lisp_string hashes its bytes here, and SipHash over the byte-string keys
// plus grow-by-rehash was a large share of babel load (bliss-pohq). Use FxHash +
// a generous pre-size so it neither SipHashes nor rehashes during a load.
type InternTable = HashMap<Vec<u8>, EgclVal, egcl_rt::fxhash::FxBuildHasher>;

fn string_intern_table() -> &'static OrderedMutex<InternTable> {
    static TABLE: OnceLock<OrderedMutex<InternTable>> = OnceLock::new();
    TABLE.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::InternedString,
            1,
            "interned string table",
            InternTable::with_capacity_and_hasher(16384, Default::default()),
        )
    })
}

/// Create a EgclVal representing a Lisp string.
/// Strings are interned so equal content produces equal EgclVal.
/// For mutable or identity-sensitive strings, use `make_lisp_string_fresh`.
///
/// An interned string lives off the GC heap and is NEVER reclaimed, so use
/// this only for a bounded set of texts (names, constants). A string built from
/// program data -- FORMAT output, a line read, a printed object -- must be
/// fresh, or every distinct value leaks (bliss-4due).
pub fn make_lisp_string(s: &str) -> EgclVal {
    let bytes = s.as_bytes();
    let mut table = string_intern_table().lock().unwrap();
    if let Some(&val) = table.get(bytes) {
        return val;
    }
    let ptr = alloc_string_object(s);
    let val = unsafe { EgclVal::from_heap_ptr(ptr) };
    table.insert(bytes.to_vec(), val);
    val
}

/// Number of entries in the interned-string table. Interned strings live off
/// the GC heap and are never reclaimed, so this must stay bounded by the
/// program's distinct constant texts, not grow with the data it processes
/// (bliss-4due).
pub fn interned_string_count() -> usize {
    string_intern_table().lock().unwrap().len()
}

/// Create a fresh (non-interned) EgclVal string.
/// Two calls with the same content produce distinct EgclVal objects,
/// preserving identity semantics for mutable strings. Issue #6.
pub fn make_lisp_string_fresh(s: &str) -> EgclVal {
    // Allocate ON the GC heap (not the historical off-heap std::alloc): an
    // off-heap fresh string is invisible to the core-image heap snapshot and is
    // not carried by the intern-table image mechanism either, so every reference
    // to one dangled after image restore — crashing (asdf:load-system :babel) in
    // a saved image when a type predicate read the dead string's header
    // (bliss-tmbg). A heap string rides the snapshot and relocates normally. It
    // MOVES under GC like any reader string, so callers root it across later
    // allocations — the same contract they already honour for SUBSEQ/REVERSE/
    // COPY-SEQ's vector/list results.
    egcl_rt::gc::alloc_character_string(s)
}

/// A fresh 8-bit `SIMPLE-BASE-STRING` — what `(make-string n :element-type
/// 'base-char)`, `(map 'base-string ...)` and `(coerce x 'base-string)` must
/// build, since TYPEP recognises a BASE-STRING by its storage width
/// (bliss-ajb3) and a base string never promotes (spec §1.6.3). The caller has
/// checked that every character is a BASE-CHAR (code point below 256). Same GC
/// contract as [`make_lisp_string_fresh`]: it moves, so root it.
pub fn make_lisp_base_string_fresh(s: &str) -> EgclVal {
    debug_assert!(
        egcl_rt::object::is_all_base_char(s),
        "a base string holds code points below 256"
    );
    let bytes: Vec<u8> = s.chars().map(|c| c as u8).collect();
    egcl_rt::gc::alloc_base_string_bytes(&bytes)
}

/// A fresh string of the same width as `like`: an 8-bit base string when
/// `like` is one, else the 32-bit CHARACTER string. COPY-SEQ, SUBSEQ and
/// REVERSE keep a string's element type (ansi COPY-SEQ.20, REVERSE-STRING.5
/// and the REMOVE family's string cases compare ARRAY-ELEMENT-TYPE of the
/// result with the input's), which the width-blind fresh constructor lost once
/// ARRAY-ELEMENT-TYPE started telling the two widths apart. The characters of
/// `s` normally come from `like` and so fit; the base-char check covers a
/// caller that widened one.
pub fn make_lisp_string_fresh_like(like: EgclVal, s: &str) -> EgclVal {
    // `string_storage_is_base` looks through a displaced or adjustable string
    // to the storage that holds its characters, the same way
    // ARRAY-ELEMENT-TYPE does, so a copy of a string displaced onto a base
    // string is a base string too (ansi COPY-SEQ.20 and REVERSE-STRING.5 run
    // over displaced strings as well as simple ones).
    if !crate::pathnames::is_registered_string(like)
        && like.is_heap_object()
        && crate::sequences::string_storage_is_base(like)
        && egcl_rt::object::is_all_base_char(s)
    {
        make_lisp_base_string_fresh(s)
    } else {
        make_lisp_string_fresh(s)
    }
}

// ── Core-image serialization of interned strings (bliss-x0f2 off-heap M3) ──
//
// `make_lisp_string` allocates OFF the GC heap, so an interned string never
// rides the heap image section — yet references to it sit anywhere a value
// can: hash-table slots, cons cells, CLOS slots, symbol plists. Carrying the
// intern table by content and folding each (old, new) header pair into the
// reloc map lets Pass 2 rewrite every such reference like an on-heap one —
// including ones buried inside structures (the cons-key case that crashed
// EQUAL hashing during a babel-core restore). `make_lisp_string_fresh`
// strings are NOT in the table and still cannot ride; converging stdlib
// strings onto real heap objects (bliss-jtc.2) retires that gap for good.

/// Serialize the interned-string table for a core image. Reads off-heap
/// content only (no EGCL allocation) — GC-safe post-STW-GC.
pub fn serialize_interned_strings() -> Vec<u8> {
    let mut out = Vec::new();
    let entries: Vec<(Vec<u8>, u64)> = {
        let table = string_intern_table().lock().unwrap();
        table
            .iter()
            .map(|(bytes, val)| (bytes.clone(), unsafe { val.as_ptr() } as u64))
            .collect()
    };
    out.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for (bytes, old_header) in entries {
        out.extend_from_slice(&old_header.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

thread_local! {
    /// (content bytes, restored string object) records between the allocate
    /// and populate phases of a core restore.
    static PENDING_INTERNED: std::cell::RefCell<Vec<(Vec<u8>, EgclVal)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Restore phase 1: re-create each saved string object and return
/// (old_header, new_header) pairs for the reloc-map fold. Runs UNDER the heap
/// lock, so it must not take the intern-table lock (lock order: InternedString
/// sits below the GC heap) — table registration is deferred to
/// [`populate_interned_strings`]. GC-safe: only `std::alloc` allocation.
pub fn allocate_interned_strings(data: &[u8]) -> Vec<(usize, usize)> {
    let mut pairs = Vec::new();
    let mut off = 0usize;
    if data.len() < 4 {
        return pairs;
    }
    let n = u32::from_le_bytes(data[..4].try_into().unwrap()) as usize;
    off += 4;
    for _ in 0..n {
        if data.len() < off + 12 {
            break;
        }
        let old_header = u64::from_le_bytes(data[off..off + 8].try_into().unwrap());
        off += 8;
        let len = u32::from_le_bytes(data[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        if data.len() < off + len {
            break;
        }
        let bytes = data[off..off + len].to_vec();
        let s = String::from_utf8_lossy(&bytes).into_owned();
        off += len;
        let val = unsafe { EgclVal::from_heap_ptr(alloc_string_object(&s)) };
        let new_header = unsafe { val.as_ptr() } as usize;
        PENDING_INTERNED.with(|p| p.borrow_mut().push((bytes, val)));
        pairs.push((old_header as usize, new_header));
    }
    pairs
}

/// Restore phase 2 (heap lock released): register the restored strings in the
/// intern table so future `make_lisp_string` calls with the same content
/// return the SAME object references remapped to. A string the fresh process
/// interned before the load keeps its table slot (the restored object still
/// exists and every remapped reference is valid; only EQ-dedup with the
/// pre-load object is lost — content equality holds either way).
pub fn populate_interned_strings() {
    let pending = PENDING_INTERNED.with(|p| std::mem::take(&mut *p.borrow_mut()));
    if pending.is_empty() {
        return;
    }
    let mut table = string_intern_table().lock().unwrap();
    for (bytes, val) in pending {
        table.entry(bytes).or_insert(val);
    }
}

fn alloc_string_object(s: &str) -> *mut u8 {
    // A constructed simple string is a 32-bit SIMPLE_CHARACTER_STRING (SBCL
    // model, spec §1.6.3): holds any code point and is freely mutable. The
    // egcl-rt choke point picks the width and writes the layout.
    let padded = egcl_rt::object::character_string_alloc_size(s);
    let layout = Layout::from_size_align(padded, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        egcl_rt::object::write_character_string(ptr, s);
        ptr
    }
}

// ── Synonym stream resolution ─────────────────────────────────────

fn synonym_table() -> &'static OrderedMutex<HashMap<u64, EgclVal>> {
    static TABLE: OnceLock<OrderedMutex<HashMap<u64, EgclVal>>> = OnceLock::new();
    TABLE.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::GcWorld,
            13,
            "synonym stream GC roots",
            HashMap::new(),
        )
    })
}

fn scan_synonym_stream_roots(visit: &mut dyn FnMut(*mut EgclVal)) {
    let mut table = synonym_table()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for stream in table.values_mut() {
        visit(stream);
    }
}

fn install_synonym_stream_root_scanner() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| egcl_rt::gc::register_root_scanner(scan_synonym_stream_roots));
}

/// Bind a symbol to a stream value for synonym stream resolution.
pub fn set_symbol_stream(symbol: EgclVal, stream: EgclVal) {
    install_synonym_stream_root_scanner();
    let mut table = synonym_table().lock().unwrap();
    table.insert(symbol.0, stream);
}

/// Remove a symbol-to-stream binding.
pub fn remove_symbol_stream(symbol: EgclVal) {
    let mut table = synonym_table().lock().unwrap();
    table.remove(&symbol.0);
}

/// Resolve a synonym symbol to its target stream.
fn resolve_synonym(symbol: EgclVal) -> Result<EgclVal, EgclError> {
    // ANSI (CLHS make-synonym-stream): a synonym stream forwards to the CURRENT
    // dynamic value of the symbol. Prefer symbol-value so any user special
    // variable works — (make-synonym-stream '*my-out*) tracks *my-out*'s current
    // binding. Fall back to the legacy synonym_table registry (pre-registered
    // standard streams) only when the symbol's value is not itself a stream.
    if let Some(idx) = symbol.symbol_index() {
        if let Some(v) = egcl_rt::symbols::symbol_value(idx) {
            if egcl_rt::types::streamp(v) {
                return Ok(v);
            }
        }
    }
    let table = synonym_table().lock().unwrap();
    table.get(&symbol.0).copied().ok_or_else(|| {
        EgclError::StreamError("synonym stream: symbol has no stream binding".into())
    })
}

// ── Stream allocation helpers ──────────────────────────────────────

fn alloc_stream(
    element_type: StreamElementType,
    inner: StreamInner,
    components: Vec<EgclVal>,
    file_namestring: Option<String>,
) -> EgclVal {
    // Composite streams are constructed after their immutable components and
    // acquire the composite lock first. Descending keys therefore put every
    // newer composite before all of its older components.
    static NEXT_STREAM_ORDER: std::sync::atomic::AtomicU64 =
        std::sync::atomic::AtomicU64::new(u64::MAX);
    egcl_rt::rooted!(component_roots = components.clone());
    // The Rust-owned state lives in an off-heap Box (stable address; never
    // relocated by the moving GC). `components` is stored beside the mutex so
    // the GC can trace it lock-free; the mutable state gets a raw back-pointer
    // to it for the composite op arms.
    let mut boxed = Box::new(StreamAlloc {
        file_namestring,
        components: components.into_boxed_slice(),
        // Two orders per stream, state first so it takes the LARGER value and
        // `input` the smaller: the pair must be acquirable as input -> state,
        // and a composite still precedes all of its older components because
        // both of its orders come from later (smaller) draws than theirs.
        state: OrderedExecutionMutex::new(
            LockLevel::Stream,
            NEXT_STREAM_ORDER.fetch_sub(1, std::sync::atomic::Ordering::Relaxed),
            "stream state",
            StreamMutableState {
                open: true,
                element_type,
                inner,
                components_ptr: std::ptr::slice_from_raw_parts(std::ptr::null::<EgclVal>(), 0),
            },
        ),
        input: OrderedExecutionMutex::new(
            LockLevel::Stream,
            NEXT_STREAM_ORDER.fetch_sub(1, std::sync::atomic::Ordering::Relaxed),
            "stream input",
            (),
        ),
    });
    let cptr: *const [EgclVal] = &*boxed.components;
    boxed.state.get_mut().components_ptr = cptr;

    // The Lisp-visible stream value is a GC-heap handle whose single body word
    // holds the box pointer. Being a normal collectible heap object, its GC
    // finalizer (registered below) drops the box when the stream becomes
    // unreachable — closing the fd and warning if it was an unclosed file
    // stream (R5.121, egcl-jtc.7a).
    let body = egcl_rt::gc::alloc_typed(8, type_id::STREAM)
        .unwrap_or_else(|| panic!("{}", egcl_rt::gc::allocation_failure_message("stream handle")));
    // The handle is now the traceable owner. Publish any component forwarding
    // performed while it was being allocated before exposing the handle.
    for (component, root) in boxed.components.iter_mut().zip(component_roots.iter()) {
        *component = *root;
    }
    let box_ptr = Box::into_raw(boxed) as u64;
    unsafe {
        *(body as *mut u64) = box_ptr;
    }
    // The finalizer key is the untagged body address (what the GC's dead-object
    // passes fire on, and what jtc.7f forwards on evacuation); the Lisp value is
    // the tagged header pointer, body − 8.
    let _ = egcl_rt::gc::register_finalizer(EgclVal::from_raw(body as u64), NIL);
    unsafe { EgclVal::from_heap_ptr(body.sub(8)) }
}

// ── Process standard stream helpers ────────────────────────────────

/// Update the line/column counters after emitting character `c`.
fn track_col(c: char, line: &mut u64, col: &mut u64) {
    if c == '\n' {
        *line += 1;
        *col = 0;
    } else {
        *col += 1;
    }
}

/// Write raw bytes to the process stdout (`err = false`) or stderr
/// (`err = true`), flushing immediately so terminal output is not buffered
/// behind a missing newline.
fn write_to_std(err: bool, bytes: &[u8]) -> Result<(), EgclError> {
    if err {
        let out = std::io::stderr();
        let mut h = out.lock();
        h.write_all(bytes)
            .and_then(|_| h.flush())
            .map_err(|e| EgclError::StreamError(format!("stderr write error: {}", e)))
    } else {
        let out = std::io::stdout();
        let mut h = out.lock();
        h.write_all(bytes)
            .and_then(|_| h.flush())
            .map_err(|e| EgclError::StreamError(format!("stdout write error: {}", e)))
    }
}

/// Read a single UTF-8 character from the process standard input.
/// Returns EOF at end of input.
fn read_char_from_stdin() -> Result<EgclVal, EgclError> {
    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut one = [0u8; 1];
    match handle.read(&mut one) {
        Ok(0) => return Ok(EOF),
        Ok(_) => {}
        Err(e) => return Err(EgclError::StreamError(format!("stdin read error: {}", e))),
    }
    let b0 = one[0];
    let len = if b0 < 0x80 {
        1
    } else if b0 >> 5 == 0b110 {
        2
    } else if b0 >> 4 == 0b1110 {
        3
    } else if b0 >> 3 == 0b11110 {
        4
    } else {
        1
    };
    let mut buf = [0u8; 4];
    buf[0] = b0;
    for slot in buf.iter_mut().take(len).skip(1) {
        match handle.read(&mut one) {
            Ok(0) => break,
            Ok(_) => *slot = one[0],
            Err(e) => return Err(EgclError::StreamError(format!("stdin read error: {}", e))),
        }
    }
    match std::str::from_utf8(&buf[..len]) {
        Ok(s) => Ok(EgclVal::from_char(s.chars().next().unwrap_or('\u{FFFD}'))),
        Err(_) => Ok(EgclVal::from_char('\u{FFFD}')),
    }
}

/// Construct the process standard-input stream object.
pub fn make_stdin() -> EgclVal {
    alloc_stream(
        StreamElementType::Character,
        StreamInner::Stdin {
            unread: None,
            line: 0,
            col: 0,
        },
        vec![],
        None,
    )
}

/// Construct the process standard-output stream object.
pub fn make_stdout() -> EgclVal {
    alloc_stream(
        StreamElementType::Character,
        StreamInner::Stdout { line: 0, col: 0 },
        vec![],
        None,
    )
}

/// Construct the process error-output stream object.
pub fn make_stderr() -> EgclVal {
    alloc_stream(
        StreamElementType::Character,
        StreamInner::Stderr { line: 0, col: 0 },
        vec![],
        None,
    )
}

/// Get a reference to the StreamAlloc from a EgclVal.
///
/// The value is a GC-heap handle (tag STREAM) whose first body word holds the
/// pointer to the off-heap `StreamAlloc` block (egcl-jtc.7a).
///
/// SAFETY: The returned reference is valid as long as the stream has not been
/// finalized. The caller must ensure the EgclVal was created by `alloc_stream`.
fn get_stream_alloc(stream: EgclVal) -> Result<&'static StreamAlloc, EgclError> {
    if !stream.is_heap_object() {
        return Err(EgclError::StreamError("not a stream".into()));
    }
    unsafe {
        let header = stream.as_ptr(); // handle header
        if (*(header as *const ObjectHeader)).type_id() != type_id::STREAM {
            return Err(EgclError::StreamError("not a stream".into()));
        }
        // Body word 0 is the pointer to the off-heap StreamAlloc block.
        let box_ptr = *(header.add(8) as *const u64) as *const StreamAlloc;
        if box_ptr.is_null() {
            return Err(EgclError::StreamError("finalized stream".into()));
        }
        Ok(&*box_ptr)
    }
}

/// Copy a file stream's immutable opening namestring without taking its I/O lock.
pub(crate) fn file_namestring(stream: EgclVal) -> Result<String, EgclError> {
    get_stream_alloc(stream)?
        .file_namestring
        .clone()
        .ok_or_else(|| EgclError::TypeError {
            datum: stream,
            expected: "stream associated with a file".into(),
        })
}

/// One attempt at an input operation: either it produced a value, or it would
/// have had to park on the descriptor.
enum InputStep<T> {
    Done(T),
    WouldBlock,
}

// Rooting follows the payload: a WouldBlock carries nothing the collector
// needs to see.
impl<T: egcl_rt::gc::TraceHostRoots> egcl_rt::gc::TraceHostRoots for InputStep<T> {
    fn trace_host_roots(&mut self, visit: &mut dyn FnMut(*mut EgclVal)) {
        if let Self::Done(value) = self {
            value.trace_host_roots(visit);
        }
    }
}

/// Run an INPUT operation, parking for data with the state mutex RELEASED.
///
/// Holds the stream's `input` mutex for the whole operation, so readers stay
/// serialized and cannot interleave their consumption of the underlying
/// octets. Inside that, `operation` runs under `state` and may answer
/// `WouldBlock`; when it does, `state` is dropped, the park happens on a
/// duplicate of the handle, and the operation is retried.
///
/// Before this, a read parked inside `state` and locked every other thread out
/// of the stream for the duration -- CLOSE, a write on the other half of a
/// socket, even INPUT-STREAM-P (bliss-vupo0). The park could not simply move
/// outside `state`, because a read consumes bytes and two readers would
/// interleave; `input` is where that serialization went.
///
/// The retry loop owns the timeout budget, as `socket_io` does inside the
/// transport, so a socket read timeout still reports the same `TimedOut` and
/// still signals EGCL-EXT:IO-TIMEOUT.
///
/// The handle is duplicated under `state` so a concurrent CLOSE cannot recycle
/// the descriptor during the park, the same reason `stream_wait_for_input`
/// duplicates it.
///
/// Releasing `state` between attempts admits a moving collection (see
/// `with_stream`), so no Lisp value is held across the gap -- only the rooted
/// STREAM -- and buffer state is re-read from the guard on every attempt
/// rather than cached across one.
fn with_stream_input<T: egcl_rt::gc::TraceHostRoots>(
    stream: EgclVal,
    mut operation: impl FnMut(&mut StreamMutableState) -> Result<InputStep<T>, EgclError>,
) -> Result<T, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let alloc = get_stream_alloc(*stream)?;
    let _input = alloc.input.lock()?;
    let started = std::time::Instant::now();
    loop {
        let parking = {
            let mut guard = alloc.state.lock()?;
            let mut step = operation(&mut guard);
            if let Err(error) = &mut step {
                attach_timeout_stream(error, *stream);
            }
            egcl_rt::rooted_ref!(_step_root = &mut step);
            let outcome = match step {
                Ok(InputStep::Done(value)) => {
                    let mut value = Ok(value);
                    egcl_rt::rooted_ref!(_value_root = &mut value);
                    drop(guard);
                    drop(_value_root);
                    return value;
                }
                Ok(InputStep::WouldBlock) => guard.parking_handle(),
                Err(error) => {
                    drop(guard);
                    return Err(error);
                }
            };
            drop(_step_root);
            outcome
        };
        // `state` is released here; the park below cannot block another thread.
        let Some((handle, budget)) = parking else {
            // Nothing to park on, and the operation declined to produce a
            // value: report it rather than spinning.
            return Err(EgclError::StreamError(
                "stream reported no input and no descriptor to wait on".into(),
            ));
        };
        let remaining = budget.map(|t| t.saturating_sub(started.elapsed()));
        let timeout_ms = remaining.map(|t| t.as_millis().min(i32::MAX as u128) as i32);
        let ready = remaining != Some(std::time::Duration::ZERO)
            && handle.wait_readable(timeout_ms).unwrap_or(true);
        // `unwrap_or(true)` deliberately treats a failed wait as ready and lets
        // the read below report the real error. An interrupt is the exception:
        // the wait ended because this execution was interrupted, so retrying
        // re-parks and the readiness error is swallowed again -- a spin, not a
        // read error. Unwind instead, leaving the interrupt pending so the next
        // safepoint signals the posted condition rather than this placeholder
        // (bliss-5uklt).
        if egcl_rt::thread::current_execution_has_interrupt() {
            return Err(EgclError::Interrupt);
        }
        if !ready {
            // Produce the timeout through the same mapper the in-mutex path
            // used, so the condition stays EGCL-EXT:IO-TIMEOUT rather than a
            // bare STREAM-ERROR. socket_io reported ErrorKind::TimedOut with
            // this message and stream_io_error turned it into IoTimeout; keep
            // both so callers and tests see no change.
            let mut error = stream_io_error(
                "file read error",
                std::io::Error::new(std::io::ErrorKind::TimedOut, "socket I/O timed out"),
            );
            attach_timeout_stream(&mut error, *stream);
            return Err(error);
        }
    }
}

/// Lock the per-stream mutex and return a guard. R5.120.
fn lock_stream(
    stream: EgclVal,
) -> Result<OrderedExecutionMutexGuard<'static, StreamMutableState>, EgclError> {
    let alloc = get_stream_alloc(stream)?;
    alloc.state.lock()
}

/// Keep the stream alive while waiting, and retain any Lisp result/error
/// through unlock: releasing an execution mutex can admit a moving collection.
fn with_stream<T: egcl_rt::gc::TraceHostRoots>(
    stream: EgclVal,
    operation: impl FnOnce(&mut StreamMutableState) -> Result<T, EgclError>,
) -> Result<T, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let mut guard = lock_stream(*stream)?;
    let mut result = operation(&mut guard);
    if let Err(error) = &mut result {
        attach_timeout_stream(error, *stream);
    }
    egcl_rt::rooted_ref!(_result_root = &mut result);
    drop(guard);
    drop(_result_root);
    result
}

// ── Stream constructors ────────────────────────────────────────────

/// Apply OPEN's :IF-DOES-NOT-EXIST policy to a missing file about to be created
/// for :OUTPUT or :IO: NIL returns NIL from OPEN, T (:ERROR) signals FILE-ERROR,
/// and :CREATE falls through (ansi streams open.error.9-14).
fn missing_file_policy(if_does_not_exist: EgclVal, path_str: &str) -> Result<(), EgclError> {
    if if_does_not_exist == IF_DOES_NOT_EXIST_CREATE_VAL {
        return Ok(());
    }
    Err(EgclError::FileError(format!("file not found: {}", path_str)))
}

pub fn open(
    pathname: EgclVal,
    direction: StreamDirection,
    element_type_val: EgclVal,
    if_exists: EgclVal,
    if_does_not_exist: EgclVal,
    external_format: ExternalFormat,
) -> Result<EgclVal, EgclError> {
    if pathname == NIL {
        return Err(EgclError::FileError("NIL is not a valid pathname".into()));
    }

    // Issue #5: Determine element type from the element_type parameter.
    // NIL or T defaults to Character. Fixnum 8 signals (unsigned-byte 8).
    let elt = StreamElementType::from_code(element_type_val);

    // `OPEN` accepts pathname designators, including registry-backed pathname
    // sentinels and registered string sentinels used by the bootstrap tests.
    let path_str = crate::pathnames::extract_path_string(pathname)
        .map_err(|_| EgclError::FileError("pathname must be a string".into()))?;
    let path = std::path::Path::new(&path_str);
    let absolute_path = std::path::absolute(path)
        .map_err(|e| EgclError::FileError(format!("{}: {}", path_str, e)))?
        .to_string_lossy()
        .into_owned();

    // An image-embedded file shadows whatever the OS has at that path, and
    // is read-only (bliss-vmqe0).
    if let Some(bytes) = crate::embedded_files::contents(&absolute_path) {
        if !matches!(direction, StreamDirection::Input | StreamDirection::Probe) {
            return Err(EgclError::FileError(format!(
                "{}: embedded files are read-only",
                path_str
            )));
        }
        egcl_rt::rooted!(stream = alloc_stream(
            elt,
            StreamInner::FileInput {
                file: StreamHandle::Memory(std::io::Cursor::new(bytes)),
                read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                buf_pos: 0,
                buf_fill: 0,
                line: 0,
                col: 0,
                unread: None,
                external_format,
                element_type: elt,
            },
            vec![],
            Some(absolute_path),
        ));
        if direction == StreamDirection::Probe {
            close(*stream, false)?;
        }
        return Ok(*stream);
    }
    match direction {
        StreamDirection::Input | StreamDirection::Probe => {
            if path.is_dir() {
                return Err(EgclError::FileError(format!(
                    "cannot open directory as a file: {}",
                    path_str
                )));
            }
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if if_does_not_exist == NIL {
                        return Ok(NIL);
                    }
                    if if_does_not_exist == IF_DOES_NOT_EXIST_CREATE_VAL {
                        // Do not truncate a file created by another process
                        // between the failed read-only open and this one.
                        std::fs::OpenOptions::new()
                            .read(true)
                            .write(true)
                            .create(true)
                            .truncate(false)
                            .open(path)
                            .map_err(|e| EgclError::FileError(format!("cannot create file: {}", e)))?
                    } else {
                        return Err(EgclError::FileError(format!(
                            "file not found: {}",
                            path_str
                        )));
                    }
                }
                Err(e) => {
                    return Err(EgclError::FileError(format!("cannot open file: {}", e)));
                }
            };
            egcl_rt::rooted!(stream = alloc_stream(
                elt,
                StreamInner::FileInput {
                    file: StreamHandle::File(file),
                    read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                    buf_pos: 0,
                    buf_fill: 0,
                    line: 0,
                    col: 0,
                    unread: None,
                    external_format,
                    element_type: elt,
                },
                vec![],
                Some(absolute_path),
            ));
            if direction == StreamDirection::Probe {
                close(*stream, false)?;
            }
            Ok(*stream)
        }
        StreamDirection::Output => {
            // Issue #8: Handle if_exists variants including :append.
            let file = if path.exists() {
                if if_exists == NIL {
                    return Ok(NIL);
                } else if if_exists == IF_EXISTS_APPEND_VAL {
                    // :append — open for writing at end
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(path)
                        .map_err(|e| {
                            EgclError::FileError(format!("cannot open file for append: {}", e))
                        })?
                } else if if_exists == IF_EXISTS_OVERWRITE_VAL {
                    // :overwrite — open for writing without truncating
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .map_err(|e| {
                            EgclError::FileError(format!("cannot open file for overwrite: {}", e))
                        })?
                } else if if_exists == IF_EXISTS_ERROR_VAL {
                    return Err(EgclError::FileError(format!(
                        "file already exists: {}",
                        path_str
                    )));
                } else {
                    // Default (:supersede / T / any other value) — truncate and create
                    std::fs::File::create(path)
                        .map_err(|e| EgclError::FileError(format!("cannot create file: {}", e)))?
                }
            } else {
                if if_does_not_exist == NIL {
                    return Ok(NIL);
                }
                missing_file_policy(if_does_not_exist, &path_str)?;
                std::fs::File::create(path)
                    .map_err(|e| EgclError::FileError(format!("cannot create file: {}", e)))?
            };
            Ok(alloc_stream(
                elt,
                StreamInner::FileOutput {
                    file: StreamHandle::File(file),
                    write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                    external_format,
                    line: 0,
                    col: 0,
                },
                vec![],
                Some(absolute_path),
            ))
        }
        StreamDirection::Io => {
            let file = if path.exists() {
                if if_exists == NIL {
                    return Ok(NIL);
                } else if if_exists == IF_EXISTS_ERROR_VAL {
                    return Err(EgclError::FileError(format!(
                        "file already exists: {}",
                        path_str
                    )));
                } else if if_exists == IF_EXISTS_APPEND_VAL {
                    let mut f = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .map_err(|e| EgclError::FileError(format!("cannot open file: {}", e)))?;
                    // Seek to end for append
                    f.seek(std::io::SeekFrom::End(0))
                        .map_err(|e| EgclError::FileError(format!("cannot seek to end: {}", e)))?;
                    f
                } else if if_exists == IF_EXISTS_OVERWRITE_VAL {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .map_err(|e| EgclError::FileError(format!("cannot open file: {}", e)))?
                } else {
                    // Default (:supersede / T) — truncate
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .truncate(true)
                        .open(path)
                        .map_err(|e| EgclError::FileError(format!("cannot open file: {}", e)))?
                }
            } else {
                if if_does_not_exist == NIL {
                    return Ok(NIL);
                }
                missing_file_policy(if_does_not_exist, &path_str)?;
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(path)
                    .map_err(|e| EgclError::FileError(format!("cannot open file: {}", e)))?
            };
            Ok(alloc_stream(
                elt,
                StreamInner::FileIo {
                    file: StreamHandle::File(file),
                    read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                    buf_pos: 0,
                    buf_fill: 0,
                    write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                    line: 0,
                    col: 0,
                    unread: None,
                    external_format,
                },
                vec![],
                Some(absolute_path),
            ))
        }
    }
}

pub fn close(stream: EgclVal, abort: bool) -> Result<(), EgclError> {
    with_stream(stream, |guard| {
        // Flush write buffers before closing (unless abort)
        if guard.open && !abort {
            match &mut guard.inner {
                StreamInner::FileOutput {
                    file, write_buf, ..
                }
                | StreamInner::FileIo {
                    file, write_buf, ..
                } => {
                    file_flush_write_buf(file, write_buf)?;
                }
                _ => {}
            }
        }
        match &mut guard.inner {
            StreamInner::FileInput { file, .. }
            | StreamInner::FileOutput { file, .. }
            | StreamInner::FileIo { file, .. } => file.close(),
            _ => {}
        }
        guard.open = false;
        Ok(())
    })
}

pub fn make_string_input_stream(
    string: EgclVal,
    start: usize,
    end: Option<usize>,
) -> Result<EgclVal, EgclError> {
    let s = extract_string_str(string)?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if start > len {
        return Err(EgclError::StreamError("start beyond string length".into()));
    }
    let actual_end = end.unwrap_or(len);
    if actual_end > len {
        return Err(EgclError::StreamError("end beyond string length".into()));
    }
    if start > actual_end {
        return Err(EgclError::StreamError("start greater than end".into()));
    }
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::StringInput {
            chars,
            position: start,
            end: actual_end,
            line: 0,
            col: 0,
            unread: None,
        },
        vec![],
        None,
    ))
}

pub fn make_string_output_stream(_element_type: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::StringOutput {
            buffer: Vec::new(),
            line: 0,
            col: 0,
        },
        vec![],
        None,
    ))
}

pub fn get_output_stream_string(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| match &mut guard.inner {
        StreamInner::StringOutput { buffer, col, line } => {
            let s = std::str::from_utf8(buffer)
                .map_err(|_| EgclError::StreamError("invalid UTF-8 in output buffer".into()))?;
            // A fresh heap string: CLHS requires a new string, and an interned
            // one lived off-heap forever, leaking every distinct result
            // (bliss-4due).
            let result = make_lisp_string_fresh(s);
            buffer.clear();
            *col = 0;
            *line = 0;
            Ok(result)
        }
        _ => Err(EgclError::StreamError("not a string output stream".into())),
    })
}

pub fn make_broadcast_stream(streams: &[EgclVal]) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Broadcast,
        streams.to_vec(),
        None,
    ))
}

pub fn make_concatenated_stream(streams: &[EgclVal]) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Concatenated { cursor: 0 },
        streams.to_vec(),
        None,
    ))
}

pub fn make_two_way_stream(input: EgclVal, output: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::TwoWay,
        vec![input, output],
        None,
    ))
}

pub fn make_echo_stream(input: EgclVal, output: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Echo,
        vec![input, output],
        None,
    ))
}

pub fn make_synonym_stream(symbol: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Synonym,
        vec![symbol],
        None,
    ))
}

// ── Stream queries ─────────────────────────────────────────────────

/// Snapshot remaining text without advancing a string or regular file stream.
/// Reader callbacks must run after this returns, with no stream lock held.
/// Other transports and large files keep their incremental input path.
pub fn input_text_snapshot(stream: EgclVal) -> Result<Option<Vec<char>>, EgclError> {
    let mut result = None;
    with_stream(stream, |guard| {
        guard.check_input()?;
        if guard.element_type != StreamElementType::Character {
            return Ok(());
        }
        match &mut guard.inner {
            StreamInner::StringInput {
                chars,
                position,
                end,
                unread,
                ..
            } => {
                result = Some(
                    unread
                        .iter()
                        .copied()
                        .chain(chars[*position..*end].iter().copied())
                        .collect(),
                );
            }
            StreamInner::FileInput {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                unread,
                external_format: ExternalFormat::Utf8 | ExternalFormat::Ascii,
                ..
            } => {
                // Do not turn reading one small form from a huge file into an
                // unbounded allocation, or read ahead on devices and pipes.
                let Ok(metadata) = file.metadata() else {
                    return Ok(());
                };
                if !metadata.is_file() {
                    return Ok(());
                }
                let offset = file
                    .stream_position()
                    .map_err(|e| EgclError::StreamError(e.to_string()))?;
                let pending = *buf_fill - *buf_pos;
                if metadata.len().saturating_sub(offset) + pending as u64 > 16 * 1024 * 1024 {
                    return Ok(());
                }
                let mut bytes = read_buf[*buf_pos..*buf_fill].to_vec();
                let read = file.read_to_end(&mut bytes);
                let restore = file.seek(std::io::SeekFrom::Start(offset));
                restore.map_err(|e| EgclError::StreamError(e.to_string()))?;
                read.map_err(|e| EgclError::StreamError(e.to_string()))?;
                // Invalid bytes later in the file must not prevent an earlier
                // valid form from being read. Let ordinary stream reads report
                // that error when they actually reach it.
                if let Ok(text) = std::str::from_utf8(&bytes) {
                    result = Some(unread.iter().copied().chain(text.chars()).collect());
                }
            }
            _ => {}
        }
        Ok(())
    })?;
    Ok(result)
}

pub fn open_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.open)).unwrap_or(false)
}

/// The standard class a native stream belongs to (CLHS 21.2), by name, for
/// TYPEP and CLASS-OF; None for a value that is not a native stream.
pub fn stream_class_name(stream: EgclVal) -> Option<&'static str> {
    let guard = lock_stream(stream).ok()?;
    Some(match &guard.inner {
        StreamInner::StringInput { .. } | StreamInner::StringOutput { .. } => "STRING-STREAM",
        StreamInner::FileInput { .. }
        | StreamInner::FileOutput { .. }
        | StreamInner::FileIo { .. }
        | StreamInner::Stdin { .. }
        | StreamInner::Stdout { .. }
        | StreamInner::Stderr { .. } => "FILE-STREAM",
        StreamInner::Broadcast => "BROADCAST-STREAM",
        StreamInner::Concatenated { .. } => "CONCATENATED-STREAM",
        StreamInner::TwoWay => "TWO-WAY-STREAM",
        StreamInner::Echo => "ECHO-STREAM",
        StreamInner::Synonym => "SYNONYM-STREAM",
        #[allow(unreachable_patterns)]
        _ => return None,
    })
}

pub fn input_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.is_input())).unwrap_or(false)
}

pub fn output_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.is_output())).unwrap_or(false)
}

pub fn stream_element_type(stream: EgclVal) -> EgclVal {
    egcl_rt::rooted!(stream = stream);
    match lock_stream(*stream) {
        Ok(guard) => guard.element_type.to_code(),
        Err(_) => NIL,
    }
}

// ── GrayStream free-function wrappers ─────────────────────────────
// Each wrapper acquires the per-stream mutex (R5.120).

pub fn stream_read_char(stream: EgclVal) -> Result<EgclVal, EgclError> {
    // Composites delegate to a component, whose read can park; do that with
    // the outer guard released (bliss-ifed5 for bytes, same shape here).
    // `components` is outside the mutex and empty for non-composites, so this
    // costs no lock on the common path.
    if !get_stream_alloc(stream)?.components.is_empty() {
        return composite_read_char(stream);
    }
    // Character reads can park on a slow device just as byte reads park on a
    // peer, so they go through the wrapper that parks with the state mutex
    // released (bliss-o61rm).
    with_stream_input(stream, |guard| guard.stream_read_char_step())
}

pub fn stream_unread_char(stream: EgclVal, ch: EgclVal) -> Result<(), EgclError> {
    let mut ch = ch;
    egcl_rt::rooted_ref!(_ch_root = &mut ch);
    with_stream(stream, |guard| guard.stream_unread_char(ch))
}

pub fn stream_read_byte(stream: EgclVal) -> Result<EgclVal, EgclError> {
    // A composite delegates to a component, and that inner read can park. Do
    // it with the OUTER stream's state mutex released, or a TWO-WAY / ECHO /
    // CONCATENATED stream reproduces bliss-vupo0 one level up (bliss-ifed5).
    //
    // `components` is immutable and lives outside the mutex -- and is empty for
    // every non-composite stream -- so this costs no lock on the common path.
    if !get_stream_alloc(stream)?.components.is_empty() {
        return composite_read_byte(stream);
    }
    // Byte reads are the paths that can park on a socket -- socket streams are
    // (UNSIGNED-BYTE 8) -- so they go through the wrapper that parks with the
    // state mutex released (bliss-vupo0). A stream that cannot park outside the
    // mutex falls back to the in-mutex path, which is where character streams
    // and files still are.
    with_stream_input(stream, |guard| guard.stream_read_byte_step())
}

/// Read a character from a composite stream without holding its state mutex
/// across the component's read.
///
/// The character twin of `composite_read_byte`; see that function for why the
/// outer `input` mutex is held throughout and how the CONCATENATED cursor is
/// advanced safely.
fn composite_read_char(stream: EgclVal) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let alloc = get_stream_alloc(*stream)?;
    let _input = alloc.input.lock()?;
    loop {
        let step = {
            let guard = alloc.state.lock()?;
            guard.check_input()?;
            let comps = guard.components();
            match &guard.inner {
                StreamInner::TwoWay => CompositeByteRead::Delegate(comps[0]),
                StreamInner::Echo => CompositeByteRead::Echo(comps[0], comps[1]),
                StreamInner::Synonym => {
                    CompositeByteRead::Delegate(resolve_synonym(comps[0])?)
                }
                StreamInner::Concatenated { cursor } => {
                    if *cursor < comps.len() {
                        CompositeByteRead::Concatenated(comps[*cursor], *cursor)
                    } else {
                        CompositeByteRead::Eof
                    }
                }
                _ => CompositeByteRead::InMutex,
            }
        };
        // `state` is released here.
        match step {
            CompositeByteRead::Delegate(inner) => return crate::streams::stream_read_char(inner),
            CompositeByteRead::Echo(source, sink) => {
                let mut ch = crate::streams::stream_read_char(source)?;
                // Root CH across the echo write for the reason the byte path
                // documents: the write allocates and this copy is returned.
                egcl_rt::rooted_ref!(_ch_root = &mut ch);
                if ch != EOF {
                    crate::streams::stream_write_char(sink, ch)?;
                }
                return Ok(ch);
            }
            CompositeByteRead::Concatenated(inner, index) => {
                let value = crate::streams::stream_read_char(inner)?;
                if value != EOF {
                    return Ok(value);
                }
                let mut guard = alloc.state.lock()?;
                if let StreamInner::Concatenated { cursor } = &mut guard.inner {
                    if *cursor == index {
                        *cursor += 1;
                    }
                } else {
                    return Ok(EOF);
                }
            }
            CompositeByteRead::Eof => return Ok(EOF),
            CompositeByteRead::InMutex => {
                return with_stream(*stream, |guard| guard.stream_read_char());
            }
        }
    }
}

/// What a composite byte read delegates to, decided under a brief hold of the
/// outer `state` and then performed with it released.
enum CompositeByteRead {
    /// Read from this component and return what it gives.
    Delegate(EgclVal),
    /// Read from the first component, echo to the second.
    Echo(EgclVal, EgclVal),
    /// Read from the component at this cursor index; on EOF advance and retry.
    Concatenated(EgclVal, usize),
    /// The component list is exhausted.
    Eof,
    /// Not a delegating read after all (a wide element type, or a composite
    /// arm that touches only its own state): run the original in-mutex path so
    /// behaviour and error ordering are unchanged.
    InMutex,
}

/// Read a byte from a composite stream without holding its state mutex across
/// the component's read.
///
/// The outer `input` mutex is held throughout, which is what orders concurrent
/// readers of this composite -- in particular it keeps two readers from
/// advancing a CONCATENATED cursor at once, now that the inner read no longer
/// happens under `state`.
///
/// Lock order still holds: a composite is constructed after its components and
/// draws smaller order numbers, so outer-input -> outer-state and
/// outer-state -> inner-input are both increasing.
fn composite_read_byte(stream: EgclVal) -> Result<EgclVal, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let alloc = get_stream_alloc(*stream)?;
    let _input = alloc.input.lock()?;
    loop {
        let step = {
            let guard = alloc.state.lock()?;
            guard.check_input()?;
            let comps = guard.components();
            match (&guard.inner, guard.element_type) {
                // A wide element type decodes several octets per element; leave
                // it on the original path.
                (_, StreamElementType::Integer { .. }) => CompositeByteRead::InMutex,
                (StreamInner::TwoWay, _) => CompositeByteRead::Delegate(comps[0]),
                (StreamInner::Echo, _) => CompositeByteRead::Echo(comps[0], comps[1]),
                (StreamInner::Synonym, _) => {
                    CompositeByteRead::Delegate(resolve_synonym(comps[0])?)
                }
                (StreamInner::Concatenated { cursor }, _) => {
                    if *cursor < comps.len() {
                        CompositeByteRead::Concatenated(comps[*cursor], *cursor)
                    } else {
                        CompositeByteRead::Eof
                    }
                }
                _ => CompositeByteRead::InMutex,
            }
        };
        // `state` is released here; the component read below cannot block
        // another thread from touching this composite.
        match step {
            CompositeByteRead::Delegate(inner) => return crate::streams::stream_read_byte(inner),
            CompositeByteRead::Echo(source, sink) => {
                let mut byte = crate::streams::stream_read_byte(source)?;
                // Root BYTE across the echo write, for the reason the in-mutex
                // arm gives: on a wide element type it is a heap bignum, the
                // write allocates, and this copy is what the caller receives.
                egcl_rt::rooted_ref!(_byte_root = &mut byte);
                if byte != EOF {
                    crate::streams::stream_write_byte(sink, byte)?;
                }
                return Ok(byte);
            }
            CompositeByteRead::Concatenated(inner, index) => {
                let value = crate::streams::stream_read_byte(inner)?;
                if value != EOF {
                    return Ok(value);
                }
                // Advance past the exhausted component, unless another reader
                // already did while `state` was released.
                let mut guard = alloc.state.lock()?;
                if let StreamInner::Concatenated { cursor } = &mut guard.inner {
                    if *cursor == index {
                        *cursor += 1;
                    }
                } else {
                    return Ok(EOF);
                }
            }
            CompositeByteRead::Eof => return Ok(EOF),
            CompositeByteRead::InMutex => {
                return with_stream(*stream, |guard| guard.stream_read_byte());
            }
        }
    }
}

/// True if STREAM has element-type (unsigned-byte 8), so sequence I/O over it
/// should transfer octets rather than characters.
pub fn is_byte_stream(stream: EgclVal) -> bool {
    with_stream(stream, |guard| {
        Ok(guard.element_type == StreamElementType::UnsignedByte8)
    })
    .unwrap_or(false)
}

/// True if STREAM has an integer element type of any width, so sequence I/O
/// over it transfers elements with READ-BYTE / WRITE-BYTE rather than
/// characters.
pub fn is_binary_stream(stream: EgclVal) -> bool {
    // A composite stream is binary when the component it reads from or writes
    // to is (ansi make-two-way-stream.12, make-concatenated-stream.22).
    let (binary, delegate) = with_stream(stream, |guard| {
        let comps = guard.components();
        Ok((
            guard.element_type.is_binary(),
            match &guard.inner {
                StreamInner::TwoWay | StreamInner::Echo => comps.first().copied(),
                StreamInner::Concatenated { cursor } => {
                    comps.get(*cursor).copied().or_else(|| comps.first().copied())
                }
                StreamInner::Synonym => resolve_synonym(comps[0]).ok(),
                _ => None,
            },
        ))
    })
    .unwrap_or((false, None));
    binary || delegate.is_some_and(is_binary_stream)
}

pub fn stream_write_char(stream: EgclVal, ch: EgclVal) -> Result<(), EgclError> {
    let mut ch = ch;
    egcl_rt::rooted_ref!(_ch_root = &mut ch);
    with_stream(stream, |guard| guard.stream_write_char(ch))
}

pub fn stream_write_byte(stream: EgclVal, byte: EgclVal) -> Result<(), EgclError> {
    let mut byte = byte;
    egcl_rt::rooted_ref!(_byte_root = &mut byte);
    with_stream(stream, |guard| guard.stream_write_byte(byte))
}

pub fn stream_write_string(
    stream: EgclVal,
    string: EgclVal,
    start: usize,
    end: Option<usize>,
) -> Result<(), EgclError> {
    let mut string = string;
    egcl_rt::rooted_ref!(_string_root = &mut string);
    with_stream(stream, |guard| {
        guard.stream_write_string(string, start, end)
    })
}

pub fn stream_force_output(stream: EgclVal) -> Result<(), EgclError> {
    with_stream(stream, |guard| guard.stream_force_output())
}

pub fn stream_finish_output(stream: EgclVal) -> Result<(), EgclError> {
    with_stream(stream, |guard| guard.stream_finish_output())
}

pub fn stream_clear_input(stream: EgclVal) -> Result<(), EgclError> {
    with_stream(stream, |guard| guard.stream_clear_input())
}

pub fn stream_listen(stream: EgclVal) -> Result<bool, EgclError> {
    with_stream(stream, |guard| guard.stream_listen())
}

pub fn stream_line_number(stream: EgclVal) -> Option<u64> {
    with_stream(stream, |guard| Ok(guard.stream_line_number())).unwrap_or(None)
}

pub fn stream_line_column(stream: EgclVal) -> Option<u64> {
    with_stream(stream, |guard| Ok(guard.stream_line_column())).unwrap_or(None)
}

// ── R5.113 additional Gray protocol free-function wrappers ────────

pub fn stream_read_char_no_hang(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| guard.stream_read_char_no_hang())
}

pub fn stream_peek_char(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| guard.stream_peek_char())
}

pub fn stream_read_line(stream: EgclVal) -> Result<(EgclVal, bool), EgclError> {
    with_stream(stream, |guard| guard.stream_read_line())
}

pub fn stream_terpri(stream: EgclVal) -> Result<(), EgclError> {
    with_stream(stream, |guard| guard.stream_terpri())
}

pub fn stream_fresh_line(stream: EgclVal) -> Result<bool, EgclError> {
    with_stream(stream, |guard| guard.stream_fresh_line())
}

pub fn stream_clear_output(stream: EgclVal) -> Result<(), EgclError> {
    with_stream(stream, |guard| guard.stream_clear_output())
}

pub fn stream_advance_to_column(stream: EgclVal, col: u64) -> Result<bool, EgclError> {
    with_stream(stream, |guard| guard.stream_advance_to_column(col))
}

pub fn stream_start_line_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.stream_start_line_p())).unwrap_or(false)
}

/// READ-SEQUENCE over a character stream, waiting with the state mutex released.
///
/// The accumulator lives here rather than inside the step so partial progress
/// survives a park: `with_stream_input` re-runs the operation after waiting, and
/// a sequence read must neither lose nor re-read what it already transferred.
/// It is rooted in place because the elements are Lisp values and a collection
/// can relocate them while this thread waits (bliss-mclc6).
pub fn stream_read_sequence(stream: EgclVal, count: usize) -> Result<Vec<EgclVal>, EgclError> {
    let mut out: Vec<EgclVal> = Vec::with_capacity(count.min(1 << 22));
    egcl_rt::rooted_ref!(_out_root = &mut out);
    let filled = with_stream_input(stream, |guard| {
        while out.len() < count {
            let element = match guard.stream_read_char_step()? {
                InputStep::Done(element) => element,
                InputStep::WouldBlock => return Ok(InputStep::WouldBlock),
            };
            if element == EOF {
                break;
            }
            out.push(element);
        }
        Ok(InputStep::Done(()))
    });
    drop(_out_root);
    filled?;
    Ok(out)
}

/// Read up to COUNT octets, locking the stream once rather than once per byte.
///
/// READ-SEQUENCE over an `(unsigned-byte 8)` stream called `stream_read_byte`
/// per octet, and each of those goes through `with_stream` -- a stream lookup
/// and lock. Reading the 6.76 MB Android runtime that way measured 5.1 s, about
/// 750 ns a byte, nearly all of it the per-byte lock rather than the read.
/// Stops early at end of file, like the character form above.
pub fn stream_read_byte_sequence(
    stream: EgclVal,
    count: usize,
) -> Result<Vec<EgclVal>, EgclError> {
    let mut out: Vec<EgclVal> = Vec::with_capacity(count.min(1 << 22));
    egcl_rt::rooted_ref!(_out_root = &mut out);
    let filled = with_stream_input(stream, |guard| {
        while out.len() < count {
            let byte = match guard.stream_read_byte_step()? {
                InputStep::Done(byte) => byte,
                InputStep::WouldBlock => return Ok(InputStep::WouldBlock),
            };
            if byte == crate::streams::EOF {
                break;
            }
            out.push(byte);
        }
        Ok(InputStep::Done(()))
    });
    drop(_out_root);
    filled?;
    Ok(out)
}

/// Read up to `count` octets from STREAM as bytes.
///
/// Preferred over `stream_read_byte_sequence` for byte streams: that one builds
/// a `Vec<EgclVal>`, which for a 6.76 MB file is a 54 MB intermediate on top of
/// a dispatch per byte.
pub fn stream_read_octets(stream: EgclVal, count: usize) -> Result<Vec<u8>, EgclError> {
    // The accumulator needs no rooting: plain octets hold no Lisp value for the
    // collector to trace or relocate. It still lives outside the step so the
    // bytes already transferred survive a park, as the sequence forms above do.
    let mut out: Vec<u8> = Vec::with_capacity(count.min(1 << 22));
    with_stream_input(stream, |guard| guard.read_octets_step(&mut out, count))?;
    Ok(out)
}

/// Write OCTETS to STREAM as bytes, taking the stream lock once.
pub fn stream_write_octets(stream: EgclVal, octets: &[u8]) -> Result<(), EgclError> {
    // As in `stream_read_octets`: octets hold no Lisp value, so only the stream
    // needs rooting.
    egcl_rt::rooted!(stream = stream);
    let mut guard = lock_stream(*stream)?;
    guard.write_octets(octets)
}

pub fn stream_write_sequence(stream: EgclVal, elements: &[EgclVal]) -> Result<(), EgclError> {
    let mut elements = elements.to_vec();
    egcl_rt::rooted_ref!(_elements_root = &mut elements);
    with_stream(stream, |guard| guard.stream_write_sequence(&elements))
}

pub fn interactive_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.interactive_stream_p())).unwrap_or(false)
}

pub fn stream_external_format(stream: EgclVal) -> ExternalFormat {
    egcl_rt::rooted!(stream = stream);
    match lock_stream(*stream) {
        Ok(guard) => guard.stream_external_format(),
        Err(_) => ExternalFormat::Utf8,
    }
}

// ── R5.124: file-position and file-length ─────────────────────────

/// Return the current file position, or `NIL` for non-positionable streams. R5.124.
pub fn file_position(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| {
        guard.check_open()?;
        let per_element = guard.element_type.octets_per_element() as u64;
        let mut comps = guard.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut guard.inner {
            StreamInner::FileIo {
                file: StreamHandle::Socket(_) | StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileInput {
                file: StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileOutput {
                file: StreamHandle::Pipe(_),
                ..
            } => Ok(NIL),
            StreamInner::FileInput {
                file,
                buf_pos,
                buf_fill,
                ..
            } => {
                // The OS position is ahead of our logical position by the buffered-but-unread bytes.
                let os_pos = file
                    .stream_position()
                    .map_err(|e| EgclError::StreamError(format!("file-position error: {}", e)))?;
                let buffered_unread = (*buf_fill - *buf_pos) as u64;
                Ok(EgclVal::from_fixnum(((os_pos - buffered_unread) / per_element) as i64))
            }
            StreamInner::FileOutput {
                file, write_buf, ..
            } => {
                let os_pos = file
                    .stream_position()
                    .map_err(|e| EgclError::StreamError(format!("file-position error: {}", e)))?;
                let pending = write_buf.len() as u64;
                Ok(EgclVal::from_fixnum(((os_pos + pending) / per_element) as i64))
            }
            StreamInner::FileIo {
                file,
                buf_pos,
                buf_fill,
                write_buf,
                ..
            } => {
                let os_pos = file
                    .stream_position()
                    .map_err(|e| EgclError::StreamError(format!("file-position error: {}", e)))?;
                let buffered_unread = (*buf_fill - *buf_pos) as u64;
                let pending_write = write_buf.len() as u64;
                Ok(EgclVal::from_fixnum(
                    ((os_pos - buffered_unread + pending_write) / per_element) as i64,
                ))
            }
            StreamInner::StringInput {
                position, unread, ..
            } => {
                // A pending UNREAD-CHAR logically rewinds the stream one character;
                // reporting the raw counter made a reader-macro caller (which
                // measures how much the handler consumed via FILE-POSITION,
                // bliss-r4mk) overshoot by one and swallow the delimiter the
                // handler pushed back.
                let pending = usize::from(unread.is_some());
                Ok(EgclVal::from_fixnum(
                    position.saturating_sub(pending) as i64
                ))
            }
            StreamInner::StringOutput { buffer, .. } => {
                Ok(EgclVal::from_fixnum(buffer.len() as i64))
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                file_position(target)
            }
            // A broadcast stream reports its LAST component's position, and 0
            // only when it has none (CLHS BROADCAST-STREAM). Answering 0
            // unconditionally made (file-position (make-broadcast-stream out))
            // disagree with (file-position out) after a write.
            StreamInner::Broadcast => match comps.last() {
                Some(last) => file_position(*last),
                None => Ok(EgclVal::from_fixnum(0)),
            },
            _ => Ok(NIL), // non-positionable
        }
    })
}

/// Octet offset of element index `position` in a stream whose elements occupy
/// `per_element` octets.
///
/// FILE-POSITION addresses ELEMENTS, not octets, so on a wide element type a
/// large index can exceed any file offset: index 2^59 on an
/// `(unsigned-byte 256)` stream asks for 2^64 octets. Multiplying unchecked
/// panicked outright in a debug build ("attempt to multiply with overflow")
/// and left the stream's state poisoned, and a negative index wrapped to a
/// huge offset instead of being rejected. SBCL signals a TYPE-ERROR for both,
/// so this does too.
fn element_octet_offset(position: EgclVal, per_element: u64) -> Result<u64, EgclError> {
    u64::try_from(position.as_fixnum())
        .ok()
        .and_then(|index| index.checked_mul(per_element))
        .ok_or_else(|| EgclError::TypeError {
            datum: position,
            expected: "file position reachable with this stream's element type".to_string(),
        })
}

/// Set the file position. Returns T on success, NIL if not positionable. R5.124.
pub fn set_file_position(stream: EgclVal, position: EgclVal) -> Result<EgclVal, EgclError> {
    let mut position = position;
    egcl_rt::rooted_ref!(_position_root = &mut position);
    with_stream(stream, |guard| {
        guard.check_open()?;
        let per_element = guard.element_type.octets_per_element() as u64;
        let mut comps = guard.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut guard.inner {
            StreamInner::FileIo {
                file: StreamHandle::Socket(_) | StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileInput {
                file: StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileOutput {
                file: StreamHandle::Pipe(_),
                ..
            } => Ok(NIL),
            StreamInner::FileInput {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            } => {
                // Invalidate read buffer on seek.
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                let pos = element_octet_offset(position, per_element)?;
                file.seek(std::io::SeekFrom::Start(pos)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::FileOutput {
                file, write_buf, ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                let pos = element_octet_offset(position, per_element)?;
                file.seek(std::io::SeekFrom::Start(pos)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::FileIo {
                file,
                read_buf: _,
                buf_pos,
                buf_fill,
                write_buf,
                unread,
                ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                let pos = element_octet_offset(position, per_element)?;
                file.seek(std::io::SeekFrom::Start(pos)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::StringInput {
                position: pos,
                end,
                unread,
                ..
            } => {
                // Seek within an in-memory input string (R5.124). Clamp to [0, end];
                // a negative or unparseable index is a failed positioning (NIL).
                let requested = position.as_fixnum();
                if requested < 0 {
                    return Ok(NIL);
                }
                *pos = (requested as usize).min(*end);
                *unread = None;
                Ok(T)
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                set_file_position(target, position)
            }
            _ => Ok(NIL),
        }
    })
}

/// Position a stream at its end — the `:end` designator of `(setf file-position)`
/// (R5.124). Kept in the stdlib because only it knows each stream variant's end.
/// Returns `T` on success, `NIL` for a non-positionable stream.
pub fn set_file_position_to_end(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| {
        guard.check_open()?;
        let mut comps = guard.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut guard.inner {
            StreamInner::FileIo {
                file: StreamHandle::Socket(_) | StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileInput {
                file: StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileOutput {
                file: StreamHandle::Pipe(_),
                ..
            } => Ok(NIL),
            StreamInner::StringInput {
                position: pos,
                end,
                unread,
                ..
            } => {
                *pos = *end;
                *unread = None;
                Ok(T)
            }
            StreamInner::FileInput {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            } => {
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                file.seek(std::io::SeekFrom::End(0)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::FileOutput {
                file, write_buf, ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                file.seek(std::io::SeekFrom::End(0)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::FileIo {
                file,
                buf_pos,
                buf_fill,
                write_buf,
                unread,
                ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                file.seek(std::io::SeekFrom::End(0)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            // A string-output stream is always logically at its end.
            StreamInner::StringOutput { .. } => Ok(T),
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                set_file_position_to_end(target)
            }
            _ => Ok(NIL),
        }
    })
}

/// Return the length of the file underlying the stream, or NIL for
/// non-positionable streams. R5.124.
pub fn file_length_fn(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| {
        guard.check_open()?;
        let per_element = guard.element_type.octets_per_element();
        let mut comps = guard.components().to_vec();
        egcl_rt::rooted_ref!(_components_root = &mut comps);
        match &mut guard.inner {
            StreamInner::FileIo {
                file: StreamHandle::Socket(_) | StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileInput {
                file: StreamHandle::Pipe(_),
                ..
            }
            | StreamInner::FileOutput {
                file: StreamHandle::Pipe(_),
                ..
            } => Ok(NIL),
            StreamInner::FileInput { file, .. }
            | StreamInner::FileOutput { file, .. }
            | StreamInner::FileIo { file, .. } => {
                // In elements, not octets, for wide binary element types.
                if let Some(len) = file.memory_len() {
                    return Ok(EgclVal::from_fixnum((len / per_element as u64) as i64));
                }
                let metadata = file
                    .metadata()
                    .map_err(|e| EgclError::StreamError(format!("file-length error: {}", e)))?;
                Ok(EgclVal::from_fixnum((metadata.len() / per_element as u64) as i64))
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                file_length_fn(target)
            }
            // A broadcast stream reports its LAST component's length, and 0
            // only when it has no components (CLHS BROADCAST-STREAM); any
            // other non-file stream is a TYPE-ERROR (ansi file-length.error.*).
            StreamInner::Broadcast => match comps.last() {
                Some(last) => file_length_fn(*last),
                None => Ok(EgclVal::from_fixnum(0)),
            },
            _ => Err(EgclError::TypeError {
                datum: stream,
                expected: "FILE-STREAM".into(),
            }),
        }
    })
}

// ── GC integration (egcl-jtc.7a) ─────────────────────────────────

/// True if a StreamInner holds an OS file descriptor whose accidental
/// non-close warrants the unclosed-stream warning (R5.121).
fn inner_is_file(inner: &StreamInner) -> bool {
    matches!(
        inner,
        StreamInner::FileInput { .. } | StreamInner::FileOutput { .. } | StreamInner::FileIo { .. }
    )
}

/// R5.121 warning is enabled unless `EGCL_WARN_UNCLOSED_STREAMS=0`.
fn warn_unclosed_enabled() -> bool {
    !matches!(
        std::env::var("EGCL_WARN_UNCLOSED_STREAMS").as_deref(),
        Ok("0")
    )
}

/// GC trace hook for a stream handle (registered via `set_stream_trace_fn`).
/// Visits the component references held in the off-heap `StreamAlloc` block so
/// the collector marks and forwards them (egcl-jtc.7a). `handle_body` points at
/// the stream handle's body; word 0 is the raw pointer to the block.
fn stream_trace(handle_body: *mut u8, visit: &mut dyn FnMut(*mut EgclVal)) {
    unsafe {
        let box_ptr = *(handle_body as *const u64) as *mut StreamAlloc;
        if box_ptr.is_null() {
            return;
        }
        // `components` is set once at construction; only the GC rewrites its
        // entries (forwarding evacuated components). Read the `Box<[EgclVal]>`
        // fat pointer without forming a reference to the shared `StreamAlloc`,
        // mirroring the collector's raw-pointer discipline at safepoints; visit()
        // then rewrites each slot in place.
        let slice: *mut [EgclVal] =
            *(std::ptr::addr_of!((*box_ptr).components) as *const *mut [EgclVal]);
        let len = slice.len();
        let data = slice as *mut EgclVal;
        for i in 0..len {
            visit(data.add(i));
        }
    }
}

/// GC finalizer dispatch for stream handles (registered via
/// `set_finalizer_dispatch`). Invoked for a dead object keyed by its untagged
/// body address. Drops the off-heap `StreamAlloc` block — closing the fd via
/// `File`'s Drop and freeing buffers — and, for an unclosed file stream, emits a
/// style warning (R5.121). Non-stream objects are ignored.
///
/// Runs inside the GC pause under the heap lock, so it must not allocate on the
/// GC heap; dropping the Box and writing to stderr are both safe here.
fn stdlib_gc_finalize(_finalizer: EgclVal, object: EgclVal) {
    // `object` is from_raw(body): body word 0 holds native storage, and the
    // handle header (body − 8) selects the resource's finalizer.
    let body = object.to_raw() as *mut u8;
    unsafe {
        let header = &*(body.sub(8) as *const ObjectHeader);
        if header.type_id() == type_id::MUTEX {
            crate::synchronization::finalize_mutex(body);
            return;
        }
        if header.type_id() == type_id::CONDITION_VARIABLE {
            crate::synchronization::finalize_condition_variable(body);
            return;
        }
        // A dead Python proxy owes CPython a reference. Releasing it HERE would be
        // wrong — this runs in the GC pause under the heap lock, and a decref can
        // run __del__, i.e. arbitrary Python — so the destructor only queues it,
        // for the next thread that crosses into Python to release.
        #[cfg(feature = "python")]
        if header.type_id() == type_id::PYTHON_OBJECT {
            egcl_rt::python::finalize_proxy(body);
            return;
        }
        if header.type_id() != type_id::STREAM {
            return; // not a stream — leave for other finalizer kinds
        }
        let box_ptr = *(body as *const u64) as *mut StreamAlloc;
        if box_ptr.is_null() {
            return;
        }
        // A dead stream has no reachable mutator, hence exclusive ownership of
        // its off-heap block. Recover the Box first and inspect the mutex state
        // through `get_mut`; acquiring the level-1 stream lock while the
        // collector holds the level-8 heap lock would invert the global order.
        let mut alloc = Box::from_raw(box_ptr);
        if warn_unclosed_enabled() {
            let state = alloc.state.get_mut();
            if state.open && inner_is_file(&state.inner) {
                eprintln!("; Warning: file stream was garbage-collected without being closed");
            }
        }
        drop(alloc); // File::drop closes the fd; buffers freed
        // Null the handle's box pointer. Finalizers run early in a major GC
        // (before the relocation pass, which traces *every* non-forwarded object,
        // including this now-dead handle). Without this, `stream_trace` would
        // dereference the freed block. A nulled pointer makes the later trace —
        // and any stray access — skip it safely (egcl-jtc.7a).
        *(body as *mut u64) = 0;
    }
}

/// Install the stdlib's GC hooks (stream tracing + finalizer dispatch). Call
/// once at interpreter startup (egcl-jtc.7a).
pub fn install_gc_hooks() {
    egcl_rt::gc::set_stream_trace_fn(stream_trace);
    egcl_rt::gc::set_finalizer_dispatch(stdlib_gc_finalize);
}

// ── TCP sockets ────────────────────────────────────────────────────
// Minimal networking primitives for the slynk backend (and general Lisp use).
// A *listening* socket is an opaque integer id into a process-wide registry (it
// is never read/written as a stream, only accept/close/local-port). An *accepted
// connection* is returned as an ordinary bidirectional octet stream, reusing
// the FileIo machinery over its owned TCP transport — so all the Gray-stream
// I/O (read-char, read-line, write-string, force-output, …) works unchanged.
use std::net::ToSocketAddrs;
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// A failed socket operation, keeping the OS error number alongside the text
/// so Lisp can classify it (EADDRINUSE, ECONNREFUSED, ...) without parsing it.
fn socket_error(context: impl std::fmt::Display, error: std::io::Error) -> EgclError {
    EgclError::SocketError {
        errno: error.raw_os_error(),
        message: format!("{context}: {error}"),
    }
}

struct SocketListener {
    socket: TcpListener,
    closed: AtomicBool,
}

impl SocketListener {
    fn check_open(&self) -> Result<(), EgclError> {
        if self.closed.load(Ordering::Acquire) {
            Err(EgclError::FileError("socket listener is closed".into()))
        } else {
            Ok(())
        }
    }

    #[cfg(unix)]
    fn wait(&self, timeout: Option<std::time::Duration>) -> Result<bool, EgclError> {
        self.check_open()?;
        // Each parked operation needs its own poller registration. Duplicates
        // share the listening socket and its shutdown, but have distinct fds.
        let waiter = self.socket.try_clone()
            .map_err(|e| socket_error("socket listener wait", e))?;
        let ready = egcl_rt::sync::wait_fd(
            waiter.as_raw_fd(), egcl_rt::sync::IoInterest::Read, timeout,
        )?;
        self.check_open()?;
        Ok(ready)
    }
}

fn socket_listeners() -> &'static OrderedMutex<HashMap<u64, Arc<SocketListener>>> {
    static LISTENERS: OnceLock<OrderedMutex<HashMap<u64, Arc<SocketListener>>>> = OnceLock::new();
    LISTENERS.get_or_init(|| {
        OrderedMutex::new(
            LockLevel::ExecutionRegistry,
            0,
            "socket listeners",
            HashMap::new(),
        )
    })
}

fn socket_listener(id: u64) -> Result<Arc<SocketListener>, EgclError> {
    socket_listeners()
        .lock()
        .unwrap()
        .get(&id)
        .cloned()
        .ok_or_else(|| EgclError::FileError("unknown or closed socket listener".into()))
}

/// Create a listening TCP socket bound to `host:port` (port 0 = any free port).
/// Returns an opaque listener id for `socket_accept`/`socket_local_port`/`socket_close_listener`.
pub fn socket_listen(host: &str, port: u16, backlog: i32) -> Result<u64, EgclError> {
    socket_listen_with_options(host, port, backlog, cfg!(unix))
}

/// Bind a TCP listener with explicit address reuse and pending-connection limit.
pub fn socket_listen_with_options(
    host: &str,
    port: u16,
    backlog: i32,
    reuse_address: bool,
) -> Result<u64, EgclError> {
    if backlog < 0 {
        return Err(EgclError::ProgramError(
            "socket-listen: negative backlog".into(),
        ));
    }
    let bind = || -> std::io::Result<TcpListener> {
        let mut last_error = std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "hostname resolved to no addresses",
        );
        for address in (host, port).to_socket_addrs()? {
            let attempt = || -> std::io::Result<TcpListener> {
                let socket = socket2::Socket::new(
                    socket2::Domain::for_address(address),
                    socket2::Type::STREAM,
                    Some(socket2::Protocol::TCP),
                )?;
                socket.set_reuse_address(reuse_address)?;
                socket.bind(&address.into())?;
                socket.listen(backlog)?;
                Ok(socket.into())
            };
            match attempt() {
                Ok(listener) => return Ok(listener),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    };
    let listener = bind()
        .map_err(|e| socket_error(format_args!("socket-listen {host}:{port}"), e))?;
    #[cfg(unix)]
    listener
        .set_nonblocking(true)
        .map_err(|e| socket_error("socket-listen", e))?;
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    socket_listeners().lock().unwrap().insert(
        id,
        Arc::new(SocketListener {
            socket: listener,
            closed: AtomicBool::new(false),
        }),
    );
    Ok(id)
}

/// The actual local port a listener is bound to (resolves port 0).
pub fn socket_local_port(id: u64) -> Option<u16> {
    socket_listener(id)
        .ok()
        .and_then(|l| l.socket.local_addr().ok())
        .map(|a| a.port())
}

/// Close and forget a listening socket.
pub fn socket_close_listener(id: u64) {
    let listener = socket_listeners().lock().unwrap().remove(&id);
    if let Some(listener) = listener {
        listener.closed.store(true, Ordering::Release);
        #[cfg(unix)]
        // SAFETY: this retained owner keeps the descriptor alive until shutdown
        // returns. Pending waiters retain it too, preventing descriptor reuse.
        unsafe {
            libc::shutdown(listener.socket.as_raw_fd(), libc::SHUT_RDWR);
        }
    }
}

/// Connect a TCP client and return an owned bidirectional octet stream.
/// The optional timeout bounds connection attempts across all resolved addresses;
/// system hostname resolution precedes that deadline.
pub fn socket_connect(
    host: &str,
    port: u16,
    timeout: Option<std::time::Duration>,
) -> Result<EgclVal, EgclError> {
    let connected = if let Some(timeout) = timeout {
        let addresses = (host, port)
            .to_socket_addrs()
            .map_err(|e| socket_error(format_args!("socket-connect {host}:{port}"), e))?;
        let start = std::time::Instant::now();
        let mut result = Err(std::io::Error::new(
            std::io::ErrorKind::AddrNotAvailable,
            "hostname resolved to no addresses",
        ));
        for address in addresses {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                result = Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "connect timeout",
                ));
                break;
            }
            result = TcpStream::connect_timeout(&address, remaining);
            if result.is_ok() {
                break;
            }
        }
        result
    } else {
        TcpStream::connect((host, port))
    }
    .map_err(|e| socket_error(format_args!("socket-connect {host}:{port}"), e))?;
    socket_stream(connected)
}

/// Accept a connection on listener `id`, returning a bidirectional octet
/// stream over the new socket (blocks until a client connects).
pub fn socket_accept(id: u64) -> Result<EgclVal, EgclError> {
    let listener = socket_listener(id)?;
    let stream = loop {
        listener.check_open()?;
        match listener.socket.accept() {
            Ok((stream, _)) => {
                listener.check_open()?;
                break stream;
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            #[cfg(unix)]
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                listener.wait(None)?;
            }
            Err(e) => return Err(socket_error("socket-accept", e)),
        }
    };
    socket_stream(stream)
}

/// Whether a listener has a connection waiting, without accepting it.
///
/// `socket_accept` blocks, and a listener is an id into a map rather than a
/// stream, so `stream_wait_for_input` cannot see it. Without this a program with
/// its own event loop -- a UI serving slynk between frames, say -- has no way to
/// offer a REPL without stalling on accept until somebody connects.
pub fn socket_listener_ready(id: u64, timeout_ms: i32) -> Result<bool, EgclError> {
    let listener = socket_listener(id)?;
    listener.check_open()?;
    #[cfg(unix)]
    {
        let timeout =
            (timeout_ms >= 0).then(|| std::time::Duration::from_millis(timeout_ms as u64));
        listener.wait(timeout)
    }
    #[cfg(not(unix))]
    {
        // Fall back to a non-blocking accept probe, restoring the mode.
        let _ = timeout_ms;
        listener.socket.set_nonblocking(true).ok();
        let ready = match listener.socket.accept() {
            Ok(_) => true,
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(_) => true,
        };
        listener.socket.set_nonblocking(false).ok();
        Ok(ready)
    }
}

/// Transfer the owned socket to the common stream/finalizer machinery.
fn socket_stream(stream: TcpStream) -> Result<EgclVal, EgclError> {
    let _ = stream.set_nodelay(true);
    let file = StreamHandle::socket(stream)
        .map_err(|e| EgclError::StreamError(format!("socket setup: {e}")))?;
    Ok(alloc_stream(
        StreamElementType::UnsignedByte8,
        StreamInner::FileIo {
            file,
            read_buf: Vec::with_capacity(FILE_BUF_SIZE),
            buf_pos: 0,
            buf_fill: 0,
            write_buf: Vec::with_capacity(FILE_BUF_SIZE),
            line: 0,
            col: 0,
            unread: None,
            external_format: ExternalFormat::Utf8,
        },
        vec![],
        None,
    ))
}

// Transfer ownership through the platform's safe owned-handle conversion.
#[cfg(unix)]
use std::os::fd::OwnedFd as OwnedPipe;
#[cfg(windows)]
use std::os::windows::io::OwnedHandle as OwnedPipe;

/// Own the child's stdin as a buffered output stream. Closing it sends EOF.
pub fn process_stdin_stream(
    pipe: std::process::ChildStdin,
    element_type: StreamElementType,
) -> EgclVal {
    alloc_stream(
        element_type,
        StreamInner::FileOutput {
            file: StreamHandle::pipe(std::fs::File::from(OwnedPipe::from(pipe))),
            write_buf: Vec::with_capacity(FILE_BUF_SIZE),
            external_format: ExternalFormat::Utf8,
            line: 0,
            col: 0,
        },
        vec![],
        None,
    )
}

/// Own the child's stdout as a nonseekable buffered input stream.
pub fn process_stdout_stream(
    pipe: std::process::ChildStdout,
    element_type: StreamElementType,
) -> EgclVal {
    process_input_stream(OwnedPipe::from(pipe), element_type)
}

/// Own the child's stderr independently so callers can drain both outputs.
pub fn process_stderr_stream(
    pipe: std::process::ChildStderr,
    element_type: StreamElementType,
) -> EgclVal {
    process_input_stream(OwnedPipe::from(pipe), element_type)
}

fn process_input_stream(pipe: OwnedPipe, element_type: StreamElementType) -> EgclVal {
    alloc_stream(
        element_type,
        StreamInner::FileInput {
            file: StreamHandle::pipe(std::fs::File::from(pipe)),
            read_buf: Vec::with_capacity(FILE_BUF_SIZE),
            buf_pos: 0,
            buf_fill: 0,
            line: 0,
            col: 0,
            unread: None,
            external_format: ExternalFormat::Utf8,
            element_type,
        },
        vec![],
        None,
    )
}

// Duplicate under the stream lock so CLOSE cannot invalidate an option call.
fn socket_option_handle(stream: EgclVal) -> Result<TcpStream, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let guard = lock_stream(*stream)?;
    guard.check_open()?;
    let StreamInner::FileIo {
        file: StreamHandle::Socket(socket),
        ..
    } = &guard.inner
    else {
        return Err(EgclError::StreamError(
            "not a bidirectional socket stream".into(),
        ));
    };
    socket
        .try_clone()
        .map_err(|e| EgclError::StreamError(format!("socket option: {e}")))
}

/// A socket address as `(octet-vector port)`.
///
/// SBCL's `sb-bsd-sockets:socket-name` returns "the address (as vector of
/// bytes) and port" as multiple values; a builtin here yields one value, so the
/// pair comes back as a two-element list and the caller spreads it (the same
/// shape `%PRUNE-CLOSURES` uses).  The vector has 4 elements for IPv4 and 16
/// for IPv6 -- do not assume the former, which is what SBCL's "depending on the
/// address family" caveat covers.
fn socket_address_pair(addr: std::net::SocketAddr) -> EgclVal {
    let octets: Vec<EgclVal> = match addr.ip() {
        std::net::IpAddr::V4(v4) => v4
            .octets()
            .iter()
            .map(|b| EgclVal::from_fixnum(*b as i64))
            .collect(),
        std::net::IpAddr::V6(v6) => v6
            .octets()
            .iter()
            .map(|b| EgclVal::from_fixnum(*b as i64))
            .collect(),
    };
    // Root the vector across the list allocation: build_list allocates conses,
    // and a minor collection there would relocate an unrooted vector.
    egcl_rt::rooted!(vector = crate::sequences::build_simple_vector(&octets));
    let port = EgclVal::from_fixnum(addr.port() as i64);
    crate::sequences::build_list(&[*vector, port])
}

/// The address and port a LISTENER is bound to, as `(octet-vector port)`.
///
/// `socket_local_port` answers only the port and discards the address, which
/// leaves usocket's `get-local-address` and `get-local-name` with nothing to
/// call (bliss-by1gf).
pub fn socket_listener_local_name(id: u64) -> Option<EgclVal> {
    let addr = socket_listener(id).ok()?.socket.local_addr().ok()?;
    Some(socket_address_pair(addr))
}

/// The address and port a connected STREAM is bound to locally.
pub fn socket_stream_local_name(stream: EgclVal) -> Result<EgclVal, EgclError> {
    let addr = socket_option_handle(stream)?
        .local_addr()
        .map_err(|e| EgclError::StreamError(format!("socket local name: {e}")))?;
    Ok(socket_address_pair(addr))
}

/// The address and port of a connected STREAM's peer.
///
/// `socket_accept` discards the peer address `accept` reports, so this asks the
/// socket rather than remembering it.
pub fn socket_stream_peer_name(stream: EgclVal) -> Result<EgclVal, EgclError> {
    let addr = socket_option_handle(stream)?
        .peer_addr()
        .map_err(|e| EgclError::StreamError(format!("socket peer name: {e}")))?;
    Ok(socket_address_pair(addr))
}

pub fn socket_read_timeout(stream: EgclVal) -> Result<Option<std::time::Duration>, EgclError> {
    socket_option_handle(stream)?
        .read_timeout()
        .map_err(|e| EgclError::StreamError(format!("socket read timeout: {e}")))
}

pub fn socket_set_read_timeout(
    stream: EgclVal,
    timeout: Option<std::time::Duration>,
) -> Result<(), EgclError> {
    socket_option_handle(stream)?
        .set_read_timeout(timeout)
        .map_err(|e| EgclError::StreamError(format!("socket read timeout: {e}")))
}

/// The raw file descriptor backing a file/socket IO stream, or None.
#[cfg(unix)]
pub fn stream_raw_fd(stream: EgclVal) -> Option<i32> {
    egcl_rt::rooted!(stream = stream);
    let guard = lock_stream(*stream).ok()?;
    guard.check_open().ok()?;
    match &guard.inner {
        StreamInner::FileIo { file, .. } => Some(file.as_raw_fd()),
        StreamInner::FileInput { file, .. } => Some(file.as_raw_fd()),
        StreamInner::FileOutput { file, .. } => Some(file.as_raw_fd()),
        _ => None,
    }
}

/// Wait for buffered or OS input, including EOF. Duplicate the handle under
/// the lock so a concurrent CLOSE cannot recycle it while the wait is running.
pub fn stream_wait_for_input(
    stream: EgclVal,
    timeout_ms: Option<i32>,
) -> Result<bool, EgclError> {
    egcl_rt::rooted!(stream = stream);
    let handle = {
        let guard = lock_stream(*stream)?;
        guard.check_input()?;
        match &guard.inner {
            StreamInner::FileIo {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            }
            | StreamInner::FileInput {
                file,
                buf_pos,
                buf_fill,
                unread,
                ..
            } => {
                if unread.is_some() || *buf_pos < *buf_fill {
                    return Ok(true);
                }
                file.try_clone()
                    .map_err(|e| EgclError::StreamError(format!("wait-for-input: {e}")))?
            }
            _ => {
                return Err(EgclError::StreamError(
                    "wait-for-input: not an OS input stream".into(),
                ));
            }
        }
    };
    handle
        .wait_readable(timeout_ms)
        .map_err(|e| EgclError::StreamError(format!("wait-for-input: {e}")))
}

// ── Composite-stream accessors (synonym / two-way) ─────────────────
/// The symbol a SYNONYM-STREAM forwards to, or None if not a synonym stream.
pub fn synonym_stream_symbol(stream: EgclVal) -> Option<EgclVal> {
    with_stream(stream, |guard| {
        Ok(if matches!(guard.inner, StreamInner::Synonym) {
            guard.components().first().copied()
        } else {
            None
        })
    })
    .ok()
    .flatten()
}

/// The input stream of a TWO-WAY / ECHO stream, or None.
pub fn two_way_stream_input_stream(stream: EgclVal) -> Option<EgclVal> {
    with_stream(stream, |guard| {
        Ok(
            if matches!(guard.inner, StreamInner::TwoWay | StreamInner::Echo) {
                guard.components().first().copied()
            } else {
                None
            },
        )
    })
    .ok()
    .flatten()
}

/// The output stream of a TWO-WAY / ECHO stream, or None.
pub fn two_way_stream_output_stream(stream: EgclVal) -> Option<EgclVal> {
    with_stream(stream, |guard| {
        Ok(
            if matches!(guard.inner, StreamInner::TwoWay | StreamInner::Echo) {
                guard.components().get(1).copied()
            } else {
                None
            },
        )
    })
    .ok()
    .flatten()
}

/// The component streams of a BROADCAST stream (CLHS BROADCAST-STREAM-STREAMS),
/// or None when `stream` is not one.
pub fn broadcast_stream_streams(stream: EgclVal) -> Option<Vec<EgclVal>> {
    with_stream(stream, |guard| {
        Ok(if matches!(guard.inner, StreamInner::Broadcast) {
            Some(guard.components().to_vec())
        } else {
            None
        })
    })
    .ok()
    .flatten()
}

/// The streams a CONCATENATED stream still has to read from, starting with the
/// current one (CLHS CONCATENATED-STREAM-STREAMS), or None when `stream` is not
/// one.
pub fn concatenated_stream_streams(stream: EgclVal) -> Option<Vec<EgclVal>> {
    with_stream(stream, |guard| {
        Ok(match &guard.inner {
            StreamInner::Concatenated { cursor } => {
                let comps = guard.components();
                Some(comps[(*cursor).min(comps.len())..].to_vec())
            }
            _ => None,
        })
    })
    .ok()
    .flatten()
}

// A Windows SOCKET cannot be represented as a Unix file descriptor.
#[cfg(windows)]
pub fn stream_raw_fd(_stream: EgclVal) -> Option<i32> {
    None
}

#[cfg(test)]
mod output_flush_tests {
    use super::file_flush_write_buf;
    use std::collections::VecDeque;
    use std::io::{self, Write};

    struct ShortWriter {
        steps: VecDeque<io::Result<usize>>,
        delivered: Vec<u8>,
    }
    impl Write for ShortWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            let count = self.steps.pop_front().unwrap_or(Ok(bytes.len()))?;
            assert!(count <= bytes.len());
            self.delivered.extend_from_slice(&bytes[..count]);
            Ok(count)
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_flush_retains_only_the_unwritten_suffix() {
        let mut writer = ShortWriter {
            steps: VecDeque::from([
                Ok(3),
                Err(io::ErrorKind::Interrupted.into()),
                Ok(2),
                Err(io::ErrorKind::WouldBlock.into()),
            ]),
            delivered: Vec::new(),
        };
        let mut pending = b"abcdefghi".to_vec();
        assert!(file_flush_write_buf(&mut writer, &mut pending).is_err());
        assert_eq!(writer.delivered, b"abcde");
        assert_eq!(pending, b"fghi");
        file_flush_write_buf(&mut writer, &mut pending).unwrap();
        assert!(pending.is_empty());
        assert_eq!(writer.delivered, b"abcdefghi");
    }

    #[test]
    fn zero_write_is_an_error_and_preserves_pending_bytes() {
        let mut writer = ShortWriter {
            steps: VecDeque::from([Ok(0)]),
            delivered: Vec::new(),
        };
        let mut pending = b"pending".to_vec();
        assert!(file_flush_write_buf(&mut writer, &mut pending).is_err());
        assert_eq!(pending, b"pending");
        assert!(writer.delivered.is_empty());
    }
}
