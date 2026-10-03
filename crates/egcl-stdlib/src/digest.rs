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

/// SHA-256 of a sequence of `(unsigned-byte 8)` values, as a fresh 32-element
/// simple vector of fixnums.
///
/// EGCL upgrades `(unsigned-byte 8)` to `T`, so the argument is an ordinary
/// vector of fixnums rather than packed bytes; each element is range-checked,
/// because a value outside 0..255 would otherwise be silently truncated into a
/// digest that looks plausible and is wrong.
pub fn sha256_of_sequence(sequence: EgclVal) -> Result<EgclVal, EgclError> {
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
    let digest = egcl_rt::digest::sha256(&bytes);
    let elements: Vec<EgclVal> = digest
        .iter()
        .map(|&b| EgclVal::from_fixnum(i64::from(b)))
        .collect();
    Ok(sequences::build_vector(&elements))
}
