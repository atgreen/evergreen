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

/// Explicit continuation context for a native call. The request belongs to the
/// caller's mapped frame; its arguments live in writable, precisely scanned
/// storage with capacity for the supplied count. Register entries spill live
/// arguments there before any helper. Slice entries use the supplied rooted
/// slice. Capture is entered only after all Rust helpers return.
///
/// SysV register calls pass this pointer in R9, slice calls in RCX. Win64
/// register calls pass it as argument six (after target/count/a0/a1/a2),
/// slice calls in R9. AAPCS64 register calls pass it in x5, slice calls in x3.
/// A Rust reentry must establish its own segment and must not forward this context.
#[repr(C)]
pub struct NativeCallContext {
    pub request: *mut u8,
    pub capture: *const u8,
    /// Invocation storage is distinct from the original capture request.
    /// Forwarding FUNCALL may skip its designator without changing that request.
    pub args: *mut crate::value::EgclVal,
    pub nargs: usize,
}

/// Ordinary Lisp register entries are distinct from checked and mapped-transfer
/// entries. The version describes this descriptor and the register/stack ABI.
pub const ORDINARY_X64_LISP_KIND: u64 = 0x5836344c495350;
pub const ORDINARY_X64_LISP_VERSION: u64 = 1;
pub const ORDINARY_ENV_NONE: u64 = 0;
pub const ORDINARY_ENV_CLOSURE: u64 = 1;

/// Immutable execution-owned descriptor. Generated callers acquire its pointer
/// from a dedicated CallCell slot, then validate signature before entering.
/// `owner` identifies retained runtime metadata, not a movable Lisp value.
/// The publishing execution retains both this allocation and its code owner
/// through every active/suspended native segment that could have loaded it.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OrdinaryEntryDescriptor {
    pub kind: u64,
    pub version: u64,
    pub revision: u64,
    pub entry: usize,
    pub code_start: usize,
    pub code_len: usize,
    pub arity: usize,
    pub environment: u64,
    pub owner: usize,
}

impl OrdinaryEntryDescriptor {
    pub fn is_compatible(&self) -> bool {
        self.kind == ORDINARY_X64_LISP_KIND
            && self.version == ORDINARY_X64_LISP_VERSION
            && matches!(self.environment, ORDINARY_ENV_NONE | ORDINARY_ENV_CLOSURE)
            && self.owner != 0
            && self.code_start != 0
            && self.contains_pc(self.entry)
    }

    pub fn contains_pc(&self, pc: usize) -> bool {
        self.code_start
            .checked_add(self.code_len)
            .is_some_and(|end| self.code_start <= pc && pc < end)
    }

    pub fn accepts(&self, nargs: usize, environment: u64) -> bool {
        self.is_compatible() && self.arity == nargs && self.environment == environment
    }
}

/// Stable executable slots and an opaque pointer to their owning execution's
/// state. Generated code embeds a slot address computed from its fixed ordinal.
#[repr(C)]
pub struct CallCell {
    // Permanent native-contract entries. The checked view below is temporary
    // compatibility linkage for callers that still poll after returning.
    native_entries: Option<[AtomicUsize; 2]>,
    ordinary_entry: AtomicUsize,
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
            native_entries: None,
            ordinary_entry: AtomicUsize::new(0),
            register_entry: AtomicUsize::new(cold_register),
            slice_entry: AtomicUsize::new(cold_slice),
            state: AtomicUsize::new(0),
            ordinal,
            cold_register,
            cold_slice,
        }
    }

    /// Install the permanent native contract alongside the migrating checked
    /// view. Target resolution and readiness still have exactly one publisher.
    pub fn with_native_entries(ordinal: usize, checked: [usize; 2], native: [usize; 2]) -> Self {
        assert!(native.iter().all(|entry| *entry != 0));
        Self {
            native_entries: Some(native.map(AtomicUsize::new)),
            ..Self::new(ordinal, checked[0], checked[1])
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
        match &self.native_entries {
            Some(entries) => &entries[usize::from(slice)],
            None => self.checked_entry_address(slice),
        }
    }

    /// Pointer to an OrdinaryEntryDescriptor, or zero for cold/ineligible targets.
    /// Loading it does not retain it: callers must be inside an owning native
    /// segment before loading or dereferencing the descriptor.
    pub fn ordinary_entry_address(&self) -> *const AtomicUsize {
        &self.ordinary_entry
    }

    /// Explicit legacy return-and-poll contract. Never call these slots as a
    /// native-transfer entry, even when a native segment happens to be active.
    pub fn checked_entry_address(&self, slice: bool) -> *const AtomicUsize {
        if slice {
            &self.slice_entry
        } else {
            &self.register_entry
        }
    }

    pub fn is_cold(&self) -> bool {
        // Native entries are permanent adapters, not readiness markers. Both
        // views resolve through this same checked target publication state.
        self.register_entry.load(Ordering::Acquire) == self.cold_register
    }

    fn invalidate(&self) {
        self.ordinary_entry.store(0, Ordering::Release);
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

    fn publish_ordinary(&self, cell: &CallCell, descriptor: &OrdinaryEntryDescriptor) -> bool {
        if !descriptor.is_compatible()
            || self.bindings[cell.ordinal].revision != descriptor.revision
        {
            return false;
        }
        cell.ordinary_entry
            .store(descriptor as *const _ as usize, Ordering::Release);
        true
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

/// Publish an already-retained ordinary descriptor under the same revision lock
/// as legacy entries. A stale compilation cannot overwrite a newer binding.
///
/// # Safety
/// The caller belongs to the cell's owning execution and must retain this exact
/// immutable descriptor and its executable code/metadata while published and
/// until every segment that could have loaded it has retired. Its entry address
/// must implement the declared ABI and its metadata must have been validated.
pub unsafe fn publish_ordinary(cell: &CallCell, descriptor: &OrdinaryEntryDescriptor) -> bool {
    TABLE
        .get()
        .expect("registered cell")
        .lock()
        .unwrap()
        .publish_ordinary(cell, descriptor)
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

    fn ordinary_descriptor(revision: u64) -> OrdinaryEntryDescriptor {
        OrdinaryEntryDescriptor {
            kind: ORDINARY_X64_LISP_KIND,
            version: ORDINARY_X64_LISP_VERSION,
            revision,
            entry: 0x1010,
            code_start: 0x1000,
            code_len: 0x100,
            arity: 4,
            environment: ORDINARY_ENV_NONE,
            owner: 0x2000,
        }
    }

    #[test]
    fn ordinary_publication_is_distinct_and_rejects_stale_revisions() {
        let mut table = Table::default();
        let ordinal = table.ordinal(42);
        let cell = Arc::new(CallCell::with_native_entries(ordinal, [11, 12], [31, 32]));
        table.bindings[ordinal].cells.push(Arc::downgrade(&cell));
        let address = cell.ordinary_entry_address();
        assert_eq!(unsafe { &*address }.load(Ordering::Acquire), 0);
        let old = ordinary_descriptor(0);
        assert!(table.publish_ordinary(&cell, &old));
        assert_eq!(
            unsafe { &*address }.load(Ordering::Acquire),
            &old as *const _ as usize
        );
        assert_eq!(
            unsafe { &*cell.entry_address(false) }.load(Ordering::Acquire),
            31
        );
        assert_eq!(
            unsafe { &*cell.checked_entry_address(false) }.load(Ordering::Acquire),
            11
        );
        table.invalidate(ordinal);
        assert_eq!(unsafe { &*address }.load(Ordering::Acquire), 0);
        assert!(!table.publish_ordinary(&cell, &old));
        let replacement = ordinary_descriptor(1);
        assert!(table.publish_ordinary(&cell, &replacement));
        assert_eq!(cell.ordinary_entry_address(), address);
    }

    #[test]
    fn ordinary_publication_rejects_incompatible_or_malformed_descriptors() {
        let mut table = Table::default();
        let ordinal = table.ordinal(43);
        let cell = CallCell::new(ordinal, 11, 12);
        for mutation in 0..7 {
            let mut entry = ordinary_descriptor(0);
            match mutation {
                0 => entry.kind ^= 1,
                1 => entry.version += 1,
                2 => entry.entry = entry.code_start + entry.code_len,
                3 => entry.code_len = usize::MAX,
                4 => entry.owner = 0,
                5 => entry.environment = 99,
                6 => entry.code_len = 0,
                _ => unreachable!(),
            }
            assert!(
                !table.publish_ordinary(&cell, &entry),
                "mutation {mutation}"
            );
            assert_eq!(
                unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
                0
            );
        }
        let entry = ordinary_descriptor(0);
        assert!(entry.accepts(4, ORDINARY_ENV_NONE));
        assert!(!entry.accepts(3, ORDINARY_ENV_NONE));
        assert!(!entry.accepts(4, ORDINARY_ENV_CLOSURE));
    }

    #[test]
    fn native_contract_survives_checked_target_invalidation() {
        let mut table = Table::default();
        let ordinal = table.ordinal(19);
        let cell = Arc::new(CallCell::with_native_entries(ordinal, [11, 12], [31, 32]));
        table.bindings[ordinal].cells.push(Arc::downgrade(&cell));
        let entries = [cell.entry_address(false), cell.entry_address(true)];
        assert!(cell.is_cold());
        assert!(table.publish(&cell, 0, 21, 22));
        assert!(!cell.is_cold());
        table.invalidate(ordinal);
        assert!(cell.is_cold());
        assert!(!table.publish(&cell, 0, 41, 42));
        for (slice, native, checked) in [(false, 31, 11), (true, 32, 12)] {
            assert_eq!(
                unsafe { (*cell.entry_address(slice)).load(Ordering::Acquire) },
                native
            );
            assert_eq!(
                unsafe { (*cell.checked_entry_address(slice)).load(Ordering::Acquire) },
                checked
            );
            assert_eq!(entries[usize::from(slice)], cell.entry_address(slice));
        }
        assert!(table.publish(&cell, 1, 41, 42));
        assert!(!cell.is_cold());
    }

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
