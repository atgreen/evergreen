// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Permanent callable dispatch. Targets are addresses of caller-scanned Lisp
//! slots, never unrooted callable bits or execution-owned code descriptors.

use super::native_transfer_entry::root_publication::PublishedAdapter;
use super::*;
use egcl_compiler::t2::native_transfer::{MappedCallRecord, emit_published_call_entry_descriptor};
use egcl_rt::function::NativeCallableEntries;
use egcl_rt::jit::JitBuffer;
use egcl_rt::native_transfer::{NativeExit, NativeOutcome};

pub(super) mod outer;

pub(in crate::cli) fn invoke(
    function: EgclVal,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    outer::invoke(function, args, env)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
static ENTRY_COUNTS: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<(usize, usize)>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new((0, 0))) };

#[cfg(test)]
pub(super) fn observe_entry() {
    let segment = egcl_rt::native_transfer::current_segment();
    assert!(
        !segment.is_null(),
        "published callable must execute inside a segment"
    );
    ENTRY_COUNTS.with(|counts| {
        let (entries, nested) = counts.get();
        counts.set((
            entries + 1,
            nested + usize::from(unsafe { !(*segment).previous().is_null() }),
        ));
    });
}

struct Entries {
    published: NativeCallableEntries,
    code: [PublishedAdapter; 2],
    ordinary: JitBuffer,
}

#[cfg(test)]
static COLLECT_NEXT: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(false)) };
#[cfg(test)]
static COLLECT_WRAPPER_ONLY: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<bool>> =
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
    COLLECT_WRAPPER_ONLY.with(|flag| flag.set(false));
    COLLECT_NEXT.with(|flag| flag.set(true));
    MOVED.with(|flag| flag.set(false));
}
#[cfg(test)]
pub(super) fn collect_next_wrapped_entry() {
    collect_next_entry();
    COLLECT_WRAPPER_ONLY.with(|flag| flag.set(true));
}
#[cfg(test)]
pub(super) fn take_relocation() -> bool {
    MOVED.with(|flag| flag.replace(false))
}
#[cfg(test)]
pub(super) unsafe fn collect_at_entry(target: u64) -> Result<(), EgclError> {
    use egcl_rt::Collector;
    // FUNCALL/APPLY can enter through a symbol designator before dispatching
    // the movable cons wrapper whose publication this fixture exercises.
    if COLLECT_WRAPPER_ONLY.with(|flag| flag.get())
        && !unsafe { *(target as *const EgclVal) }.is_cons()
    {
        return Ok(());
    }
    if COLLECT_NEXT.with(|flag| flag.replace(false)) {
        COLLECT_WRAPPER_ONLY.with(|flag| flag.set(false));
        let before = unsafe { *(target as *const EgclVal) }.to_raw();
        // Full collection also relocates actuals aged by stress collections
        // during fixture construction.
        egcl_rt::HeapCollector::new().full_gc()?;
        let after = unsafe { *(target as *const EgclVal) }.to_raw();
        MOVED.with(|flag| flag.set(before != after));
    }
    Ok(())
}

static ENTRIES: std::sync::OnceLock<Option<Entries>> = std::sync::OnceLock::new();

pub(super) fn mapped_adapter(pc: usize) -> Option<&'static PublishedAdapter> {
    ENTRIES
        .get()?
        .as_ref()?
        .code
        .iter()
        .find(|entry| entry.contains_return(pc))
}

pub(super) fn entries() -> Option<&'static NativeCallableEntries> {
    let entries = ENTRIES
        .get_or_init(|| {
            let entry = |slice| {
                PublishedAdapter::new(emit_published_call_entry_descriptor(
                    slice,
                    native_transfer_entry::prepare_callable,
                    native_transfer_entry::finish_callable,
                    native_transfer_entry::resume_callable,
                    interpreted,
                ))
            };
            let code = [entry(false)?, entry(true)?];
            let ordinary = JitBuffer::new(
                &egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
                    ordinary_interpreted,
                    0,
                ),
            )?;
            Some(Entries {
                published: NativeCallableEntries {
                    registers: code[0].code.as_ptr() as usize,
                    slice: code[1].code.as_ptr() as usize,
                },
                code,
                ordinary,
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

/// Root-segment entry for arbitrary callable objects. Legacy mapped entries
/// remain published for generated callers that still own mapped call records.
fn ordinary_entry() -> Option<*const u8> {
    entries()?;
    Some(ENTRIES.get()?.as_ref()?.ordinary.as_ptr())
}

#[cfg(test)]
static ORDINARY_ENTRIES: egcl_rt::execution_local::ExecutionLocal<std::cell::Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| std::cell::Cell::new(0)) };

/// This entry is invoked in its own native segment. A selected transfer returns
/// through every Rust dispatcher frame before the veneer leaves that segment.
unsafe extern "C" fn ordinary_interpreted(
    request: *mut u8,
    out: *mut egcl_compiler::t2::native_transfer::OrdinaryHelperOutcome,
    image: *mut egcl_compiler::t2::native_transfer::SysvTransferCapture,
) {
    use native_transfer_entry::{ordinary_calls, root_publication};
    let request = request.cast::<egcl_compiler::t2::native_transfer::OrdinaryEntryRequest>();
    outer::observe_ordinary_entry();
    #[cfg(test)]
    {
        observe_entry();
        ORDINARY_ENTRIES.with(|count| count.set(count.get() + 1));
    }
    unsafe {
        ordinary_calls::with_entry_roots(request, || {
            root_publication::published(image, || {
                let result = guard_c2i(|| {
                    #[cfg(test)]
                    if FAIL_NEXT_POLL.with(|flag| flag.replace(false)) {
                        NATIVE_ERROR.with(|slot| {
                            slot.set_first(EgclError::ProgramError(
                                "injected callable poll transfer".into(),
                            ))
                        });
                    }
                    let mut poll = NativeOutcome {
                        value: NIL,
                        exit: NativeExit::Returned,
                    };
                    native_transfer_entry::poll_or_transfer_unpublished(
                        std::ptr::null_mut(),
                        &mut poll,
                    );
                    if poll.exit != NativeExit::Returned {
                        return Ok(NIL);
                    }
                    #[cfg(test)]
                    collect_at_entry(std::ptr::addr_of_mut!((*request).closure) as u64)?;
                    egcl_rt::rooted!(args = Vec::new());
                    let count = (*request).nargs;
                    args.try_reserve_exact(count).map_err(|_| EgclError::Oom)?;
                    args.extend_from_slice(&(&(*request).register_args)[..count.min(3)]);
                    if count > 3 {
                        args.extend_from_slice(std::slice::from_raw_parts(
                            (*request).overflow_args,
                            count - 3,
                        ));
                    }
                    let env = NATIVE_ENV.with(std::cell::Cell::get);
                    if env.is_null() {
                        return Err(EgclError::ProgramError(
                            "ordinary callable has no environment".into(),
                        ));
                    }
                    if let Some(result) = native_transfer_entry::funcall_fast::apply(
                        (*request).closure,
                        &args,
                        &mut *env,
                        native_transfer_entry::funcall_fast::Admit::Any,
                    ) {
                        return result;
                    }
                    super::super::apply_function_impl((*request).closure, &args, &mut *env)
                });
                ordinary_calls::finish(result, out);
            });
        });
    }
}

unsafe extern "C" fn interpreted(target: u64, record: *mut MappedCallRecord) {
    // This adapter applies an arbitrary callable, so publish the generated
    // caller suspended beneath the adapter frame first.
    unsafe {
        native_transfer_entry::published_mapped_record(record, || {
            interpreted_published(target, record)
        })
    }
}

unsafe fn interpreted_published(target: u64, record: *mut MappedCallRecord) {
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
        if let Some(result) = native_transfer_entry::funcall_fast::apply(
            *function,
            args,
            unsafe { &mut *env },
            native_transfer_entry::funcall_fast::Admit::Any,
        ) {
            return result;
        }
        super::super::apply_function_impl(*function, args, unsafe { &mut *env })
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
        if outcome.exit == NativeExit::Returned {
            let env = NATIVE_ENV.with(std::cell::Cell::get);
            (*record).return_values = (*env).native_return_values();
        }
    }
}
