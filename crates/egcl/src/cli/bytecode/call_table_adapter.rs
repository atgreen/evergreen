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
    descriptor: OrdinaryEntryDescriptor,
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
    let (code, code_len) = if let Some(previous) = adapters.first() {
        (Rc::clone(&previous._code), previous.descriptor.code_len)
    } else {
        // The adapter carries its call cell, which outlives it: the state
        // owns both, so the helper need not resolve the symbol per call.
        let bytes = egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
            super::super::native_transfer_entry::ordinary_calls::named_entry,
            Arc::as_ptr(&state.cell) as usize,
        );
        let Some(code) = egcl_rt::jit::JitBuffer::new(&bytes) else {
            return;
        };
        (Rc::new(code), bytes.len())
    };
    if !super::ordinary::has_active_segments() {
        adapters.clear();
    }
    let address = code.as_ptr() as usize;
    let mut adapter = Box::new(NamedAdapter {
        _code: code,
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
