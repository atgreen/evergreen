// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Permanent callable dispatch. Targets are addresses of caller-scanned Lisp
//! slots, never unrooted callable bits or execution-owned code descriptors.

use super::*;
use egcl_compiler::t2::native_transfer::{MappedCallRecord, emit_published_call_entry};
use egcl_rt::function::NativeCallableEntries;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

struct Entries {
    published: NativeCallableEntries,
    _code: [JitBuffer; 2],
}

#[cfg(test)]
static COLLECT_NEXT: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(false)) };
#[cfg(test)]
static MOVED: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(false)) };
#[cfg(test)]
static FAIL_NEXT_POLL: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(false)) };
#[cfg(test)]
pub(super) fn fail_next_entry_poll() {
    FAIL_NEXT_POLL.with(|flag| flag.set(true));
}
#[cfg(test)]
pub(super) fn inject_entry_poll_error() {
    if FAIL_NEXT_POLL.with(|flag| flag.replace(false)) {
        assert_eq!(native_transfer_entry::pending_argument_count(), 1);
        NATIVE_ERROR.with(|slot| {
            slot.set_first(EgclError::ProgramError(
                "injected callable poll transfer".into(),
            ))
        });
    }
}

#[cfg(test)]
pub(super) fn collect_next_entry() {
    COLLECT_NEXT.with(|flag| flag.set(true));
    MOVED.with(|flag| flag.set(false));
}
#[cfg(test)]
pub(super) fn take_relocation() -> bool {
    MOVED.with(|flag| flag.replace(false))
}
#[cfg(test)]
pub(super) unsafe fn collect_at_entry(target: u64) -> Result<(), EgclError> {
    use egcl_rt::Collector;
    if COLLECT_NEXT.with(|flag| flag.replace(false)) {
        let before = unsafe { *(target as *const EgclVal) }.to_raw();
        egcl_rt::HeapCollector::new().minor_gc()?;
        let after = unsafe { *(target as *const EgclVal) }.to_raw();
        MOVED.with(|flag| flag.set(before != after));
    }
    Ok(())
}

pub(super) fn entries() -> Option<&'static NativeCallableEntries> {
    static ENTRIES: std::sync::OnceLock<Option<Entries>> = std::sync::OnceLock::new();
    let entries = ENTRIES
        .get_or_init(|| {
            let entry = |slice| {
                JitBuffer::new(&emit_published_call_entry(
                    slice,
                    native_transfer_entry::prepare_callable,
                    native_transfer_entry::finish_callable,
                    native_transfer_entry::resume_callable,
                    interpreted,
                ))
            };
            let code = [entry(false)?, entry(true)?];
            Some(Entries {
                published: NativeCallableEntries {
                    registers: code[0].as_ptr() as usize,
                    slice: code[1].as_ptr() as usize,
                },
                _code: code,
            })
        })
        .as_ref()?;
    egcl_rt::function::install_native_entries(&entries.published);
    Some(&entries.published)
}

pub(super) fn entries_for(function: EgclVal) -> Option<&'static NativeCallableEntries> {
    let entries = entries()?;
    if egcl_rt::function::is_interpreted_function(function) {
        egcl_rt::function::native_entries(function)
    } else {
        // Symbols, moving wrappers, generic and funcallable objects all enter
        // the same resolver contract. Invalid designators signal inside it.
        Some(entries)
    }
}

unsafe extern "C" fn interpreted(target: u64, record: *mut MappedCallRecord) {
    egcl_rt::rooted!(function = unsafe { *(target as *const EgclVal) });
    egcl_rt::rooted!(
        owned_arguments = unsafe { native_transfer_entry::take_call_arguments(record) }
    );
    let invocation = unsafe { &*(*record).context };
    let args = if let Some(values) = owned_arguments.as_ref() {
        &values[1..]
    } else if invocation.nargs == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(invocation.args, invocation.nargs) }
    };
    let result = guard_c2i(|| {
        let env = NATIVE_ENV.with(std::cell::Cell::get);
        if env.is_null() {
            return Err(EgclError::ProgramError(
                "native callable has no environment".into(),
            ));
        }
        // Every native reentry reached through this Rust adapter creates its
        // own segment. It must never inherit the caller's capture context.
        apply_function(*function, args, unsafe { &mut *env })
    });
    let outcome = match result {
        Ok(value) => NativeOutcome {
            value,
            exit: NativeExit::Returned,
        },
        Err(error) => {
            NATIVE_ERROR.with(|slot| slot.set_first(error));
            NativeOutcome {
                value: NIL,
                exit: NativeExit::Transfer,
            }
        }
    };
    unsafe {
        (*record).outcome = outcome;
    }
}
