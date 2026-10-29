// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-local notification words with signal-safe lifetime management.
//!
//! Producers publish their authoritative state before arming a slot. The owner
//! clears the notification BEFORE draining that state, then rearms persistent
//! obligations. An arrival during the drain therefore survives for another poll.
//! Slots are not the authoritative state and may conservatively remain armed.
//!
//! Signal handlers walk immutable, process-lifetime chunks, never execution
//! objects or registry locks. Retired slots are reused under the writer mutex;
//! a stale broadcast can only arm the next owner unnecessarily. New owners start
//! armed, including owners registered after a broadcast passed their chunk.
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};

#[repr(C)]
pub struct PollSlot {
    /// First field: generated code observes this AtomicU64 at offset zero.
    pending: AtomicU64,
    occupied: AtomicBool,
    carrier: AtomicU64,
}

impl PollSlot {
    const fn new() -> Self {
        Self {
            pending: AtomicU64::new(0),
            occupied: AtomicBool::new(false),
            carrier: AtomicU64::new(0),
        }
    }

    pub fn request(&self) {
        self.pending.store(1, Ordering::Release);
    }

    pub fn pending(&self) -> bool {
        self.pending.load(Ordering::Acquire) != 0
    }

    pub fn begin(&self) -> bool {
        self.pending.swap(0, Ordering::AcqRel) != 0
    }

    pub(crate) fn set_carrier(&self, carrier: u64) {
        self.carrier.store(carrier, Ordering::SeqCst);
        self.request();
    }

    pub(crate) fn retire(&self) {
        // No generated reader may survive its execution. Broadcast readers may
        // outlive it safely because the slot, unlike the execution, is retained.
        self.occupied.store(false, Ordering::Release);
    }
}

const SLOTS_PER_CHUNK: usize = 64;
struct PollChunk {
    slots: [PollSlot; SLOTS_PER_CHUNK],
    next: AtomicPtr<PollChunk>,
}

impl PollChunk {
    const fn new() -> Self {
        Self {
            slots: [const { PollSlot::new() }; SLOTS_PER_CHUNK],
            next: AtomicPtr::new(std::ptr::null_mut()),
        }
    }

    fn next(&self) -> Option<&'static Self> {
        // Published chunks are initialized before Release and never reclaimed.
        unsafe { self.next.load(Ordering::Acquire).as_ref() }
    }
}

static SLOTS: PollChunk = PollChunk::new();
static WRITER: Mutex<()> = Mutex::new(());

pub(crate) fn register(carrier: u64) -> &'static PollSlot {
    let _writer = WRITER.lock().unwrap();
    let mut chunk = &SLOTS;
    loop {
        for slot in &chunk.slots {
            if !slot.occupied.load(Ordering::Acquire) {
                slot.occupied.store(true, Ordering::Release);
                slot.set_carrier(carrier);
                return slot;
            }
        }
        chunk = match chunk.next() {
            Some(next) => next,
            None => {
                let next = Box::leak(Box::new(PollChunk::new()));
                chunk.next.store(next, Ordering::Release);
                next
            }
        };
    }
}

/// Async-signal-safe broadcast. Do not filter by occupancy: a concurrent new
/// owner must not be hidden by an earlier inactive observation. Visiting unused
/// slots is harmless and avoids any execution-pointer reclamation protocol.
pub fn request_all() {
    let mut chunk = &SLOTS;
    loop {
        for slot in &chunk.slots {
            slot.request();
        }
        match chunk.next() {
            Some(next) => chunk = next,
            None => return,
        }
    }
}

/// Wake executions that can consume a carrier-directed request. Mount publishes
/// the new carrier and arms independently, covering a concurrent association
/// change. Suspended fibers retaining this association may get a harmless poll.
pub fn request_carrier(carrier: u64) {
    let mut chunk = &SLOTS;
    loop {
        for slot in &chunk.slots {
            if slot.carrier.load(Ordering::SeqCst) == carrier {
                slot.request();
            }
        }
        match chunk.next() {
            Some(next) => chunk = next,
            None => return,
        }
    }
}

const _: () = assert!(std::mem::offset_of!(PollSlot, pending) == 0);
