// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Stable ordinals for named calls. Name resolution and publication take the
//! table lock; generated calls only load a cell's atomic executable entry.
//!
//! Native code currently belongs to a Lisp execution. Each execution therefore
//! registers its own callable cell for the process-wide ordinal. Other threads
//! may redirect that cell to its cold entry, but never access its owner state.
//! Cells own no executable code: the execution retains the resolved target,
//! and an entered adapter retains that exact target before any safepoint.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// Stable executable slots and an opaque pointer to their owning execution's
/// state. Generated code embeds a slot address computed from its fixed ordinal.
#[repr(C)]
pub struct CallCell {
    register_entry: AtomicUsize,
    slice_entry: AtomicUsize,
    state: AtomicUsize,
    ordinal: usize,
    cold_register: usize,
    cold_slice: usize,
}

impl CallCell {
    pub fn new(ordinal: usize, cold_register: usize, cold_slice: usize) -> Self {
        Self {
            register_entry: AtomicUsize::new(cold_register),
            slice_entry: AtomicUsize::new(cold_slice),
            state: AtomicUsize::new(0),
            ordinal,
            cold_register,
            cold_slice,
        }
    }

    /// Bind the state once, before exposing this cell to generated code.
    pub fn bind_state(&self, address: usize) {
        assert_ne!(address, 0);
        assert!(
            self.state
                .compare_exchange(0, address, Ordering::Release, Ordering::Relaxed)
                .is_ok()
        );
    }

    /// Only the owning execution may dereference this address. It must keep
    /// the state alive for every activation of code that embeds the cell.
    pub fn state_address(&self) -> usize {
        self.state.load(Ordering::Acquire)
    }

    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    pub fn entry_address(&self, slice: bool) -> *const AtomicUsize {
        if slice {
            &self.slice_entry
        } else {
            &self.register_entry
        }
    }

    pub fn is_cold(&self) -> bool {
        self.register_entry.load(Ordering::Acquire) == self.cold_register
    }

    fn invalidate(&self) {
        self.register_entry
            .store(self.cold_register, Ordering::Release);
        self.slice_entry.store(self.cold_slice, Ordering::Release);
    }
}

struct Binding {
    revision: u64,
    cells: Vec<Weak<CallCell>>,
}

#[derive(Default)]
struct Table {
    by_symbol: HashMap<u32, usize, crate::fxhash::FxBuildHasher>,
    bindings: Vec<Binding>,
}

impl Table {
    fn ordinal(&mut self, symbol: u32) -> usize {
        *self.by_symbol.entry(symbol).or_insert_with(|| {
            let ordinal = self.bindings.len();
            self.bindings.push(Binding {
                revision: 0,
                cells: Vec::new(),
            });
            ordinal
        })
    }

    fn invalidate(&mut self, ordinal: usize) {
        let binding = &mut self.bindings[ordinal];
        binding.revision = binding.revision.wrapping_add(1);
        binding.cells.retain(|weak| {
            if let Some(cell) = weak.upgrade() {
                cell.invalidate();
                true
            } else {
                false
            }
        });
    }

    fn publish(&self, cell: &CallCell, revision: u64, registers: usize, slice: usize) -> bool {
        if self.bindings[cell.ordinal].revision != revision {
            return false;
        }
        cell.register_entry.store(registers, Ordering::Release);
        cell.slice_entry.store(slice, Ordering::Release);
        true
    }
}

static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();

/// Assign a dense ordinal once for an interned function name, including forward
/// references. Unknown and collectable uninterned symbols use ordinary dispatch.
pub fn resolve_ordinal(symbol: u32) -> Option<usize> {
    if crate::symbols::is_uninterned(symbol) || crate::symbols::symbol_object_ptr(symbol).is_none()
    {
        return None;
    }
    Some(
        TABLE
            .get_or_init(|| Mutex::new(Table::default()))
            .lock()
            .unwrap()
            .ordinal(symbol),
    )
}

pub fn register(cell: &Arc<CallCell>) {
    let mut table = TABLE
        .get()
        .expect("resolve ordinal before registering")
        .lock()
        .unwrap();
    table.bindings[cell.ordinal]
        .cells
        .push(Arc::downgrade(cell));
}

/// Capture before resolving a cold target. Publication checks it under the
/// writer lock so a redefinition cannot be overwritten by stale compilation.
pub fn revision(ordinal: usize) -> u64 {
    TABLE
        .get()
        .expect("resolved ordinal")
        .lock()
        .unwrap()
        .bindings[ordinal]
        .revision
}

pub fn publish(cell: &CallCell, revision: u64, registers: usize, slice: usize) -> bool {
    TABLE
        .get()
        .expect("registered cell")
        .lock()
        .unwrap()
        .publish(cell, revision, registers, slice)
}

pub fn invalidate(symbol: u32) {
    let Some(table) = TABLE.get() else { return };
    let mut table = table.lock().unwrap();
    if let Some(&ordinal) = table.by_symbol.get(&symbol) {
        table.invalidate(ordinal);
    }
}

/// Existing broad native-code invalidations also redirect callable slots.
/// This runs on definition/tier changes, never on an ordinary warmed call.
pub fn invalidate_all() {
    let Some(table) = TABLE.get() else { return };
    let mut table = table.lock().unwrap();
    for ordinal in 0..table.bindings.len() {
        table.invalidate(ordinal);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collectable_and_unknown_symbols_do_not_get_embedded_slots() {
        let private = crate::symbols::make_uninterned("PRIVATE-CALL-SLOT");
        assert!(resolve_ordinal(private.as_symbol_index()).is_none());
        assert!(resolve_ordinal(0x7fff_ffff).is_none());
        let named = crate::symbols::intern("NAMED-CALL-SLOT");
        assert_eq!(resolve_ordinal(named), resolve_ordinal(named));
        assert!(resolve_ordinal(named).is_some());
    }

    #[test]
    fn ordinals_and_addresses_survive_growth_and_redefinition() {
        let mut table = Table::default();
        let ordinal = table.ordinal(42);
        let cell = Arc::new(CallCell::new(ordinal, 11, 12));
        table.bindings[ordinal].cells.push(Arc::downgrade(&cell));
        let address = cell.entry_address(false);
        assert!(table.publish(&cell, 0, 21, 22));
        for symbol in 100..10000 {
            table.ordinal(symbol);
        }
        assert_eq!(table.ordinal(42), ordinal);
        assert_eq!(cell.entry_address(false), address);
        assert_eq!(cell.register_entry.load(Ordering::Acquire), 21);
        table.invalidate(ordinal);
        assert!(cell.is_cold());
        assert_eq!(cell.slice_entry.load(Ordering::Acquire), 12);
        assert!(
            !table.publish(&cell, 0, 31, 32),
            "reject pre-redefinition target"
        );
        assert!(table.publish(&cell, 1, 31, 32));
        assert_eq!(cell.register_entry.load(Ordering::Acquire), 31);
    }

    #[test]
    fn invalidation_reaches_each_execution_without_owning_its_state() {
        let mut table = Table::default();
        let ordinal = table.ordinal(7);
        let first = Arc::new(CallCell::new(ordinal, 1, 2));
        let second = Arc::new(CallCell::new(ordinal, 3, 4));
        table.bindings[ordinal]
            .cells
            .extend([Arc::downgrade(&first), Arc::downgrade(&second)]);
        assert!(table.publish(&first, 0, 5, 6));
        assert!(table.publish(&second, 0, 7, 8));
        table.invalidate(ordinal);
        assert!(first.is_cold() && second.is_cold());
        drop(second);
        table.invalidate(ordinal);
        assert_eq!(table.bindings[ordinal].cells.len(), 1);
    }
}
