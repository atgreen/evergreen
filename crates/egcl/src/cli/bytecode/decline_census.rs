// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Decline census (bliss-gg3i1.7.23): `EGCL_DECLINE_CENSUS=<path>` counts the
//! calls that cross between native code and the T0 bytecode interpreter, and
//! at exit says why each function involved had no native code.
//!
//! Each thread tracks the innermost activation entered from Rust: a T0 body
//! (`run_with_binding`) or a native one (`run_callable_in_definition_scope`).
//! Native code calling native code directly enters no Rust and changes
//! nothing. A T0 entry whose enclosing activation is native is a crossing
//! INTO the interpreter, charged to the callee; a native entry whose
//! enclosing activation is T0 is a crossing OUT of it, charged to the T0
//! caller. Either way the charged function is the one running in T0.
//!
//! The report classifies each charged function by its state at exit:
//! `native-at-exit` (it crossed while warming up, or has native code yet was
//! entered in T0), `t1-declined` with the mapped T1 decline reason,
//! `not-promoted` (never hot enough; a compile would succeed), or
//! `not-promoted-would-decline` with the reason a compile would fail. Tree-walked functions, which have no bytecode, never
//! reach T0; `EGCL_TW_CENSUS` reports those.
//!
//! When the variable is unset every hook is one relaxed load.

use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

static STATE: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 off, 2 on

const NONE: u8 = 0;
const T0: u8 = 1;
const NATIVE: u8 = 2;

thread_local! {
    static CURRENT: Cell<(u8, u32)> = const { Cell::new((NONE, 0)) };
}

/// Per charged function: crossings into T0, crossings out of T0.
static COUNTS: std::sync::Mutex<Option<HashMap<u32, (u64, u64)>>> = std::sync::Mutex::new(None);

#[inline]
fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => {
            let on = std::env::var_os("EGCL_DECLINE_CENSUS").is_some();
            STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
            on
        }
    }
}

/// Restores the enclosing activation kind on drop.
pub(super) struct Frame(Option<(u8, u32)>);

impl Drop for Frame {
    fn drop(&mut self) {
        if let Some(previous) = self.0 {
            CURRENT.with(|current| current.set(previous));
        }
    }
}

fn charge(symbol: u32, into: bool) {
    if let Ok(mut counts) = COUNTS.lock() {
        let entry = counts
            .get_or_insert_with(HashMap::new)
            .entry(symbol)
            .or_insert((0, 0));
        if into {
            entry.0 += 1;
        } else {
            entry.1 += 1;
        }
    }
}

/// A T0 activation of `symbol` begins.
#[inline]
pub(super) fn enter_t0(symbol: u32) -> Frame {
    if !enabled() {
        return Frame(None);
    }
    let previous = CURRENT.with(|current| current.replace((T0, symbol)));
    if previous.0 == NATIVE {
        charge(symbol, true);
    }
    Frame(Some(previous))
}

/// A native activation of `symbol` begins. Native activations exist only
/// where the mapped transfer does.
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
#[inline]
pub(super) fn enter_native(symbol: Option<u32>) -> Frame {
    if !enabled() {
        return Frame(None);
    }
    let previous = CURRENT.with(|current| current.replace((NATIVE, symbol.unwrap_or(0))));
    if previous.0 == T0 {
        charge(previous.1, false);
    }
    Frame(Some(previous))
}

/// Why `symbol` has no native code at exit, as a classification and detail.
fn classify(symbol: u32) -> (&'static str, String) {
    if NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&symbol)) {
        return ("native-at-exit", String::new());
    }
    let Some(body) = REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned()) else {
        return ("no-bytecode-at-exit", String::new());
    };
    let declined = T1_DECLINED.with(|declined| declined.borrow().contains(&symbol));
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let reason = native_transfer_entry::mapped_t1_decline_reason(symbol, &body);
    #[cfg(not(all(target_arch = "x86_64", target_os = "linux")))]
    let reason = format!("mapped native entry unavailable ({})", body.name);
    if declined {
        ("t1-declined", reason)
    } else if reason.ends_with("compiled on replay") {
        // Nothing would stop it compiling; it was never hot enough.
        ("not-promoted", String::new())
    } else {
        ("not-promoted-would-decline", reason)
    }
}

/// For an anonymous body, its lambda list and the first variable names it
/// uses, which usually identify the source closure.
fn origin_hint(symbol: u32) -> String {
    let Some(body) = REGISTRY.with(|registry| registry.borrow().get(&symbol).cloned()) else {
        return String::new();
    };
    if sym_label(symbol) != "CLOSURE" {
        return String::new();
    }
    let mut lambda_list = String::new();
    crate::cli::print_val(body.params_form, &mut lambda_list);
    let names: Vec<&str> = body.names.iter().take(8).map(String::as_str).collect();
    format!(" body={:?} {lambda_list} names={names:?}", body.name)
}

/// Collapse a reason's function names and numbers so equal causes group.
fn reason_key(reason: &str) -> String {
    let mut key = String::with_capacity(reason.len());
    let mut digits = false;
    for c in reason.chars() {
        if c.is_ascii_digit() {
            if !digits {
                key.push('#');
            }
            digits = true;
        } else {
            digits = false;
            key.push(c);
        }
    }
    key
}

/// Write the report to `$EGCL_DECLINE_CENSUS`. Reasons are computed for the
/// `LIMIT` most-charged functions only; the rest are summed as `unexamined`.
pub(in crate::cli) fn dump_on_exit() {
    const LIMIT: usize = 2000;
    let Some(path) = std::env::var_os("EGCL_DECLINE_CENSUS") else {
        return;
    };
    let mut rows: Vec<(u32, u64, u64)> = COUNTS
        .lock()
        .ok()
        .and_then(|mut counts| counts.take())
        .unwrap_or_default()
        .into_iter()
        .map(|(symbol, (into, out))| (symbol, into, out))
        .collect();
    rows.sort_by_key(|&(symbol, into, out)| (std::cmp::Reverse(into + out), symbol));
    let total: u64 = rows.iter().map(|&(_, into, out)| into + out).sum();
    let mut by_reason: HashMap<(&'static str, String), (u64, u64, u64)> = HashMap::new();
    let mut detail = String::new();
    for (index, &(symbol, into, out)) in rows.iter().enumerate() {
        let (class, reason) = if index < LIMIT {
            classify(symbol)
        } else {
            ("unexamined", String::new())
        };
        let group = by_reason
            .entry((class, reason_key(&reason)))
            .or_insert((0, 0, 0));
        group.0 += into + out;
        group.1 += into;
        group.2 += 1;
        if index < LIMIT {
            detail.push_str(&format!(
                "{:>12} {:>12} {:>12} {class} {}{} | {reason}\n",
                into + out,
                into,
                out,
                sym_label(symbol),
                origin_hint(symbol),
            ));
        }
    }
    let mut groups: Vec<_> = by_reason.into_iter().collect();
    groups.sort_by(|a, b| b.1.0.cmp(&a.1.0).then_with(|| a.0.cmp(&b.0)));
    let mut out = format!(
        "# decline census: {total} crossings between native code and T0, {} functions\n\
         # by reason: crossings into+out, of which into T0, functions, share, class | reason\n",
        rows.len()
    );
    for ((class, reason), (crossings, into, functions)) in &groups {
        let share = if total == 0 {
            0.0
        } else {
            100.0 * *crossings as f64 / total as f64
        };
        out.push_str(&format!(
            "{crossings:>12} {into:>12} {functions:>8} {share:6.2}% {class} | {reason}\n"
        ));
    }
    out.push_str(
        "\n# by function: crossings into+out, into T0, out of T0, class, function | reason\n",
    );
    out.push_str(&detail);
    let _ = std::fs::write(path, out);
}
