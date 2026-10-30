// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Prepared machine bindings. Misses retain the validated R6 runtime route.
use super::*;
use egcl_compiler::t2::native_transfer::{
    GeneratedActivation, GeneratedCallHelpers, GeneratedLane, GeneratedScopeStorage,
    MappedCallRecord,
};
use std::mem::{align_of, size_of};
use std::sync::atomic::Ordering;

pub(super) struct Prepared {
    pub(super) binding: GeneratedActivation,
    _publication: root_publication::PreparedMappedPublication,
}

const _: () = assert!(std::mem::offset_of!(CaptureContext, generated) == 0);
static NO_CLOSURE: EgclVal = NIL;

#[cfg(test)]
static FAST_ENTRIES: egcl_rt::execution_local::ExecutionLocal<Cell<usize>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(0)) };
#[cfg(test)]
pub(in crate::cli::bytecode) fn take_fast_entries_for_test() -> usize {
    FAST_ENTRIES.with(|count| count.replace(0))
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn fast_entries_for_test() -> usize {
    FAST_ENTRIES.with(Cell::get)
}

/// Fill only after an ordinary runtime call has established its exact lane and
/// control. The mapped activation already holds an ordinary-directory segment
/// lease, so its descriptors cannot be swept until this context retires.
/// No Lisp allocation, evaluation or yield occurs while binding raw homes.
pub(super) unsafe fn bind(
    parent: *mut CaptureContext,
    bridge: *mut mapped_ordinary::Bridge,
    cell: u64,
    offset: usize,
) {
    let cell_ref = unsafe { &*(cell as *const egcl_rt::call_table::CallCell) };
    let pointer = unsafe { &*cell_ref.ordinary_entry_address() }.load(Ordering::Acquire);
    if pointer == 0 {
        return;
    }
    let descriptor = unsafe { &*(pointer as *const egcl_rt::call_table::OrdinaryEntryDescriptor) };
    let code = unsafe { &(*bridge).code };
    let revision = unsafe { &*cell_ref.revision_address() }.load(Ordering::Acquire);
    if !descriptor.is_compatible()
        || descriptor.owner != Rc::as_ptr(code) as usize
        || descriptor.revision != revision
        || descriptor.arity != usize::from(code.body.arity)
        || descriptor.entry != unsafe { code.code.as_ptr().add(offset) } as usize
        || code.needs_recompile()
    {
        return;
    }
    let control = unsafe {
        (*bridge)
            .context
            .native_recursive
            .as_mut()
            .map(std::ptr::from_mut)
    };
    let Some(control) = control else {
        return;
    };
    if unsafe { (*parent).generated_owner.is_none() } {
        let publication = unsafe { root_publication::prepare_mapped_publication(parent) };
        let execution = egcl_rt::thread::current_execution_context_for_native()
            .expect("mapped owning execution");
        let runtime = egcl_rt::native_transfer::generated_segment_bindings(
            unsafe { &*execution },
            egcl_rt::current_stack(),
        );
        let parent_code = unsafe { &*(*parent).owner };
        let prepared = Box::new(Prepared {
            binding: GeneratedActivation {
                lanes: [std::ptr::null(); 4],
                execution,
                env: NATIVE_ENV.with(Cell::get).cast(),
                code_start: parent_code.code.as_ptr() as usize,
                code_end: parent_code.code.as_ptr() as usize + parent_code.code_len,
                runtime,
                depth: NATIVE_DEPTH.with(Cell::as_ptr),
                publication: publication.binding,
                fast_entries: {
                    #[cfg(test)]
                    {
                        FAST_ENTRIES.with(Cell::as_ptr)
                    }
                    #[cfg(not(test))]
                    {
                        std::ptr::null_mut()
                    }
                },
                total_entries: mapped_ordinary::entry_counter(),
            },
            _publication: publication,
        });
        unsafe { (*parent).generated_owner = Some(prepared) };
    }
    // The pinned bridge contains the actual callable word; a hidden closure is
    // never reconstructed from its compilation owner or descriptor address.
    let function = if code.native_callable_slot.is_some() {
        unsafe { std::ptr::from_ref(&(*bridge).dispatch_function) }
    } else {
        &NO_CLOSURE
    };
    let lane = GeneratedLane {
        cell,
        descriptor_word: cell_ref.ordinary_entry_address(),
        descriptor: pointer as *const _,
        revision_word: cell_ref.revision_address(),
        revision,
        recompile: code.recompile.as_ptr(),
        function,
        context: unsafe { std::ptr::from_mut(&mut *(*bridge).context).cast() },
        control,
        busy: unsafe { std::ptr::from_mut(&mut (*bridge).busy) },
        entry: descriptor.entry as *const u8,
        arity: descriptor.arity,
        stack_required: descriptor.stack_required,
    };
    unsafe { (*bridge).generated = Some(lane) };
    let lane = unsafe { (*bridge).generated.as_ref().unwrap() as *const GeneratedLane };
    let prepared = unsafe { (*parent).generated_owner.as_mut().unwrap() };
    let binding = std::ptr::from_mut(&mut prepared.binding);
    let bridges = unsafe { &*(*parent).mapped_ordinary };
    for (index, cached) in bridges.iter().enumerate() {
        if std::ptr::eq(cached.as_ref().get_ref(), bridge) {
            prepared.binding.lanes[index] = lane;
            break;
        }
    }
    unsafe { (*parent).generated = binding };
}

struct ScopeGuards {
    _block: BlockScopeGuard,
    _catch: CatchScopeGuard,
}
const _: () = {
    assert!(size_of::<ScopeGuards>() <= size_of::<GeneratedScopeStorage>());
    assert!(align_of::<ScopeGuards>() <= align_of::<GeneratedScopeStorage>());
};

/// Tiny nonallocating opaque operation; all selection and publication is in
/// machine code. The owner enum remains constructed by the RT implementation.
unsafe extern "C" fn enter(record: *mut MappedCallRecord) {
    let hot = unsafe { &mut (*record).generated };
    let binding = unsafe { &*hot.activation };
    let env = unsafe { &mut *binding.env.cast::<Env>() };
    let guards = ScopeGuards {
        _catch: CatchScopeGuard {
            env,
            base: env.catch_stack.len(),
        },
        _block: BlockScopeGuard {
            env,
            blocks: std::mem::take(&mut env.block_stack),
            tags: std::mem::take(&mut env.tag_stack),
        },
    };
    unsafe { hot.scope.0.as_mut_ptr().cast::<ScopeGuards>().write(guards) };
    hot.entered = 1;
    // Ordinary full entries require an empty canonical incoming MV buffer.
    env.clear_mv();
    unsafe {
        egcl_rt::native_transfer::initialize_generated_segment(
            hot.segment.as_mut_ptr(),
            hot.previous_segment,
            binding.runtime.stack,
        );
        *(*hot.lane).busy = true;
    }
    #[cfg(test)]
    unsafe {
        if !binding.fast_entries.is_null() {
            *binding.fast_entries += 1;
        }
        if !binding.total_entries.is_null() {
            *binding.total_entries += 1;
        }
    }
}

unsafe fn restore_float_environment(record: *mut MappedCallRecord) {
    let hot = unsafe { &mut (*record).generated };
    unsafe {
        std::arch::asm!("stmxcsr [{}]", in(reg) &mut hot.current_mxcsr, options(nostack, preserves_flags));
        if hot.current_mxcsr != hot.mxcsr {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &hot.mxcsr, options(nostack, preserves_flags));
        }
        std::arch::asm!("fnstcw [{}]", in(reg) &mut hot.current_fpcw, options(nostack, preserves_flags));
        if hot.current_fpcw != hot.fpcw {
            std::arch::asm!("fldcw [{}]", in(reg) &hot.fpcw, options(nostack, preserves_flags));
        }
    }
}

/// Idle returns only retire opaque lexical guards. A real pending return takes
/// the old rooted ordinary finish path; its result is rooted before any poll.
unsafe extern "C" fn leave(record: *mut MappedCallRecord) {
    unsafe { restore_float_environment(record) };
    let binding = unsafe { &*(*record).generated.activation };
    if unsafe {
        (*record).outcome.exit == NativeExit::Returned && (&*binding.execution).poll_pending()
    } {
        // Only this branch may collect. The idle guard retirement needs no
        // root-list access or execution-local lookup at all.
        egcl_rt::rooted!(primary = unsafe { (*record).outcome.value });
        let mut result = egcl_compiler::t2::native_transfer::OrdinaryHelperOutcome {
            outcome: NativeOutcome {
                value: *primary,
                exit: NativeExit::Returned,
            },
            values: unsafe { (*record).return_values },
            anchor: unsafe { (*record).generated.segment.as_mut_ptr() },
        };
        unsafe { ordinary_calls::finish(Ok(*primary), &mut result) };
        unsafe {
            (*record).outcome = result.outcome;
            (*record).return_values = result.values
        };
    }
    unsafe {
        if (*record).generated.entered != 0 {
            std::ptr::drop_in_place(
                (*record)
                    .generated
                    .scope
                    .0
                    .as_mut_ptr()
                    .cast::<ScopeGuards>(),
            );
            (*record).generated.entered = 0;
        }
        // A pending-return callback can change controls after the initial
        // restore. No more Lisp runs after this final restore.
        restore_float_environment(record);
    }
}

pub(super) fn helpers() -> GeneratedCallHelpers {
    GeneratedCallHelpers { enter, leave }
}
