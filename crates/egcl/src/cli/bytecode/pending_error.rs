// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned error slot with a conservative process-wide summary.
use std::cell::RefCell;
use std::sync::atomic::{AtomicUsize, Ordering};

pub(super) struct PendingError<'a, T> {
    value: RefCell<Option<T>>,
    count: &'a AtomicUsize,
    events: Option<&'a egcl_rt::pending_events::PendingEvents>,
}

impl<'a, T> PendingError<'a, T> {
    #[cfg(test)]
    pub(super) const fn new(count: &'a AtomicUsize) -> Self {
        Self {
            value: RefCell::new(None),
            count,
            events: None,
        }
    }

    pub(super) const fn with_events(
        count: &'a AtomicUsize,
        events: &'a egcl_rt::pending_events::PendingEvents,
    ) -> Self {
        Self {
            value: RefCell::new(None),
            count,
            events: Some(events),
        }
    }

    fn reserve(&self) {
        if let Some(events) = self.events {
            events.reserve();
        }
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    fn release(&self) {
        self.count.fetch_sub(1, Ordering::Relaxed);
        if let Some(events) = self.events {
            events.release();
        }
    }

    pub(super) fn is_some(&self) -> bool {
        self.value.borrow().is_some()
    }

    pub(super) fn set_first(&self, error: T) {
        let mut value = self.value.borrow_mut();
        if value.is_none() {
            self.reserve();
            *value = Some(error);
        }
    }

    pub(super) fn replace(&self, next: Option<T>) -> Option<T> {
        let mut value = self.value.borrow_mut();
        let was_pending = value.is_some();
        let will_be_pending = next.is_some();
        if !was_pending && will_be_pending {
            self.reserve();
        }
        let previous = std::mem::replace(&mut *value, next);
        if was_pending && !will_be_pending {
            self.release();
        }
        previous
    }

    pub(super) fn take(&self) -> Option<T> {
        self.replace(None)
    }

    // GC may relocate the payload, but cannot change whether the slot is occupied.
    pub(super) fn visit(&self, visit: impl FnOnce(&mut T)) {
        if let Some(value) = self.value.borrow_mut().as_mut() {
            visit(value);
        }
    }
}

impl<T> Drop for PendingError<'_, T> {
    fn drop(&mut self) {
        if self.value.get_mut().take().is_some() {
            self.release();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn event_word_tracks_replacement_restore_and_destruction() {
        let count = AtomicUsize::new(0);
        let events = egcl_rt::pending_events::PendingEvents::new();
        let outer = PendingError::with_events(&count, &events);
        let inner = PendingError::with_events(&count, &events);
        outer.set_first(1);
        inner.set_first(2);
        let saved = outer.take();
        assert!(events.pending());
        outer.replace(saved);
        drop(inner);
        assert!(events.pending());
        outer.replace(Some(3));
        assert_eq!(outer.take(), Some(3));
        assert!(!events.pending());
        outer.set_first(4);
        drop(outer);
        assert!(!events.pending());
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn first_error_wins_and_nested_calls_restore_outer_error() {
        let count = AtomicUsize::new(0);
        let slot = PendingError::new(&count);
        slot.set_first("outer");
        slot.set_first("ignored");
        assert_eq!(count.load(Ordering::Relaxed), 1);
        let saved = slot.take();
        assert_eq!(saved, Some("outer"));
        assert_eq!(count.load(Ordering::Relaxed), 0);
        slot.set_first("inner");
        assert_eq!(slot.take(), Some("inner"));
        slot.replace(saved);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(slot.take(), Some("outer"));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn clearing_or_retiring_one_execution_does_not_hide_another() {
        let count = AtomicUsize::new(0);
        let a = PendingError::new(&count);
        let b = PendingError::new(&count);
        a.set_first(1);
        b.set_first(2);
        assert_eq!(count.load(Ordering::Relaxed), 2);
        assert_eq!(a.take(), Some(1));
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert!(b.is_some());
        drop(a);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        drop(b);
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn replacement_and_unwind_balance_the_summary() {
        let count = AtomicUsize::new(0);
        let _ = std::panic::catch_unwind(|| {
            let slot = PendingError::new(&count);
            slot.replace(Some(1));
            assert_eq!(slot.replace(Some(2)), Some(1));
            assert_eq!(count.load(Ordering::Relaxed), 1);
            panic!("retire on unwind");
        });
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;

    #[test]
    fn concurrent_execution_exit_keeps_the_surviving_error_visible() {
        let count = AtomicUsize::new(0);
        let survivor = PendingError::new(&count);
        survivor.set_first(99);
        std::thread::scope(|scope| {
            for _ in 0..4 {
                let count = &count;
                scope.spawn(move || {
                    for _ in 0..10000 {
                        let slot = PendingError::new(count);
                        slot.set_first(1);
                        assert!(count.load(Ordering::Relaxed) >= 2);
                    }
                });
            }
        });
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(survivor.take(), Some(99));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn scanning_updates_payload_without_changing_occupancy() {
        let count = AtomicUsize::new(0);
        let slot = PendingError::new(&count);
        slot.visit(|_: &mut usize| panic!("empty slot must not be visited"));
        slot.set_first(1);
        slot.visit(|value| *value = 2);
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(slot.take(), Some(2));
        assert_eq!(count.load(Ordering::Relaxed), 0);
    }
}
