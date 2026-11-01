// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Captured-environment access counters (Backpack stage 0,
//! bliss-gg3i1.7.15.1).
//!
//! `(egcl::%env-access-counts t)` resets and enables counting on every thread,
//! `(egcl::%env-access-counts nil)` disables it, and
//! `(egcl::%env-access-counts)` returns this thread's counts as a list in
//! [`Counter`] order. Counting is off by default; a disabled counter costs one
//! relaxed load.

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

#[derive(Clone, Copy)]
#[allow(dead_code)] // SlotFallbacks: heap environments never fall back
pub(super) enum Counter {
    /// Compiled environment operations resolved by name.
    NameOps = 0,
    /// Parent-frame hops taken by any name-based lexical lookup or assignment,
    /// including the tree-walker's.
    NameHops = 1,
    /// Heap-environment slot operations (`EnvRef`/`EnvSet`, every tier).
    SlotOps = 2,
    /// Parent-environment links followed to reach a slot.
    SlotHops = 3,
    /// Slot operations whose frame did not hold the expected binding and so
    /// fell back to the name path (also counted in `NameOps`).
    SlotFallbacks = 4,
}

const COUNTERS: usize = 5;

static ENABLED: AtomicBool = AtomicBool::new(false);
/// Bumped by every enable, so each thread discards counts from before it.
static EPOCH: AtomicU64 = AtomicU64::new(0);

thread_local! {
    static COUNTS: ([Cell<u64>; COUNTERS], Cell<u64>) =
        const { ([const { Cell::new(0) }; COUNTERS], Cell::new(0)) };
}

#[inline]
pub(super) fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

#[inline]
pub(super) fn bump(counter: Counter, amount: u64) {
    if enabled() {
        bump_slow(counter, amount);
    }
}

#[cold]
#[inline(never)]
fn bump_slow(counter: Counter, amount: u64) {
    COUNTS.with(|(counts, epoch)| {
        sync_epoch(counts, epoch);
        let cell = &counts[counter as usize];
        cell.set(cell.get().wrapping_add(amount));
    });
}

fn sync_epoch(counts: &[Cell<u64>; COUNTERS], epoch: &Cell<u64>) {
    let current = EPOCH.load(Ordering::Relaxed);
    if epoch.get() != current {
        epoch.set(current);
        for cell in counts {
            cell.set(0);
        }
    }
}

/// Enable (and reset) or disable counting.
pub(super) fn set_enabled(on: bool) {
    if on {
        EPOCH.fetch_add(1, Ordering::Relaxed);
    }
    ENABLED.store(on, Ordering::Relaxed);
}

/// This thread's counts since counting was last enabled.
pub(super) fn snapshot() -> [u64; COUNTERS] {
    COUNTS.with(|(counts, epoch)| {
        sync_epoch(counts, epoch);
        std::array::from_fn(|index| counts[index].get())
    })
}
