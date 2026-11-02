// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Character codes that Rust text cannot hold (bliss-5fn6m).
//!
//! An EGCL character is any code below `CHAR-CODE-LIMIT` (`#x110000`). That
//! includes the surrogate codes `#xD800`-`#xDFFF`, as in SBCL: Babel's UTF-8B
//! decoder, for one, maps each undecodable octet to a character
//! `#xDC80`-`#xDCFF`. Character immediates and simple strings store the code
//! itself (strings as 8- or 32-bit units), so the data model holds every code.
//!
//! A Rust `char`, `str` or `String` cannot hold a surrogate. Code that keeps
//! Lisp text in Rust therefore uses one of two forms:
//!
//! - **Exact:** a code sequence (`u32`), or a WTF-8 byte buffer: UTF-8
//!   generalised so a surrogate is its own three-octet sequence
//!   (`ED A0 80`-`ED BF BF`). String output streams buffer WTF-8, so
//!   `WITH-OUTPUT-TO-STRING` and `GET-OUTPUT-STREAM-STRING` keep surrogates.
//! - **Lossy:** a Rust `char`/`String`, where [`char_lossy`] turns a surrogate
//!   into U+FFFD REPLACEMENT CHARACTER. This is used only where the text leaves
//!   for Rust APIs or the operating system (UTF-8 file and terminal output,
//!   symbol and pathname names, messages). It is never used for character
//!   identity: comparison, hashing and storage go through the code.

/// `CHAR-CODE-LIMIT`: one more than the largest character code.
pub const CHAR_CODE_LIMIT: u32 = 0x110000;

/// The code Rust text uses in place of a surrogate.
pub const REPLACEMENT: char = '\u{FFFD}';

/// True for the surrogate codes `#xD800`-`#xDFFF`.
#[inline(always)]
pub fn is_surrogate(code: u32) -> bool {
    (0xD800..=0xDFFF).contains(&code)
}

/// The Rust `char` for `code`, with U+FFFD in place of a surrogate (or any
/// other code Rust rejects). LOSSY; see the module documentation.
#[inline(always)]
pub fn char_lossy(code: u32) -> char {
    char::from_u32(code).unwrap_or(REPLACEMENT)
}

/// Append `code` to a WTF-8 buffer: ordinary UTF-8, except that a surrogate
/// is encoded as the three octets UTF-8 would give it if it were legal.
#[inline]
pub fn push_wtf8(buf: &mut Vec<u8>, code: u32) {
    if code < 0x80 {
        buf.push(code as u8);
    } else if code < 0x800 {
        buf.extend_from_slice(&[0xC0 | (code >> 6) as u8, 0x80 | (code & 0x3F) as u8]);
    } else if code < 0x10000 {
        buf.extend_from_slice(&[
            0xE0 | (code >> 12) as u8,
            0x80 | ((code >> 6) & 0x3F) as u8,
            0x80 | (code & 0x3F) as u8,
        ]);
    } else {
        buf.extend_from_slice(&[
            0xF0 | (code >> 18) as u8,
            0x80 | ((code >> 12) & 0x3F) as u8,
            0x80 | ((code >> 6) & 0x3F) as u8,
            0x80 | (code & 0x3F) as u8,
        ]);
    }
}

/// The codes of a WTF-8 buffer, in order. A malformed sequence (which
/// [`push_wtf8`] never produces) yields U+FFFD for its first octet.
pub fn wtf8_codes(bytes: &[u8]) -> Wtf8Codes<'_> {
    Wtf8Codes { bytes, pos: 0 }
}

/// Iterator returned by [`wtf8_codes`].
pub struct Wtf8Codes<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Iterator for Wtf8Codes<'_> {
    type Item = u32;

    #[inline]
    fn next(&mut self) -> Option<u32> {
        let bytes = self.bytes;
        let b0 = *bytes.get(self.pos)?;
        if b0 < 0x80 {
            self.pos += 1;
            return Some(b0 as u32);
        }
        let (len, init) = match b0 {
            0xC2..=0xDF => (2, (b0 & 0x1F) as u32),
            0xE0..=0xEF => (3, (b0 & 0x0F) as u32),
            0xF0..=0xF4 => (4, (b0 & 0x07) as u32),
            _ => {
                self.pos += 1;
                return Some(REPLACEMENT as u32);
            }
        };
        let tail = match bytes.get(self.pos + 1..self.pos + len) {
            Some(tail) if tail.iter().all(|b| b & 0xC0 == 0x80) => tail,
            _ => {
                self.pos += 1;
                return Some(REPLACEMENT as u32);
            }
        };
        let code = tail
            .iter()
            .fold(init, |acc, b| (acc << 6) | (b & 0x3F) as u32);
        self.pos += len;
        Some(code)
    }
}

/// A WTF-8 buffer as Rust text, with U+FFFD for each surrogate. LOSSY.
pub fn wtf8_to_string_lossy(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => wtf8_codes(bytes).map(char_lossy).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wtf8_round_trips_every_code() {
        let mut buf = Vec::new();
        for code in (0..CHAR_CODE_LIMIT).step_by(7).chain(0xD7FE..0xE002) {
            buf.clear();
            push_wtf8(&mut buf, code);
            if let Some(c) = char::from_u32(code) {
                // Identical to UTF-8 wherever UTF-8 is defined.
                assert_eq!(buf, c.to_string().into_bytes(), "{code:#x}");
            }
            assert_eq!(
                wtf8_codes(&buf).collect::<Vec<_>>(),
                vec![code],
                "{code:#x}"
            );
        }
    }

    #[test]
    fn lossy_views_replace_only_surrogates() {
        assert_eq!(char_lossy(0x41), 'A');
        assert_eq!(char_lossy(0xD800), REPLACEMENT);
        assert_eq!(char_lossy(0xDFFF), REPLACEMENT);
        assert_eq!(char_lossy(0xE000), '\u{E000}');
        let mut buf = b"a".to_vec();
        push_wtf8(&mut buf, 0xDC80);
        push_wtf8(&mut buf, 0x10FFFF);
        assert_eq!(wtf8_to_string_lossy(&buf), "a\u{FFFD}\u{10FFFF}");
    }
}
