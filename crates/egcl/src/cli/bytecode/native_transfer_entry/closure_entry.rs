// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Closures and FUNCALL through the entry word (Backpack calls stage 3,
//! bliss-gg3i1.7.15.10.3).
//!
//! An ordinary T2 call to the builtin FUNCALL first tries the lean FUNCALL
//! (`egcl_compiler::t2::emit::FUNCALL_FIRST_ARGUMENT`): when the function is
//! an interpreted function whose entry word this execution owns, it calls
//! that entry with the function in RAX, exactly as a lean named call does.
//! A plain global function's word is claimed by its call cell's ordinary
//! publication (stage 1).
//!
//! A closure template gets one process-lifetime [`TemplateRecord`]: an
//! `[entry, owner]` pair and a dispatch stub that enters `entry` when the
//! caller's execution owns the pair. Every instance's entry word is that stub,
//! owned by `SHARED_ENTRY_OWNER`, so every execution's lean FUNCALL calls it;
//! the prototype's word is the stub too, shared only when the template's code
//! accepts the prototype itself as its receiver (a closure that captures
//! nothing is its prototype). Instances made after the stub exists copy it at
//! allocation; older ones are armed here when they first miss.
//!
//! The execution whose installed T2 code of the template has a lean ordinary
//! entry claims the pair, from the runtime path a missed call takes. The claim
//! lives exactly as long as that code is the template's current registry
//! entry: `TransferCode::retire_private_entries` (replacement, removal,
//! recompilation, the code's retirement) releases it, so tiering a template up
//! is one store to its pair and its instances follow at once. Only the owning
//! execution, or the collector with every execution stopped, writes a claimed
//! pair, so the stub's single owner comparison pairs with that execution's
//! current entry. Another execution misses to the runtime, as before.
//!
//! The directory retains the claimed code's descriptor for this execution, as
//! the FUNCALL probe did, so native frames of it remain walkable and its code
//! outlives every active segment that may have entered it.
//!
//! Records and stubs are never freed: instance words name them and are not
//! tracked. Each costs a few dozen bytes per template that reached T2 and was
//! called through FUNCALL.

use super::*;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// A closure template's claimable entry and the stub that enters it.
#[repr(C)]
pub(in crate::cli::bytecode) struct TemplateRecord {
    /// The claimed code's lean ordinary entry, or 0. Read by the stub first.
    entry: AtomicUsize,
    /// The claiming execution's code-owner key, or 0.
    owner: AtomicU64,
    /// The dispatch stub, an instance's entry word.
    stub: usize,
}

const _: () = {
    assert!(std::mem::offset_of!(TemplateRecord, entry) == 0);
    assert!(std::mem::offset_of!(TemplateRecord, owner) == 8);
};

/// One code version's claim on a record, released when that version retires.
pub(in crate::cli::bytecode) struct TemplateClaim {
    record: &'static TemplateRecord,
    owner: u64,
    entry: usize,
}

impl TemplateClaim {
    /// Release the pair if it still names this claim. Only the owning
    /// execution, or the collector with it stopped, gets here.
    pub(in crate::cli::bytecode) fn release(&self) {
        if self.record.owner.load(Ordering::Acquire) == self.owner
            && self.record.entry.load(Ordering::Acquire) == self.entry
        {
            // Entry first: no other execution can claim before the owner
            // clears, and this one is not calling the stub meanwhile.
            self.record.entry.store(0, Ordering::Release);
            self.record.owner.store(0, Ordering::Release);
        }
    }
}

/// Every record, by stub address. Records are never freed, so a stub address
/// never names another template's record.
static RECORDS: OnceLock<std::sync::Mutex<HashMap<usize, &'static TemplateRecord>>> =
    OnceLock::new();

/// Whether ordinary FUNCALL call sites try the lean FUNCALL. On with lean
/// calls; `EGCL_LEAN_FUNCALL=0` keeps the FUNCALL cell's admission for every
/// call, for comparison.
pub(in crate::cli::bytecode) fn lean_funcall_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        super::lean_calls_enabled()
            && !std::env::var("EGCL_LEAN_FUNCALL").is_ok_and(|value| value == "0")
            && miss_veneer().is_some()
    })
}

/// Whether `symbol` is FUNCALL. Safe on any thread: compilation may run off
/// the mutator. A user may not redefine a standard function, so a call
/// compiled as FUNCALL stays one.
pub(in crate::cli::bytecode) fn is_funcall(symbol: u32) -> bool {
    static FUNCALL: OnceLock<Option<u32>> = OnceLock::new();
    *FUNCALL.get_or_init(|| egcl_rt::symbols::find_index("FUNCALL")) == Some(symbol)
}

/// The process-lifetime veneer a stub misses to.
fn miss_veneer() -> Option<usize> {
    static VENEER: OnceLock<Option<usize>> = OnceLock::new();
    *VENEER.get_or_init(|| {
        let veneer = JitBuffer::new(
            &egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
                ordinary_calls::closure_miss,
                0,
            ),
        )?;
        Some(Box::leak(Box::new(veneer)).as_ptr() as usize)
    })
}

/// `prototype`'s record, made and installed in its word on first use. Its
/// word is shared, so that a call of the prototype itself enters the stub,
/// only when `callable`. `None` when the word holds something else.
fn record_for(prototype: EgclVal, callable: bool) -> Option<&'static TemplateRecord> {
    let mut records = RECORDS
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let (word, _) = egcl_rt::function::entry_word(prototype)?;
    let record = if word != 0 {
        *records.get(&word)?
    } else {
        let mut record = Box::new(TemplateRecord {
            entry: AtomicUsize::new(0),
            owner: AtomicU64::new(0),
            stub: 0,
        });
        let address = std::ptr::from_ref(&*record) as u64;
        let stub = JitBuffer::new(&egcl_compiler::t2::emit::emit_template_entry_stub(
            address,
            miss_veneer()? as u64,
        ))?;
        record.stub = Box::leak(Box::new(stub)).as_ptr() as usize;
        let record: &'static TemplateRecord = Box::leak(record);
        records.insert(record.stub, record);
        record
    };
    // SAFETY: a live prototype; the stub lives for the process.
    unsafe { egcl_rt::function::arm_shared_entry(prototype, record.stub, callable) }
        .then_some(record)
}

/// Offer `function`, which a FUNCALL with `nargs` arguments is about to apply
/// through the runtime, to its template's entry claim: when it is a closure
/// whose template has current T2 code with a lean ordinary entry in this
/// execution, claim the template's pair for that code and arm `function`'s
/// word, so its next call enters the code directly. Allocation-free and
/// non-collecting; a no-op for anything else.
pub(in crate::cli::bytecode) fn teach(function: EgclVal, nargs: usize) {
    if !lean_funcall_enabled() || !egcl_rt::function::is_interpreted_function(function) {
        return;
    }
    let prototype = if let Some(prototype) = egcl_rt::function::closure_prototype(function) {
        prototype
    } else if egcl_rt::function::is_closure_template(function) {
        function
    } else {
        return;
    };
    let Some((_, code, selected, native)) = funcall_fast::native_target(function, nargs) else {
        return;
    };
    if selected != function || code.native_callable_slot.is_none() {
        return;
    }
    let Some(entry) = code.ordinary_lean_entry_address() else {
        return;
    };
    // Retain the code's private descriptor in this execution's directory, so
    // its frames stay walkable and its code outlives every active segment.
    let Some((descriptor, _, _)) =
        super::super::call_table::private_probe_target(Rc::clone(&code), native)
    else {
        return;
    };
    if !unsafe { &*(descriptor as *const egcl_rt::call_table::OrdinaryEntryDescriptor) }
        .contains_pc(entry)
    {
        return;
    }
    let Some(execution) = egcl_rt::thread::current_execution_context_for_native() else {
        return;
    };
    let owner = unsafe { &*execution }.code_owner();
    let Some(record) = record_for(prototype, code.accepts_explicit_callable(prototype)) else {
        return;
    };
    if record
        .owner
        .compare_exchange(0, owner, Ordering::AcqRel, Ordering::Acquire)
        .is_err_and(|current| current != owner)
    {
        // Another execution's code of this template holds the pair.
        return;
    }
    record.entry.store(entry, Ordering::Release);
    code.note_template_claim(TemplateClaim {
        record,
        owner,
        entry,
    });
    if function != prototype {
        // SAFETY: a live instance; the stub lives for the process.
        unsafe { egcl_rt::function::arm_shared_entry(function, record.stub, true) };
    }
}

/// The record `stub` dispatches through (tests).
#[cfg(test)]
pub(in crate::cli::bytecode) fn record_state_for_test(stub: usize) -> Option<(usize, u64)> {
    let records = RECORDS.get()?.lock().unwrap_or_else(|e| e.into_inner());
    let record = records.get(&stub)?;
    Some((
        record.entry.load(Ordering::Acquire),
        record.owner.load(Ordering::Acquire),
    ))
}

/// Overwrite the pair `stub` dispatches through, as another execution's
/// claim would (tests).
#[cfg(test)]
pub(in crate::cli::bytecode) fn set_record_for_test(stub: usize, entry: usize, owner: u64) {
    let records = RECORDS
        .get()
        .unwrap()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let record = records[&stub];
    record.entry.store(entry, Ordering::Release);
    record.owner.store(owner, Ordering::Release);
}
