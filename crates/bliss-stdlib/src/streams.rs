//! Streams — Gray streams protocol and built-in stream types.
//!
//! See spec §5.5.

use std::alloc::Layout;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Mutex, OnceLock};

use bliss_rt::error::BlissError;
use bliss_rt::object::{type_id, ObjectHeader};
use bliss_rt::value::{BlissVal, EOF, NIL, T};

// ── Gray streams protocol ──────────────────────────────────────────

/// Gray stream operations trait. R5.23.
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

// ── Internal stream state ──────────────────────────────────────────

struct StreamState {
    header: ObjectHeader,
    open: bool,
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
    },
    FileInput {
        file: std::fs::File,
        read_buf: Vec<u8>,
        buf_pos: usize,
        buf_fill: usize,
        line: u64,
        col: u64,
        unread: Option<char>,
        _external_format: ExternalFormat,
    },
    FileOutput {
        file: std::fs::File,
        write_buf: Vec<u8>,
        _external_format: ExternalFormat,
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
        _external_format: ExternalFormat,
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

impl StreamState {
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
    // We may need up to 4 bytes for a UTF-8 character
    let mut char_buf = [0u8; 4];
    let mut char_len = 0usize;
    loop {
        // Refill buffer if exhausted
        if *buf_pos >= *buf_fill {
            read_buf.resize(FILE_BUF_SIZE, 0);
            let n = file.read(&mut read_buf[..])
                .map_err(|e| BlissError::StreamError(format!("file read error: {}", e)))?;
            if n == 0 {
                if char_len > 0 {
                    // Incomplete UTF-8 at EOF
                    return Err(BlissError::StreamError("incomplete UTF-8 sequence at EOF".into()));
                }
                return Ok(EOF);
            }
            *buf_pos = 0;
            *buf_fill = n;
        }
        // Consume one byte
        char_buf[char_len] = read_buf[*buf_pos];
        char_len += 1;
        *buf_pos += 1;
        // Try to decode a char
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
                    // Invalid sequence
                    return Err(BlissError::StreamError("invalid UTF-8 in file stream".into()));
                }
                // Need more bytes - continue loop
            }
        }
    }
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

// ── GrayStream implementation on StreamState ───────────────────────

impl GrayStream for StreamState {
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
                // Flush write buffer before reading
                file_flush_write_buf(file, write_buf)?;
                file_read_char_buffered(file, read_buf, buf_pos, buf_fill, line, col, unread)
            }
            StreamInner::TwoWay { input, .. } | StreamInner::Echo { input, .. } => {
                let result = crate::streams::stream_read_char(*input)?;
                if let StreamInner::Echo { output, .. } = &self.inner {
                    if result != EOF {
                        crate::streams::stream_write_char(*output, result)?;
                    }
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
        match &mut self.inner {
            StreamInner::StringInput { unread, .. } => {
                *unread = Some(ch.as_char());
                Ok(())
            }
            StreamInner::FileInput { unread, .. } | StreamInner::FileIo { unread, .. } => {
                *unread = Some(ch.as_char());
                Ok(())
            }
            StreamInner::TwoWay { input, .. } | StreamInner::Echo { input, .. } => {
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

    fn stream_read_byte(&mut self) -> Result<BlissVal, BlissError> {
        self.check_input()?;
        let ch = self.stream_read_char()?;
        if ch == EOF {
            Ok(EOF)
        } else {
            Ok(BlissVal::from_fixnum(ch.as_char() as i64))
        }
    }

    fn stream_write_char(&mut self, ch: BlissVal) -> Result<(), BlissError> {
        self.check_output()?;
        let c = ch.as_char();
        match &mut self.inner {
            StreamInner::StringOutput { buffer } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                buffer.extend_from_slice(encoded.as_bytes());
                Ok(())
            }
            StreamInner::FileOutput { file, write_buf, .. } => {
                let mut buf = [0u8; 4];
                let encoded = c.encode_utf8(&mut buf);
                write_buf.extend_from_slice(encoded.as_bytes());
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
            StreamInner::StringOutput { buffer } => {
                buffer.push(b);
                Ok(())
            }
            StreamInner::FileOutput { file, write_buf, .. } => {
                write_buf.push(b);
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

    fn stream_write_string(&mut self, string: BlissVal, start: usize, end: Option<usize>) -> Result<(), BlissError> {
        self.check_output()?;
        let bytes = extract_string_bytes(string)?;
        let end = end.unwrap_or(bytes.len());
        let slice = &bytes[start..end];
        match &mut self.inner {
            StreamInner::StringOutput { buffer } => {
                buffer.extend_from_slice(slice);
                Ok(())
            }
            StreamInner::FileOutput { file, write_buf, .. } => {
                write_buf.extend_from_slice(slice);
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
                    crate::streams::stream_write_string(s, string, start, Some(end))?;
                }
                Ok(())
            }
            StreamInner::TwoWay { output, .. } | StreamInner::Echo { output, .. } => {
                crate::streams::stream_write_string(*output, string, start, Some(end))
            }
            StreamInner::Synonym { symbol } => {
                let target = resolve_synonym(*symbol)?;
                crate::streams::stream_write_string(target, string, start, Some(end))
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
            StreamInner::FileInput { line, .. } | StreamInner::FileIo { line, .. } => Some(*line),
            _ => None,
        }
    }

    fn stream_line_column(&self) -> Option<u64> {
        match &self.inner {
            StreamInner::StringInput { col, .. } => Some(*col),
            StreamInner::FileInput { col, .. } | StreamInner::FileIo { col, .. } => Some(*col),
            _ => None,
        }
    }
}

// ── String interning ───────────────────────────────────────────────

fn string_intern_table() -> &'static Mutex<HashMap<Vec<u8>, BlissVal>> {
    static TABLE: OnceLock<Mutex<HashMap<Vec<u8>, BlissVal>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Create a BlissVal representing a Lisp string.
/// Strings are interned so equal content produces equal BlissVal.
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

/// Global table mapping symbol BlissVals to their current stream value.
/// Used by synonym streams to resolve `symbol-value` to the target stream.
fn synonym_table() -> &'static Mutex<HashMap<u64, BlissVal>> {
    static TABLE: OnceLock<Mutex<HashMap<u64, BlissVal>>> = OnceLock::new();
    TABLE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Bind a symbol to a stream value for synonym stream resolution.
/// In a full CL implementation this would use the symbol's value cell;
/// here we use a global table keyed by the symbol's raw bits.
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
            format!("synonym stream: symbol has no stream binding")
        ))
}

// ── Stream allocation helpers ──────────────────────────────────────

fn alloc_stream(inner: StreamInner) -> BlissVal {
    let state = Box::new(StreamState {
        header: ObjectHeader::new(type_id::STREAM, 1),
        open: true,
        inner,
    });
    let ptr = Box::into_raw(state) as *mut u8;
    unsafe { BlissVal::from_heap_ptr(ptr) }
}

fn get_stream(stream: BlissVal) -> Result<*mut StreamState, BlissError> {
    if !stream.is_heap_object() {
        return Err(BlissError::StreamError("not a stream".into()));
    }
    unsafe {
        let ptr = stream.as_ptr() as *mut StreamState;
        if (*ptr).header.type_id() != type_id::STREAM {
            return Err(BlissError::StreamError("not a stream".into()));
        }
        Ok(ptr)
    }
}

// ── Stream constructors ────────────────────────────────────────────

pub fn open(
    pathname: BlissVal,
    direction: StreamDirection,
    _element_type: BlissVal,
    if_exists: BlissVal,
    if_does_not_exist: BlissVal,
    external_format: ExternalFormat,
) -> Result<BlissVal, BlissError> {
    if pathname == NIL {
        return Err(BlissError::FileError("NIL is not a valid pathname".into()));
    }

    // Extract pathname string
    let path_bytes = extract_string_bytes(pathname)
        .map_err(|_| BlissError::FileError("pathname must be a string".into()))?;
    let path_str = std::str::from_utf8(path_bytes)
        .map_err(|_| BlissError::FileError("pathname contains invalid UTF-8".into()))?;
    let path = std::path::Path::new(path_str);

    match direction {
        StreamDirection::Input => {
            // if_does_not_exist: NIL means return NIL, otherwise error (default)
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
            Ok(alloc_stream(StreamInner::FileInput {
                file,
                read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                buf_pos: 0,
                buf_fill: 0,
                line: 0,
                col: 0,
                unread: None,
                _external_format: external_format,
            }))
        }
        StreamDirection::Output => {
            // Handle if_exists / if_does_not_exist for output
            let file = if path.exists() {
                if if_exists == NIL {
                    // :if-exists nil => return NIL
                    return Ok(NIL);
                }
                // Default: supersede (overwrite)
                std::fs::File::create(path)
                    .map_err(|e| BlissError::FileError(format!("cannot create file: {}", e)))?
            } else {
                if if_does_not_exist == NIL {
                    return Ok(NIL);
                }
                // Default: create
                std::fs::File::create(path)
                    .map_err(|e| BlissError::FileError(format!("cannot create file: {}", e)))?
            };
            Ok(alloc_stream(StreamInner::FileOutput {
                file,
                write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                _external_format: external_format,
            }))
        }
        StreamDirection::Io => {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(if_does_not_exist != NIL)
                .open(path)
                .map_err(|e| BlissError::FileError(format!("cannot open file: {}", e)))?;
            Ok(alloc_stream(StreamInner::FileIo {
                file,
                read_buf: Vec::with_capacity(FILE_BUF_SIZE),
                buf_pos: 0,
                buf_fill: 0,
                write_buf: Vec::with_capacity(FILE_BUF_SIZE),
                line: 0,
                col: 0,
                unread: None,
                _external_format: external_format,
            }))
        }
    }
}

pub fn close(stream: BlissVal, _abort: bool) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe {
        // Flush write buffers before closing (unless abort)
        if (*ptr).open && !_abort {
            match &mut (*ptr).inner {
                StreamInner::FileOutput { file, write_buf, .. }
                | StreamInner::FileIo { file, write_buf, .. } => {
                    let _ = file_flush_write_buf(file, write_buf);
                }
                _ => {}
            }
        }
        (*ptr).open = false;
    }
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
    Ok(alloc_stream(StreamInner::StringInput {
        chars,
        position: start,
        end: actual_end,
        line: 0,
        col: 0,
        unread: None,
    }))
}

pub fn make_string_output_stream(_element_type: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::StringOutput { buffer: Vec::new() }))
}

pub fn get_output_stream_string(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let ptr = get_stream(stream)?;
    unsafe {
        match &(*ptr).inner {
            StreamInner::StringOutput { buffer } => {
                let s = std::str::from_utf8(buffer)
                    .map_err(|_| BlissError::StreamError("invalid UTF-8 in output buffer".into()))?;
                Ok(make_lisp_string(s))
            }
            _ => Err(BlissError::StreamError("not a string output stream".into())),
        }
    }
}

pub fn make_broadcast_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::Broadcast { streams: streams.to_vec() }))
}

pub fn make_concatenated_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::Concatenated { streams: streams.to_vec() }))
}

pub fn make_two_way_stream(input: BlissVal, output: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::TwoWay { input, output }))
}

pub fn make_echo_stream(input: BlissVal, output: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::Echo { input, output }))
}

pub fn make_synonym_stream(symbol: BlissVal) -> Result<BlissVal, BlissError> {
    Ok(alloc_stream(StreamInner::Synonym { symbol }))
}

// ── Stream queries ─────────────────────────────────────────────────

pub fn open_stream_p(stream: BlissVal) -> bool {
    match get_stream(stream) {
        Ok(ptr) => unsafe { (*ptr).open },
        Err(_) => false,
    }
}

pub fn input_stream_p(stream: BlissVal) -> bool {
    match get_stream(stream) {
        Ok(ptr) => unsafe { (*ptr).is_input() },
        Err(_) => false,
    }
}

pub fn output_stream_p(stream: BlissVal) -> bool {
    match get_stream(stream) {
        Ok(ptr) => unsafe { (*ptr).is_output() },
        Err(_) => false,
    }
}

pub fn stream_element_type(stream: BlissVal) -> BlissVal {
    match get_stream(stream) {
        Ok(_) => T, // character element type for all string-based streams
        Err(_) => NIL,
    }
}

// ── GrayStream free-function wrappers ─────────────────────────────

pub fn stream_read_char(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_read_char() }
}

pub fn stream_unread_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_unread_char(ch) }
}

pub fn stream_read_byte(stream: BlissVal) -> Result<BlissVal, BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_read_byte() }
}

pub fn stream_write_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_write_char(ch) }
}

pub fn stream_write_byte(stream: BlissVal, byte: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_write_byte(byte) }
}

pub fn stream_write_string(
    stream: BlissVal,
    string: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_write_string(string, start, end) }
}

pub fn stream_force_output(stream: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_force_output() }
}

pub fn stream_finish_output(stream: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_finish_output() }
}

pub fn stream_clear_input(stream: BlissVal) -> Result<(), BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_clear_input() }
}

pub fn stream_listen(stream: BlissVal) -> Result<bool, BlissError> {
    let ptr = get_stream(stream)?;
    unsafe { (*ptr).stream_listen() }
}

pub fn stream_line_number(stream: BlissVal) -> Option<u64> {
    match get_stream(stream) {
        Ok(ptr) => unsafe { (*ptr).stream_line_number() },
        Err(_) => None,
    }
}

pub fn stream_line_column(stream: BlissVal) -> Option<u64> {
    match get_stream(stream) {
        Ok(ptr) => unsafe { (*ptr).stream_line_column() },
        Err(_) => None,
    }
}
