// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Files that travel with the image.
//!
//! A library such as local-time reads host files (`/usr/share/zoneinfo/...`)
//! through Lisp streams. An image cross-built or copied to another host lands
//! where those files do not exist. `embed` reads a file on the BUILD host, now,
//! and records its bytes under the path the application will ask for at run
//! time; the file operations that go to the operating system consult this
//! registry first (bliss-vmqe0).
//!
//! Only the Lisp image sees embedded files. `open` for input, `probe-file`,
//! `truename`, `file-length`, `file-write-date`, `directory` and `load` find
//! them; the OS, foreign code and child processes do not, by decision. An
//! embedded entry shadows a real file at the same path, so behaviour does not
//! depend on the host, and it is read-only: opening it for output, renaming or
//! deleting it is a file error.
//!
//! The registry is two EQUAL hash tables in symbol value cells
//! (`EGCL-INTERNAL::*EMBEDDED-FILES*` and `*EMBEDDED-FILE-DATES*`), so it is
//! ordinary persistent data: `save-lisp-and-die` carries it, the shaker keeps
//! it as a runtime registry, and the GC sees the strings through the tables.
//! Contents are stored as `SIMPLE-BASE-STRING`s, one byte per octet, so a
//! megabyte of zoneinfo is a megabyte of image.

use crate::hashtable::{
    HashTest, MakeHashTableOptions, gethash, hash_table_entries, hash_table_p, make_hash_table,
    set_gethash,
};
use egcl_rt::error::EgclError;
use egcl_rt::symbols;
use egcl_rt::value::{EgclVal, NIL};

const CONTENTS_VAR: &str = "EGCL-INTERNAL::*EMBEDDED-FILES*";
const DATES_VAR: &str = "EGCL-INTERNAL::*EMBEDDED-FILE-DATES*";

/// The registry key for a path: absolute, with no trailing separator. The
/// same normalisation the lookups apply, so an entry embedded as `./x` is
/// found as `/cwd/x` and the other way round.
pub fn normalize(path: &str) -> String {
    let absolute = std::path::absolute(path)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| path.to_owned());
    let trimmed = absolute.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The table in VAR's value cell, if one has been created (in this process or
/// in the image this process restored).
fn table(var: &str) -> Option<EgclVal> {
    let index = symbols::find_index(var)?;
    symbols::symbol_value(index).filter(|value| hash_table_p(*value))
}

fn table_or_create(var: &str) -> Result<EgclVal, EgclError> {
    if let Some(table) = table(var) {
        return Ok(table);
    }
    let options = MakeHashTableOptions {
        test: HashTest::Equal,
        ..Default::default()
    };
    let table = make_hash_table(&options)?;
    symbols::set_symbol_value(symbols::intern(var), table);
    Ok(table)
}

/// Whether anything is embedded at all: the cheap test the file operations
/// make before building a key string.
pub fn any() -> bool {
    table(CONTENTS_VAR).is_some()
}

/// Look PATH up in VAR's table. Allocates the key string, so callers hold no
/// unrooted values across it.
fn get(var: &str, path: &str) -> Option<EgclVal> {
    table(var)?;
    let key_text = normalize(path);
    egcl_rt::rooted!(key = egcl_rt::gc::alloc_character_string(&key_text));
    // Re-fetch after the allocation: the table object may have moved.
    let table = table(var)?;
    let (value, found) = gethash(*key, table, NIL).ok()?;
    found.then_some(value)
}

/// Record BYTES as the contents of PATH, with WRITE_DATE (a universal time)
/// as its `file-write-date`. Returns the normalised path. A second embed of
/// the same path replaces the first.
pub fn embed(path: &str, bytes: &[u8], write_date: i64) -> Result<String, EgclError> {
    let key_text = normalize(path);
    egcl_rt::rooted!(key = egcl_rt::gc::alloc_character_string(&key_text));
    egcl_rt::rooted!(contents = egcl_rt::gc::alloc_base_string_bytes(bytes));
    egcl_rt::rooted!(contents_table = table_or_create(CONTENTS_VAR)?);
    set_gethash(*key, *contents_table, *contents)?;
    egcl_rt::rooted!(dates_table = table_or_create(DATES_VAR)?);
    set_gethash(*key, *dates_table, EgclVal::from_fixnum(write_date))?;
    Ok(key_text)
}

/// Whether PATH names an embedded file.
pub fn is_embedded(path: &str) -> bool {
    any() && get(CONTENTS_VAR, path).is_some()
}

/// A copy of the embedded contents of PATH.
pub fn contents(path: &str) -> Option<Vec<u8>> {
    if !any() {
        return None;
    }
    let value = get(CONTENTS_VAR, path)?;
    value.is_string().then(|| string_bytes(value))
}

/// The recorded `file-write-date` of PATH, as a universal time.
pub fn write_date(path: &str) -> Option<i64> {
    if !any() {
        return None;
    }
    let value = get(DATES_VAR, path)?;
    value.is_fixnum().then(|| value.as_fixnum())
}

/// Every embedded path, in no particular order.
pub fn paths() -> Vec<String> {
    let Some(table) = table(CONTENTS_VAR) else {
        return Vec::new();
    };
    hash_table_entries(table)
        .map(|entries| {
            entries
                .into_iter()
                .filter(|(key, _)| key.is_string())
                .map(|(key, _)| key.as_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Embedded paths directly inside DIRECTORY (an absolute directory path
/// with or without its trailing separator), as (name, is-directory) pairs:
/// what a listing of that directory should show in addition to the OS's
/// entries. Deeper entries contribute their first component as a directory.
pub fn entries_in(directory: &str) -> Vec<(String, bool)> {
    let prefix = {
        let d = normalize(directory);
        if d == "/" { d } else { format!("{d}/") }
    };
    let mut seen = std::collections::BTreeSet::new();
    for path in paths() {
        if let Some(rest) = path.strip_prefix(&prefix) {
            if rest.is_empty() {
                continue;
            }
            match rest.split_once('/') {
                Some((first, _)) => {
                    seen.insert((first.to_owned(), true));
                }
                None => {
                    seen.insert((rest.to_owned(), false));
                }
            }
        }
    }
    seen.into_iter().collect()
}

/// The octets of a contents string: one per character, as stored.
fn string_bytes(value: EgclVal) -> Vec<u8> {
    value.as_string().chars().map(|c| c as u32 as u8).collect()
}
