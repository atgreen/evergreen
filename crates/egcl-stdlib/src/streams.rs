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
}

#[derive(Clone, Debug, PartialEq, Eq)]
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
}

// ── if_exists keyword constants ───────────────────────────────────
// Callers pass these as EgclVal to open(). NIL and T are also valid.
// NIL = return nil if exists; T = default (:supersede).
pub const IF_EXISTS_ERROR_VAL: EgclVal = EgclVal(1 << 3); // fixnum 1
pub const IF_EXISTS_SUPERSEDE_VAL: EgclVal = EgclVal(2 << 3); // fixnum 2
pub const IF_EXISTS_APPEND_VAL: EgclVal = EgclVal(3 << 3); // fixnum 3
pub const IF_EXISTS_OVERWRITE_VAL: EgclVal = EgclVal(4 << 3); // fixnum 4

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
            StreamInner::Synonym => StreamDirection::Io,
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

const FILE_BUF_SIZE: usize = 8192;

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
) -> Result<EgclVal, EgclError> {
    if let Some(c) = unread.take() {
        return Ok(EgclVal::from_char(c));
    }
    loop {
        // Decode without consuming an incomplete prefix. NO-HANG can return
        // NIL between any two bytes of a character and resume on its next call.
        for length in 1..=(*buf_fill - *buf_pos).min(4) {
            match std::str::from_utf8(&read_buf[*buf_pos..*buf_pos + length]) {
                Ok(text) => {
                    let ch = text.chars().next().unwrap();
                    *buf_pos += length;
                    track_col(ch, line, col);
                    return Ok(EgclVal::from_char(ch));
                }
                Err(error) if error.error_len().is_some() || length == 4 => {
                    *buf_pos += length;
                    return Err(EgclError::StreamError(
                        "invalid UTF-8 in file stream".into(),
                    ));
                }
                Err(_) => {}
            }
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

/// Read a whole line (up to and consuming a `\n`, or to EOF) from a buffered
/// UTF-8/ASCII file input in bulk, instead of decoding and pushing one char at a
/// time (the per-char loop paid a UTF-8 decode + a `Vec<char>` push per byte).
/// The newline byte 0x0A never occurs inside a UTF-8 multi-byte sequence (those
/// are 0x80–0xBF continuation / 0xC0+ leading bytes), so scanning the raw buffer
/// for 0x0A splits exactly at line boundaries; the bytes between boundaries are
/// therefore complete UTF-8 and decode as one `String`. Returns `(line,
/// missing-newline-p)`; `(EOF, true)` at end of input with nothing buffered.
fn file_read_line_buffered(
    file: &mut StreamHandle,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    line: &mut u64,
    col: &mut u64,
    unread: &mut Option<char>,
) -> Result<(EgclVal, bool), EgclError> {
    let mut line_bytes: Vec<u8> = Vec::new();
    // A pushed-back (unread) char is logically the first char of the line.
    if let Some(c) = unread.take() {
        if c == '\n' {
            *line += 1;
            *col = 0;
            return Ok((make_lisp_string(""), false));
        }
        let mut tmp = [0u8; 4];
        line_bytes.extend_from_slice(c.encode_utf8(&mut tmp).as_bytes());
    }
    let decode = |bytes: Vec<u8>| -> Result<String, EgclError> {
        String::from_utf8(bytes)
            .map_err(|_| EgclError::StreamError("invalid UTF-8 in file stream".into()))
    };
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
                return Ok((make_lisp_string(&s), true));
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
                return Ok((make_lisp_string(&s), false));
            }
            None => {
                line_bytes.extend_from_slice(slice);
                *buf_pos = *buf_fill;
            }
        }
    }
}

/// Read a single raw byte from a buffered file input (for binary streams). Issue #10.
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
            .map_err(|e| EgclError::StreamError(format!("file read error: {}", e)))?;
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
                ..
            } => {
                file_read_char_buffered(file, read_buf, buf_pos, buf_fill, line, col, unread, true)
            }
            StreamInner::FileIo {
                file,
                read_buf,
                buf_pos,
                buf_fill,
                write_buf,
                line,
                col,
                unread,
                ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                file_read_char_buffered(file, read_buf, buf_pos, buf_fill, line, col, unread, true)
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
                ..
            } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_buf.extend_from_slice(encoded.as_bytes());
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
                ..
            } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_buf.extend_from_slice(encoded.as_bytes());
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
            StreamInner::Broadcast => {
                // GC can update the rooted vector during a child call; do not
                // retain an iterator borrow across that call.
                #[allow(clippy::needless_range_loop)]
                for index in 0..comps.len() {
                    let s = comps[index];
                    crate::streams::stream_write_byte(s, byte)?;
                }
                Ok(())
            }
            StreamInner::TwoWay | StreamInner::Echo => {
                crate::streams::stream_write_byte(comps[1], byte)
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                crate::streams::stream_write_byte(target, byte)
            }
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
                ..
            } => {
                write_buf.extend_from_slice(slice);
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
                ..
            } => {
                write_buf.extend_from_slice(slice);
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
            ..
        } = &mut self.inner
        {
            if matches!(file, StreamHandle::Socket(_) | StreamHandle::Pipe(_)) {
                // Do not flush pending writes: a NO-HANG input call must not
                // block on a peer that has stopped reading our output.
                return file_read_char_buffered(
                    file, read_buf, buf_pos, buf_fill, line, col, unread, false,
                );
            }
        }
        // For string streams, if exhausted return NIL.
        // For file streams, reads never block on regular files, so just call stream_read_char.
        if let StreamInner::StringInput { position, end, .. } = &self.inner {
            if *position >= *end {
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
                    ExternalFormat::Utf8 | ExternalFormat::Ascii
                )
            {
                return file_read_line_buffered(
                    file, read_buf, buf_pos, buf_fill, line, col, unread,
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
                return Ok((make_lisp_string(&s), true));
            }
            let c = ch.as_char();
            if c == '\n' {
                let s: String = chars.into_iter().collect();
                return Ok((make_lisp_string(&s), false));
            }
            chars.push(c);
        }
    }

    fn stream_terpri(&mut self) -> Result<(), EgclError> {
        self.stream_write_char(EgclVal::from_char('\n'))
    }

    fn stream_fresh_line(&mut self) -> Result<bool, EgclError> {
        self.check_output()?;
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
            } else if elem.is_fixnum() {
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
            } => external_format.clone(),
            StreamInner::FileOutput {
                external_format, ..
            } => external_format.clone(),
            StreamInner::FileIo {
                external_format, ..
            } => external_format.clone(),
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
        components: components.into_boxed_slice(),
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
    });
    let cptr: *const [EgclVal] = &*boxed.components;
    boxed.state.get_mut().components_ptr = cptr;

    // The Lisp-visible stream value is a GC-heap handle whose single body word
    // holds the box pointer. Being a normal collectible heap object, its GC
    // finalizer (registered below) drops the box when the stream becomes
    // unreachable — closing the fd and warning if it was an unclosed file
    // stream (R5.121, egcl-jtc.7a).
    let body = egcl_rt::gc::alloc_typed(8, type_id::STREAM)
        .expect("GC heap unavailable for stream handle");
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
    )
}

/// Construct the process standard-output stream object.
pub fn make_stdout() -> EgclVal {
    alloc_stream(
        StreamElementType::Character,
        StreamInner::Stdout { line: 0, col: 0 },
        vec![],
    )
}

/// Construct the process error-output stream object.
pub fn make_stderr() -> EgclVal {
    alloc_stream(
        StreamElementType::Character,
        StreamInner::Stderr { line: 0, col: 0 },
        vec![],
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
    egcl_rt::rooted_ref!(_result_root = &mut result);
    drop(guard);
    drop(_result_root);
    result
}

// ── Stream constructors ────────────────────────────────────────────

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
    let elt = if element_type_val == EgclVal::from_fixnum(8) {
        StreamElementType::UnsignedByte8
    } else {
        StreamElementType::Character
    };

    // `OPEN` accepts pathname designators, including registry-backed pathname
    // sentinels and registered string sentinels used by the bootstrap tests.
    let path_str = crate::pathnames::extract_path_string(pathname)
        .map_err(|_| EgclError::FileError("pathname must be a string".into()))?;
    let path = std::path::Path::new(&path_str);

    match direction {
        StreamDirection::Input => {
            if path.is_dir() {
                return Err(EgclError::FileError(format!(
                    "cannot open directory as a file: {}",
                    path_str
                )));
            }
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // CLHS: `:if-does-not-exist nil` returns NIL rather than
                    // signalling — the caller decides. Any other value (the
                    // default `:error`, or `:create` which we don't create for
                    // input) signals a file-error.
                    if if_does_not_exist == NIL {
                        return Ok(NIL);
                    }
                    return Err(EgclError::FileError(format!(
                        "file not found: {}",
                        path_str
                    )));
                }
                Err(e) => {
                    return Err(EgclError::FileError(format!("cannot open file: {}", e)));
                }
            };
            Ok(alloc_stream(
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
            ))
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
    ))
}

pub fn get_output_stream_string(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| match &mut guard.inner {
        StreamInner::StringOutput { buffer, col, line } => {
            let s = std::str::from_utf8(buffer)
                .map_err(|_| EgclError::StreamError("invalid UTF-8 in output buffer".into()))?;
            let result = make_lisp_string(s);
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
    ))
}

pub fn make_concatenated_stream(streams: &[EgclVal]) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Concatenated { cursor: 0 },
        streams.to_vec(),
    ))
}

pub fn make_two_way_stream(input: EgclVal, output: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::TwoWay,
        vec![input, output],
    ))
}

pub fn make_echo_stream(input: EgclVal, output: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Echo,
        vec![input, output],
    ))
}

pub fn make_synonym_stream(symbol: EgclVal) -> Result<EgclVal, EgclError> {
    Ok(alloc_stream(
        StreamElementType::Character,
        StreamInner::Synonym,
        vec![symbol],
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

pub fn input_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.is_input())).unwrap_or(false)
}

pub fn output_stream_p(stream: EgclVal) -> bool {
    with_stream(stream, |guard| Ok(guard.is_output())).unwrap_or(false)
}

pub fn stream_element_type(stream: EgclVal) -> EgclVal {
    egcl_rt::rooted!(stream = stream);
    match lock_stream(*stream) {
        Ok(guard) => match guard.element_type {
            StreamElementType::Character => T,
            StreamElementType::UnsignedByte8 => EgclVal::from_fixnum(8),
        },
        Err(_) => NIL,
    }
}

// ── GrayStream free-function wrappers ─────────────────────────────
// Each wrapper acquires the per-stream mutex (R5.120).

pub fn stream_read_char(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| guard.stream_read_char())
}

pub fn stream_unread_char(stream: EgclVal, ch: EgclVal) -> Result<(), EgclError> {
    let mut ch = ch;
    egcl_rt::rooted_ref!(_ch_root = &mut ch);
    with_stream(stream, |guard| guard.stream_unread_char(ch))
}

pub fn stream_read_byte(stream: EgclVal) -> Result<EgclVal, EgclError> {
    with_stream(stream, |guard| guard.stream_read_byte())
}

/// True if STREAM has element-type (unsigned-byte 8), so sequence I/O over it
/// should transfer octets rather than characters.
pub fn is_byte_stream(stream: EgclVal) -> bool {
    with_stream(stream, |guard| {
        Ok(guard.element_type == StreamElementType::UnsignedByte8)
    })
    .unwrap_or(false)
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

pub fn stream_read_sequence(stream: EgclVal, count: usize) -> Result<Vec<EgclVal>, EgclError> {
    with_stream(stream, |guard| guard.stream_read_sequence(count))
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
    with_stream(stream, |guard| {
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            let byte = guard.stream_read_byte()?;
            if byte == crate::streams::EOF {
                break;
            }
            out.push(byte);
        }
        Ok(out)
    })
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
                Ok(EgclVal::from_fixnum((os_pos - buffered_unread) as i64))
            }
            StreamInner::FileOutput {
                file, write_buf, ..
            } => {
                let os_pos = file
                    .stream_position()
                    .map_err(|e| EgclError::StreamError(format!("file-position error: {}", e)))?;
                let pending = write_buf.len() as u64;
                Ok(EgclVal::from_fixnum((os_pos + pending) as i64))
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
                    (os_pos - buffered_unread + pending_write) as i64,
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
            _ => Ok(NIL), // non-positionable
        }
    })
}

/// Set the file position. Returns T on success, NIL if not positionable. R5.124.
pub fn set_file_position(stream: EgclVal, position: EgclVal) -> Result<EgclVal, EgclError> {
    let mut position = position;
    egcl_rt::rooted_ref!(_position_root = &mut position);
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
                let pos = position.as_fixnum() as u64;
                file.seek(std::io::SeekFrom::Start(pos)).map_err(|e| {
                    EgclError::StreamError(format!("set-file-position error: {}", e))
                })?;
                Ok(T)
            }
            StreamInner::FileOutput {
                file, write_buf, ..
            } => {
                file_flush_write_buf(file, write_buf)?;
                let pos = position.as_fixnum() as u64;
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
                let pos = position.as_fixnum() as u64;
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
                let metadata = file
                    .metadata()
                    .map_err(|e| EgclError::StreamError(format!("file-length error: {}", e)))?;
                Ok(EgclVal::from_fixnum(metadata.len() as i64))
            }
            StreamInner::Synonym => {
                let target = resolve_synonym(comps[0])?;
                file_length_fn(target)
            }
            _ => Ok(NIL),
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
// A *listening* socket is an opaque integer id into a thread-local registry (it
// is never read/written as a stream, only accept/close/local-port). An *accepted
// connection* is returned as an ordinary bidirectional character stream, reusing
// the FileIo machinery over its owned TCP transport — so all the Gray-stream
// I/O (read-char, read-line, write-string, force-output, …) works unchanged.
use std::cell::{Cell, RefCell};
use std::net::ToSocketAddrs;
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::fd::AsRawFd;

thread_local! {
    static SOCKET_LISTENERS: RefCell<HashMap<u64, TcpListener>> = RefCell::new(HashMap::new());
    static SOCKET_NEXT_ID: Cell<u64> = const { Cell::new(1) };
}

/// Create a listening TCP socket bound to `host:port` (port 0 = any free port).
/// Returns an opaque listener id for `socket_accept`/`socket_local_port`/`socket_close_listener`.
pub fn socket_listen(host: &str, port: u16, _backlog: i32) -> Result<u64, EgclError> {
    let listener = TcpListener::bind((host, port))
        .map_err(|e| EgclError::FileError(format!("socket-listen {host}:{port}: {e}")))?;
    let id = SOCKET_NEXT_ID.with(|c| {
        let v = c.get();
        c.set(v + 1);
        v
    });
    SOCKET_LISTENERS.with(|m| m.borrow_mut().insert(id, listener));
    Ok(id)
}

/// The actual local port a listener is bound to (resolves port 0).
pub fn socket_local_port(id: u64) -> Option<u16> {
    SOCKET_LISTENERS
        .with(|m| m.borrow().get(&id).and_then(|l| l.local_addr().ok()))
        .map(|a| a.port())
}

/// Close and forget a listening socket.
pub fn socket_close_listener(id: u64) {
    SOCKET_LISTENERS.with(|m| {
        m.borrow_mut().remove(&id);
    });
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
            .map_err(|e| EgclError::FileError(format!("socket-connect {host}:{port}: {e}")))?;
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
    .map_err(|e| EgclError::FileError(format!("socket-connect {host}:{port}: {e}")))?;
    socket_stream(connected)
}

/// Accept a connection on listener `id`, returning a bidirectional octet
/// stream over the new socket (blocks until a client connects).
pub fn socket_accept(id: u64) -> Result<EgclVal, EgclError> {
    let stream = SOCKET_LISTENERS.with(|m| {
        let map = m.borrow();
        let listener = map.get(&id).ok_or_else(|| {
            EgclError::FileError("socket-accept: unknown or closed listener".into())
        })?;
        listener
            .accept()
            .map(|(s, _)| s)
            .map_err(|e| EgclError::FileError(format!("socket-accept: {e}")))
    })?;
    socket_stream(stream)
}

/// Whether a listener has a connection waiting, without accepting it.
///
/// `socket_accept` blocks, and a listener is an id into a map rather than a
/// stream, so `stream_wait_for_input` cannot see it. Without this a program with
/// its own event loop -- a UI serving slynk between frames, say -- has no way to
/// offer a REPL without stalling on accept until somebody connects.
pub fn socket_listener_ready(id: u64, timeout_ms: i32) -> Result<bool, EgclError> {
    SOCKET_LISTENERS.with(|m| {
        let map = m.borrow();
        let listener = map.get(&id).ok_or_else(|| {
            EgclError::FileError("socket-listener-ready: unknown or closed listener".into())
        })?;
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use egcl_rt::syscall::{POLLERR, POLLHUP, POLLIN, POLLNVAL, PollFd};
            let mut descriptor = PollFd {
                fd: listener.as_raw_fd(),
                events: POLLIN,
                revents: 0,
            };
            // SAFETY: the listener is alive for this borrow and owns the fd.
            let count = unsafe { egcl_rt::syscall::poll(&mut descriptor, 1, timeout_ms) }
                .map_err(|errno| {
                    EgclError::FileError(format!("socket-listener-ready: errno {errno}"))
                })?;
            if descriptor.revents & POLLNVAL != 0 {
                return Err(EgclError::FileError(
                    "socket-listener-ready: invalid descriptor".into(),
                ));
            }
            Ok(count > 0 && descriptor.revents & (POLLIN | POLLHUP | POLLERR) != 0)
        }
        #[cfg(not(unix))]
        {
            // Fall back to a non-blocking accept probe, restoring the mode.
            let _ = timeout_ms;
            listener.set_nonblocking(true).ok();
            let ready = match listener.accept() {
                Ok(_) => true,
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
                Err(_) => true,
            };
            listener.set_nonblocking(false).ok();
            Ok(ready)
        }
    })
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
