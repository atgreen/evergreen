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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
/// Binding adapters accept the actual supplied count and validate it themselves.
pub const ORDINARY_ARITY_ANY: usize = usize::MAX;
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
    /// Maximum bytes below entry RSP, including temporary move storage.
    /// Excludes the return address and caller-owned outgoing arguments.
    pub stack_required: usize,
}

impl OrdinaryEntryDescriptor {
    pub fn is_compatible(&self) -> bool {
        self.kind == ORDINARY_X64_LISP_KIND
            && self.version == ORDINARY_X64_LISP_VERSION
            && matches!(self.environment, ORDINARY_ENV_NONE | ORDINARY_ENV_CLOSURE)
            && self.owner != 0
            && (8..=isize::MAX as usize).contains(&self.stack_required)
            && self.code_start != 0
            && self.contains_pc(self.entry)
    }

    pub fn contains_pc(&self, pc: usize) -> bool {
        self.code_start
            .checked_add(self.code_len)
            .is_some_and(|end| self.code_start <= pc && pc < end)
    }

    pub fn accepts(&self, nargs: usize, environment: u64) -> bool {
        self.is_compatible()
            && (self.arity == nargs || self.arity == ORDINARY_ARITY_ANY)
            && self.environment == environment
    }
}

/// Immutable profiling record retained with its exact ordinary descriptor.
/// Counter addresses name pinned, traced callable fields. A mismatching
/// descriptor must never dereference them, including after redefinition.
#[repr(C)]
#[derive(PartialEq, Eq)]
pub struct OrdinaryCallHeat {
    pub descriptor: usize,
    pub invocation_counter: usize,
    pub back_edge_counter: usize,
    pub invocation_threshold: u32,
    pub back_edge_threshold: u32,
}

/// Stable executable slots and an opaque pointer to their owning execution's
/// state. Generated code embeds a slot address computed from its fixed ordinal.
#[repr(C)]
pub struct CallCell {
    // Permanent native-contract entries. The checked view below is temporary
    // compatibility linkage for callers that still poll after returning.
    native_entries: Option<[AtomicUsize; 2]>,
    ordinary_entry: AtomicUsize,
    ordinary_heat: AtomicUsize,
    register_entry: AtomicUsize,
    slice_entry: AtomicUsize,
    state: AtomicUsize,
    ordinal: usize,
    cold_register: usize,
    cold_slice: usize,
    revision: AtomicU64,
}

impl CallCell {
    pub fn new(ordinal: usize, cold_register: usize, cold_slice: usize) -> Self {
        Self {
            native_entries: None,
            ordinary_entry: AtomicUsize::new(0),
            ordinary_heat: AtomicUsize::new(0),
            register_entry: AtomicUsize::new(cold_register),
            slice_entry: AtomicUsize::new(cold_slice),
            state: AtomicUsize::new(0),
            ordinal,
            cold_register,
            cold_slice,
            revision: AtomicU64::new(0),
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

    /// Stable owning-cell revision word for generated ordinary admission.
    /// Registration seeds it under the binding lock before generated exposure.
    /// Its address remains valid while the execution retains this cell.
    pub fn revision_address(&self) -> *const AtomicU64 {
        &self.revision
    }

    /// Pointer to an OrdinaryEntryDescriptor, or zero for cold/ineligible targets.
    /// Loading it does not retain it: callers must be inside an owning native
    /// segment before loading or dereferencing the descriptor.
    pub fn ordinary_entry_address(&self) -> *const AtomicUsize {
        &self.ordinary_entry
    }

    /// Atomic pointer to retained immutable OrdinaryCallHeat metadata.
    pub fn ordinary_heat_address(&self) -> *const AtomicUsize {
        &self.ordinary_heat
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

    fn invalidate(&self, revision: u64) {
        // A caller that already loaded the old descriptor must reject it even
        // before the dedicated entry slot is cleared.
        self.revision.store(revision, Ordering::Release);
        self.ordinary_entry.store(0, Ordering::Release);
        ORDINARY_EPOCH.fetch_add(1, Ordering::AcqRel);
        self.ordinary_heat.store(0, Ordering::Release);
        self.register_entry
            .store(self.cold_register, Ordering::Release);
        self.slice_entry.store(self.cold_slice, Ordering::Release);
    }
}

struct Binding {
    revision: u64,
    cells: Vec<Weak<CallCell>>,
}

impl Binding {
    /// Index one more execution's cell. Each execution (native thread or
    /// fiber) owns its own cell for a name, and its retirement drops it, but a
    /// dead `Weak` still pins the cell's allocation. Sweep them out before the
    /// vector would grow, so a thread-per-task workload keeps this bounded by
    /// about twice the live executions instead of growing with every thread
    /// that ever ran (bliss-orayb). The sweep is amortized O(1) per push.
    fn push_cell(&mut self, cell: &Arc<CallCell>) {
        if self.cells.len() == self.cells.capacity() {
            self.cells.retain(|weak| weak.strong_count() > 0);
        }
        self.cells.push(Arc::downgrade(cell));
    }
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
        INVALIDATION_EPOCH.fetch_add(1, Ordering::AcqRel);
        let binding = &mut self.bindings[ordinal];
        binding.revision = binding.revision.wrapping_add(1);
        binding.cells.retain(|weak| {
            if let Some(cell) = weak.upgrade() {
                cell.invalidate(binding.revision);
                true
            } else {
                false
            }
        });
    }

    fn publish_ordinary(&self, cell: &CallCell, descriptor: &OrdinaryEntryDescriptor) -> bool {
        self.publish_ordinary_with_heat(cell, descriptor, 0)
    }

    fn publish_ordinary_with_heat(&self, cell: &CallCell, descriptor: &OrdinaryEntryDescriptor, heat: usize) -> bool {
        if !descriptor.is_compatible()
            || self.bindings[cell.ordinal].revision != descriptor.revision
        {
            return false;
        }
        cell.ordinary_heat.store(heat, Ordering::Release);
        cell.ordinary_entry
            .store(descriptor as *const _ as usize, Ordering::Release);
        ORDINARY_EPOCH.fetch_add(1, Ordering::AcqRel);
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

/// Bumped after every change to any cell's published ordinary entry. A
/// directory that saw no change since its last sweep has nothing to retire.
static ORDINARY_EPOCH: AtomicU64 = AtomicU64::new(0);

/// Bumped by every invalidation of any binding: a definition change, a tier
/// publication or a broad native-code invalidation.
static INVALIDATION_EPOCH: AtomicU64 = AtomicU64::new(0);

/// The current invalidation epoch. A cache of call targets that records it
/// when it selects a target, and rejects the target once it has changed, sees
/// every change that would make a call cell select again.
pub fn invalidation_epoch() -> u64 {
    INVALIDATION_EPOCH.load(Ordering::Acquire)
}

/// The address of the invalidation epoch word, for generated code that
/// compares a recorded epoch with the current one.
pub fn invalidation_epoch_address() -> usize {
    INVALIDATION_EPOCH.as_ptr() as usize
}

/// The current ordinary publication epoch. Read it BEFORE inspecting the
/// slots it summarizes: a change racing that inspection then shows up as a
/// newer epoch on the next read.
pub fn ordinary_epoch() -> u64 {
    ORDINARY_EPOCH.load(Ordering::Acquire)
}

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
    let binding = &mut table.bindings[cell.ordinal];
    cell.revision.store(binding.revision, Ordering::Release);
    binding.push_cell(cell);
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
    // Even a name no cell has resolved yet: callable caches key on this.
    INVALIDATION_EPOCH.fetch_add(1, Ordering::AcqRel);
    let Some(table) = TABLE.get() else { return };
    let mut table = table.lock().unwrap();
    if let Some(&ordinal) = table.by_symbol.get(&symbol) {
        table.invalidate(ordinal);
    }
}

/// Publish retained profiling metadata atomically with the descriptor's
/// revision check. Zero removes any previous profile (e.g. a named adapter).
///
/// # Safety
/// Both pointers must remain valid under the same execution segment lease;
/// a nonzero heat pointer must identify this exact descriptor.
pub unsafe fn publish_ordinary_with_heat(
    cell: &CallCell, descriptor: &OrdinaryEntryDescriptor, heat: usize,
) -> bool {
    TABLE.get_or_init(Default::default).lock().unwrap()
        .publish_ordinary_with_heat(cell, descriptor, heat)
}

/// Existing broad native-code invalidations also redirect callable slots.
/// This runs on definition/tier changes, never on an ordinary warmed call.
pub fn invalidate_all() {
    INVALIDATION_EPOCH.fetch_add(1, Ordering::AcqRel);
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
            stack_required: 8,
        }
    }

    #[test]
    #[cfg(target_pointer_width = "64")]
    fn ordinary_descriptor_preserves_existing_offsets_and_appends_stack_bound() {
        assert_eq!(std::mem::offset_of!(OrdinaryEntryDescriptor, owner), 64);
        assert_eq!(
            std::mem::offset_of!(OrdinaryEntryDescriptor, stack_required),
            72
        );
        assert_eq!(std::mem::size_of::<OrdinaryEntryDescriptor>(), 80);
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
        let published = CallCell::new(ordinal, 11, 12);
        let valid = ordinary_descriptor(0);
        assert!(table.publish_ordinary(&published, &valid));
        for mutation in 0..12 {
            let mut entry = ordinary_descriptor(0);
            match mutation {
                0 => entry.kind ^= 1,
                1 => entry.version += 1,
                2 => entry.entry = entry.code_start + entry.code_len,
                3 => entry.code_len = usize::MAX,
                4 => entry.owner = 0,
                5 => entry.environment = 99,
                6 => entry.code_len = 0,
                7 => entry.stack_required = 0,
                8 => entry.stack_required = usize::MAX,
                9 => {
                    // Keep the entry inside the range so this specifically
                    // checks that address zero is not a valid code base.
                    entry.code_start = 0;
                    entry.entry = 0x10;
                }
                10 => entry.entry = entry.code_start - 1,
                11 => entry.stack_required = 7,
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
            assert!(
                !table.publish_ordinary(&published, &entry),
                "invalid replacement {mutation}"
            );
            assert_eq!(
                unsafe { &*published.ordinary_entry_address() }.load(Ordering::Acquire),
                &valid as *const _ as usize,
                "rejected replacement {mutation} must preserve the valid target"
            );
        }
        let entry = ordinary_descriptor(0);
        assert!(entry.accepts(4, ORDINARY_ENV_NONE));
        assert!(!entry.accepts(3, ORDINARY_ENV_NONE));
        assert!(!entry.accepts(4, ORDINARY_ENV_CLOSURE));
        let binder = OrdinaryEntryDescriptor { arity: ORDINARY_ARITY_ANY, ..entry };
        assert!(binder.accepts(0, ORDINARY_ENV_NONE));
        assert!(binder.accepts(19, ORDINARY_ENV_NONE));
        assert!(!binder.accepts(19, ORDINARY_ENV_CLOSURE));
    }

    #[test]
    fn retired_execution_cells_do_not_accumulate_in_their_binding() {
        let mut table = Table::default();
        let ordinal = table.ordinal(23);
        let live = Arc::new(CallCell::new(ordinal, 11, 12));
        table.bindings[ordinal].push_cell(&live);
        // A thousand short-lived executions each register and drop a cell.
        for _ in 0..1000 {
            let retired = Arc::new(CallCell::new(ordinal, 11, 12));
            table.bindings[ordinal].push_cell(&retired);
        }
        let cells = &table.bindings[ordinal].cells;
        assert!(cells.len() <= 4, "dead cells kept: {}", cells.len());
        assert!(cells.iter().any(|weak| weak.as_ptr() == Arc::as_ptr(&live)));
        table.invalidate(ordinal);
        assert_eq!(live.revision.load(Ordering::Acquire), 1);
    }

    #[test]
    fn late_execution_cells_start_at_the_current_binding_revision() {
        let symbol = crate::symbols::intern("ORDINARY-LATE-REVISION-CELL");
        let ordinal = resolve_ordinal(symbol).unwrap();
        let initial = revision(ordinal);
        invalidate(symbol);
        invalidate(symbol);
        let cell = Arc::new(CallCell::new(ordinal, 11, 12));
        register(&cell);
        let revision_address = cell.revision_address();
        assert_eq!(
            unsafe { &*revision_address }.load(Ordering::Acquire),
            initial + 2
        );
        let mut stale = ordinary_descriptor(initial);
        assert!(
            !TABLE
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .publish_ordinary(&cell, &stale)
        );
        stale.revision = initial + 2;
        assert!(
            TABLE
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .publish_ordinary(&cell, &stale)
        );
        invalidate(symbol);
        assert_eq!(cell.revision_address(), revision_address);
        assert_eq!(
            unsafe { &*revision_address }.load(Ordering::Acquire),
            initial + 3
        );
        assert_eq!(
            unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
            0
        );
    }

    #[test]
    fn every_execution_cell_keeps_a_stable_current_revision_word() {
        let mut table = Table::default();
        let ordinal = table.ordinal(7);
        let first = Arc::new(CallCell::new(ordinal, 1, 2));
        let second = Arc::new(CallCell::new(ordinal, 3, 4));
        table.bindings[ordinal]
            .cells
            .extend([Arc::downgrade(&first), Arc::downgrade(&second)]);
        let addresses = [first.revision_address(), second.revision_address()];
        assert_ne!(addresses[0], addresses[1]);
        let old = ordinary_descriptor(0);
        assert!(table.publish_ordinary(&first, &old));
        assert!(table.publish_ordinary(&second, &old));
        for symbol in 100..10000 {
            table.ordinal(symbol);
        }
        table.invalidate(ordinal);
        for (cell, address) in [&first, &second].into_iter().zip(addresses) {
            assert_eq!(cell.revision_address(), address);
            assert_eq!(unsafe { &*address }.load(Ordering::Acquire), 1);
            assert_eq!(
                unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire),
                0
            );
            assert!(!table.publish_ordinary(cell, &old));
        }
        drop(second);
        table.invalidate(ordinal);
        assert_eq!(unsafe { &*addresses[0] }.load(Ordering::Acquire), 2);
        assert_eq!(table.bindings[ordinal].cells.len(), 1);
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
