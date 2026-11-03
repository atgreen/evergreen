// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Optional census of managed native activation preparations and retirements.
//! EGCL_ACTIVATION_CENSUS names the output file; SPROF-START resets the table,
//! and SPROF-STOP writes <path>.window. Counts describe lifecycle events, not
//! CPU time. Counts cover `prepare_reusing` entries and general state recycle
//! attempts, not direct `prepare` (including OSR reservations) or native-child
//! bridge construction and retirement. Shape fields describe code
//! capabilities, not the entry path taken. Keys contain scalar metadata only;
//! no Lisp objects are retained.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(crate) struct Shape {
    pub owner: Option<u32>,
    pub t2: bool,
    pub ordinary: bool,
    pub captured: bool,
    pub self_calls: bool,
    pub osr: bool,
    pub variadic: bool,
}

static ENABLED: AtomicU8 = AtomicU8::new(0);
// Each row holds prepare_reusing entries and recycle attempts respectively.
static COUNTS: Mutex<Option<HashMap<Shape, [u64; 2]>>> = Mutex::new(None);

#[inline]
pub(crate) fn enabled() -> bool {
    match ENABLED.load(Ordering::Relaxed) {
        1 => false,
        2 => true,
        _ => initialize(),
    }
}

#[cold]
fn initialize() -> bool {
    let enabled = std::env::var_os("EGCL_ACTIVATION_CENSUS").is_some();
    ENABLED.store(if enabled { 2 } else { 1 }, Ordering::Relaxed);
    enabled
}

// The caller checks enabled before collecting the shape.
// Managed native activation entries exist only on this backend.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[cold]
pub(crate) fn note(shape: Shape, recycle: bool) {
    if let Ok(mut counts) = COUNTS.lock() {
        counts
            .get_or_insert_with(HashMap::new)
            .entry(shape)
            .or_default()[usize::from(recycle)] += 1;
    }
}

pub(crate) fn reset() {
    if enabled()
        && let Ok(mut counts) = COUNTS.lock()
    {
        *counts = None;
    }
}

fn render() -> String {
    // Release the host lock before resolving names. Name resolution never runs
    // inside the measured activation entry or retirement.
    let mut rows: Vec<_> = COUNTS
        .lock()
        .ok()
        .and_then(|counts| {
            counts
                .as_ref()
                .map(|counts| counts.iter().map(|(key, n)| (*key, *n)).collect())
        })
        .unwrap_or_default();
    rows.sort_by(|a, b| {
        b.1[0]
            .cmp(&a.1[0])
            .then(b.1[1].cmp(&a.1[1]))
            .then(a.0.cmp(&b.0))
    });
    let mut out = String::from(
        "# prepare_reusing recycle_attempts tier has_ordinary_entry has_env has_self_calls has_osr_entries variadic owner_id name\n",
    );
    for (shape, [prepare, recycle]) in rows {
        let name = shape
            .owner
            .map(|owner| {
                super::sym_name_uncached(egcl_rt::value::EgclVal::from_symbol_index(owner))
            })
            .unwrap_or_else(|| "<uninstalled>".to_owned());
        out.push_str(&format!(
            "{prepare} {recycle} {} {} {} {} {} {} {} {name}\n",
            if shape.t2 { "T2" } else { "T1" },
            shape.ordinary,
            shape.captured,
            shape.self_calls,
            shape.osr,
            shape.variadic,
            shape
                .owner
                .map(|owner| owner.to_string())
                .unwrap_or_else(|| "none".to_owned())
        ));
    }
    out
}

fn write(window: bool) {
    if !enabled() {
        return;
    }
    if let Ok(mut path) = std::env::var("EGCL_ACTIVATION_CENSUS") {
        if window {
            path.push_str(".window");
        }
        if let Err(error) = std::fs::write(&path, render()) {
            eprintln!("egcl: could not write EGCL_ACTIVATION_CENSUS to {path}: {error}");
        }
    }
}

pub(crate) fn write_window() {
    write(true);
}
pub fn maybe_write_on_exit() {
    write(false);
}
