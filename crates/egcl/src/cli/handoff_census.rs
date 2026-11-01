// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Compiled-code → tree-walker handoff census (bliss-gg3i1.7.15.7).
//!
//! `EGCL_HANDOFF_CENSUS=<path>` counts every place where bytecode-compiled code
//! hands a lambda or form to the tree-walker, keyed by the handoff kind and the
//! reason the lowerer chose it, and writes the table to `<path>` at exit.
//!
//! Keys are built from a bounded, read-only sketch: nothing here allocates on
//! the Lisp heap, so it cannot move or orphan a value. When the variable is
//! unset each probe costs one relaxed atomic load.

use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use egcl_rt::value::EgclVal;

static STATE: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 off, 2 on
static COUNTS: Mutex<Option<HashMap<String, u64>>> = Mutex::new(None);
/// Ids of tree-walker closures that compiled code created through a host
/// handoff, so their later interpreted calls can be attributed to it.
static HANDOFF_CLOSURES: Mutex<Option<HashSet<u64>>> = Mutex::new(None);

#[inline]
pub(crate) fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os("EGCL_HANDOFF_CENSUS").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

fn sketch_into(out: &mut String, v: EgclVal, depth: usize) {
    if out.len() > 100 {
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

/// A short printed sketch of `form` for a census key.
pub(crate) fn sketch(form: EgclVal) -> String {
    let mut s = String::new();
    sketch_into(&mut s, form, 3);
    s
}

/// Count one occurrence of `key`.
pub(crate) fn note(key: String) {
    if let Ok(mut g) = COUNTS.lock() {
        *g.get_or_insert_with(HashMap::new).entry(key).or_insert(0) += 1;
    }
}

/// Record a runtime host handoff of `form` and, when its value is a tree-walker
/// closure, remember the closure so its later calls are attributed to it.
pub(crate) fn note_runtime(kind: &str, form: EgclVal, value: Option<EgclVal>) {
    note(format!("run:{kind}"));
    note(format!("run:{kind} {}", sketch(form)));
    if let Some(value) = value
        && super::is_closure_cons(value)
        && let id = super::cp(value).1.as_fixnum() as u64
        && let Ok(mut g) = HANDOFF_CLOSURES.lock()
    {
        g.get_or_insert_with(HashSet::new).insert(id);
    }
}

/// Whether the tree-walker closure `id` was created by a host handoff.
pub(crate) fn from_handoff(id: u64) -> bool {
    HANDOFF_CLOSURES
        .lock()
        .ok()
        .is_some_and(|g| g.as_ref().is_some_and(|s| s.contains(&id)))
}

/// Write the census (sorted, descending) to `EGCL_HANDOFF_CENSUS`. Called from
/// main.
pub fn dump_on_exit() {
    let Some(path) = std::env::var_os("EGCL_HANDOFF_CENSUS") else {
        return;
    };
    let mut rows: Vec<(String, u64)> = COUNTS
        .lock()
        .ok()
        .and_then(|mut g| g.take())
        .unwrap_or_default()
        .into_iter()
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let mut out = String::new();
    for (k, n) in rows {
        out.push_str(&format!("{n:10} {k}\n"));
    }
    let _ = std::fs::write(path, out);
}
