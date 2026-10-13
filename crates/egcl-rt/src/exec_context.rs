// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The Lisp execution context that generated code reaches through a reserved
//! register (spec §2.3.1, R4.72/R4.73; ABI settled on `bliss-q861`).
//!
//! # Not to be confused with two neighbours of the same name
//!
//! - [`crate::context`] is the **machine** context switch: a saved stack pointer
//!   plus the callee-saved registers pushed on a fiber's own stack.
//! - `thread::FiberExecutionContext` is that saved stack pointer together with
//!   the fiber's native stack allocation.
//!
//! Neither is this. A `LispExecutionContext` is the **Lisp-level** state that
//! generated code needs at fixed offsets: which execution is running, the
//! carrier it is running on, whether a safepoint has been requested, and the
//! stack bounds it publishes for the collector.
//!
//! # Why a register and not a thread-local
//!
//! The fiber switch in [`crate::context`] already pushes and pops
//! `rbp rbx r12 r13 r14 r15` onto each fiber's own stack, so a context pointer
//! in `r13` is saved when a fiber swaps out and restored when it resumes —
//! including onto a *different* carrier. Migration needs no action and there is
//! no per-switch bookkeeping to omit. A thread-local slot is per **carrier**, so
//! it would have to be rewritten at every mount, unmount, migration and nested
//! foreign→Lisp re-entry, and one missed store would let generated code read
//! another fiber's context. `Fiber::scheduler_return` carries the same lesson,
//! learned the same way (bliss-bca.5).
//!
//! Measurement did not decide it: for the realistic one-read-per-region pattern
//! `mov r, %fs:disp` and `mov r, [reg+disp]` are within noise of each other
//! (1.44 vs 1.45 cycles/iteration on Meteorlake).
//!
//! # Single source of truth
//!
//! This struct **owns** the state it exposes; `Fiber` and `NativeThread` hold one
//! and delegate to it. It must never become a mirror of fields those types also
//! keep, because generated code and the runtime would then disagree about which
//! copy is current — the precise failure the register choice was made to avoid.

use std::sync::atomic::{AtomicU64, Ordering};

/// Which kind of execution a context describes. Kept word-sized so generated
/// code can branch on it with an ordinary load and compare.
#[repr(u64)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ExecutionKind {
    /// Lisp running on a bare native/carrier thread with no fiber mounted.
    NativeThread = 0,
    /// Lisp running on a fiber.
    Fiber = 1,
}

/// Lisp execution context (§2.3.1). `#[repr(C)]` with word-sized fields because generated
/// code loads these by fixed byte offset; see [`offsets`] and the test that pins
/// them. Adding a field is fine; REORDERING one silently changes the ABI, which
/// is why the offsets are asserted rather than assumed.
#[repr(C)]
pub struct LispExecutionContext {
    /// [`ExecutionKind`] as a plain word.
    kind: u64,
    /// `FiberId` or `NativeThreadId` of the execution this context belongs to.
    execution_id: u64,
    /// Carrier currently running this execution. Refreshed on migration before
    /// Lisp resumes, per §2.3.1; for a bare native thread it is its own id.
    carrier_id: AtomicU64,
    /// Cooperative preemption request, polled at safepoints (§2.5.3 step 4).
    /// A word rather than a bool so its offset and width are stable for codegen.
    yield_requested: AtomicU64,
    /// Raw address of the owning execution's active native segment control.
    /// Zero outside a segment; the segment owner retains the pointee.
    active_native_control: AtomicU64,
    /// Stable notification slot; its first word is the generated poll ABI.
    poll_slot: &'static crate::execution_poll::PollSlot,
}

/// Byte offsets generated code uses. These are the ABI: the emitters bake them,
/// so they are asserted against the real layout in the tests below rather than
/// being maintained by hand.
pub mod offsets {
    /// `kind`
    pub const KIND: usize = 0;
    /// `execution_id`
    pub const EXECUTION_ID: usize = 8;
    /// `carrier_id`
    pub const CARRIER_ID: usize = 16;
    /// `yield_requested`
    pub const YIELD_REQUESTED: usize = 24;
    /// `active_native_control`
    pub const ACTIVE_NATIVE_CONTROL: usize = 32;
    /// Pointer to the process-lifetime execution poll slot.
    pub const POLL_SLOT: usize = 40;
}

impl LispExecutionContext {
    pub fn new(kind: ExecutionKind, execution_id: u64, carrier_id: u64) -> Self {
        Self {
            kind: kind as u64,
            execution_id,
            carrier_id: AtomicU64::new(carrier_id),
            yield_requested: AtomicU64::new(0),
            active_native_control: AtomicU64::new(0),
            poll_slot: crate::execution_poll::register(carrier_id),
        }
    }

    pub fn kind(&self) -> ExecutionKind {
        if self.kind == ExecutionKind::Fiber as u64 {
            ExecutionKind::Fiber
        } else {
            ExecutionKind::NativeThread
        }
    }

    pub fn execution_id(&self) -> u64 {
        self.execution_id
    }

    pub fn carrier_id(&self) -> u64 {
        self.carrier_id.load(Ordering::Acquire)
    }

    /// Refresh the carrier association. Called before Lisp resumes on another OS
    /// thread (§2.3.1); the context's ADDRESS is unchanged by migration, which is
    /// what lets a cached pointer stay valid across a fiber switch.
    pub fn set_carrier_id(&self, carrier: u64) {
        self.carrier_id.store(carrier, Ordering::Release);
        self.poll_slot.set_carrier(carrier);
    }

    /// Request cooperative preemption at the next safepoint.
    pub fn request_yield(&self) {
        self.yield_requested.store(1, Ordering::SeqCst);
        self.request_poll();
    }

    /// Take the pending yield request, clearing it. `SeqCst` to match the
    /// swap-based accessors this replaces.
    pub fn take_yield_request(&self) -> bool {
        // Leave the idle flag untouched. A request posted after the load is
        // retained for the next poll; consumption keeps the original ordering.
        self.yield_requested.load(Ordering::SeqCst) != 0
            && self.yield_requested.swap(0, Ordering::SeqCst) != 0
    }

    pub fn yield_requested(&self) -> bool {
        self.yield_requested.load(Ordering::Acquire) != 0
    }

    /// Publish authoritative work before requesting a poll.
    pub fn request_poll(&self) {
        self.poll_slot.request();
    }

    pub fn poll_pending(&self) -> bool {
        self.poll_slot.pending()
    }

    /// Clear before draining, never after: a concurrent arrival must survive.
    pub fn begin_poll(&self) -> bool {
        self.poll_slot.begin()
    }

    /// Read the active segment control address without dereferencing it.
    /// Reading an address grants no access to the execution-owned pointee.
    pub fn active_native_control(&self) -> u64 {
        self.active_native_control.load(Ordering::Acquire)
    }

    /// Install a segment control address, returning the enclosing address.
    ///
    /// # Safety
    /// Only the owning execution may change this word. A nonzero address must
    /// identify an initialized control object with the layout expected by its
    /// generated code. The caller must retain that object across all active
    /// and suspended uses, restore the enclosing address before its owner is
    /// released (including unwind), and prevent access after either lifetime
    /// ends. Atomic publication does not make the pointee concurrently mutable.
    pub unsafe fn swap_active_native_control(&self, address: u64) -> u64 {
        self.active_native_control.swap(address, Ordering::AcqRel)
    }

    /// Publish pinned control storage only for the dynamic extent of `run`.
    /// Nested entries restore the enclosing address even when `run` unwinds.
    /// The restore guard is private so callers cannot forget it or drop it early.
    ///
    /// # Safety
    /// This must run on the owning execution. `control` and every pointer it
    /// contains must have the layout and lifetimes expected by generated code.
    /// Native access must end before `run` returns or unwinds, including any
    /// suspended continuation. Such access must respect Rust aliasing rules;
    /// atomic address publication does not permit concurrent pointee access.
    pub unsafe fn with_active_native_control<T, R>(
        &self,
        mut control: std::pin::Pin<&mut T>,
        run: impl FnOnce() -> R,
    ) -> R {
        struct Restore<'a> {
            execution: &'a LispExecutionContext,
            previous: u64,
        }
        impl Drop for Restore<'_> {
            fn drop(&mut self) {
                // SAFETY: the enclosing invocation still owns its storage and
                // this private guard cannot escape the nested dynamic scope.
                unsafe { self.execution.swap_active_native_control(self.previous); }
            }
        }
        // SAFETY: obtaining the address does not move the pinned storage.
        let address = unsafe { control.as_mut().get_unchecked_mut() as *mut T as u64 };
        let previous = unsafe { self.swap_active_native_control(address) };
        let _restore = Restore { execution: self, previous };
        run()
    }

    /// Address generated code will hold in the reserved register.
    pub fn as_ptr(&self) -> *const Self {
        self as *const Self
    }
}

impl Drop for LispExecutionContext {
    fn drop(&mut self) {
        self.poll_slot.retire();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scoped_native_control_restores_nested_state_on_unwind() {
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 7, 3);
        let other = LispExecutionContext::new(ExecutionKind::Fiber, 8, 3);
        let mut outer = std::pin::pin!(11u64);
        let outer_address = (&*outer as *const u64) as u64;
        unsafe {
            execution.with_active_native_control(outer.as_mut(), || {
                assert_eq!(execution.active_native_control(), outer_address);
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut inner = std::pin::pin!(22u64);
                    let inner_address = (&*inner as *const u64) as u64;
                    execution.with_active_native_control(inner.as_mut(), || {
                        assert_eq!(execution.active_native_control(), inner_address);
                        assert_eq!(other.active_native_control(), 0);
                        execution.set_carrier_id(9);
                        assert_eq!(execution.active_native_control(), inner_address);
                        panic!("leave nested native entry");
                    });
                }));
                assert!(result.is_err());
                assert_eq!(execution.active_native_control(), outer_address);
            });
        }
        assert_eq!(execution.active_native_control(), 0);
        assert_eq!(execution.carrier_id(), 9);
    }

    /// The offsets in [`offsets`] ARE the codegen ABI. If a field is reordered or
    /// resized this test fails rather than letting generated code read the wrong
    /// word at runtime.
    #[test]
    fn declared_offsets_match_the_real_layout() {
        let ctx = LispExecutionContext::new(ExecutionKind::Fiber, 7, 3);
        let base = &ctx as *const _ as usize;
        assert_eq!(&ctx.kind as *const _ as usize - base, offsets::KIND);
        assert_eq!(
            &ctx.execution_id as *const _ as usize - base,
            offsets::EXECUTION_ID
        );
        assert_eq!(
            &ctx.carrier_id as *const _ as usize - base,
            offsets::CARRIER_ID
        );
        assert_eq!(
            &ctx.yield_requested as *const _ as usize - base,
            offsets::YIELD_REQUESTED
        );
        assert_eq!(
            &ctx.active_native_control as *const _ as usize - base,
            offsets::ACTIVE_NATIVE_CONTROL
        );
        assert_eq!(&ctx.poll_slot as *const _ as usize - base, offsets::POLL_SLOT);
    }

    #[test]
    fn active_native_control_is_execution_local_and_survives_migration() {
        let first = LispExecutionContext::new(ExecutionKind::Fiber, 11, 1);
        let second = LispExecutionContext::new(ExecutionKind::NativeThread, 12, 1);
        let mut outer = 17u64;
        let mut inner = 23u64;
        let outer_address = &mut outer as *mut u64 as u64;
        let inner_address = &mut inner as *mut u64 as u64;
        assert_eq!(first.active_native_control(), 0);
        assert_eq!(second.active_native_control(), 0);
        // This test owns both inactive contexts and keeps each pointee alive
        // until its address has been restored or cleared.
        unsafe {
            assert_eq!(first.swap_active_native_control(outer_address), 0);
            assert_eq!(second.swap_active_native_control(inner_address), 0);
            let previous = first.swap_active_native_control(inner_address);
            assert_eq!(previous, outer_address);
            first.set_carrier_id(9);
            assert_eq!(first.active_native_control(), inner_address);
            assert_eq!(first.swap_active_native_control(previous), inner_address);
            assert_eq!(first.active_native_control(), outer_address);
            assert_eq!(second.active_native_control(), inner_address);
            assert_eq!(first.swap_active_native_control(0), outer_address);
            assert_eq!(second.swap_active_native_control(0), inner_address);
        }
        assert_eq!(first.active_native_control(), 0);
        assert_eq!(second.active_native_control(), 0);
        assert_eq!(offsets::ACTIVE_NATIVE_CONTROL, 32);
        assert_eq!(std::mem::size_of::<LispExecutionContext>(), 48);
    }

    /// Every field is one machine word, so generated code can load any of them
    /// with a single `mov` at a fixed displacement.
    #[test]
    fn every_field_is_one_machine_word() {
        assert_eq!(std::mem::size_of::<LispExecutionContext>(), 48);
        assert_eq!(std::mem::align_of::<LispExecutionContext>(), 8);
    }

    #[test]
    fn kind_round_trips_through_its_word_representation() {
        let f = LispExecutionContext::new(ExecutionKind::Fiber, 1, 1);
        let n = LispExecutionContext::new(ExecutionKind::NativeThread, 2, 2);
        assert_eq!(f.kind(), ExecutionKind::Fiber);
        assert_eq!(n.kind(), ExecutionKind::NativeThread);
        assert_eq!(f.execution_id(), 1);
        assert_eq!(n.execution_id(), 2);
    }

    /// Migration changes the carrier but NOT the context's address; that is what
    /// makes a pointer cached in the reserved register survive a fiber switch.
    #[test]
    fn migration_refreshes_the_carrier_without_moving_the_context() {
        let ctx = LispExecutionContext::new(ExecutionKind::Fiber, 9, 1);
        let before = ctx.as_ptr();
        assert_eq!(ctx.carrier_id(), 1);
        ctx.set_carrier_id(2);
        assert_eq!(ctx.carrier_id(), 2);
        assert_eq!(ctx.as_ptr(), before);
    }

    #[test]
    fn a_yield_request_is_taken_exactly_once() {
        let ctx = LispExecutionContext::new(ExecutionKind::NativeThread, 1, 1);
        assert!(!ctx.yield_requested());
        ctx.request_yield();
        assert!(ctx.yield_requested());
        assert!(ctx.take_yield_request());
        assert!(
            !ctx.take_yield_request(),
            "a request must not be taken twice"
        );
        assert!(!ctx.yield_requested());
    }

    #[test]
    fn requests_posted_after_idle_polls_are_taken_and_can_be_rearmed() {
        use std::sync::mpsc::sync_channel;
        use std::time::Duration;

        for kind in [ExecutionKind::NativeThread, ExecutionKind::Fiber] {
            let ctx = LispExecutionContext::new(kind, 1, 1);
            let (request_tx, request_rx) = sync_channel(0);
            let (posted_tx, posted_rx) = sync_channel(0);
            std::thread::scope(|scope| {
                let ctx = &ctx;
                scope.spawn(move || {
                    for _ in 0..64 {
                        request_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                        ctx.request_yield();
                        posted_tx.send(()).unwrap();
                    }
                });
                for _ in 0..64 {
                    assert!(!ctx.take_yield_request(), "start with an idle poll");
                    request_tx.send(()).unwrap();
                    posted_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                    assert!(ctx.take_yield_request(), "retain the later request");
                    assert!(!ctx.take_yield_request(), "take each request once");
                }
            });
        }
    }
}

#[cfg(test)]
mod execution_poll_tests {
    use super::*;
    // Process broadcasts elsewhere in the runtime suite must not turn an idle
    // assertion into a timing-dependent failure. Each protocol test owns its
    // notification universe, while its child still uses the real global slots.
    fn isolated() -> bool {
        let name = std::thread::current().name().unwrap().to_owned();
        if std::env::var("EGCL_EXECUTION_POLL_TEST").ok().as_deref() == Some(&name) {
            return false;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture"])
            .env("EGCL_EXECUTION_POLL_TEST", &name)
            .output().unwrap();
        assert!(output.status.success(), "{}\n{}",
            String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        true
    }

    #[test]
    fn directed_request_does_not_arm_an_unrelated_execution() {
        if isolated() { return; }
        let a = LispExecutionContext::new(ExecutionKind::Fiber, 7001, 1);
        let b = LispExecutionContext::new(ExecutionKind::Fiber, 7002, 1);
        assert!(a.begin_poll());
        assert!(b.begin_poll());
        a.request_poll();
        assert!(a.poll_pending());
        assert!(!b.poll_pending());
        assert!(a.begin_poll());
        assert!(!a.poll_pending());
    }

    #[test]
    fn arrival_after_begin_survives_the_earlier_drain() {
        if isolated() { return; }
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 7003, 1);
        execution.begin_poll();
        let payload = AtomicU64::new(0);
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                barrier.wait();
                payload.store(42, Ordering::Release);
                execution.request_poll();
                barrier.wait();
            });
            execution.begin_poll();
            assert_eq!(payload.load(Ordering::Acquire), 0);
            barrier.wait();
            barrier.wait();
            assert!(execution.poll_pending());
            assert_eq!(payload.load(Ordering::Acquire), 42);
        });
    }

    #[test]
    fn migration_and_explicit_yield_arm_the_same_poll() {
        if isolated() { return; }
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 7004, 1);
        execution.begin_poll();
        execution.set_carrier_id(2);
        assert!(execution.begin_poll());
        execution.request_yield();
        assert!(execution.poll_pending());
        assert!(execution.take_yield_request());
    }

    #[test]
    fn broadcast_reaches_every_live_slot_and_new_registrations_start_armed() {
        if isolated() { return; }
        let a = LispExecutionContext::new(ExecutionKind::NativeThread, 7005, 1);
        let b = LispExecutionContext::new(ExecutionKind::Fiber, 7006, 2);
        a.begin_poll(); b.begin_poll();
        crate::execution_poll::request_all();
        assert!(a.poll_pending()); assert!(b.poll_pending());
        let c = LispExecutionContext::new(ExecutionKind::Fiber, 7007, 2);
        assert!(c.poll_pending());
    }

    #[test]
    fn retired_slots_are_reused_and_broadcast_remains_safe_during_recycling() {
        if isolated() { return; }
        let execution = LispExecutionContext::new(ExecutionKind::Fiber, 7010, 1);
        let slot = execution.poll_slot as *const crate::execution_poll::PollSlot;
        drop(execution);
        let reused = LispExecutionContext::new(ExecutionKind::Fiber, 7011, 1);
        assert_eq!(slot, reused.poll_slot as *const _);
        assert!(reused.poll_pending());
        let done = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !done.load(Ordering::Acquire) {
                    crate::execution_poll::request_all();
                }
            });
            for id in 0..1000 {
                let execution = LispExecutionContext::new(ExecutionKind::Fiber, 7100 + id, 2);
                assert!(execution.poll_pending());
                execution.begin_poll();
            }
            done.store(true, Ordering::Release);
        });
        // A signal reader may retain the slot address across retirement safely.
        unsafe { &*slot }.request();
        assert!(reused.poll_pending());
    }

}
