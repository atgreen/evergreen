// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Explicit bindings for an execution-owned generated nested boundary.
//!
//! Rust constructs the opaque segment owner. Generated code accesses only the
//! documented scalar slots and publishes an initialized, per-call anchor.

use super::*;
use crate::exec_context::LispExecutionContext;
use std::sync::atomic::AtomicU64;

/// Borrowed slot addresses, not ownership of an execution or its stack.
///
/// Only the owning execution may access these slots. Its context, managed
/// stack and execution-local storage must stay alive and at stable addresses
/// throughout every active or suspended use. `active` is an execution-local
/// Cell slot, not an atomic; `control` requires atomic acquire-release exchange.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GeneratedSegmentBindings {
    pub active: *mut *mut NativeSegment,
    pub control: *const AtomicU64,
    pub execution: *const LispExecutionContext,
    pub stack: *const EgclStack,
}

/// Scalar machine fields that generated code may initialize and retire.
/// Other fields, particularly the Rust owner enum, remain opaque.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct GeneratedSegmentLayout {
    pub saved_sp: usize,
    pub landing_pc: usize,
    pub entry_return_pc: usize,
    pub landing_sp: usize,
}

pub const GENERATED_SEGMENT_LAYOUT: GeneratedSegmentLayout = GeneratedSegmentLayout {
    saved_sp: std::mem::offset_of!(NativeSegment, saved_sp),
    landing_pc: std::mem::offset_of!(NativeSegment, landing_pc),
    entry_return_pc: std::mem::offset_of!(NativeSegment, entry_return_pc),
    landing_sp: std::mem::offset_of!(NativeSegment, landing_sp),
};

/// Prepare addresses on the execution that owns `execution` and `stack`.
/// This does not allocate Lisp objects, poll, or grant cross-execution access.
/// Call only after the ordinary native capability/entry checks have succeeded;
/// this binding does not admit native code on an unsupported destination.
pub fn generated_segment_bindings(
    execution: &LispExecutionContext,
    stack: &EgclStack,
) -> GeneratedSegmentBindings {
    GeneratedSegmentBindings {
        active: ACTIVE.with(Cell::as_ptr),
        control: execution.native_control_word(),
        execution,
        stack,
    }
}

/// Initialize a per-call anchor without linking it or changing control state.
/// No allocation, callback, execution-local lookup or Lisp operation occurs.
/// The owner/carrier come from the live parent, already revalidated by the
/// scheduler before migration resumes; they are not cached in the binding.
///
/// # Safety
/// `previous` is this execution's live, validated enclosing segment, and
/// `stack` is its current managed stack. `storage` is writable, properly aligned
/// uninitialized storage for a `NativeSegment`, disjoint from both, which will
/// remain pinned until unlinked after every child and Rust helper has returned.
/// The caller must install exact saved SP, normal return PC and cold landing
/// geometry before publishing the anchor. Only then may it link the segment
/// and exchange the active control, restoring both on every exit. No cold jump
/// may skip a live Rust frame. This constructor does not check native capability.
pub unsafe fn initialize_generated_segment(
    storage: *mut NativeSegment,
    previous: *mut NativeSegment,
    stack: *const EgclStack,
) {
    debug_assert!(!previous.is_null() && !storage.is_null() && !stack.is_null());
    let parent = unsafe { &*previous };
    let stack = unsafe { &*stack };
    unsafe {
        storage.write(NativeSegment {
            saved_sp: 0,
            landing_pc: 0,
            entry_return_pc: 0,
            previous,
            owner: parent.owner,
            carrier: parent.carrier,
            stack_sp: stack.sp(),
            stack_fp: stack.fp(),
            _pinned: std::marker::PhantomPinned,
            landing_sp: 0,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exec_context::ExecutionKind;
    use std::mem::MaybeUninit;
    use std::sync::atomic::Ordering;

    fn parent(stack: &EgclStack, owner: SegmentOwner, carrier: u64) -> NativeSegment {
        NativeSegment {
            saved_sp: 0x1000,
            landing_pc: 0x2000,
            entry_return_pc: 0x3000,
            previous: std::ptr::null_mut(),
            owner,
            carrier: crate::thread::NativeThreadId(carrier),
            stack_sp: stack.sp(),
            stack_fp: stack.fp(),
            _pinned: std::marker::PhantomPinned,
            landing_sp: 0x4000,
        }
    }

    #[test]
    fn generated_anchor_preserves_opaque_ownership_and_accepts_machine_geometry() {
        let stack = EgclStack::new(4096);
        let frame = stack.push_frame(NIL, std::ptr::null(), 2, 0).unwrap();
        let mut outer = std::pin::pin!(parent(
            &stack,
            SegmentOwner::Thread(crate::thread::NativeThreadId(3)),
            3
        ));
        let outer_pointer = unsafe { outer.as_mut().get_unchecked_mut() as *mut NativeSegment };
        let mut storage = std::pin::pin!(MaybeUninit::<NativeSegment>::uninit());
        let child = unsafe { storage.as_mut().get_unchecked_mut().as_mut_ptr() };
        let active = current_segment();
        unsafe { initialize_generated_segment(child, outer_pointer, &stack) };
        assert_eq!(current_segment(), active, "construction must not publish");
        let initialized = unsafe { &*child };
        assert_eq!(initialized.previous(), outer_pointer);
        assert_eq!(initialized.owner(), outer.owner());
        assert_eq!(initialized.carrier(), outer.carrier());
        assert_eq!(
            initialized.stack_watermark(),
            (stack.sp(), frame.cast_const())
        );
        assert_eq!(
            (
                initialized.saved_sp,
                initialized.landing_pc,
                initialized.entry_return_pc,
                initialized.landing_sp
            ),
            (0, 0, 0, 0)
        );
        // Exercise the exact scalar API used by generated callers, not the
        // private owner representation. These stores cannot touch its metadata.
        for (offset, value) in [
            (GENERATED_SEGMENT_LAYOUT.saved_sp, 0x5000),
            (GENERATED_SEGMENT_LAYOUT.landing_pc, 0x6000),
            (GENERATED_SEGMENT_LAYOUT.entry_return_pc, 0x7000),
            (GENERATED_SEGMENT_LAYOUT.landing_sp, 0x8000),
        ] {
            unsafe { child.cast::<u8>().add(offset).cast::<usize>().write(value) };
        }
        let initialized = unsafe { &*child };
        assert_eq!(initialized.return_pc(), 0x7000);
        assert_eq!(initialized.saved_sp, 0x5000);
        assert_eq!(initialized.landing_sp, 0x8000);
        assert_eq!(initialized.owner(), outer.owner());
        assert_eq!(initialized.previous(), outer_pointer);
        assert_eq!(outer.saved_sp, 0x1000);
        assert_eq!(outer.return_pc(), 0x3000);
    }

    #[test]
    fn generated_anchor_samples_parent_carrier_and_stack_for_each_call() {
        let stack = EgclStack::new(4096);
        let mut outer = std::pin::pin!(parent(
            &stack,
            SegmentOwner::Fiber(crate::thread::FiberId(11)),
            11
        ));
        let active = current_segment();
        let outer_pointer = unsafe { outer.as_mut().get_unchecked_mut() as *mut NativeSegment };
        let mut first = MaybeUninit::<NativeSegment>::uninit();
        unsafe { initialize_generated_segment(first.as_mut_ptr(), outer_pointer, &stack) };
        let first = unsafe { first.assume_init_ref() };
        // Model the scheduler's already-validated metadata update. This unit
        // tests fresh construction, not actual fiber migration or native entry.
        unsafe { (*outer_pointer).carrier = crate::thread::NativeThreadId(13) };
        let frame = stack.push_frame(NIL, std::ptr::null(), 1, 0).unwrap();
        let mut second = MaybeUninit::<NativeSegment>::uninit();
        unsafe { initialize_generated_segment(second.as_mut_ptr(), outer_pointer, &stack) };
        let second = unsafe { second.assume_init_ref() };
        assert_eq!(first.carrier(), crate::thread::NativeThreadId(11));
        assert_eq!(second.carrier(), crate::thread::NativeThreadId(13));
        assert_eq!(second.owner(), first.owner());
        assert_ne!(first.stack_watermark(), second.stack_watermark());
        assert_eq!(second.stack_watermark(), (stack.sp(), frame.cast_const()));
        assert_eq!(second.previous(), outer_pointer);
        assert_eq!(current_segment(), active);
    }

    #[test]
    fn generated_bindings_expose_the_same_atomic_control_and_stable_execution_slot() {
        let stack = EgclStack::new(0);
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 17, 3);
        let other = LispExecutionContext::new(ExecutionKind::Fiber, 19, 3);
        let first = generated_segment_bindings(&execution, &stack);
        let previous_segment = current_segment();
        assert_eq!(first.active, ACTIVE.with(Cell::as_ptr));
        assert_eq!(unsafe { first.active.read() }, previous_segment);
        assert_eq!(first.control, execution.native_control_word());
        assert_eq!(first.execution, &execution as *const _);
        assert_eq!(first.stack, &stack as *const _);
        assert_eq!(
            first.control as usize % std::mem::align_of::<AtomicU64>(),
            0
        );
        let mut control = std::pin::pin!(23u64);
        let address = (&mut *control as *mut u64) as u64;
        let slot = unsafe { &*first.control };
        assert_eq!(slot.swap(address, Ordering::AcqRel), 0);
        assert_eq!(execution.active_native_control(), address);
        assert_eq!(other.active_native_control(), 0);
        execution.set_carrier_id(5);
        let again = generated_segment_bindings(&execution, &stack);
        assert_eq!(again.active, first.active);
        assert_eq!(again.control, first.control);
        assert_eq!(slot.swap(0, Ordering::AcqRel), address);
        assert_eq!(execution.active_native_control(), 0);
        assert_eq!(current_segment(), previous_segment);
    }
}
