//! Streams — Gray streams protocol and built-in stream types.
//!
//! See spec §5.5.

use std::alloc::Layout;
use std::collections::HashMap;
use std::io::{Read, Seek, Write};
use std::sync::{Mutex, MutexGuard, OnceLock};

use bliss_rt::error::BlissError;
use bliss_rt::object::{type_id, ObjectHeader};
use bliss_rt::value::{BlissVal, EOF, NIL, T};

// ── Gray streams protocol ──────────────────────────────────────────

/// Gray stream operations trait. R5.23 / R5.113.
pub trait GrayStream {
    fn stream_read_char(&mut self) -> Result<BlissVal, BlissError>;
    fn stream_unread_char(&mut self, ch: BlissVal) -> Result<(), BlissError>;
    fn stream_read_byte(&mut self) -> Result<BlissVal, BlissError>;
    fn stream_write_char(&mut self, ch: BlissVal) -> Result<(), BlissError>;
    fn stream_write_byte(&mut self, byte: BlissVal) -> Result<(), BlissError>;
    fn stream_write_string(&mut self, string: BlissVal, start: usize, end: Option<usize>) -> Result<(), BlissError>;
    fn stream_force_output(&mut self) -> Result<(), BlissError>;
    fn stream_finish_output(&mut self) -> Result<(), BlissError>;
    fn stream_clear_input(&mut self) -> Result<(), BlissError>;
    fn stream_listen(&self) -> Result<bool, BlissError>;
    fn stream_line_number(&self) -> Option<u64>;
    fn stream_line_column(&self) -> Option<u64>;
    // R5.113 additional Gray protocol methods:
    fn stream_read_char_no_hang(&mut self) -> Result<BlissVal, BlissError>;
    fn stream_peek_char(&mut self) -> Result<BlissVal, BlissError>;
    fn stream_read_line(&mut self) -> Result<(BlissVal, bool), BlissError>;
    fn stream_terpri(&mut self) -> Result<(), BlissError>;
    fn stream_fresh_line(&mut self) -> Result<bool, BlissError>;
    fn stream_clear_output(&mut self) -> Result<(), BlissError>;
    fn stream_advance_to_column(&mut self, col: u64) -> Result<bool, BlissError>;
    fn stream_start_line_p(&self) -> bool;
    fn stream_read_sequence(&mut self, count: usize) -> Result<Vec<BlissVal>, BlissError>;
    fn stream_write_sequence(&mut self, elements: &[BlissVal]) -> Result<(), BlissError>;
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
// Callers pass these as BlissVal to open(). NIL and T are also valid.
// NIL = return nil if exists; T = default (:supersede).
pub const IF_EXISTS_ERROR_VAL: BlissVal = BlissVal(1 << 3);       // fixnum 1
pub const IF_EXISTS_SUPERSEDE_VAL: BlissVal = BlissVal(2 << 3);   // fixnum 2
pub const IF_EXISTS_APPEND_VAL: BlissVal = BlissVal(3 << 3);      // fixnum 3
pub const IF_EXISTS_OVERWRITE_VAL: BlissVal = BlissVal(4 << 3);   // fixnum 4

// ── Internal stream state ──────────────────────────────────────────

/// Heap-allocated stream object. Contains an ObjectHeader (for type checks
/// without locking) and a per-stream Mutex protecting all mutable state (R5.120).
struct StreamAlloc {
    header: ObjectHeader,
    /// Per-stream mutex — every operation spanning multiple elements is atomic
    /// under this lock (R5.120).
    state: Mutex<StreamMutableState>,
}

/// Mutable portion of stream state, protected by the per-stream mutex.
struct StreamMutableState {
    open: bool,
    element_type: StreamElementType,
    inner: StreamInner,
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
        file: std::fs::File,
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
        file: std::fs::File,
        write_buf: Vec<u8>,
        external_format: ExternalFormat,
        line: u64,
        col: u64,
    },
    FileIo {
        file: std::fs::File,
        read_buf: Vec<u8>,
        buf_pos: usize,
        buf_fill: usize,
        write_buf: Vec<u8>,
        line: u64,
        col: u64,
        unread: Option<char>,
        external_format: ExternalFormat,
    },
    Broadcast {
        streams: Vec<BlissVal>,
    },
    Concatenated {
        streams: Vec<BlissVal>,
    },
    TwoWay {
        input: BlissVal,
        output: BlissVal,
    },
    Echo {
        input: BlissVal,
        output: BlissVal,
    },
    Synonym {
        symbol: BlissVal,
    },
}

impl StreamMutableState {
    fn direction(&self) -> StreamDirection {
        match &self.inner {
            StreamInner::StringInput { .. } => StreamDirection::Input,
            StreamInner::StringOutput { .. } => StreamDirection::Output,
            StreamInner::FileInput { .. } => StreamDirection::Input,
            StreamInner::FileOutput { .. } => StreamDirection::Output,
            StreamInner::FileIo { .. } => StreamDirection::Io,
            StreamInner::Broadcast { .. } => StreamDirection::Output,
            StreamInner::Concatenated { .. } => StreamDirection::Input,
            StreamInner::TwoWay { .. } => StreamDirection::Io,
            StreamInner::Echo { .. } => StreamDirection::Io,
            StreamInner::Synonym { .. } => StreamDirection::Io,
        }
    }

    fn is_input(&self) -> bool {
        matches!(self.direction(), StreamDirection::Input | StreamDirection::Io)
    }

    fn is_output(&self) -> bool {
        matches!(self.direction(), StreamDirection::Output | StreamDirection::Io)
    }

    fn check_open(&self) -> Result<(), BlissError> {
        if !self.open {
            return Err(BlissError::StreamError("operation on closed stream".into()));
        }
        Ok(())
    }

    fn check_input(&self) -> Result<(), BlissError> {
        self.check_open()?;
        if !self.is_input() {
            return Err(BlissError::StreamError("not an input stream".into()));
        }
        Ok(())
    }

    fn check_output(&self) -> Result<(), BlissError> {
        self.check_open()?;
        if !self.is_output() {
            return Err(BlissError::StreamError("not an output stream".into()));
        }
        Ok(())
    }
}

// ── File I/O helpers ──────────────────────────────────────────────

const FILE_BUF_SIZE: usize = 8192;

/// Read a single UTF-8 char from a buffered file input, refilling buffer as needed.
fn file_read_char_buffered(
    file: &mut std::fs::File,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
    line: &mut u64,
    col: &mut u64,
    unread: &mut Option<char>,
) -> Result<BlissVal, BlissError> {
    if let Some(c) = unread.take() {
        return Ok(BlissVal::from_char(c));
    }
    let mut char_buf = [0u8; 4];
    let mut char_len = 0usize;
    loop {
        if *buf_pos >= *buf_fill {
            read_buf.resize(FILE_BUF_SIZE, 0);
            let n = file.read(&mut read_buf[..])
                .map_err(|e| BlissError::StreamError(format!("file read error: {}", e)))?;
            if n == 0 {
                if char_len > 0 {
                    return Err(BlissError::StreamError("incomplete UTF-8 sequence at EOF".into()));
                }
                return Ok(EOF);
            }
            *buf_pos = 0;
            *buf_fill = n;
        }
        char_buf[char_len] = read_buf[*buf_pos];
        char_len += 1;
        *buf_pos += 1;
        match std::str::from_utf8(&char_buf[..char_len]) {
            Ok(s) => {
                let c = s.chars().next().unwrap();
                if c == '\n' {
                    *line += 1;
                    *col = 0;
                } else {
                    *col += 1;
                }
                return Ok(BlissVal::from_char(c));
            }
            Err(e) => {
                if char_len >= 4 || e.error_len().is_some() {
                    return Err(BlissError::StreamError("invalid UTF-8 in file stream".into()));
                }
            }
        }
    }
}

/// Read a single raw byte from a buffered file input (for binary streams). Issue #10.
fn file_read_byte_raw(
    file: &mut std::fs::File,
    read_buf: &mut Vec<u8>,
    buf_pos: &mut usize,
    buf_fill: &mut usize,
) -> Result<BlissVal, BlissError> {
    if *buf_pos >= *buf_fill {
        read_buf.resize(FILE_BUF_SIZE, 0);
        let n = file.read(&mut read_buf[..])
            .map_err(|e| BlissError::StreamError(format!("file read error: {}", e)))?;
        if n == 0 {
            return Ok(EOF);
        }
        *buf_pos = 0;
        *buf_fill = n;
    }
    let byte = read_buf[*buf_pos];
    *buf_pos += 1;
    Ok(BlissVal::from_fixnum(byte as i64))
}

/// Flush a write buffer to file.
fn file_flush_write_buf(
    file: &mut std::fs::File,
    write_buf: &mut Vec<u8>,
) -> Result<(), BlissError> {
    if !write_buf.is_empty() {
        file.write_all(write_buf)
            .map_err(|e| BlissError::StreamError(format!("file write error: {}", e)))?;
        write_buf.clear();
    }
    Ok(())
}

// ── Helper: extract string as &str for character-index slicing ────

fn extract_string_str(val: BlissVal) -> Result<&'static str, BlissError> {
    let bytes = extract_string_bytes(val)?;
    std::str::from_utf8(bytes)
        .map_err(|_| BlissError::StreamError("invalid UTF-8 in string".into()))
}

// ── GrayStream implementation on StreamMutableState ────────────────

impl GrayStream for StreamMutableState {
    fn stream_read_char(&mut self) -> Result<BlissVal, BlissError> {
        self.check_input()?;
        match &mut self.inner {
            StreamInner::StringInput { chars, position, end, line, col, unread } => {
                if let Some(c) = unread.take() {
                    return Ok(BlissVal::from_char(c));
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
                Ok(BlissVal::from_char(c))
            }
            StreamInner::FileInput { file, read_buf, buf_pos, buf_fill, line, col, unread, .. } => {
                file_read_char_buffered(file, read_buf, buf_pos, buf_fill, line, col, unread)
            }
            StreamInner::FileIo { file, read_buf, buf_pos, buf_fill, write_buf, line, col, unread, .. } => {
                file_flush_write_buf(file, write_buf)?;
                file_read_char_buffered(file, read_buf, buf_pos, buf_fill, line, col, unread)
            }
            // Issue #1: Handle TwoWay and Echo as separate arms to avoid
            // borrow-checker conflict when reading from input then writing to output.
            StreamInner::TwoWay { input, .. } => {
                let inp = *input;
                crate::streams::stream_read_char(inp)
            }
            StreamInner::Echo { input, output, .. } => {
                let inp = *input;
                let out = *output;
                let result = crate::streams::stream_read_char(inp)?;
                if result != EOF {
                    crate::streams::stream_write_char(out, result)?;
                }
                Ok(result)
            }
            StreamInner::Concatenated { streams } => {
                while !streams.is_empty() {
                    let s = streams[0];
                    let result = crate::streams::stream_read_char(s)?;
                    if result != EOF {
                        return Ok(result);
                    }
                    streams.remove(0);
                }
                Ok(EOF)
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_read_char(target)
            }
            _ => Err(BlissError::StreamError("not an input stream".into())),
        }
    }

    fn stream_unread_char(&mut self, ch: BlissVal) -> Result<(), BlissError> {
        self.check_input()?;
        let c = ch.as_char();
        match &mut self.inner {
            StreamInner::StringInput { unread, line, col, .. } => {
                *unread = Some(c);
                if c == '\n' {
                    // Can't perfectly restore col after newline unread, but decrement line
                    if *line > 0 { *line -= 1; }
                    // col is unknown after unreading a newline; leave as-is (best effort)
                } else {
                    if *col > 0 { *col -= 1; }
                }
                Ok(())
            }
            StreamInner::FileInput { unread, line, col, .. }
            | StreamInner::FileIo { unread, line, col, .. } => {
                *unread = Some(c);
                if c == '\n' {
                    if *line > 0 { *line -= 1; }
                } else {
                    if *col > 0 { *col -= 1; }
                }
                Ok(())
            }
            StreamInner::TwoWay { input, .. } => {
                crate::streams::stream_unread_char(*input, ch)
            }
            StreamInner::Echo { input, .. } => {
                crate::streams::stream_unread_char(*input, ch)
            }
            StreamInner::Concatenated { streams } => {
                if let Some(&s) = streams.first() {
                    crate::streams::stream_unread_char(s, ch)
                } else {
                    Err(BlissError::StreamError("no streams in concatenated stream".into()))
                }
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_unread_char(target, ch)
            }
            _ => Err(BlissError::StreamError("not an input stream".into())),
        }
    }

    // Issue #10: For binary file streams, read a single octet (0-255).
    // For character-based streams, read a character and return its codepoint.
    fn stream_read_byte(&mut self) -> Result<BlissVal, BlissError> {
        self.check_input()?;
        match &mut self.inner {
            StreamInner::FileInput { element_type, file, read_buf, buf_pos, buf_fill, .. }
                if *element_type == StreamElementType::UnsignedByte8 =>
            {
                file_read_byte_raw(file, read_buf, buf_pos, buf_fill)
            }
            _ => {
                // Character stream fallback: read char, return codepoint.
                let ch = self.stream_read_char()?;
                if ch == EOF {
                    Ok(EOF)
                } else {
                    Ok(BlissVal::from_fixnum(ch.as_char() as i64))
                }
            }
        }
    }

    fn stream_write_char(&mut self, ch: BlissVal) -> Result<(), BlissError> {
        self.check_output()?;
        let c = ch.as_char();
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
            StreamInner::FileOutput { file, write_buf, line, col, .. } => {
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
            StreamInner::FileIo { file, write_buf, .. } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_buf.extend_from_slice(encoded.as_bytes());
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::Broadcast { streams } => {
                for &s in streams.iter() {
                    crate::streams::stream_write_char(s, ch)?;
                }
                Ok(())
            }
            StreamInner::TwoWay { output, .. } | StreamInner::Echo { output, .. } => {
                crate::streams::stream_write_char(*output, ch)
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_write_char(target, ch)
            }
            _ => Err(BlissError::StreamError("not an output stream".into())),
        }
    }

    fn stream_write_byte(&mut self, byte: BlissVal) -> Result<(), BlissError> {
        self.check_output()?;
        let b = byte.as_fixnum() as u8;
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
            StreamInner::FileOutput { file, write_buf, line, col, .. } => {
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
            StreamInner::FileIo { file, write_buf, .. } => {
                write_buf.push(b);
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::Broadcast { streams } => {
                for &s in streams.iter() {
                    crate::streams::stream_write_byte(s, byte)?;
                }
                Ok(())
            }
            StreamInner::TwoWay { output, .. } | StreamInner::Echo { output, .. } => {
                crate::streams::stream_write_byte(*output, byte)
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_write_byte(target, byte)
            }
            _ => Err(BlissError::StreamError("not an output stream".into())),
        }
    }

    // Issue #11: start/end are character indices, not byte indices.
    fn stream_write_string(&mut self, string: BlissVal, start: usize, end: Option<usize>) -> Result<(), BlissError> {
        self.check_output()?;
        // Extract string as &str so we can index by character position.
        let s = extract_string_str(string)?;
        let char_count = s.chars().count();
        let actual_end = end.unwrap_or(char_count);
        // Compute byte range from character indices.
        let byte_start = s.char_indices()
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
        match &mut self.inner {
            StreamInner::StringOutput { buffer, line, col } => {
                buffer.extend_from_slice(slice);
                for &b in slice {
                    if b == b'\n' {
                        *line += 1;
                        *col = 0;
                    } else {
                        *col += 1;
                    }
                }
                Ok(())
            }
            StreamInner::FileOutput { file, write_buf, line, col, .. } => {
                write_buf.extend_from_slice(slice);
                for &b in slice {
                    if b == b'\n' {
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
            StreamInner::FileIo { file, write_buf, .. } => {
                write_buf.extend_from_slice(slice);
                if write_buf.len() >= FILE_BUF_SIZE {
                    file_flush_write_buf(file, write_buf)?;
                }
                Ok(())
            }
            StreamInner::Broadcast { streams } => {
                for &s in streams.iter() {
                    crate::streams::stream_write_string(s, string, start, Some(actual_end))?;
                }
                Ok(())
            }
            StreamInner::TwoWay { output, .. } | StreamInner::Echo { output, .. } => {
                crate::streams::stream_write_string(*output, string, start, Some(actual_end))
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_write_string(target, string, start, Some(actual_end))
            }
            _ => Err(BlissError::StreamError("not an output stream".into())),
        }
    }

    fn stream_force_output(&mut self) -> Result<(), BlissError> {
        self.check_open()?;
        match &mut self.inner {
            StreamInner::FileOutput { file, write_buf, .. }
            | StreamInner::FileIo { file, write_buf, .. } => {
                file_flush_write_buf(file, write_buf)
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_force_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_finish_output(&mut self) -> Result<(), BlissError> {
        self.check_open()?;
        match &mut self.inner {
            StreamInner::FileOutput { file, write_buf, .. }
            | StreamInner::FileIo { file, write_buf, .. } => {
                file_flush_write_buf(file, write_buf)?;
                file.flush()
                    .map_err(|e| BlissError::StreamError(format!("flush error: {}", e)))
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_finish_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_clear_input(&mut self) -> Result<(), BlissError> {
        match &mut self.inner {
            StreamInner::FileInput { buf_pos, buf_fill, unread, .. }
            | StreamInner::FileIo { buf_pos, buf_fill, unread, .. } => {
                *buf_pos = 0;
                *buf_fill = 0;
                *unread = None;
                Ok(())
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_clear_input(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_listen(&self) -> Result<bool, BlissError> {
        self.check_open()?;
        match &self.inner {
            StreamInner::StringInput { position, end, unread, .. } => {
                Ok(unread.is_some() || *position < *end)
            }
            StreamInner::FileInput { buf_pos, buf_fill, unread, .. }
            | StreamInner::FileIo { buf_pos, buf_fill, unread, .. } => {
                Ok(unread.is_some() || *buf_pos < *buf_fill)
            }
            StreamInner::Concatenated { streams } => {
                Ok(!streams.is_empty())
            }
            StreamInner::TwoWay { input, .. } | StreamInner::Echo { input, .. } => {
                crate::streams::stream_listen(*input)
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_listen(target)
            }
            _ => Ok(false),
        }
    }

    fn stream_line_number(&self) -> Option<u64> {
        match &self.inner {
            StreamInner::StringInput { line, .. } => Some(*line),
            StreamInner::StringOutput { line, .. } => Some(*line),
            StreamInner::FileInput { line, .. } | StreamInner::FileIo { line, .. } => Some(*line),
            StreamInner::FileOutput { line, .. } => Some(*line),
            _ => None,
        }
    }

    fn stream_line_column(&self) -> Option<u64> {
        match &self.inner {
            StreamInner::StringInput { col, .. } => Some(*col),
            StreamInner::StringOutput { col, .. } => Some(*col),
            StreamInner::FileInput { col, .. } | StreamInner::FileIo { col, .. } => Some(*col),
            StreamInner::FileOutput { col, .. } => Some(*col),
            _ => None,
        }
    }

    // ── R5.113 additional Gray protocol methods ──────────────────────

    fn stream_read_char_no_hang(&mut self) -> Result<BlissVal, BlissError> {
        self.check_input()?;
        // For string streams, if exhausted return NIL.
        // For file streams, reads never block on regular files, so just call stream_read_char.
        match &self.inner {
            StreamInner::StringInput { position, end, .. } => {
                if *position >= *end {
                    return Ok(NIL);
                }
            }
            _ => {}
        }
        // For file streams (FileInput, FileIo) and all others, read_char never blocks
        // on regular files — attempt the read directly.
        self.stream_read_char()
    }

    fn stream_peek_char(&mut self) -> Result<BlissVal, BlissError> {
        self.check_input()?;
        let ch = self.stream_read_char()?;
        if ch != EOF {
            self.stream_unread_char(ch)?;
        }
        Ok(ch)
    }

    fn stream_read_line(&mut self) -> Result<(BlissVal, bool), BlissError> {
        self.check_input()?;
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

    fn stream_terpri(&mut self) -> Result<(), BlissError> {
        self.stream_write_char(BlissVal::from_char('\n'))
    }

    fn stream_fresh_line(&mut self) -> Result<bool, BlissError> {
        self.check_output()?;
        if !self.stream_start_line_p() {
            self.stream_terpri()?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    fn stream_clear_output(&mut self) -> Result<(), BlissError> {
        self.check_open()?;
        match &mut self.inner {
            StreamInner::FileOutput { write_buf, .. }
            | StreamInner::FileIo { write_buf, .. } => {
                write_buf.clear();
                Ok(())
            }
            StreamInner::StringOutput { buffer, .. } => {
                buffer.clear();
                Ok(())
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_clear_output(target)
            }
            _ => Ok(()),
        }
    }

    fn stream_advance_to_column(&mut self, target_col: u64) -> Result<bool, BlissError> {
        self.check_output()?;
        let current = self.stream_line_column().unwrap_or(0);
        if current >= target_col {
            return Ok(false);
        }
        for _ in current..target_col {
            self.stream_write_char(BlissVal::from_char(' '))?;
        }
        Ok(true)
    }

    fn stream_start_line_p(&self) -> bool {
        self.stream_line_column() == Some(0)
    }

    fn stream_read_sequence(&mut self, count: usize) -> Result<Vec<BlissVal>, BlissError> {
        self.check_input()?;
        let mut result = Vec::with_capacity(count);
        for _ in 0..count {
            let ch = self.stream_read_char()?;
            if ch == EOF {
                break;
            }
            result.push(ch);
        }
        Ok(result)
    }

    fn stream_write_sequence(&mut self, elements: &[BlissVal]) -> Result<(), BlissError> {
        self.check_output()?;
        for &elem in elements {
            if elem.is_character() {
                self.stream_write_char(elem)?;
            } else if elem.is_fixnum() {
                self.stream_write_byte(elem)?;
            } else {
                return Err(BlissError::StreamError("invalid element in write-sequence".into()));
            }
        }
        Ok(())
    }

    fn interactive_stream_p(&self) -> bool {
        // File streams connected to a terminal could be interactive;
        // string/composite streams are never interactive.
        false
    }

    fn stream_external_format(&self) -> ExternalFormat {
        match &self.inner {
            StreamInner::FileInput { external_format, .. } => external_format.clone(),
            StreamInner::FileOutput { external_format, .. } => external_format.clone(),
            StreamInner::FileIo { external_format, .. } => external_format.clone(),
            _ => ExternalFormat::Utf8,
        }
    }
}

// ── String allocation ─────────────────────────────────────────────

fn string_intern_table() -> &'static Mutex<HashMap<Vec<u8>, BlissVal>> {
    static TABLE: OnceLock<Mutex<HashMap<Vec<u8>, BlissVal>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Create a BlissVal representing a Lisp string.
/// Strings are interned so equal content produces equal BlissVal.
/// For mutable or identity-sensitive strings, use `make_lisp_string_fresh`.
pub fn make_lisp_string(s: &str) -> BlissVal {
    let bytes = s.as_bytes();
    let mut table = string_intern_table().lock().unwrap();
    if let Some(&val) = table.get(bytes) {
        return val;
    }
    let ptr = alloc_string_object(bytes);
    let val = unsafe { BlissVal::from_heap_ptr(ptr) };
    table.insert(bytes.to_vec(), val);
    val
}

/// Create a fresh (non-interned) BlissVal string.
/// Two calls with the same content produce distinct BlissVal objects,
/// preserving identity semantics for mutable strings. Issue #6.
pub fn make_lisp_string_fresh(s: &str) -> BlissVal {
    let ptr = alloc_string_object(s.as_bytes());
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

fn alloc_string_object(bytes: &[u8]) -> *mut u8 {
    let total = 16 + bytes.len(); // header(8) + length(8) + bytes
    let padded = (total + 7) & !7;
    let layout = Layout::from_size_align(padded, 8).unwrap();
    unsafe {
        let ptr = std::alloc::alloc_zeroed(layout);
        let header = ObjectHeader::new(type_id::SIMPLE_BASE_STRING, (padded / 8) as u16);
        *(ptr as *mut ObjectHeader) = header;
        *((ptr as *mut u64).add(1)) = bytes.len() as u64;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.add(16), bytes.len());
        ptr
    }
}

fn extract_string_bytes(val: BlissVal) -> Result<&'static [u8], BlissError> {
    if !val.is_heap_object() {
        return Err(BlissError::TypeError { datum: val, expected: "string".into() });
    }
    unsafe {
        let ptr = val.as_ptr();
        let header = *(ptr as *const ObjectHeader);
        if header.type_id() != type_id::SIMPLE_BASE_STRING {
            return Err(BlissError::TypeError { datum: val, expected: "string".into() });
        }
        let len = *((ptr as *const u64).add(1)) as usize;
        Ok(std::slice::from_raw_parts(ptr.add(16), len))
    }
}

// ── Synonym stream resolution ─────────────────────────────────────

fn synonym_table() -> &'static Mutex<HashMap<u64, BlissVal>> {
    static TABLE: OnceLock<Mutex<HashMap<u64, BlissVal>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bind a symbol to a stream value for synonym stream resolution.
pub fn set_symbol_stream(symbol: BlissVal, stream: BlissVal) {
    let mut table = synonym_table().lock().unwrap();
    table.insert(symbol.0, stream);
}

/// Remove a symbol-to-stream binding.
pub fn remove_symbol_stream(symbol: BlissVal) {
    let mut table = synonym_table().lock().unwrap();
    table.remove(&symbol.0);
}

/// Resolve a synonym symbol to its target stream.
fn resolve_synonym(symbol: BlissVal) -> Result<BlissVal, BlissError> {
    let table = synonym_table().lock().unwrap();
    table.get(&symbol.0)
        .copied()
        .ok_or_else(|| BlissError::StreamError(
            "synonym stream: symbol has no stream binding".into()
        ))
}

// ── Stream allocation helpers ──────────────────────────────────────

fn alloc_stream(element_type: StreamElementType, inner: StreamInner) -> BlissVal {
    let state = Box::new(StreamAlloc {
        header: ObjectHeader::new(type_id::STREAM, 1),
        state: Mutex::new(StreamMutableState {
            open: true,
            element_type,
            inner,
        }),
    });
    // SAFETY: Box<StreamAlloc> is at least 8-byte aligned (contains u64 fields),
    // matching the TAG_HEAP_OBJECT alignment requirement. The pointer is stored
    // tagged; `as_ptr()` correctly strips the 3-bit tag via `self.0 & !TAG_MASK`.
    // Issue #9: verified alignment and tag-stripping correctness.
    let ptr = Box::into_raw(state) as *mut u8;
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

/// Get a reference to the StreamAlloc from a BlissVal.
///
/// SAFETY: The returned reference is valid as long as the stream has not been
/// finalized. The caller must ensure the BlissVal was created by `alloc_stream`.
fn get_stream_alloc(stream: BlissVal) -> Result<&'static StreamAlloc, BlissError> {
    if !stream.is_heap_object() {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    unsafe {
        let ptr = stream.as_ptr() as *const StreamAlloc;
        if (*ptr).header.type_id() != type_id::STREAM {
            return Err(BlissError::StreamError("not a stream".into()));
        }
        Ok(&*ptr)
    }
}

/// Lock the per-stream mutex and return a guard. R5.120.
fn lock_stream(stream: BlissVal) -> Result<MutexGuard<'static, StreamMutableState>, BlissError> {
    let alloc = get_stream_alloc(stream)?;
    Ok(alloc.state.lock().unwrap())
}

// ── Stream constructors ────────────────────────────────────────────

pub fn open(
    pathname: BlissVal,
    direction: StreamDirection,
    element_type_val: BlissVal,
    if_exists: BlissVal,
    if_does_not_exist: BlissVal,
    external_format: ExternalFormat,
) -> Result<BlissVal, BlissError> {
    if pathname == NIL {
        return Err(BlissError::FileError("NIL is not a valid pathname".into()));
    }

    // Issue #5: Determine element type from the element_type parameter.
    // NIL or T defaults to Character. Fixnum 8 signals (unsigned-byte 8).
    let elt = if element_type_val == BlissVal::from_fixnum(8) {
        StreamElementType::UnsignedByte8
    } else {
        StreamElementType::Character
    };

    let path_bytes = extract_string_bytes(pathname)
        .map_err(|_| BlissError::FileError("pathname must be a string".into()))?;
    let path_str = std::str::from_utf8(path_bytes)
        .map_err(|_| BlissError::FileError("pathname contains invalid UTF-8".into()))?;
    let path = std::path::Path::new(path_str);

    match direction {
        StreamDirection::Input => {
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    if if_does_not_exist == NIL {
                        return Ok(NIL);
                    }
                    return Err(BlissError::FileError(format!("file not found: {}", path_str)));
                }
                Err(e) => {
                    return Err(BlissError::FileError(format!("cannot open file: {}", e)));
                }
            };
            Ok(alloc_stream(elt, StreamInner::FileInput {
                file,
                read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                buf_pos: 0,
                buf_fill: 0,
                line: 0,
                col: 0,
                unread: None,
                external_format,
                element_type: elt,
            }))
        }
        StreamDirection::Output => {
            // Issue #8: Handle if_exists variants including :append.
            let file = if path.exists() {
                if if_exists == NIL {
                    return Ok(NIL);
                } else if if_exists == IF_EXISTS_APPEND_VAL {
                    // :append — open for writing at end
                    std::fs::OpenOptions::new()
                        .write(true)
                        .append(true)
                        .open(path)
                        .map_err(|e| BlissError::FileError(format!("cannot open file for append: {}", e)))?
                } else if if_exists == IF_EXISTS_OVERWRITE_VAL {
                    // :overwrite — open for writing without truncating
                    std::fs::OpenOptions::new()
                        .write(true)
                        .open(path)
                        .map_err(|e| BlissError::FileError(format!("cannot open file for overwrite: {}", e)))?
                } else if if_exists == IF_EXISTS_ERROR_VAL {
                    return Err(BlissError::FileError(format!("file already exists: {}", path_str)));
                } else {
                    // Default (:supersede / T / any other value) — truncate and create
                    std::fs::File::create(path)
                        .map_err(|e| BlissError::FileError(format!("cannot create file: {}", e)))?
                }
            } else {
                if if_does_not_exist == NIL {
                    return Ok(NIL);
                }
                std::fs::File::create(path)
                    .map_err(|e| BlissError::FileError(format!("cannot create file: {}", e)))?
            };
            Ok(alloc_stream(elt, StreamInner::FileOutput {
                file,
                write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                external_format,
                line: 0,
                col: 0,
            }))
        }
        StreamDirection::Io => {
            let file = if path.exists() {
                if if_exists == NIL {
                    return Ok(NIL);
                } else if if_exists == IF_EXISTS_ERROR_VAL {
                    return Err(BlissError::FileError(format!("file already exists: {}", path_str)));
                } else if if_exists == IF_EXISTS_APPEND_VAL {
                    let mut f = std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .map_err(|e| BlissError::FileError(format!("cannot open file: {}", e)))?;
                    // Seek to end for append
                    f.seek(std::io::SeekFrom::End(0))
                        .map_err(|e| BlissError::FileError(format!("cannot seek to end: {}", e)))?;
                    f
                } else if if_exists == IF_EXISTS_OVERWRITE_VAL {
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(path)
                        .map_err(|e| BlissError::FileError(format!("cannot open file: {}", e)))?
                } else {
                    // Default (:supersede / T) — truncate
                    std::fs::OpenOptions::new()
                        .read(true)
                        .write(true)
                        .truncate(true)
                        .open(path)
                        .map_err(|e| BlissError::FileError(format!("cannot open file: {}", e)))?
                }
            } else {
                if if_does_not_exist == NIL {
                    return Ok(NIL);
                }
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .open(path)
                    .map_err(|e| BlissError::FileError(format!("cannot open file: {}", e)))?
            };
            Ok(alloc_stream(elt, StreamInner::FileIo {
                file,
                read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                buf_pos: 0,
                buf_fill: 0,
                write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                line: 0,
                col: 0,
                unread: None,
                external_format,
            }))
        }
    }
}

pub fn close(stream: BlissVal, abort: bool) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    // Flush write buffers before closing (unless abort)
    if guard.open && !abort {
        match &mut guard.inner {
            StreamInner::FileOutput { file, write_buf, .. }
            | StreamInner::FileIo { file, write_buf, .. } => {
                let _ = file_flush_write_buf(file, write_buf);
            }
            _ => {}
        }
    }
    guard.open = false;
    Ok(())
}

pub fn make_string_input_stream(
    string: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<BlissVal, BlissError> {
    let bytes = extract_string_bytes(string)?;
    let s = std::str::from_utf8(bytes)
        .map_err(|_| BlissError::StreamError("invalid UTF-8 in string".into()))?;
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    if start > len {
        return Err(BlissError::StreamError("start beyond string length".into()));
    }
    let actual_end = end.unwrap_or(len);
    if actual_end > len {
        return Err(BlissError::StreamError("end beyond string length".into()));
    }
    if start > actual_end {
        return Err(BlissError::StreamError("start greater than end".into()));
    }
    Ok(alloc_stream(StreamElementType::Character, StreamInner::StringInput {
        chars,
        position: start,
        end: actual_end,
        line: 0,
        col: 0,
        unread: None,
    }))
}

pub fn make_string_output_stream(_element_type: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::StringOutput { buffer: Vec::new(), line: 0, col: 0 }))
}

pub fn get_output_stream_string(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    match &mut guard.inner {
        StreamInner::StringOutput { buffer, col, line } => {
            let s = std::str::from_utf8(buffer)
                .map_err(|_| BlissError::StreamError("invalid UTF-8 in output buffer".into()))?;
            let result = make_lisp_string(s);
            buffer.clear();
            *col = 0;
            *line = 0;
            Ok(result)
        }
        _ => Err(BlissError::StreamError("not a string output stream".into())),
    }
}

pub fn make_broadcast_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::Broadcast { streams: streams.to_vec() }))
}

pub fn make_concatenated_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::Concatenated { streams: streams.to_vec() }))
}

pub fn make_two_way_stream(input: BlissVal, output: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::TwoWay { input, output }))
}

pub fn make_echo_stream(input: BlissVal, output: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::Echo { input, output }))
}

pub fn make_synonym_stream(symbol: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamElementType::Character, StreamInner::Synonym { symbol }))
}

// ── Stream queries ─────────────────────────────────────────────────

pub fn open_stream_p(stream: BlissVal) -> bool {
    match lock_stream(stream) {
        Ok(guard) => guard.open,
        Err(_) => false,
    }
}

pub fn input_stream_p(stream: BlissVal) -> bool {
    match lock_stream(stream) {
        Ok(guard) => guard.is_input(),
        Err(_) => false,
    }
}

pub fn output_stream_p(stream: BlissVal) -> bool {
    match lock_stream(stream) {
        Ok(guard) => guard.is_output(),
        Err(_) => false,
    }
}

pub fn stream_element_type(stream: BlissVal) -> BlissVal {
    match lock_stream(stream) {
        Ok(guard) => {
            match guard.element_type {
                StreamElementType::Character => T,
                StreamElementType::UnsignedByte8 => BlissVal::from_fixnum(8),
            }
        }
        Err(_) => NIL,
    }
}

// ── GrayStream free-function wrappers ─────────────────────────────
// Each wrapper acquires the per-stream mutex (R5.120).

pub fn stream_read_char(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_read_char()
}

pub fn stream_unread_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_unread_char(ch)
}

pub fn stream_read_byte(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_read_byte()
}

pub fn stream_write_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_write_char(ch)
}

pub fn stream_write_byte(stream: BlissVal, byte: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_write_byte(byte)
}

pub fn stream_write_string(
    stream: BlissVal,
    string: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_write_string(string, start, end)
}

pub fn stream_force_output(stream: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_force_output()
}

pub fn stream_finish_output(stream: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_finish_output()
}

pub fn stream_clear_input(stream: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_clear_input()
}

pub fn stream_listen(stream: BlissVal) -> Result<bool, BlissError> {
    let guard = lock_stream(stream)?;
    guard.stream_listen()
}

pub fn stream_line_number(stream: BlissVal) -> Option<u64> {
    match lock_stream(stream) {
        Ok(guard) => guard.stream_line_number(),
        Err(_) => None,
    }
}

pub fn stream_line_column(stream: BlissVal) -> Option<u64> {
    match lock_stream(stream) {
        Ok(guard) => guard.stream_line_column(),
        Err(_) => None,
    }
}

// ── R5.113 additional Gray protocol free-function wrappers ────────

pub fn stream_read_char_no_hang(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_read_char_no_hang()
}

pub fn stream_peek_char(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_peek_char()
}

pub fn stream_read_line(stream: BlissVal) -> Result<(BlissVal, bool), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_read_line()
}

pub fn stream_terpri(stream: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_terpri()
}

pub fn stream_fresh_line(stream: BlissVal) -> Result<bool, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_fresh_line()
}

pub fn stream_clear_output(stream: BlissVal) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_clear_output()
}

pub fn stream_advance_to_column(stream: BlissVal, col: u64) -> Result<bool, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_advance_to_column(col)
}

pub fn stream_start_line_p(stream: BlissVal) -> bool {
    match lock_stream(stream) {
        Ok(guard) => guard.stream_start_line_p(),
        Err(_) => false,
    }
}

pub fn stream_read_sequence(stream: BlissVal, count: usize) -> Result<Vec<BlissVal>, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_read_sequence(count)
}

pub fn stream_write_sequence(stream: BlissVal, elements: &[BlissVal]) -> Result<(), BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.stream_write_sequence(elements)
}

pub fn interactive_stream_p(stream: BlissVal) -> bool {
    match lock_stream(stream) {
        Ok(guard) => guard.interactive_stream_p(),
        Err(_) => false,
    }
}

pub fn stream_external_format(stream: BlissVal) -> ExternalFormat {
    match lock_stream(stream) {
        Ok(guard) => guard.stream_external_format(),
        Err(_) => ExternalFormat::Utf8,
    }
}

// ── R5.124: file-position and file-length ─────────────────────────

/// Return the current file position, or `NIL` for non-positionable streams. R5.124.
pub fn file_position(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.check_open()?;
    match &mut guard.inner {
        StreamInner::FileInput { file, buf_pos, buf_fill, .. } => {
            // The OS position is ahead of our logical position by the buffered-but-unread bytes.
            let os_pos = file.stream_position()
                .map_err(|e| BlissError::StreamError(format!("file-position error: {}", e)))?;
            let buffered_unread = (*buf_fill - *buf_pos) as u64;
            Ok(BlissVal::from_fixnum((os_pos - buffered_unread) as i64))
        }
        StreamInner::FileOutput { file, write_buf, .. } => {
            let os_pos = file.stream_position()
                .map_err(|e| BlissError::StreamError(format!("file-position error: {}", e)))?;
            let pending = write_buf.len() as u64;
            Ok(BlissVal::from_fixnum((os_pos + pending) as i64))
        }
        StreamInner::FileIo { file, buf_pos, buf_fill, write_buf, .. } => {
            let os_pos = file.stream_position()
                .map_err(|e| BlissError::StreamError(format!("file-position error: {}", e)))?;
            let buffered_unread = (*buf_fill - *buf_pos) as u64;
            let pending_write = write_buf.len() as u64;
            Ok(BlissVal::from_fixnum((os_pos - buffered_unread + pending_write) as i64))
        }
        StreamInner::StringInput { position, .. } => {
            Ok(BlissVal::from_fixnum(*position as i64))
        }
        StreamInner::StringOutput { buffer, .. } => {
            Ok(BlissVal::from_fixnum(buffer.len() as i64))
        }
        StreamInner::Synonym { symbol } => {
            let target = resolve_synonym(*symbol)?;
            file_position(target)
        }
        _ => Ok(NIL), // non-positionable
    }
}

/// Set the file position. Returns T on success, NIL if not positionable. R5.124.
pub fn set_file_position(stream: BlissVal, position: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.check_open()?;
    match &mut guard.inner {
        StreamInner::FileInput { file, buf_pos, buf_fill, unread, .. } => {
            // Invalidate read buffer on seek.
            *buf_pos = 0;
            *buf_fill = 0;
            *unread = None;
            let pos = position.as_fixnum() as u64;
            file.seek(std::io::SeekFrom::Start(pos))
                .map_err(|e| BlissError::StreamError(format!("set-file-position error: {}", e)))?;
            Ok(T)
        }
        StreamInner::FileOutput { file, write_buf, .. } => {
            file_flush_write_buf(file, write_buf)?;
            let pos = position.as_fixnum() as u64;
            file.seek(std::io::SeekFrom::Start(pos))
                .map_err(|e| BlissError::StreamError(format!("set-file-position error: {}", e)))?;
            Ok(T)
        }
        StreamInner::FileIo { file, read_buf: _, buf_pos, buf_fill, write_buf, unread, .. } => {
            file_flush_write_buf(file, write_buf)?;
            *buf_pos = 0;
            *buf_fill = 0;
            *unread = None;
            let pos = position.as_fixnum() as u64;
            file.seek(std::io::SeekFrom::Start(pos))
                .map_err(|e| BlissError::StreamError(format!("set-file-position error: {}", e)))?;
            Ok(T)
        }
        StreamInner::Synonym { symbol } => {
            let target = resolve_synonym(*symbol)?;
            set_file_position(target, position)
        }
        _ => Ok(NIL),
    }
}

/// Return the length of the file underlying the stream, or NIL for
/// non-positionable streams. R5.124.
pub fn file_length_fn(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let mut guard = lock_stream(stream)?;
    guard.check_open()?;
    match &mut guard.inner {
        StreamInner::FileInput { file, .. }
        | StreamInner::FileOutput { file, .. }
        | StreamInner::FileIo { file, .. } => {
            let metadata = file.metadata()
                .map_err(|e| BlissError::StreamError(format!("file-length error: {}", e)))?;
            Ok(BlissVal::from_fixnum(metadata.len() as i64))
        }
        StreamInner::Synonym { symbol } => {
            let target = resolve_synonym(*symbol)?;
            file_length_fn(target)
        }
        _ => Ok(NIL),
    }
}

// ── Stream finalization (Issue #7) ────────────────────────────────

/// Free the stream's heap allocation. Must be called by the GC finalizer
/// when the stream object is unreachable (R5.121).
///
/// # Safety
/// The stream BlissVal must not be used after this call.
pub unsafe fn finalize_stream(stream: BlissVal) {
    if stream.is_heap_object() {
        unsafe {
            let ptr = stream.as_ptr() as *mut StreamAlloc;
            let header = (*ptr).header;
            if header.type_id() == type_id::STREAM {
                drop(Box::from_raw(ptr));
            }
        }
    }
}
