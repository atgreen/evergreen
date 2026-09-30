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

/// Lisp execution context (§2.3.1). `#[repr(C)]` and all-`u64` because generated
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
}

impl LispExecutionContext {
    pub fn new(kind: ExecutionKind, execution_id: u64, carrier_id: u64) -> Self {
        Self {
            kind: kind as u64,
            execution_id,
            carrier_id: AtomicU64::new(carrier_id),
            yield_requested: AtomicU64::new(0),
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
    }

    /// Request cooperative preemption at the next safepoint.
    pub fn request_yield(&self) {
        self.yield_requested.store(1, Ordering::SeqCst);
    }

    /// Take the pending yield request, clearing it. `SeqCst` to match the
    /// swap-based accessors this replaces.
    pub fn take_yield_request(&self) -> bool {
        self.yield_requested.swap(0, Ordering::SeqCst) != 0
    }

    pub fn yield_requested(&self) -> bool {
        self.yield_requested.load(Ordering::Acquire) != 0
    }

    /// Address generated code will hold in the reserved register.
    pub fn as_ptr(&self) -> *const Self {
        self as *const Self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }

    /// Every field is one machine word, so generated code can load any of them
    /// with a single `mov` at a fixed displacement.
    #[test]
    fn every_field_is_one_machine_word() {
        assert_eq!(std::mem::size_of::<LispExecutionContext>(), 32);
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
}
