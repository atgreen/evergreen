// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! One conservative observation for global native admission work.
//!
//! SIGUSR1 is an authoritative bit; counted obligations belong to independent
//! GC requests, posted signal bits and occupied native-error slots. A producer
//! reserves before publishing its obligation, and a consumer removes the
//! obligation before releasing. Duplicate signal producers refund only their
//! own reservation. Consequently zero cannot hide a live obligation; a paused
//! producer can only cause an extra cold poll. An arrival after an idle read is
//! observed at the next unchanged poll site, as with the individual flags.
//!
//! Flag RMWs preserve every counter bit and counter CAS operations preserve the
//! flags. Saturation is permanently conservative rather than wrapping to zero.
//! The process signal epoch, per-execution yield request, carrier identity and
//! armed deadline retain their separate existing observations.
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

const SIGUSR1: u64 = 1;
const SATURATED: u64 = 2;
const COUNT: u64 = 4;

pub struct PendingEvents(AtomicU64);

/// Process-lifetime word. Signal handlers use only lock-free flag RMWs.
pub static GLOBAL: PendingEvents = PendingEvents::new();

impl Default for PendingEvents {
    fn default() -> Self {
        Self::new()
    }
}

impl PendingEvents {
    pub const fn new() -> Self {
        Self(AtomicU64::new(0))
    }
    pub fn address(&self) -> *const AtomicU64 {
        &self.0
    }
    pub fn pending(&self) -> bool {
        self.0.load(Ordering::SeqCst) != 0
    }
    pub fn post_sigusr1(&self) {
        self.0.fetch_or(SIGUSR1, Ordering::Relaxed);
    }
    pub fn take_sigusr1(&self) -> bool {
        self.sigusr1_pending() && self.0.fetch_and(!SIGUSR1, Ordering::Relaxed) & SIGUSR1 != 0
    }
    pub fn sigusr1_pending(&self) -> bool {
        self.0.load(Ordering::Relaxed) & SIGUSR1 != 0
    }
    /// Reserve before publishing an independently consumable obligation. A paused
    /// producer causes a conservative cold poll, never an invisible obligation.
    pub fn reserve(&self) {
        let mut old = self.0.load(Ordering::Relaxed);
        loop {
            if old & SATURATED != 0 {
                return;
            }
            // Fail closed even at an unreachable number of live obligations.
            let next = old.checked_add(COUNT).unwrap_or(old | SATURATED);
            match self
                .0
                .compare_exchange_weak(old, next, Ordering::Release, Ordering::Relaxed)
            {
                Ok(_) => return,
                Err(value) => old = value,
            }
        }
    }
    /// Release only after consuming the obligation, or refund a duplicate post.
    pub fn release(&self) {
        let mut old = self.0.load(Ordering::Relaxed);
        loop {
            if old & SATURATED != 0 {
                return;
            }
            assert!(old >= COUNT, "pending event count underflow");
            match self.0.compare_exchange_weak(
                old,
                old - COUNT,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(value) => old = value,
            }
        }
    }
    /// Publish one bit in a set whose occupied bits are owned by this counter.
    pub fn post_signal(&self, bits: &AtomicU8, bit: u8) -> bool {
        if !bit.is_power_of_two() {
            return false;
        }
        self.reserve();
        let posted = bits.fetch_or(bit, Ordering::Release) & bit == 0;
        if !posted {
            self.release();
        }
        posted
    }
    /// Consume a bit previously published through `post_signal` (or an explicit
    /// reservation). A stale snapshot or invalid bit leaves both objects alone.
    pub fn take_signal(&self, bits: &AtomicU8, old: u8, bit: u8) -> bool {
        if !bit.is_power_of_two() || old & bit == 0 {
            return false;
        }
        if bits
            .compare_exchange(old, old & !bit, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.release();
            true
        } else {
            false
        }
    }
}

/// A persistent request with independent ownership and a shared observation.
pub struct PendingFlag<'a> {
    requested: AtomicBool,
    events: &'a PendingEvents,
}

impl<'a> PendingFlag<'a> {
    pub const fn new(events: &'a PendingEvents) -> Self {
        Self {
            requested: AtomicBool::new(false),
            events,
        }
    }
    pub fn request(&self) {
        self.events.reserve();
        if self.requested.swap(true, Ordering::SeqCst) {
            self.events.release();
        }
    }
    pub fn clear(&self) {
        if self.requested.swap(false, Ordering::SeqCst) {
            self.events.release();
        }
    }
    pub fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }
    pub fn address(&self) -> *const AtomicBool {
        &self.requested
    }
}

impl Drop for PendingFlag<'_> {
    fn drop(&mut self) {
        if std::mem::take(self.requested.get_mut()) {
            self.events.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_request_owners_survive_other_owners_clear_and_drop() {
        let events = PendingEvents::new();
        let first = PendingFlag::new(&events);
        let second = PendingFlag::new(&events);
        first.request();
        assert!(!second.is_requested(), "requests belong to their own page");
        second.request();
        first.request();
        first.clear();
        first.clear();
        assert!(second.is_requested());
        assert!(events.pending());
        drop(first);
        assert!(events.pending());
        drop(second);
        assert!(
            !events.pending(),
            "destruction retires the last active request"
        );
    }

    #[test]
    fn invalid_or_absent_bits_never_acquire_or_release_an_obligation() {
        let events = PendingEvents::new();
        let bits = AtomicU8::new(0);
        assert!(!events.post_signal(&bits, 0));
        assert!(!events.post_signal(&bits, 3));
        assert!(!events.take_signal(&bits, 0, 1));
        assert!(!events.take_signal(&bits, 0, 0));
        assert_eq!(bits.load(Ordering::Relaxed), 0);
        assert!(!events.pending());
    }

    #[test]
    fn categories_and_multiple_executions_do_not_clear_each_other() {
        let events = PendingEvents::new();
        let gc = PendingFlag::new(&events);
        gc.request();
        events.post_sigusr1();
        events.reserve();
        events.reserve();
        gc.clear();
        assert!(
            events.pending(),
            "signal and occupied slots survive GC resume"
        );
        assert!(events.take_sigusr1());
        assert!(!events.take_sigusr1());
        events.release();
        assert!(events.pending(), "another execution still owns a slot");
        events.release();
        assert!(!events.pending());
    }

    #[test]
    fn duplicate_posts_refund_only_their_own_reservation() {
        let events = PendingEvents::new();
        let bits = AtomicU8::new(0);
        assert!(events.post_signal(&bits, 1));
        assert!(!events.post_signal(&bits, 1));
        assert!(events.post_signal(&bits, 2));
        assert!(events.take_signal(&bits, 3, 1));
        assert!(events.pending(), "the second posted signal remains visible");
        assert!(!events.take_signal(&bits, 3, 2));
        assert!(events.take_signal(&bits, 2, 2));
        assert!(!events.pending());
    }

    #[test]
    fn reservation_before_publication_survives_take_and_repost() {
        let events = PendingEvents::new();
        let bits = AtomicU8::new(0);
        events.reserve();
        assert!(
            events.pending(),
            "a paused producer is conservatively visible"
        );
        assert_eq!(bits.fetch_or(1, Ordering::Release), 0);
        assert!(events.take_signal(&bits, 1, 1));
        assert!(events.post_signal(&bits, 1));
        assert!(
            events.pending(),
            "the later producer cannot be acknowledged by the earlier take"
        );
        assert!(events.take_signal(&bits, 1, 1));
        assert!(!events.pending());
    }

    #[test]
    fn concurrent_consumers_cannot_hide_a_surviving_obligation() {
        let events = PendingEvents::new();
        events.reserve();
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let events = &events;
                scope.spawn(move || {
                    let bits = AtomicU8::new(0);
                    for _ in 0..10_000 {
                        assert!(events.post_signal(&bits, 1));
                        assert!(events.take_signal(&bits, 1, 1));
                        assert!(events.pending());
                    }
                });
            }
        });
        events.release();
        assert!(!events.pending());
    }

    #[test]
    fn racing_duplicate_posts_and_consumption_balance_the_same_bitset() {
        let events = PendingEvents::new();
        let bits = AtomicU8::new(0);
        let finished = std::sync::atomic::AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for index in 0..4 {
                let (events, bits, finished) = (&events, &bits, &finished);
                scope.spawn(move || {
                    for _ in 0..10_000 {
                        events.post_signal(bits, 1 << (index % 2));
                    }
                    finished.fetch_add(1, Ordering::Release);
                });
            }
            loop {
                let old = bits.load(Ordering::Acquire);
                if old != 0 {
                    events.take_signal(&bits, old, 1 << old.trailing_zeros());
                } else if finished.load(Ordering::Acquire) == 4 {
                    // The acquire observes every producer's final publication.
                    if bits.load(Ordering::Acquire) == 0 {
                        break;
                    }
                }
                std::hint::spin_loop();
            }
        });
        assert_eq!(bits.load(Ordering::Acquire), 0);
        assert!(
            !events.pending(),
            "all successful posts were consumed, duplicates refunded"
        );
    }

    #[test]
    fn saturation_is_sticky_and_never_wraps_to_idle() {
        let events = PendingEvents(AtomicU64::new(u64::MAX & !(COUNT - 1)));
        events.reserve();
        events.release();
        let gc = PendingFlag::new(&events);
        gc.request();
        gc.clear();
        events.post_sigusr1();
        events.take_sigusr1();
        assert_ne!(events.0.load(Ordering::Relaxed) & SATURATED, 0);
        assert!(events.pending());
    }

    #[test]
    #[should_panic(expected = "pending event count underflow")]
    fn unmatched_release_does_not_wrap_the_count() {
        PendingEvents::new().release();
    }
}
