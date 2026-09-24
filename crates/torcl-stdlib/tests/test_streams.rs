//! Tests for torcl-stdlib streams module.
//!
//! Covers: StreamDirection, ExternalFormat, GrayStream trait operations,
//! stream constructors, composite streams, stream queries, and error conditions.

use torcl_rt::value::{EOF, NIL, TorclVal};
use torcl_stdlib::streams::*;

// ── StreamDirection enum ──────────────────────────────────────────

#[test]
fn stream_direction_variants_exist() {
    let _input = StreamDirection::Input;
    let _output = StreamDirection::Output;
    let _io = StreamDirection::Io;
}

#[test]
fn stream_direction_equality() {
    assert_eq!(StreamDirection::Input, StreamDirection::Input);
    assert_eq!(StreamDirection::Output, StreamDirection::Output);
    assert_eq!(StreamDirection::Io, StreamDirection::Io);
    assert_ne!(StreamDirection::Input, StreamDirection::Output);
    assert_ne!(StreamDirection::Input, StreamDirection::Io);
    assert_ne!(StreamDirection::Output, StreamDirection::Io);
}

#[test]
fn stream_direction_clone() {
    let d = StreamDirection::Input;
    let d2 = d;
    assert_eq!(d, d2);
}

#[test]
fn stream_direction_copy() {
    let d = StreamDirection::Output;
    let d2 = d; // Copy
    assert_eq!(d, d2); // original still usable
}

#[test]
fn stream_direction_debug() {
    let s = format!("{:?}", StreamDirection::Input);
    assert!(s.contains("Input"));
    let s = format!("{:?}", StreamDirection::Output);
    assert!(s.contains("Output"));
    let s = format!("{:?}", StreamDirection::Io);
    assert!(s.contains("Io"));
}

// ── ExternalFormat enum ───────────────────────────────────────────

#[test]
fn external_format_variants_exist() {
    let _utf8 = ExternalFormat::Utf8;
    let _ascii = ExternalFormat::Ascii;
    let _latin1 = ExternalFormat::Latin1;
    let _utf16 = ExternalFormat::Utf16;
    let _utf32 = ExternalFormat::Utf32;
}

#[test]
fn external_format_equality() {
    assert_eq!(ExternalFormat::Utf8, ExternalFormat::Utf8);
    assert_eq!(ExternalFormat::Ascii, ExternalFormat::Ascii);
    assert_eq!(ExternalFormat::Latin1, ExternalFormat::Latin1);
    assert_eq!(ExternalFormat::Utf16, ExternalFormat::Utf16);
    assert_eq!(ExternalFormat::Utf32, ExternalFormat::Utf32);
    assert_ne!(ExternalFormat::Utf8, ExternalFormat::Ascii);
    assert_ne!(ExternalFormat::Utf8, ExternalFormat::Latin1);
    assert_ne!(ExternalFormat::Utf16, ExternalFormat::Utf32);
}

#[test]
fn external_format_clone() {
    let f = ExternalFormat::Utf8;
    let f2 = ExternalFormat::Utf8;
    assert_eq!(f, f2);
}

#[test]
fn external_format_debug() {
    let s = format!("{:?}", ExternalFormat::Utf8);
    assert!(s.contains("Utf8"));
    let s = format!("{:?}", ExternalFormat::Latin1);
    assert!(s.contains("Latin1"));
}

// ── GrayStream trait methods via string input stream ──────────────
// Issue 1: Test stream_read_char, stream_unread_char, stream_read_byte,
// stream_listen, stream_line_number, stream_line_column on a string input stream.

#[test]
fn string_input_stream_read_char_returns_first_char() {
    let lisp_str = make_lisp_string("hello");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let ch = stream_read_char(stream).unwrap();
    // The first character read should be 'h'.
    assert_eq!(ch, TorclVal::from_char('h'));
}

#[test]
fn string_input_stream_read_char_sequential() {
    let lisp_str = make_lisp_string("ab");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let ch1 = stream_read_char(stream).unwrap();
    let ch2 = stream_read_char(stream).unwrap();
    assert_eq!(ch1, TorclVal::from_char('a'));
    assert_eq!(ch2, TorclVal::from_char('b'));
}

#[test]
fn string_input_stream_unread_char_then_reread() {
    let lisp_str = make_lisp_string("xyz");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let ch = stream_read_char(stream).unwrap();
    assert_eq!(ch, TorclVal::from_char('x'));
    // Unread and re-read the same character.
    stream_unread_char(stream, ch).unwrap();
    let ch_again = stream_read_char(stream).unwrap();
    assert_eq!(ch_again, TorclVal::from_char('x'));
}

#[test]
fn string_input_stream_read_byte() {
    let lisp_str = make_lisp_string("A");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let byte_val = stream_read_byte(stream).unwrap();
    // 'A' is ASCII 65; the byte should be a fixnum 65.
    assert_eq!(byte_val, TorclVal::from_fixnum(65));
}

#[test]
fn string_input_stream_listen_returns_true_when_data_available() {
    let lisp_str = make_lisp_string("data");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let available = stream_listen(stream).unwrap();
    assert!(
        available,
        "stream_listen should return true when data is available"
    );
}

#[test]
fn string_input_stream_listen_returns_false_at_eof() {
    let lisp_str = make_lisp_string("");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let available = stream_listen(stream).unwrap();
    assert!(
        !available,
        "stream_listen should return false on empty stream"
    );
}

#[test]
fn string_input_stream_line_number_initial() {
    let lisp_str = make_lisp_string("hello");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    // Before any reads, line number should be 0 or 1 (implementation-defined),
    // but it must return Some.
    let ln = stream_line_number(stream);
    assert!(
        ln.is_some(),
        "stream_line_number should return Some for a string input stream"
    );
}

#[test]
fn string_input_stream_line_column_initial() {
    let lisp_str = make_lisp_string("hello");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    // Before any reads, column should be 0.
    let col = stream_line_column(stream);
    assert!(
        col.is_some(),
        "stream_line_column should return Some for a string input stream"
    );
    assert_eq!(col.unwrap(), 0);
}

#[test]
fn string_input_stream_with_start_end() {
    // Create string input stream with start=2, end=Some(5).
    // For "hello world", reading from [2..5) should yield "llo".
    let lisp_str = make_lisp_string("hello world");
    let stream = make_string_input_stream(lisp_str, 2, Some(5)).unwrap();
    let ch1 = stream_read_char(stream).unwrap();
    let ch2 = stream_read_char(stream).unwrap();
    let ch3 = stream_read_char(stream).unwrap();
    assert_eq!(ch1, TorclVal::from_char('l'));
    assert_eq!(ch2, TorclVal::from_char('l'));
    assert_eq!(ch3, TorclVal::from_char('o'));
}

#[test]
fn string_input_stream_with_start_only() {
    let lisp_str = make_lisp_string("abcde");
    let stream = make_string_input_stream(lisp_str, 3, None).unwrap();
    let ch1 = stream_read_char(stream).unwrap();
    assert_eq!(ch1, TorclVal::from_char('d'));
}

#[test]
fn string_input_stream_is_input() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    assert!(input_stream_p(stream));
}

#[test]
fn string_input_stream_is_not_output() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    assert!(!output_stream_p(stream));
}

#[test]
fn string_input_stream_is_open() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    assert!(open_stream_p(stream));
}

// ── GrayStream trait methods via string output stream ─────────────
// Issue 2: Test stream_write_char, stream_write_byte, stream_write_string,
// stream_force_output, stream_finish_output, stream_clear_input on output stream.

#[test]
fn string_output_stream_construction() {
    let stream = make_string_output_stream(NIL).unwrap();
    assert_ne!(stream, NIL);
}

#[test]
fn string_output_stream_write_char() {
    let stream = make_string_output_stream(NIL).unwrap();
    let ch = TorclVal::from_char('A');
    let result = stream_write_char(stream, ch);
    assert!(
        result.is_ok(),
        "stream_write_char should succeed on an output stream"
    );
    // Verify by extracting accumulated string.
    let output = get_output_stream_string(stream).unwrap();
    assert_ne!(output, NIL);
}

#[test]
fn string_output_stream_write_byte() {
    let stream = make_string_output_stream(NIL).unwrap();
    let byte = TorclVal::from_fixnum(66); // 'B'
    let result = stream_write_byte(stream, byte);
    assert!(
        result.is_ok(),
        "stream_write_byte should succeed on an output stream"
    );
}

#[test]
fn string_output_stream_write_string() {
    let stream = make_string_output_stream(NIL).unwrap();
    let lisp_str = make_lisp_string("hello");
    let result = stream_write_string(stream, lisp_str, 0, None);
    assert!(
        result.is_ok(),
        "stream_write_string should succeed on an output stream"
    );
}

#[test]
fn string_output_stream_force_output() {
    let stream = make_string_output_stream(NIL).unwrap();
    let result = stream_force_output(stream);
    assert!(
        result.is_ok(),
        "stream_force_output should succeed on an output stream"
    );
}

#[test]
fn string_output_stream_finish_output() {
    let stream = make_string_output_stream(NIL).unwrap();
    let result = stream_finish_output(stream);
    assert!(
        result.is_ok(),
        "stream_finish_output should succeed on an output stream"
    );
}

#[test]
fn string_output_stream_clear_input_is_noop_or_ok() {
    let stream = make_string_output_stream(NIL).unwrap();
    // clear_input on an output stream should be a no-op or return Ok.
    let result = stream_clear_input(stream);
    assert!(
        result.is_ok(),
        "stream_clear_input should be a no-op on an output stream"
    );
}

#[test]
fn string_output_stream_is_output() {
    let stream = make_string_output_stream(NIL).unwrap();
    assert!(output_stream_p(stream));
}

#[test]
fn string_output_stream_is_not_input() {
    let stream = make_string_output_stream(NIL).unwrap();
    assert!(!input_stream_p(stream));
}

#[test]
fn string_output_stream_is_open() {
    let stream = make_string_output_stream(NIL).unwrap();
    assert!(open_stream_p(stream));
}

// Issue 7: get_output_stream_string should verify content after writing.
#[test]
fn get_output_stream_string_reflects_written_content() {
    let stream = make_string_output_stream(NIL).unwrap();
    // Write known characters to the output stream.
    stream_write_char(stream, TorclVal::from_char('H')).unwrap();
    stream_write_char(stream, TorclVal::from_char('i')).unwrap();
    let result = get_output_stream_string(stream).unwrap();
    // The result should represent the string "Hi".
    // Since we're working with TorclVal, compare against a lisp string.
    let expected = make_lisp_string("Hi");
    assert_eq!(result, expected);
}

#[test]
fn get_output_stream_string_empty_stream() {
    let stream = make_string_output_stream(NIL).unwrap();
    let result = get_output_stream_string(stream).unwrap();
    // Empty output stream should yield an empty string.
    let expected = make_lisp_string("");
    assert_eq!(result, expected);
}

// ── Stream constructors: open/close ───────────────────────────────

// Issue 6: open tests must make meaningful assertions, not tautologies.
#[test]
fn open_input_stream() {
    let stream = open(
        NIL, // pathname placeholder
        StreamDirection::Input,
        NIL, // element_type
        NIL, // if_exists
        NIL, // if_does_not_exist
        ExternalFormat::Utf8,
    );
    // NIL is not a valid pathname, so open should return an error.
    assert!(stream.is_err(), "open with NIL pathname should return Err");
}

#[test]
fn open_output_stream() {
    let stream = open(
        NIL,
        StreamDirection::Output,
        NIL,
        NIL,
        NIL,
        ExternalFormat::Utf8,
    );
    // NIL is not a valid pathname, so open should return an error.
    assert!(stream.is_err(), "open with NIL pathname should return Err");
}

#[test]
fn open_io_stream() {
    let stream = open(
        NIL,
        StreamDirection::Io,
        NIL,
        NIL,
        NIL,
        ExternalFormat::Ascii,
    );
    // NIL is not a valid pathname, so open should return an error.
    assert!(stream.is_err(), "open with NIL pathname should return Err");
}

#[test]
fn open_with_various_formats() {
    for fmt in &[
        ExternalFormat::Utf8,
        ExternalFormat::Ascii,
        ExternalFormat::Latin1,
        ExternalFormat::Utf16,
        ExternalFormat::Utf32,
    ] {
        let result = open(NIL, StreamDirection::Input, NIL, NIL, NIL, fmt.clone());
        // NIL is not a valid pathname, all should fail.
        assert!(result.is_err());
    }
}

#[test]
fn close_stream_no_abort() {
    let stream = make_string_output_stream(NIL).unwrap();
    let result = close(stream, false);
    assert!(result.is_ok());
}

#[test]
fn close_stream_with_abort() {
    let stream = make_string_output_stream(NIL).unwrap();
    let result = close(stream, true);
    assert!(result.is_ok());
}

#[test]
fn close_makes_stream_not_open() {
    let stream = make_string_output_stream(NIL).unwrap();
    close(stream, false).unwrap();
    assert!(!open_stream_p(stream));
}

// ── Composite streams ─────────────────────────────────────────────

#[test]
fn broadcast_stream_empty() {
    let stream = make_broadcast_stream(&[]).unwrap();
    assert_ne!(stream, NIL);
    assert!(output_stream_p(stream));
}

#[test]
fn broadcast_stream_multiple() {
    let s1 = make_string_output_stream(NIL).unwrap();
    let s2 = make_string_output_stream(NIL).unwrap();
    let broadcast = make_broadcast_stream(&[s1, s2]).unwrap();
    assert_ne!(broadcast, NIL);
    assert!(output_stream_p(broadcast));
}

#[test]
fn concatenated_stream_construction() {
    let lisp_str = make_lisp_string("abc");
    let s1 = make_string_input_stream(lisp_str, 0, None).unwrap();
    let s2 = make_string_input_stream(lisp_str, 0, None).unwrap();
    let concat = make_concatenated_stream(&[s1, s2]).unwrap();
    assert_ne!(concat, NIL);
    assert!(input_stream_p(concat));
}

#[test]
fn concatenated_stream_empty() {
    let concat = make_concatenated_stream(&[]).unwrap();
    assert_ne!(concat, NIL);
    assert!(input_stream_p(concat));
}

#[test]
fn two_way_stream_construction() {
    let lisp_str = make_lisp_string("test");
    let input = make_string_input_stream(lisp_str, 0, None).unwrap();
    let output = make_string_output_stream(NIL).unwrap();
    let two_way = make_two_way_stream(input, output).unwrap();
    assert_ne!(two_way, NIL);
    assert!(input_stream_p(two_way));
    assert!(output_stream_p(two_way));
}

#[test]
fn echo_stream_construction() {
    let lisp_str = make_lisp_string("test");
    let input = make_string_input_stream(lisp_str, 0, None).unwrap();
    let output = make_string_output_stream(NIL).unwrap();
    let echo = make_echo_stream(input, output).unwrap();
    assert_ne!(echo, NIL);
    assert!(input_stream_p(echo));
    assert!(output_stream_p(echo));
}

#[test]
fn synonym_stream_construction() {
    let sym = TorclVal::from_symbol_index(0);
    let synonym = make_synonym_stream(sym).unwrap();
    assert_ne!(synonym, NIL);
}

// ── Stream queries ────────────────────────────────────────────────

#[test]
fn stream_element_type_for_string_stream() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let elt_type = stream_element_type(stream);
    // String streams have character element type; should not be NIL.
    assert_ne!(elt_type, NIL);
}

#[test]
fn input_stream_p_false_for_output_only() {
    let stream = make_string_output_stream(NIL).unwrap();
    assert!(!input_stream_p(stream));
}

#[test]
fn output_stream_p_false_for_input_only() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    assert!(!output_stream_p(stream));
}

#[test]
fn open_stream_p_true_for_new_stream() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    assert!(open_stream_p(stream));
}

#[test]
fn open_stream_p_false_after_close() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    close(stream, false).unwrap();
    assert!(!open_stream_p(stream));
}

// ── Error conditions ──────────────────────────────────────────────

// Issue 4: Actually attempt read/write operations and assert they error.
#[test]
fn read_from_output_only_stream_errors() {
    let stream = make_string_output_stream(NIL).unwrap();
    // Attempting to read a character from an output-only stream must error.
    let result = stream_read_char(stream);
    assert!(
        result.is_err(),
        "stream_read_char on output-only stream should return Err"
    );
}

#[test]
fn write_to_input_only_stream_errors() {
    let lisp_str = make_lisp_string("test");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    // Attempting to write a character to an input-only stream must error.
    let ch = TorclVal::from_char('x');
    let result = stream_write_char(stream, ch);
    assert!(
        result.is_err(),
        "stream_write_char on input-only stream should return Err"
    );
}

// Issue 3: Test reading past end-of-stream.
#[test]
fn read_past_end_of_stream_returns_eof() {
    let lisp_str = make_lisp_string("ab");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    // Read all available characters.
    let _ch1 = stream_read_char(stream).unwrap(); // 'a'
    let _ch2 = stream_read_char(stream).unwrap(); // 'b'
    // Next read should indicate end-of-stream (EOF value or error).
    let result = stream_read_char(stream);
    match result {
        Ok(val) => assert_eq!(val, EOF, "reading past end should return EOF sentinel"),
        Err(_) => { /* also acceptable: an error indicating end-of-stream */ }
    }
}

#[test]
fn read_byte_past_end_of_stream() {
    let lisp_str = make_lisp_string("X");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    let _byte1 = stream_read_byte(stream).unwrap(); // 'X' = 88
    // Next read_byte should indicate end-of-stream.
    let result = stream_read_byte(stream);
    match result {
        Ok(val) => assert_eq!(val, EOF, "reading byte past end should return EOF sentinel"),
        Err(_) => { /* also acceptable */ }
    }
}

#[test]
fn make_string_input_stream_invalid_start_past_end() {
    // start > end should be an error
    let lisp_str = make_lisp_string("hello");
    let result = make_string_input_stream(lisp_str, 10, Some(5));
    assert!(result.is_err());
}

#[test]
fn make_string_input_stream_start_beyond_string_length() {
    // start beyond the string length should be an error
    let lisp_str = make_lisp_string("short");
    let result = make_string_input_stream(lisp_str, usize::MAX, None);
    assert!(result.is_err());
}

#[test]
fn make_string_input_stream_end_beyond_string_length() {
    // end beyond the string length should be an error
    let lisp_str = make_lisp_string("short");
    let result = make_string_input_stream(lisp_str, 0, Some(usize::MAX));
    assert!(result.is_err());
}

// Issue 5: Actually test I/O operations on a closed stream (not just open_stream_p).
#[test]
fn read_on_closed_stream_errors() {
    let lisp_str = make_lisp_string("hello");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    close(stream, false).unwrap();
    assert!(!open_stream_p(stream));
    // Attempting to read from a closed stream should error.
    let result = stream_read_char(stream);
    assert!(
        result.is_err(),
        "stream_read_char on closed stream should return Err"
    );
}

#[test]
fn write_on_closed_stream_errors() {
    let stream = make_string_output_stream(NIL).unwrap();
    close(stream, false).unwrap();
    assert!(!open_stream_p(stream));
    // Attempting to write to a closed stream should error.
    let ch = TorclVal::from_char('x');
    let result = stream_write_char(stream, ch);
    assert!(
        result.is_err(),
        "stream_write_char on closed stream should return Err"
    );
}

#[test]
fn close_already_closed_stream() {
    let lisp_str = make_lisp_string("hello");
    let stream = make_string_input_stream(lisp_str, 0, None).unwrap();
    close(stream, false).unwrap();
    // Per CL spec, closing an already-closed stream returns successfully.
    let result = close(stream, false);
    assert!(
        result.is_ok(),
        "closing an already-closed stream should succeed per CL spec"
    );
}

#[test]
fn broadcast_stream_is_not_input() {
    let broadcast = make_broadcast_stream(&[]).unwrap();
    assert!(!input_stream_p(broadcast));
}

#[test]
fn concatenated_stream_is_not_output() {
    let concat = make_concatenated_stream(&[]).unwrap();
    assert!(!output_stream_p(concat));
}

#[test]
fn two_way_stream_is_both_input_and_output() {
    let lisp_str = make_lisp_string("test");
    let input = make_string_input_stream(lisp_str, 0, None).unwrap();
    let output = make_string_output_stream(NIL).unwrap();
    let tw = make_two_way_stream(input, output).unwrap();
    assert!(input_stream_p(tw));
    assert!(output_stream_p(tw));
}

#[test]
fn echo_stream_is_both_input_and_output() {
    let lisp_str = make_lisp_string("test");
    let input = make_string_input_stream(lisp_str, 0, None).unwrap();
    let output = make_string_output_stream(NIL).unwrap();
    let echo = make_echo_stream(input, output).unwrap();
    assert!(input_stream_p(echo));
    assert!(output_stream_p(echo));
}

#[test]
fn close_with_abort_true_discards() {
    // When abort=true, close should discard any pending output.
    let stream = make_string_output_stream(NIL).unwrap();
    let result = close(stream, true);
    assert!(result.is_ok());
    assert!(!open_stream_p(stream));
}

#[test]
fn stream_element_type_for_output_stream() {
    let stream = make_string_output_stream(NIL).unwrap();
    let elt_type = stream_element_type(stream);
    // String output streams have character element type.
    assert_ne!(elt_type, NIL);
}

#[test]
fn broadcast_stream_with_single_stream() {
    let s = make_string_output_stream(NIL).unwrap();
    let broadcast = make_broadcast_stream(&[s]).unwrap();
    assert!(output_stream_p(broadcast));
    assert!(!input_stream_p(broadcast));
}

#[test]
fn concatenated_stream_with_single_stream() {
    let lisp_str = make_lisp_string("test");
    let s = make_string_input_stream(lisp_str, 0, None).unwrap();
    let concat = make_concatenated_stream(&[s]).unwrap();
    assert!(input_stream_p(concat));
    assert!(!output_stream_p(concat));
}

#[test]
fn synonym_stream_is_valid() {
    let sym = TorclVal::from_symbol_index(1);
    let synonym = make_synonym_stream(sym).unwrap();
    assert_ne!(synonym, NIL);
    // A synonym stream should be open once created.
    assert!(open_stream_p(synonym));
}

#[test]
fn get_output_stream_string_on_non_string_output_stream_errors() {
    // Calling get_output_stream_string on a non-string-output stream should error.
    let lisp_str = make_lisp_string("test");
    let input = make_string_input_stream(lisp_str, 0, None).unwrap();
    let result = get_output_stream_string(input);
    assert!(result.is_err());
}

// ── Additional stream edge cases ─────────────────────────────────

#[test]
fn write_byte_to_input_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("abc"), 0, None).unwrap();
    assert!(stream_write_byte(s, TorclVal::from_fixnum(65)).is_err());
}

#[test]
fn write_string_to_input_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("abc"), 0, None).unwrap();
    assert!(stream_write_string(s, make_lisp_string("x"), 0, None).is_err());
}

#[test]
fn read_byte_from_output_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    assert!(stream_read_byte(s).is_err());
}

#[test]
fn unread_char_on_output_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    assert!(stream_unread_char(s, TorclVal::from_char('x')).is_err());
}

#[test]
fn element_type_for_broadcast_stream() {
    let s = make_string_output_stream(NIL).unwrap();
    let b = make_broadcast_stream(&[s]).unwrap();
    assert_ne!(stream_element_type(b), NIL);
}

#[test]
fn element_type_for_two_way_stream() {
    let inp = make_string_input_stream(make_lisp_string("t"), 0, None).unwrap();
    let out = make_string_output_stream(NIL).unwrap();
    let tw = make_two_way_stream(inp, out).unwrap();
    assert_ne!(stream_element_type(tw), NIL);
}

#[test]
fn write_string_with_start_end_substring() {
    let s = make_string_output_stream(NIL).unwrap();
    stream_write_string(s, make_lisp_string("hello world"), 6, Some(11)).unwrap();
    assert_eq!(
        get_output_stream_string(s).unwrap(),
        make_lisp_string("world")
    );
}

#[test]
fn write_string_with_start_only() {
    let s = make_string_output_stream(NIL).unwrap();
    stream_write_string(s, make_lisp_string("abcdef"), 3, None).unwrap();
    assert_eq!(
        get_output_stream_string(s).unwrap(),
        make_lisp_string("def")
    );
}

#[test]
fn force_output_on_closed_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    close(s, false).unwrap();
    assert!(stream_force_output(s).is_err());
}

#[test]
fn finish_output_on_closed_stream_errors() {
    let s = make_string_output_stream(NIL).unwrap();
    close(s, false).unwrap();
    assert!(stream_finish_output(s).is_err());
}

#[test]
fn listen_on_closed_stream_errors() {
    let s = make_string_input_stream(make_lisp_string("d"), 0, None).unwrap();
    close(s, false).unwrap();
    assert!(stream_listen(s).is_err());
}
