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
            entries: std::array::from_fn(|_| std::cell::UnsafeCell::new(
                egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry {
                    function: NIL.0,
                    // No epoch reaches this value, so an empty entry never
                    // reads its revision word.
                    epoch: u64::MAX,
                    collections: u64::MAX,
                    revision_address: std::ptr::from_ref(&NO_REVISION) as u64,
                    revision: 0,
                    descriptor: 0,
                })),
            next: std::cell::Cell::new(0),
            rejected: std::cell::Cell::new((NIL.0, usize::MAX, u64::MAX, u64::MAX)),
        }
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
    let Some(probe) = state.ordinary_adapters.borrow().first().and_then(|adapter| adapter.probe.clone()) else {
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
    probe: &FuncallProbe, bits: u64, function: EgclVal, nargs: usize, epoch: u64, collections: u64,
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
    let Some((symbol, code)) = super::super::native_transfer_entry::funcall_fast::native_target(callable, nargs) else {
        return false;
    };
    if egcl_rt::symbols::symbol_function(symbol) != Some(callable) {
        return false;
    }
    let Some(cell) = resolve(symbol) else { return false };
    let current = || {
        let pointer = unsafe { &*cell.ordinary_entry_address() }.load(Ordering::Acquire);
        (pointer != 0 && !cell.is_cold()).then_some(pointer).filter(|&pointer| {
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
            super::publish_target(target_state, linkage::revision(cell.ordinal()), super::select_target(symbol, nargs));
            let Some(pointer) = current() else {
                return false;
            };
            pointer
        }
    };
    let descriptor = unsafe { &*(pointer as *const OrdinaryEntryDescriptor) };
    if !descriptor.accepts(nargs, ORDINARY_ENV_NONE) {
        return false;
    }
    let index = probe.next.get();
    probe.next.set((index + 1) % probe.entries.len());
    unsafe {
        probe.entries[index].get().write(egcl_compiler::t2::native_transfer::OrdinaryFuncallProbeEntry {
            function: bits,
            epoch,
            collections,
            revision_address: cell.revision_address() as u64,
            revision: descriptor.revision,
            descriptor: pointer as u64,
        });
    }
    true
}

/// The FUNCALL cell's entry: the probe, then the runtime veneer.
fn funcall_adapter_code(state: &State, probe: &FuncallProbe) -> Vec<u8> {
    let mut bytes = egcl_compiler::t2::native_transfer::emit_ordinary_funcall_probe(
        egcl_compiler::t2::native_transfer::OrdinaryFuncallProbe {
            table: probe.entries.as_ptr() as u64,
            epoch: egcl_rt::call_table::invalidation_epoch_address() as u64,
            collections: super::super::native_transfer_entry::funcall_fast::collection_stamp_address() as u64,
            active_control: egcl_rt::exec_context::offsets::ACTIVE_NATIVE_CONTROL as i32,
            stack_limit: std::mem::offset_of!(egcl_compiler::t2::emit::NativeRecursiveControl, stack_limit) as u8,
        });
    bytes.extend_from_slice(&egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
        super::super::native_transfer_entry::ordinary_calls::named_entry,
        Arc::as_ptr(&state.cell) as usize,
    ));
    bytes
}

pub(super) fn ensure(state: &State) {
    use std::sync::atomic::Ordering;
    let current = unsafe { &*state.cell.ordinary_entry_address() }.load(Ordering::Acquire);
    if current != 0 {
        // A public binding may have been warmed before primitive IDs were
        // initialized. Once reserved, only this state's semantic adapters may
        // remain in its ordinary slot; an earlier native body must be replaced.
        let private = super::super::native_transfer_entry::ordinary_primitives::is_private(state.symbol);
        if !private || state.ordinary_adapters.borrow().iter().any(|adapter|
            std::ptr::from_ref(&adapter.descriptor) as usize == current)
        {
            return;
        }
    }
    let revision = linkage::revision(state.cell.ordinal());
    // A revision can be republished after invalidation without mutating any
    // descriptor that a suspended native caller might still hold.
    let mut adapters = state.ordinary_adapters.borrow_mut();
    if let Some(adapter) = adapters
        .iter()
        .find(|entry| entry.descriptor.revision == revision)
    {
        unsafe {
            linkage::publish_ordinary(&state.cell, &adapter.descriptor);
        }
        return;
    }
    let (code, probe, code_len) = if let Some(previous) = adapters.first() {
        (Rc::clone(&previous._code), previous.probe.clone(), previous.descriptor.code_len)
    } else {
        // The builtin FUNCALL cell first probes for a recorded target.
        let probe = super::super::call_table_funcall::is_builtin(state.symbol).then(|| Rc::new(FuncallProbe::new()));
        // The adapter carries its call cell, which outlives it: the state
        // owns both, so the helper need not resolve the symbol per call.
        let bytes = match &probe {
            Some(probe) => funcall_adapter_code(state, probe),
            None => egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
                super::super::native_transfer_entry::ordinary_calls::named_entry,
                Arc::as_ptr(&state.cell) as usize,
            ),
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
