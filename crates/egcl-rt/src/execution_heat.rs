// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Execution-owned T1 sampling countdowns, distinct from canonical function
//! invocation/back-edge counters. Native artifacts carry only a never-reused
//! identity; publication installs its cell before exposing an executable entry.
//! All cells and immutable views survive until execution retirement, including
//! cells for replaced artifacts with suspended native activations.

use crate::lock_order::{LockLevel, OrderedMutex};
use std::sync::atomic::{AtomicPtr, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// One physical artifact/version, never a public function name. Companions and
/// replacement compilations receive different identities.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HeatId(usize);
static NEXT_ID: AtomicUsize = AtomicUsize::new(0);
impl HeatId {
    pub fn new() -> Option<Self> {
        // Generated x86 selectors use a signed 32-bit byte displacement.
        NEXT_ID
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id < i32::MAX as usize / 8).then_some(id + 1)
            })
            .ok()
            .map(Self)
    }
    pub fn ordinal(self) -> usize {
        self.0
    }
}

#[repr(C)]
pub struct HeatView {
    pub slots: *const AtomicPtr<AtomicU32>,
    pub len: usize,
}
// SAFETY: immutable view, atomic slots; backing and cells remain alive until
// the containing execution has retired all active/suspended native frames.
unsafe impl Send for HeatView {}
unsafe impl Sync for HeatView {}
struct HeatCell {
    counter: AtomicU32,
    batch: u32,
}
struct Storage {
    #[allow(clippy::vec_box)] // native frames cache these exact cell addresses
    cells: Vec<Option<Box<HeatCell>>>,
    #[allow(clippy::vec_box)] // native selectors load these immutable addresses
    views: Vec<Box<HeatView>>,
    backing: Vec<Box<[AtomicPtr<AtomicU32>]>>,
}
#[repr(C)]
pub struct ExecutionHeatDirectory {
    view: AtomicPtr<HeatView>,
    storage: OrderedMutex<Storage>,
}
// High execution-object namespace, field2 reserved for heat directories;
// call linkage uses field1. Instance identities are stable and never reused.
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);
impl Default for ExecutionHeatDirectory {
    fn default() -> Self {
        let instance = NEXT_DIRECTORY
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id < (1_u64 << 61) - 1).then_some(id + 1)
            })
            .expect("heat directory lock identity exhausted");
        Self {
            view: AtomicPtr::new(std::ptr::null_mut()),
            storage: OrderedMutex::new(
                LockLevel::ExecutionObject,
                (1_u64 << 63) | (instance << 2) | 2,
                "execution heat directory",
                Storage {
                    cells: Vec::new(),
                    views: Vec::new(),
                    backing: Vec::new(),
                },
            ),
        }
    }
}
impl ExecutionHeatDirectory {
    pub const VIEW_OFFSET: usize = std::mem::offset_of!(Self, view);

    /// Owner-only publication operation; does not allocate Lisp objects, poll,
    /// or collect. Never reset an already-installed countdown on reentry.
    pub fn install(&self, id: HeatId, batch: u32) {
        assert!(batch > 0);
        let ordinal = id.ordinal();
        let mut storage = self.storage.lock().unwrap_or_else(|e| e.into_inner());
        if ordinal >= storage.cells.len() {
            let len = (ordinal + 1).next_power_of_two().max(16);
            storage.cells.resize_with(len, || None);
            let backing: Box<[_]> = storage
                .cells
                .iter()
                .map(|cell| {
                    AtomicPtr::new(cell.as_ref().map_or(std::ptr::null_mut(), |cell| {
                        std::ptr::from_ref(&cell.counter).cast_mut()
                    }))
                })
                .collect();
            let mut view = Box::new(HeatView {
                slots: backing.as_ptr(),
                len,
            });
            let pointer = std::ptr::from_mut(&mut *view);
            storage.backing.push(backing);
            storage.views.push(view);
            self.view.store(pointer, Ordering::Release);
        }
        if let Some(cell) = &storage.cells[ordinal] {
            assert_eq!(cell.batch, batch, "one artifact has one heat batch");
            return;
        }
        let cell = Box::new(HeatCell {
            counter: AtomicU32::new(batch),
            batch,
        });
        let pointer = std::ptr::from_ref(&cell.counter).cast_mut();
        storage.cells[ordinal] = Some(cell);
        storage.backing.last().unwrap()[ordinal].store(pointer, Ordering::Release);
    }

    /// Noncollecting lookup, also used to inspect the native binding in tests.
    /// The returned cell is retained even if later installations grow the view.
    pub fn counter(&self, id: HeatId) -> Option<&AtomicU32> {
        let view = unsafe { self.view.load(Ordering::Acquire).as_ref() }?;
        if id.ordinal() >= view.len {
            return None;
        }
        let pointer = unsafe { &*view.slots.add(id.ordinal()) }.load(Ordering::Acquire);
        unsafe { pointer.as_ref() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_countdowns_survive_reentry_growth_and_replacement() {
        let a = ExecutionHeatDirectory::default();
        let b = ExecutionHeatDirectory::default();
        let old = HeatId::new().unwrap();
        let replacement = HeatId::new().unwrap();
        assert!(a.counter(old).is_none());
        a.install(old, 4);
        b.install(old, 4);
        let pointer = a.counter(old).unwrap() as *const _;
        a.counter(old).unwrap().fetch_sub(2, Ordering::Relaxed);
        a.install(old, 4); // another activation must not reset it
        assert_eq!(a.counter(old).unwrap().load(Ordering::Relaxed), 2);
        assert_eq!(b.counter(old).unwrap().load(Ordering::Relaxed), 4);
        a.install(replacement, 4);
        for _ in 0..64 {
            a.install(HeatId::new().unwrap(), 4);
        }
        assert_eq!(a.counter(old).unwrap() as *const _, pointer);
        assert_eq!(a.counter(old).unwrap().load(Ordering::Relaxed), 2);
        assert_eq!(a.counter(replacement).unwrap().load(Ordering::Relaxed), 4);
        assert!(b.counter(replacement).is_none());
    }
}
