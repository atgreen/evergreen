// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! The Lisp-visible side of `egcl_rt::digest`.
//!
//! A portable Common Lisp SHA-256 is not viable on this engine yet: Ironclad's
//! measured ~0.012 MiB/s made signing a 6.8 MB APK take about 22 minutes
//! (bliss-omaps). Ironclad itself uses implementation-specific routines
//! wherever they exist; this is EGCL's.

use crate::sequences;
use egcl_rt::error::EgclError;
use egcl_rt::value::EgclVal;

/// The `(unsigned-byte 8)` elements of SEQUENCE as bytes.
///
/// EGCL upgrades `(unsigned-byte 8)` to `T`, so this is an ordinary vector of
/// fixnums rather than packed bytes. Every element is range-checked: a value
/// outside 0..255 would otherwise be silently truncated into a digest or
/// checksum that looks plausible and is wrong.
fn sequence_bytes(sequence: EgclVal) -> Result<Vec<u8>, EgclError> {
    let len = sequences::length(sequence)?;
    let mut bytes = Vec::with_capacity(len);
    for index in 0..len {
        let element = sequences::aref(sequence, index)?;
        let byte = if element.is_fixnum() {
            element.as_fixnum()
        } else {
            return Err(EgclError::TypeError {
                datum: element,
                expected: "an (unsigned-byte 8) element".into(),
            });
        };
        if !(0..=255).contains(&byte) {
            return Err(EgclError::TypeError {
                datum: element,
                expected: "an (unsigned-byte 8) element".into(),
            });
        }
        bytes.push(byte as u8);
    }
    Ok(bytes)
}

/// CRC-32 (the ZIP/PNG/gzip polynomial) of a byte sequence, as a fixnum.
///
/// Like SHA-256 this is a byte-at-a-time Lisp loop otherwise: it measured
/// 8641 ms over the 6.76 MB Android runtime, the largest single cost left in a
/// self-hosted APK build once hashing was native.
pub fn crc32_of_sequence(sequence: EgclVal) -> Result<EgclVal, EgclError> {
    let bytes = sequence_bytes(sequence)?;
    Ok(EgclVal::from_fixnum(i64::from(egcl_rt::digest::crc32(&bytes))))
}

/// `(egcl-ext:inflate octets &optional format)`: decompress a sequence of
/// `(unsigned-byte 8)` values holding a raw deflate stream (`:deflate`, the
/// default), a zlib stream (`:zlib`) or gzip members (`:gzip`), checking the
/// wrapper's trailers. Returns a fresh simple vector of fixnums.
pub fn inflate_sequence(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let (sequence, format) = match args {
        [sequence] => (*sequence, None),
        [sequence, format] => (*sequence, Some(*format)),
        _ => {
            return Err(EgclError::ProgramError(format!(
                "EGCL-EXT:INFLATE takes one or two arguments, got {}",
                args.len()
            )));
        }
    };
    // A symbol's name comes back package-qualified, as KEYWORD:GZIP.
    let name = |f: EgclVal| {
        let name = egcl_rt::symbols::symbol_name_of(f).unwrap_or_default();
        name.rsplit(':').next().unwrap_or_default().to_owned()
    };
    let format = match format.map(|f| (f, name(f))) {
        None => egcl_rt::container::Compression::Deflate,
        Some((_, name)) if name == "DEFLATE" => egcl_rt::container::Compression::Deflate,
        Some((_, name)) if name == "ZLIB" => egcl_rt::container::Compression::Zlib,
        Some((_, name)) if name == "GZIP" => egcl_rt::container::Compression::Gzip,
        Some((datum, _)) => {
            return Err(EgclError::TypeError {
                datum,
                expected: "(MEMBER :DEFLATE :ZLIB :GZIP)".into(),
            });
        }
    };
    let out = egcl_rt::container::decompress(format, &sequence_bytes(sequence)?)
        .map_err(|message| EgclError::StreamError(format!("EGCL-EXT:INFLATE: {message}")))?;
    let vector = sequences::build_filled_simple_vector(out.len(), EgclVal::from_fixnum(0));
    let (slots, _) = sequences::simple_vector_slots(vector).expect("a fresh simple vector");
    // Fixnums only, and nothing allocates while the slot pointer is live.
    for (index, &byte) in out.iter().enumerate() {
        unsafe { *slots.add(index) = EgclVal::from_fixnum(i64::from(byte)) };
    }
    Ok(vector)
}

/// SHA-256 of a sequence of `(unsigned-byte 8)` values, as a fresh 32-element
/// simple vector of fixnums.
pub fn sha256_of_sequence(sequence: EgclVal) -> Result<EgclVal, EgclError> {
    let digest = egcl_rt::digest::sha256(&sequence_bytes(sequence)?);
    let elements: Vec<EgclVal> = digest
        .iter()
        .map(|&b| EgclVal::from_fixnum(i64::from(b)))
        .collect();
    Ok(sequences::build_vector(&elements))
}
