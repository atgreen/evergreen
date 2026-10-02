// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Byte/string conversion shared by the extension API. Decode is deliberately
//! lossy so kernel and executable metadata with malformed text remains readable.
use egcl_rt::{EgclError, EgclVal};

pub fn call(args: &[EgclVal]) -> Result<EgclVal, EgclError> {
    let [operation, source, format, terminate] = args else {
        return Err(EgclError::ProgramError(
            "invalid text codec arguments".into(),
        ));
    };
    let operation = egcl_rt::symbols::symbol_name_of(*operation).unwrap_or_default();
    let format = egcl_rt::symbols::symbol_name_of(*format).unwrap_or_default();
    let ascii = match format.rsplit(':').next().unwrap_or_default() {
        "ASCII" | "US-ASCII" => true,
        "UTF-8" | "UTF8" | "DEFAULT" => false,
        _ => {
            return Err(EgclError::ProgramError(format!(
                "unsupported external format: {format}"
            )));
        }
    };
    match operation.rsplit(':').next().unwrap_or_default() {
        "ENCODE" => {
            let mut bytes = crate::sequences::string_content_bytes(*source).ok_or_else(|| {
                EgclError::TypeError {
                    datum: *source,
                    expected: "STRING".into(),
                }
            })?;
            if ascii && !bytes.is_ascii() {
                return Err(EgclError::ProgramError(
                    "cannot encode non-ASCII characters as ASCII".into(),
                ));
            }
            if !terminate.is_nil() {
                bytes.push(0);
            }
            let values: Vec<_> = bytes
                .into_iter()
                .map(|byte| EgclVal::from_fixnum(i64::from(byte)))
                .collect();
            Ok(crate::sequences::build_simple_vector(&values))
        }
        "DECODE" => {
            egcl_rt::rooted!(source = *source);
            let length = crate::sequences::length(*source)?;
            let mut bytes = Vec::with_capacity(length);
            for index in 0..length {
                let value = crate::sequences::elt(*source, index)?;
                let byte = if value.is_fixnum() {
                    u8::try_from(value.as_fixnum()).ok()
                } else {
                    None
                };
                bytes.push(byte.ok_or_else(|| EgclError::TypeError {
                    datum: value,
                    expected: "(UNSIGNED-BYTE 8)".into(),
                })?);
            }
            let text = if ascii {
                bytes
                    .into_iter()
                    .map(|byte| {
                        if byte < 128 {
                            char::from(byte)
                        } else {
                            '\u{fffd}'
                        }
                    })
                    .collect()
            } else {
                String::from_utf8_lossy(&bytes).into_owned()
            };
            Ok(crate::streams::make_lisp_string_fresh(&text))
        }
        _ => Err(EgclError::ProgramError(
            "invalid text codec operation".into(),
        )),
    }
}
