// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Tree-walker census (bliss-gg3i1.7.14): `EGCL_TW_CENSUS=<path>` counts every
//! interpreted lambda application by what is being walked and writes the table
//! to `<path>` at exit. A profile can say THAT the tree-walker runs; this says
//! WHICH functions it runs and through which dispatch path, so the reason each
//! one is not compiled can be read off directly.
//!
//! Keys are built from a bounded, read-only sketch of the body: nothing here
//! allocates on the Lisp heap, so it cannot move or orphan a value.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use egcl_rt::value::EgclVal;

static STATE: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 off, 2 on
static COUNTS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
/// Why a lazy compile of each declined defun failed (first bail reason), so
/// its interpreted calls can be bucketed by the construct that blocked it.
static DECLINED: Mutex<Option<HashMap<u32, String>>> = Mutex::new(None);

pub(crate) fn note_declined(sym: u32, reason: &str) {
    if let Ok(mut g) = DECLINED.lock() {
        g.get_or_insert_with(HashMap::new)
            .insert(sym, reason.to_string());
    }
}

pub(crate) fn declined_reason(sym: u32) -> Option<String> {
    DECLINED
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|m| m.get(&sym).cloned()))
}

#[inline]
pub(crate) fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os("EGCL_TW_CENSUS").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

fn sketch_into(out: &mut String, v: EgclVal, depth: usize) {
    if out.len() > 120 {
        return;
    }
    if v.is_cons() {
        if depth == 0 {
            out.push_str("(..)");
            return;
        }
        out.push('(');
        let mut cur = v;
        let mut n = 0;
        while cur.is_cons() {
            if n > 0 {
                out.push(' ');
            }
            if n == 4 {
                out.push_str("..");
                break;
            }
            let (car, cdr) = super::cp(cur);
            sketch_into(out, car, depth - 1);
            cur = cdr;
            n += 1;
        }
        out.push(')');
    } else if v.is_nil() {
        out.push_str("NIL");
    } else if v.is_symbol() {
        out.push_str(&super::sym_name(v));
    } else if v.is_fixnum() {
        out.push_str(&v.as_fixnum().to_string());
    } else {
        out.push('#');
    }
}

/// A short printed sketch of `body` for a census key.
pub(crate) fn sketch(body: EgclVal) -> String {
    let mut s = String::new();
    sketch_into(&mut s, body, 3);
    s
}

pub(crate) fn note(key: String) {
    if let Ok(mut g) = COUNTS.lock() {
        *g.get_or_insert_with(HashMap::new).entry(key).or_insert(0) += 1;
    }
}

/// Write the census (sorted, descending) to `EGCL_TW_CENSUS`. Called from main.
pub fn dump_on_exit() {
    let Some(path) = std::env::var_os("EGCL_TW_CENSUS") else {
        return;
    };
    let mut rows: Vec<(String, u64)> = COUNTS
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .unwrap_or_default()
        .into_iter()
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1));
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let mut out = format!("total {total}\n");
    for (k, n) in rows {
        out.push_str(&format!("{n:10} {k}\n"));
    }
    let _ = std::fs::write(path, out);
}
