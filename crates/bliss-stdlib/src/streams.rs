//! Streams — Gray streams protocol and built-in stream types.
//!
//! See spec §5.5.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

// ── Gray streams protocol ──────────────────────────────────────────

/// Gray stream operations trait. R5.23.
/// All built-in stream classes implement these operations.
pub trait GrayStream {
    /// Read one character. Returns EOF marker on end-of-stream.
    fn stream_read_char(&mut self) -> Result<BlissVal, BlissError>;

    /// Unread a character (push back).
    fn stream_unread_char(&mut self, ch: BlissVal) -> Result<(), BlissError>;

    /// Read one byte. Returns EOF marker on end-of-stream.
    fn stream_read_byte(&mut self) -> Result<BlissVal, BlissError>;

    /// Write one character.
    fn stream_write_char(&mut self, ch: BlissVal) -> Result<(), BlissError>;

    /// Write one byte.
    fn stream_write_byte(&mut self, byte: BlissVal) -> Result<(), BlissError>;

    /// Write a string.
    fn stream_write_string(
        &mut self,
        string: BlissVal,
        start: usize,
        end: Option<usize>,
    ) -> Result<(), BlissError>;

    /// Force output.
    fn stream_force_output(&mut self) -> Result<(), BlissError>;

    /// Finish output (flush all buffers).
    fn stream_finish_output(&mut self) -> Result<(), BlissError>;

    /// Clear input buffer.
    fn stream_clear_input(&mut self) -> Result<(), BlissError>;

    /// Check if input is available.
    fn stream_listen(&self) -> Result<bool, BlissError>;

    /// Get the current line number (for error reporting).
    fn stream_line_number(&self) -> Option<u64>;

    /// Get the current column number.
    fn stream_line_column(&self) -> Option<u64>;
}

// ── Stream direction ───────────────────────────────────────────────

/// Stream direction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamDirection {
    Input,
    Output,
    Io,
}

/// External format for character streams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExternalFormat {
    Utf8,
    Ascii,
    Latin1,
    Utf16,
    Utf32,
}

// ── Stream constructors ────────────────────────────────────────────

/// Open a file stream (CL `OPEN`). R5.24.
pub fn open(
    pathname: BlissVal,
    direction: StreamDirection,
    element_type: BlissVal,
    if_exists: BlissVal,
    if_does_not_exist: BlissVal,
    external_format: ExternalFormat,
) -> Result<BlissVal, BlissError> {
    unimplemented!("open")
}

/// Close a stream (CL `CLOSE`).
pub fn close(stream: BlissVal, abort: bool) -> Result<(), BlissError> {
    unimplemented!("close")
}

/// Create a string input stream.
pub fn make_string_input_stream(
    string: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<BlissVal, BlissError> {
    unimplemented!("make_string_input_stream")
}

/// Create a string output stream.
pub fn make_string_output_stream(element_type: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("make_string_output_stream")
}

/// Get the accumulated string from a string output stream.
pub fn get_output_stream_string(stream: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("get_output_stream_string")
}

/// Create a broadcast stream (output to multiple streams).
pub fn make_broadcast_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    unimplemented!("make_broadcast_stream")
}

/// Create a concatenated stream (input from multiple streams).
pub fn make_concatenated_stream(streams: &[BlissVal]) -> Result<BlissVal, BlissError> {
    unimplemented!("make_concatenated_stream")
}

/// Create a two-way stream.
pub fn make_two_way_stream(
    input: BlissVal,
    output: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("make_two_way_stream")
}

/// Create an echo stream.
pub fn make_echo_stream(
    input: BlissVal,
    output: BlissVal,
) -> Result<BlissVal, BlissError> {
    unimplemented!("make_echo_stream")
}

/// Create a synonym stream.
pub fn make_synonym_stream(symbol: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("make_synonym_stream")
}

// ── Stream queries ─────────────────────────────────────────────────

/// Check if a stream is open.
pub fn open_stream_p(stream: BlissVal) -> bool {
    unimplemented!("open_stream_p")
}

/// Check if a stream is an input stream.
pub fn input_stream_p(stream: BlissVal) -> bool {
    unimplemented!("input_stream_p")
}

/// Check if a stream is an output stream.
pub fn output_stream_p(stream: BlissVal) -> bool {
    unimplemented!("output_stream_p")
}

/// Get the element type of a stream.
pub fn stream_element_type(stream: BlissVal) -> BlissVal {
    unimplemented!("stream_element_type")
}

// ── GrayStream free-function wrappers ─────────────────────────────
// These extract the GrayStream impl from a BlissVal stream and
// delegate to the trait methods.

/// Read one character from a stream.
pub fn stream_read_char(stream: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("stream_read_char")
}

/// Unread a character back onto a stream.
pub fn stream_unread_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_unread_char")
}

/// Read one byte from a stream.
pub fn stream_read_byte(stream: BlissVal) -> Result<BlissVal, BlissError> {
    unimplemented!("stream_read_byte")
}

/// Write one character to a stream.
pub fn stream_write_char(stream: BlissVal, ch: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_write_char")
}

/// Write one byte to a stream.
pub fn stream_write_byte(stream: BlissVal, byte: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_write_byte")
}

/// Write a string to a stream.
pub fn stream_write_string(
    stream: BlissVal,
    string: BlissVal,
    start: usize,
    end: Option<usize>,
) -> Result<(), BlissError> {
    unimplemented!("stream_write_string")
}

/// Force output on a stream.
pub fn stream_force_output(stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_force_output")
}

/// Finish output (flush all buffers) on a stream.
pub fn stream_finish_output(stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_finish_output")
}

/// Clear the input buffer of a stream.
pub fn stream_clear_input(stream: BlissVal) -> Result<(), BlissError> {
    unimplemented!("stream_clear_input")
}

/// Check if input is available on a stream.
pub fn stream_listen(stream: BlissVal) -> Result<bool, BlissError> {
    unimplemented!("stream_listen")
}

/// Get the current line number of a stream.
pub fn stream_line_number(stream: BlissVal) -> Option<u64> {
    unimplemented!("stream_line_number")
}

/// Get the current column number of a stream.
pub fn stream_line_column(stream: BlissVal) -> Option<u64> {
    unimplemented!("stream_line_column")
}

/// Create a BlissVal representing a Lisp string (helper for tests).
pub fn make_lisp_string(s: &str) -> BlissVal {
    unimplemented!("make_lisp_string")
}
