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
//!
//! Capacity and busy-lane refusals are written separately to <path>.refusals
//! and <path>.refusals.window. These count only those two bridge returns, not
//! all root entries or all refusals. Their probe runs only on refusal branches.
//! The first five fields are scalar columns; display names use quoted Rust
//! string syntax so whitespace and control characters stay unambiguous.

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

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) enum Refusal {
    Capacity,
    Busy,
}

impl Refusal {
    fn label(self) -> &'static str {
        match self {
            Self::Capacity => "capacity",
            Self::Busy => "busy",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
struct RefusalKey {
    reason: Refusal,
    parent: Option<u32>,
    callee: Option<u32>,
    t2: bool,
}

static REFUSALS: Mutex<Option<HashMap<RefusalKey, u64>>> = Mutex::new(None);

// No probe on successful entries. Scalar metadata is collected only after
// one of the unchanged capacity/busy guards has decided to decline.
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[cold]
pub(crate) fn note_refusal(reason: Refusal, parent: Option<u32>, callee: Option<u32>, t2: bool) {
    if enabled()
        && let Ok(mut counts) = REFUSALS.lock()
    {
        *counts
            .get_or_insert_with(HashMap::new)
            .entry(RefusalKey {
                reason,
                parent,
                callee,
                t2,
            })
            .or_default() += 1;
    }
}

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
    if enabled() {
        clear(&COUNTS);
        clear(&REFUSALS);
    }
}

fn clear<T>(state: &Mutex<Option<T>>) {
    if let Ok(mut state) = state.lock() {
        *state = None;
    }
}

fn refusal_snapshot(state: &Mutex<Option<HashMap<RefusalKey, u64>>>) -> Vec<(RefusalKey, u64)> {
    state
        .lock()
        .ok()
        .and_then(|state| {
            state
                .as_ref()
                .map(|rows| rows.iter().map(|(key, n)| (*key, *n)).collect())
        })
        .unwrap_or_default()
}

fn render_refusals(
    mut rows: Vec<(RefusalKey, u64)>,
    mut name: impl FnMut(Option<u32>) -> String,
) -> String {
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let mut out = String::from(
        "# count reason callee_tier parent_owner_id callee_owner_id parent_name callee_name\n",
    );
    for (key, count) in rows {
        let id = |owner: Option<u32>| {
            owner
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_owned())
        };
        out.push_str(&format!(
            "{count} {} {} {} {} {:?} {:?}\n",
            key.reason.label(),
            if key.t2 { "T2" } else { "T1" },
            id(key.parent),
            id(key.callee),
            name(key.parent),
            name(key.callee),
        ));
    }
    out
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
    if let Ok(base) = std::env::var("EGCL_ACTIVATION_CENSUS") {
        let (path, refusal_path) = output_paths(&base, window);
        if let Err(error) = std::fs::write(&path, render()) {
            eprintln!("egcl: could not write EGCL_ACTIVATION_CENSUS to {path}: {error}");
        }
        // Snapshot scalars and release the census lock before name lookup.
        let rows = refusal_snapshot(&REFUSALS);
        let output = render_refusals(rows, |owner| {
            owner
                .map(|index| {
                    super::sym_name_uncached(egcl_rt::value::EgclVal::from_symbol_index(index))
                })
                .unwrap_or_else(|| "<uninstalled>".to_owned())
        });
        if let Err(error) = std::fs::write(&refusal_path, output) {
            eprintln!("egcl: could not write activation refusals to {refusal_path}: {error}");
        }
    }
}

fn output_paths(base: &str, window: bool) -> (String, String) {
    let suffix = if window { ".window" } else { "" };
    (
        format!("{base}{suffix}"),
        format!("{base}.refusals{suffix}"),
    )
}

pub(crate) fn write_window() {
    write(true);
}
pub fn maybe_write_on_exit() {
    write(false);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(reason: Refusal, callee: Option<u32>, t2: bool) -> RefusalKey {
        RefusalKey {
            reason,
            parent: Some(7),
            callee,
            t2,
        }
    }

    #[test]
    fn refusal_schema_keeps_reason_tier_and_exact_owner_ids_separate() {
        let rendered = render_refusals(
            vec![
                (key(Refusal::Capacity, Some(9), false), 1),
                (key(Refusal::Busy, Some(8), true), 3),
                (key(Refusal::Capacity, None, true), 1),
            ],
            |_| "same-name".to_owned(),
        );
        assert_eq!(
            rendered,
            "# count reason callee_tier parent_owner_id callee_owner_id parent_name callee_name\n\
             3 busy T2 7 8 \"same-name\" \"same-name\"\n\
             1 capacity T2 7 none \"same-name\" \"same-name\"\n\
             1 capacity T1 7 9 \"same-name\" \"same-name\"\n"
        );
        assert_eq!(
            render_refusals(Vec::new(), |_| unreachable!()),
            rendered.lines().next().unwrap().to_owned() + "\n"
        );
        let escaped = render_refusals(vec![(key(Refusal::Capacity, Some(9), false), 1)], |owner| {
            if owner == Some(7) {
                "(SETF A)\t".to_owned()
            } else {
                "a\"b\n".to_owned()
            }
        });
        assert_eq!(
            escaped.lines().nth(1),
            Some(r#"1 capacity T1 7 9 "(SETF A)\t" "a\"b\n""#)
        );
    }

    #[test]
    fn refusal_snapshot_releases_lock_before_names_and_reset_preserves_snapshot() {
        let state = Mutex::new(Some(HashMap::from([
            (key(Refusal::Capacity, Some(9), false), 2),
            (key(Refusal::Busy, Some(9), false), 1),
        ])));
        let rows = refusal_snapshot(&state);
        let output = render_refusals(rows.clone(), |owner| {
            assert!(
                state.try_lock().is_ok(),
                "name lookup must not hold census lock"
            );
            owner.unwrap().to_string()
        });
        assert!(output.contains("2 capacity T1 7 9 \"7\" \"9\"\n"));
        clear(&state);
        assert!(refusal_snapshot(&state).is_empty());
        assert!(state.lock().unwrap().is_none());
        assert_eq!(
            render_refusals(rows, |owner| owner.unwrap().to_string()),
            output
        );
    }

    #[test]
    fn companion_paths_preserve_lifecycle_exit_and_window_paths() {
        assert_eq!(
            output_paths("/tmp/counts", false),
            ("/tmp/counts".to_owned(), "/tmp/counts.refusals".to_owned())
        );
        assert_eq!(
            output_paths("/tmp/counts", true),
            (
                "/tmp/counts.window".to_owned(),
                "/tmp/counts.refusals.window".to_owned()
            )
        );
    }
}
