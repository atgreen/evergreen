// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Live requested bytes owned by Rust allocator clients.
//!
//! This excludes freed blocks retained by an allocator, size-class rounding,
//! direct OS mappings, and the Lisp heap. Hooks must record every successful
//! global allocation, including before `main`, and its eventual release.
//! Recording never allocates, locks, formats, or requests a collection.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

struct Ledger {
    live: AtomicUsize,
    active: AtomicBool,
}

impl Ledger {
    const fn new() -> Self {
        Self {
            live: AtomicUsize::new(0),
            active: AtomicBool::new(false),
        }
    }

    #[inline]
    fn record_alloc(&self, size: usize) {
        self.live.fetch_add(size, Ordering::Relaxed);
    }

    #[inline]
    fn record_dealloc(&self, size: usize) {
        self.live.fetch_sub(size, Ordering::Relaxed);
    }

    #[inline]
    fn record_resize(&self, old: usize, new: usize) {
        if new >= old {
            self.live.fetch_add(new - old, Ordering::Relaxed);
        } else {
            self.live.fetch_sub(old - new, Ordering::Relaxed);
        }
    }

    #[inline]
    fn live_bytes(&self) -> usize {
        self.live.load(Ordering::Relaxed)
    }

    fn enable_tracking(&self) {
        self.active.store(true, Ordering::Release);
    }

    fn tracking_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }
}

static HOST_MEMORY: Ledger = Ledger::new();

/// Record a successful allocation's requested size, not allocator capacity.
#[inline]
pub fn record_alloc(size: usize) {
    HOST_MEMORY.record_alloc(size);
}

/// Record the requested layout size of an allocation released by its client.
/// A block returned to an allocator's cache is no longer live requested memory.
#[inline]
pub fn record_dealloc(size: usize) {
    HOST_MEMORY.record_dealloc(size);
}

/// Record one successful resize as a single net update. A failed resize must
/// not call this: the old allocation remains live and unchanged.
#[inline]
pub fn record_resize(old: usize, new: usize) {
    HOST_MEMORY.record_resize(old, new);
}

/// A snapshot of live requested Rust bytes; use only when tracking is active.
/// Concurrent allocator operations may change the count immediately afterward.
#[inline]
pub fn live_bytes() -> usize {
    HOST_MEMORY.live_bytes()
}

/// Whether the executable has installed complete global allocator hooks.
/// Unhooked library/embedding use remains unavailable rather than reporting a
/// meaningful zero. Allocator hooks record bytes even before activation.
pub fn tracking_active() -> bool {
    HOST_MEMORY.tracking_active()
}

/// Declare complete allocator coverage before consulting host memory policy.
/// This is idempotent and never resets bytes recorded before `main`. Embedders
/// may call it only after wiring all allocation paths from process startup.
pub fn enable_tracking() {
    HOST_MEMORY.enable_tracking();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_preserves_pre_main_bytes_and_is_idempotent() {
        let ledger = Ledger::new();
        assert!(!ledger.tracking_active());
        ledger.record_alloc(37);
        ledger.enable_tracking();
        ledger.enable_tracking();
        assert!(ledger.tracking_active());
        assert_eq!(ledger.live_bytes(), 37);
        ledger.record_dealloc(37);
        assert_eq!(ledger.live_bytes(), 0);
    }

    #[test]
    fn requested_bytes_follow_growth_shrink_and_release() {
        let ledger = Ledger::new();
        ledger.record_alloc(9);
        ledger.record_alloc(513);
        assert_eq!(ledger.live_bytes(), 522);
        ledger.record_resize(9, 13);
        assert_eq!(ledger.live_bytes(), 526);
        ledger.record_resize(513, 128);
        assert_eq!(ledger.live_bytes(), 141);
        ledger.record_resize(13, 13);
        ledger.record_dealloc(13);
        ledger.record_dealloc(128);
        assert_eq!(ledger.live_bytes(), 0);
    }

    #[test]
    fn owned_ledger_accounts_cross_thread_allocations_and_releases() {
        let ledger = std::sync::Arc::new(Ledger::new());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(5));
        ledger.record_alloc(7);
        std::thread::scope(|scope| {
            for size in [10, 20, 30, 40] {
                let ledger = std::sync::Arc::clone(&ledger);
                let barrier = std::sync::Arc::clone(&barrier);
                scope.spawn(move || {
                    ledger.record_alloc(size);
                    barrier.wait();
                    barrier.wait();
                    ledger.record_dealloc(size);
                });
            }
            barrier.wait();
            let live_with_workers = ledger.live_bytes();
            barrier.wait();
            assert_eq!(live_with_workers, 107);
        });
        assert_eq!(ledger.live_bytes(), 7);
        // The client may release a request from a different native thread.
        std::thread::scope(|scope| {
            scope.spawn(|| ledger.record_dealloc(7));
        });
        assert_eq!(ledger.live_bytes(), 0);
    }
}
