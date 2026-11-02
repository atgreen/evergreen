// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Census of named calls that leave generated or interpreted code and reach
//! the Rust runtime (bliss-gg3i1.7.15.10.2).
//!
//! `EGCL_BUILTIN_CENSUS=<path>` counts each such call keyed by callee symbol,
//! the route it took (which also names the caller's tier) and its argument
//! count, and writes the table to `<path>` at exit. While the variable is set,
//! `EGCL-EXT:SPROF-START` clears the counts and `EGCL-EXT:SPROF-STOP` writes
//! the window to `<path>.window`, so a driver can measure one phase.
//!
//! Routes:
//! - `T0`: the bytecode interpreter's general call of a non-bytecode callee;
//! - `T1-direct` / `T2-direct`: `c2i_call_builtin` from T1 and its register
//!   form from T2 (a direct builtin slot baked at compile time);
//! - `c2i`: `c2i_call_args`, the generic compiled-to-runtime call (T1/T2);
//! - `ordinary`: the ordinary named adapter's runtime veneer (`named_entry`),
//!   reached by ordinary-ABI T2 and mapped code after any leaf stub missed;
//! - `ordinary-cold`: the ordinary cold-call veneer.
//!
//! Leaf-helper hits are not counted: they never reach the runtime veneer.
//! Nothing here allocates on the Lisp heap. When the variable is unset each
//! probe costs one relaxed atomic load.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

use egcl_rt::value::EgclVal;

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) enum Route {
    T0,
    T1Direct,
    T2Direct,
    C2i,
    /// Reached only through the x86-64 Linux ordinary adapters.
    #[cfg_attr(
        not(all(target_arch = "x86_64", target_os = "linux")),
        allow(dead_code)
    )]
    Ordinary,
    #[cfg_attr(
        not(all(target_arch = "x86_64", target_os = "linux")),
        allow(dead_code)
    )]
    OrdinaryCold,
}

impl Route {
    fn label(self) -> &'static str {
        match self {
            Route::T0 => "T0",
            Route::T1Direct => "T1-direct",
            Route::T2Direct => "T2-direct",
            Route::C2i => "c2i",
            Route::Ordinary => "ordinary",
            Route::OrdinaryCold => "ordinary-cold",
        }
    }
}

type Key = (u32, Route, u8);

static STATE: AtomicU8 = AtomicU8::new(0); // 0 unknown, 1 off, 2 on
static COUNTS: Mutex<Option<HashMap<Key, u64>>> = Mutex::new(None);

#[inline]
pub(crate) fn enabled() -> bool {
    match STATE.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => initialize(),
    }
}

#[cold]
#[inline(never)]
fn initialize() -> bool {
    let on = std::env::var_os("EGCL_BUILTIN_CENSUS").is_some();
    STATE.store(if on { 2 } else { 1 }, Ordering::Relaxed);
    on
}

/// Count one call of `symbol` with `nargs` arguments through `route`. Only a
/// relaxed load and a branch are inlined into the counted paths.
#[inline(always)]
pub(crate) fn note(symbol: u32, route: Route, nargs: usize) {
    if STATE.load(Ordering::Relaxed) == 1 {
        return;
    }
    note_slow(symbol, route, nargs);
}

#[cold]
#[inline(never)]
fn note_slow(symbol: u32, route: Route, nargs: usize) {
    if !enabled() {
        return;
    }
    let nargs = u8::try_from(nargs).unwrap_or(u8::MAX);
    if let Ok(mut counts) = COUNTS.lock() {
        *counts
            .get_or_insert_with(HashMap::new)
            .entry((symbol, route, nargs))
            .or_insert(0) += 1;
    }
}

/// Clear the counts (SPROF-START while the census is on).
pub(crate) fn reset() {
    if enabled()
        && let Ok(mut counts) = COUNTS.lock()
    {
        *counts = None;
    }
}

fn kind(symbol: u32, nargs: usize) -> &'static str {
    if super::installed_function(symbol).is_some_and(egcl_rt::function::is_interpreted_function) {
        "lisp-fn"
    } else if super::direct_builtin_slot_memoized(symbol, nargs).is_some() {
        "builtin-direct"
    } else if super::generic_call::names_dispatchable_generic(symbol) {
        "generic"
    } else if super::installed_function(symbol).is_some() {
        "other-fn"
    } else {
        "builtin"
    }
}

fn render() -> String {
    let snapshot: Vec<(Key, u64)> = COUNTS
        .lock()
        .ok()
        .and_then(|counts| {
            counts
                .as_ref()
                .map(|c| c.iter().map(|(k, v)| (*k, *v)).collect())
        })
        .unwrap_or_default();
    let total: u64 = snapshot.iter().map(|(_, n)| n).sum();
    // By name (all routes and arities), then the full breakdown.
    let mut by_name: HashMap<u32, (u64, Vec<(Key, u64)>)> = HashMap::new();
    for &(key, n) in &snapshot {
        let entry = by_name.entry(key.0).or_default();
        entry.0 += n;
        entry.1.push((key, n));
    }
    let mut names: Vec<_> = by_name.into_iter().collect();
    names.sort_by(|a, b| b.1.0.cmp(&a.1.0).then(a.0.cmp(&b.0)));
    let mut out = format!("# total {total} calls, {} callees\n", names.len());
    out.push_str("# rank count pct kind name | route/nargs=count ...\n");
    for (rank, (symbol, (count, mut parts))) in names.into_iter().enumerate() {
        parts.sort_by(|a, b| b.1.cmp(&a.1));
        let name = super::sym_name_uncached(EgclVal::from_symbol_index(symbol));
        let nargs = usize::from(parts[0].0.2);
        let pct = if total == 0 {
            0.0
        } else {
            100.0 * count as f64 / total as f64
        };
        out.push_str(&format!(
            "{:4} {count:12} {pct:6.2}% {:14} {name} |",
            rank + 1,
            kind(symbol, nargs)
        ));
        for ((_, route, nargs), n) in parts {
            out.push_str(&format!(" {}/{nargs}={n}", route.label()));
        }
        out.push('\n');
    }
    out
}

fn write(path: &str) {
    if let Err(e) = std::fs::write(path, render()) {
        eprintln!("egcl: could not write EGCL_BUILTIN_CENSUS to {path}: {e}");
    }
}

/// Write the window since the last SPROF-START (SPROF-STOP while on).
pub(crate) fn write_window() {
    if !enabled() {
        return;
    }
    if let Ok(path) = std::env::var("EGCL_BUILTIN_CENSUS") {
        write(&format!("{path}.window"));
    }
}

/// Write the table at exit.
pub fn maybe_write_on_exit() {
    if !enabled() {
        return;
    }
    if let Ok(path) = std::env::var("EGCL_BUILTIN_CENSUS") {
        write(&path);
    }
}
