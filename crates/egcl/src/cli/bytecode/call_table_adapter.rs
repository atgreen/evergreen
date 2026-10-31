// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary register entries for named targets that require runtime binding.
//! The machine adapter owns no Lisp activation. Its helper publishes the caller
//! and enters the existing interpreter or protected-body boundary from Rust.

use super::*;
use egcl_rt::call_table::{
    ORDINARY_ARITY_ANY, ORDINARY_ENV_NONE, ORDINARY_X64_LISP_KIND, ORDINARY_X64_LISP_VERSION,
    OrdinaryEntryDescriptor,
};

pub(super) struct NamedAdapter {
    _code: Rc<egcl_rt::jit::JitBuffer>,
    /// The builtin FUNCALL cell's probe table, read by `_code`.
    probe: Option<Rc<FuncallProbe>>,
    descriptor: OrdinaryEntryDescriptor,
}

/// Retain immutable descriptors for suspended callers, while keeping lookup
/// independent of the number of binding revisions accumulated by a segment.
/// The indices contain only Rust metadata; boxes still own all code and probes.
#[derive(Default)]
pub(super) struct AdapterHistory {
    #[allow(clippy::vec_box)] // machine callers borrow immutable descriptor addresses
    entries: Vec<Box<NamedAdapter>>,
    by_revision: HashMap<u64, usize, egcl_rt::fxhash::FxBuildHasher>,
    descriptors: std::collections::HashSet<usize, egcl_rt::fxhash::FxBuildHasher>,
}

impl std::ops::Deref for AdapterHistory {
    type Target = [Box<NamedAdapter>];

    fn deref(&self) -> &Self::Target {
        &self.entries
    }
}

impl AdapterHistory {
    fn for_revision(&self, revision: u64) -> Option<&NamedAdapter> {
        self.by_revision
            .get(&revision)
            .map(|&index| self.entries[index].as_ref())
    }

    fn contains_descriptor(&self, address: usize) -> bool {
        self.descriptors.contains(&address)
    }

    fn push(&mut self, adapter: Box<NamedAdapter>) {
        // Match the former forward search even if revisions eventually wrap.
        self.by_revision
            .entry(adapter.descriptor.revision)
            .or_insert(self.entries.len());
        self.descriptors
            .insert(std::ptr::from_ref(&adapter.descriptor) as usize);
        self.entries.push(adapter);
    }

    fn clear(&mut self) {
        self.by_revision.clear();
        self.descriptors.clear();
        self.entries.clear();
    }
}

/// The table a FUNCALL cell's ordinary entry probes before its runtime
/// veneer (bliss-6m6ca): which function objects run which named cell's
/// ordinary entry. Only this execution writes it, between generated reads.
pub(super) struct FuncallProbe {
    entries: [std::cell::UnsafeCell<egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry>;
        egcl_compiler::t2::native_transfer::ORDINARY_FUNCALL_PROBE_ENTRIES],
    next: std::cell::Cell<usize>,
    /// The last (function, arity, epoch, collection stamp) found ineligible, so a
    /// call the probe can never take does not pay for selection each time.
    rejected: std::cell::Cell<(u64, usize, u64, u64)>,
}

/// The revision word of an empty probe entry; no cell revision reaches it.
static NO_REVISION: u64 = u64::MAX;

impl FuncallProbe {
    fn new() -> Self {
        Self {
            entries: std::array::from_fn(|_| {
                std::cell::UnsafeCell::new(
                    egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry {
                        function: NIL.0,
                        selected_callable: NIL.0,
                        // No epoch reaches this value, so an empty entry never
                        // reads its revision word.
                        epoch: u64::MAX,
                        collections: u64::MAX,
                        revision_address: std::ptr::from_ref(&NO_REVISION) as u64,
                        revision: 0,
                        descriptor: 0,
                    },
                )
            }),
            next: std::cell::Cell::new(0),
            rejected: std::cell::Cell::new((NIL.0, usize::MAX, u64::MAX, u64::MAX)),
        }
    }

    fn record(&self, entry: egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry) {
        let index = if super::super::native_transfer_entry::ordinary_function_designators_enabled()
        {
            // A newly selected hot callable should take the first generated
            // comparison. Coalesce its old slot rather than filling the table
            // with duplicates when a mapped caller repeatedly records it.
            let previous = self
                .entries
                .iter()
                .position(|slot| unsafe { (*slot.get()).function == entry.function })
                .unwrap_or(self.entries.len() - 1);
            if previous == 0 && unsafe { *self.entries[0].get() == entry } {
                return;
            }
            for index in (1..=previous).rev() {
                // Only this execution writes, without allocating or yielding;
                // no generated reader can observe this partial permutation.
                unsafe {
                    self.entries[index]
                        .get()
                        .write(*self.entries[index - 1].get());
                }
            }
            0
        } else {
            let index = self.next.get();
            self.next.set((index + 1) % self.entries.len());
            index
        };
        unsafe { self.entries[index].get().write(entry) };
    }
}

/// Record that FUNCALL of `function` with `nargs` actuals runs its named
/// cell's ordinary entry, for the FUNCALL cell's generated probe.
///
/// Only the current definition of a name qualifies: the probe then enters
/// that cell's published descriptor exactly as a named call does, under the
/// same revision check, so a redefinition, tier change or republication
/// misses. The invalidation epoch and the collection stamp are read first:
/// a change during selection, which may resolve a cell, retires the entry.
pub(super) fn record_funcall(state: &State, function: EgclVal, nargs: usize) {
    if nargs > 3 {
        return;
    }
    let Some(probe) = state
        .ordinary_adapters
        .borrow()
        .first()
        .and_then(|adapter| adapter.probe.clone())
    else {
        return;
    };
    let epoch = egcl_rt::call_table::invalidation_epoch();
    let collections = super::super::native_transfer_entry::funcall_fast::collection_stamp();
    let bits = function.0;
    if probe.rejected.get() == (bits, nargs, epoch, collections) {
        return;
    }
    if !record_funcall_target(&probe, bits, function, nargs, epoch, collections) {
        probe.rejected.set((bits, nargs, epoch, collections));
    }
}

fn record_funcall_target(
    probe: &FuncallProbe,
    bits: u64,
    function: EgclVal,
    nargs: usize,
    epoch: u64,
    collections: u64,
) -> bool {
    use std::sync::atomic::Ordering;
    // A symbol designator selects its current function after all actuals
    // have been evaluated. The probe still keys on the original symbol;
    // rebinding it changes the invalidation epoch before another hit.
    let callable = if let Some(symbol) = function.symbol_index() {
        if !super::super::native_transfer_entry::ordinary_function_designators_enabled() {
            return false;
        }
        let Some(callable) = egcl_rt::symbols::symbol_function(symbol) else {
            return false;
        };
        callable
    } else {
        function
    };
    let Some((symbol, code, selected, native_owner)) =
        super::super::native_transfer_entry::funcall_fast::native_target(callable, nargs)
    else {
        return false;
    };
    let captured = code.native_callable_slot.is_some();
    if captured {
        if !code.accepts_explicit_callable(selected) {
            return false;
        }
    } else if egcl_rt::symbols::symbol_function(symbol) != Some(callable) {
        return false;
    }
    if captured {
        let Some((descriptor, revision_address, revision)) =
            super::ordinary::private_probe_target(Rc::clone(&code), native_owner)
        else {
            return false;
        };
        if super::super::native_transfer_entry::funcall_fast::collection_stamp() != collections {
            return false;
        }
        probe.record(
            egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry {
                function: bits,
                selected_callable: selected.0,
                epoch,
                collections,
                revision_address: revision_address as u64,
                revision,
                descriptor: descriptor as u64,
            },
        );
        return true;
    }
    let Some(cell) = resolve(symbol) else {
        return false;
    };
    let current = || {
        let pointer = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
        (pointer != 0 && (captured || !cell.is_cold()))
            .then_some(pointer)
            .filter(|&pointer| {
                let descriptor = unsafe { &*(pointer as *const OrdinaryEntryDescriptor) };
                descriptor.owner == Rc::as_ptr(&code) as usize
                    && descriptor.revision == linkage::revision(cell.ordinal())
            })
    };
    // A cell that only FUNCALL reaches is never re-resolved by a named call,
    // so after an invalidation it stays cold. Publish what a named call would
    // now select, as its cold path does. A warm cell without a matching entry
    // (a typed or variadic body, say) is left alone: nothing to record.
    let pointer = match current() {
        Some(pointer) => pointer,
        None if !cell.is_cold() => return false,
        None => {
            let target_state = unsafe { super::state(Arc::as_ptr(&cell) as u64) };
            super::publish_target(
                target_state,
                linkage::revision(cell.ordinal()),
                super::select_target(symbol, nargs),
            );
            let Some(pointer) = current() else {
                return false;
            };
            pointer
        }
    };
    let descriptor = unsafe { &*(pointer as *const OrdinaryEntryDescriptor) };
    if !descriptor.accepts(
        nargs,
        if captured {
            linkage::ORDINARY_ENV_CLOSURE
        } else {
            ORDINARY_ENV_NONE
        },
    ) {
        return false;
    }
    // Publication may retire a previous descriptor (or park at a GC gate in
    // the legacy environment-free route). Never bless stale weak bits with a
    // freshly read stamp: retry selection with current inputs on the next call.
    if super::super::native_transfer_entry::funcall_fast::collection_stamp() != collections {
        return false;
    }
    probe.record(
        egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry {
            function: bits,
            selected_callable: if captured { selected.0 } else { NIL.0 },
            epoch,
            collections,
            revision_address: cell.revision_address() as u64,
            revision: descriptor.revision,
            descriptor: pointer as u64,
        },
    );
    true
}

/// The FUNCALL cell's entry: the probe, then the runtime veneer.
fn funcall_adapter_code(state: &State, probe: &FuncallProbe) -> Vec<u8> {
    let mut bytes = egcl_compiler::t2::native_transfer::emit_ordinary_funcall_probe(
        egcl_compiler::t2::native_transfer::OrdinaryFuncallProbe {
            table: probe.entries.as_ptr() as u64,
            epoch: egcl_rt::call_table::invalidation_epoch_address() as u64,
            collections: super::super::native_transfer_entry::funcall_fast::collection_stamp_address(
            ) as u64,
            active_control: egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL as i32,
            stack_limit: std::mem::offset_of!(
                egcl_compiler::t2::emit::NativeRecursiveControl,
                stack_limit
            ) as u8,
        },
    );
    bytes.extend_from_slice(
        &egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
            super::super::native_transfer_entry::ordinary_calls::named_entry,
            Arc::as_ptr(&state.cell) as usize,
        ),
    );
    bytes
}

pub(super) fn ensure(state: &State) {
    use std::sync::atomic::Ordering;
    let current = unsafe { &*state.cell.ordinary_entry_address() }.load(Ordering::Acquire);
    if current != 0 {
        // A public binding may have been warmed before primitive IDs were
        // initialized. Once reserved, only this state's semantic adapters may
        // remain in its ordinary slot; an earlier native body must be replaced.
        let private =
            super::super::native_transfer_entry::ordinary_primitives::is_private(state.symbol);
        if !private
            || state
                .ordinary_adapters
                .borrow()
                .contains_descriptor(current)
        {
            return;
        }
    }
    let revision = linkage::revision(state.cell.ordinal());
    // A revision can be republished after invalidation without mutating any
    // descriptor that a suspended native caller might still hold.
    let mut adapters = state.ordinary_adapters.borrow_mut();
    if let Some(adapter) = adapters.for_revision(revision) {
        unsafe {
            linkage::publish_ordinary(&state.cell, &adapter.descriptor);
        }
        return;
    }
    let (code, probe, code_len) = if let Some(previous) = adapters.first() {
        (
            Rc::clone(&previous._code),
            previous.probe.clone(),
            previous.descriptor.code_len,
        )
    } else {
        // The builtin FUNCALL cell first probes for a recorded target.
        let probe = super::super::call_table_funcall::is_builtin(state.symbol)
            .then(|| Rc::new(FuncallProbe::new()));
        // The adapter carries its call cell, which outlives it: the state
        // owns both, so the helper need not resolve the symbol per call.
        let bytes = match &probe {
            Some(probe) => funcall_adapter_code(state, probe),
            None => {
                // A builtin with leaf helpers answers them before the veneer
                // (bliss-gg3i1.7.3); the stub falls through into it.
                let mut bytes = super::super::leaf_builtins::ordinary_stub(state.symbol);
                bytes.extend_from_slice(
                    &egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
                        super::super::native_transfer_entry::ordinary_calls::named_entry,
                        Arc::as_ptr(&state.cell) as usize,
                    ),
                );
                bytes
            }
        };
        let Some(code) = egcl_rt::jit::JitBuffer::new(&bytes) else {
            return;
        };
        (Rc::new(code), probe, bytes.len())
    };
    if !super::ordinary::has_active_segments() {
        adapters.clear();
    }
    let address = code.as_ptr() as usize;
    let mut adapter = Box::new(NamedAdapter {
        _code: code,
        probe,
        descriptor: OrdinaryEntryDescriptor {
            kind: ORDINARY_X64_LISP_KIND,
            version: ORDINARY_X64_LISP_VERSION,
            revision,
            entry: address,
            code_start: address,
            code_len,
            arity: ORDINARY_ARITY_ANY,
            environment: ORDINARY_ENV_NONE,
            owner: 0,
            // Includes the capture request and emergency helper-call reserve.
            stack_required: 4096,
        },
    });
    adapter.descriptor.owner = std::ptr::from_ref(&*adapter) as usize;
    adapters.push(adapter);
    unsafe {
        linkage::publish_ordinary(&state.cell, &adapters.last().unwrap().descriptor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_adapter_indexes_aged_history_and_clears_retired_indices() {
        use std::sync::atomic::Ordering;

        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        egcl_rt::rooted!(_env = Env::new(false));
        let symbol = egcl_rt::symbols::intern("ORDINARY-ADAPTER-AGED-HISTORY");
        let cell = resolve(symbol).unwrap();
        let state = unsafe { super::super::state(Arc::as_ptr(&cell) as u64) };
        let lease = super::super::ordinary::enter_ordinary_segment();
        let original = std::ptr::from_ref(&state.ordinary_adapters.borrow()[0].descriptor);
        let original_revision = unsafe { (*original).revision };
        let original_entry = unsafe { (*original).entry };
        let original_owner = unsafe { (*original).owner };

        const REVISIONS: usize = 4096;
        for _ in 0..REVISIONS {
            linkage::invalidate(symbol);
            ensure(state);
        }
        let current = unsafe { &*cell.ordinary_entry_address() };
        let published = current.load(Ordering::Acquire);
        {
            let history = state.ordinary_adapters.borrow();
            assert_eq!(history.len(), REVISIONS + 1);
            assert_eq!(history.by_revision.len(), history.len());
            assert_eq!(history.descriptors.len(), history.len());
            for adapter in history.iter() {
                let descriptor = std::ptr::from_ref(&adapter.descriptor);
                assert!(history.contains_descriptor(descriptor as usize));
                assert!(std::ptr::eq(
                    &history
                        .for_revision(adapter.descriptor.revision)
                        .unwrap()
                        .descriptor,
                    descriptor,
                ));
                assert_eq!(adapter.descriptor.entry, original_entry);
            }
            assert!(!history.contains_descriptor(0));
            assert!(
                history
                    .for_revision(linkage::revision(cell.ordinal()).wrapping_add(1))
                    .is_none()
            );
        }

        // Exercise the real ensure revision-hit path, not just the index API.
        // A cleared publication at the same revision must reuse its descriptor.
        for _ in 0..32 {
            current.store(0, Ordering::Release);
            ensure(state);
            assert_eq!(current.load(Ordering::Acquire), published);
            ensure(state); // an already-published entry is also unchanged
        }
        assert_eq!(state.ordinary_adapters.borrow().len(), REVISIONS + 1);
        assert_eq!(unsafe { (*original).revision }, original_revision);
        assert_eq!(unsafe { (*original).owner }, original_owner);
        assert_eq!(unsafe { (*original).entry }, original_entry);

        drop(lease);
        linkage::invalidate(symbol);
        ensure(state);
        let history = state.ordinary_adapters.borrow();
        assert_eq!(history.len(), 1);
        assert_eq!(history.by_revision.len(), 1);
        assert_eq!(history.descriptors.len(), 1);
        assert!(history.for_revision(original_revision).is_none());
        assert_eq!(history[0].descriptor.entry, original_entry);
        assert!(history.contains_descriptor(current.load(Ordering::Acquire)));
    }

    #[test]
    fn named_adapter_reuses_code_and_keeps_live_revision_descriptors() {
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let _env = Env::new(false);
        let symbol = egcl_rt::symbols::intern("ORDINARY-ADAPTER-REVISION-LIFETIME");
        let cell = resolve(symbol).unwrap();
        let state = unsafe { super::super::state(Arc::as_ptr(&cell) as u64) };
        let address = state.ordinary_adapters.borrow()[0].descriptor.entry;
        let lease = super::super::ordinary::enter_ordinary_segment();
        let original = std::ptr::from_ref(&state.ordinary_adapters.borrow()[0].descriptor);
        for _ in 0..4 {
            linkage::invalidate(symbol);
            resolve(symbol).unwrap();
        }
        assert_eq!(state.ordinary_adapters.borrow().len(), 5);
        assert!(
            state
                .ordinary_adapters
                .borrow()
                .iter()
                .all(|adapter| adapter.descriptor.entry == address)
        );
        assert_eq!(
            unsafe { (*original).entry },
            address,
            "suspended descriptor remains valid"
        );
        drop(lease);
        linkage::invalidate(symbol);
        resolve(symbol).unwrap();
        assert_eq!(state.ordinary_adapters.borrow().len(), 1);
        assert_eq!(
            state.ordinary_adapters.borrow()[0].descriptor.entry,
            address
        );
    }
}
