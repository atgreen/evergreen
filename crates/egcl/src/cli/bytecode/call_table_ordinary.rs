// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary entry ownership is execution-local, but stack walkers receive an
//! explicit directory for the execution they are inspecting. No hot machine
//! call allocates a managed activation or increments a Rust reference count.

use super::{CallCell, linkage};
use egcl_rt::call_table::OrdinaryEntryDescriptor;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::super::native_transfer_entry::TransferCode;

struct RetainedEntry<C> {
    descriptor: OrdinaryEntryDescriptor,
    cell: Arc<CallCell>,
    code: Rc<C>,
}

struct RetainedEntries<C> {
    segments: usize,
    // Descriptor addresses are embedded in atomic slots and stay fixed while
    // publication/redefinition grows this directory.
    #[allow(clippy::vec_box)]
    entries: Vec<Box<RetainedEntry<C>>>,
}

impl<C> Default for RetainedEntries<C> {
    fn default() -> Self {
        Self {
            segments: 0,
            entries: Vec::new(),
        }
    }
}

impl<C> RetainedEntries<C> {
    fn begin_segment(&mut self) {
        self.segments = self
            .segments
            .checked_add(1)
            .expect("native segment depth overflow");
    }

    fn finish_segment(&mut self) {
        self.segments = self
            .segments
            .checked_sub(1)
            .expect("unbalanced native segment retirement");
        self.sweep();
    }

    fn retain(
        &mut self,
        cell: Arc<CallCell>,
        descriptor: OrdinaryEntryDescriptor,
        code: Rc<C>,
    ) -> Option<*const OrdinaryEntryDescriptor> {
        if !descriptor.is_compatible() || descriptor.owner != Rc::as_ptr(&code) as usize {
            return None;
        }
        if let Some(entry) = self
            .entries
            .iter()
            .find(|entry| Arc::ptr_eq(&entry.cell, &cell) && entry.descriptor == descriptor)
        {
            return Some(&entry.descriptor);
        }
        let entry = Box::new(RetainedEntry {
            descriptor,
            cell,
            code,
        });
        let pointer = std::ptr::from_ref(&entry.descriptor);
        self.entries.push(entry);
        Some(pointer)
    }

    fn sweep(&mut self) {
        if self.segments != 0 {
            return;
        }
        self.entries.retain(|entry| {
            let published =
                unsafe { &*entry.cell.ordinary_entry_address() }.load(Ordering::Acquire);
            published == std::ptr::from_ref(&entry.descriptor) as usize
        });
    }

    fn code_for_pc(&self, pc: usize) -> Option<Rc<C>> {
        self.entries
            .iter()
            .find(|entry| entry.descriptor.contains_pc(pc))
            .map(|entry| Rc::clone(&entry.code))
    }
}

/// Borrow only for the duration of lookup. The explicit handle may be read by
/// a collector/debugger while its owning execution is stopped, just like the
/// existing PublishedBoundary owner; it must not be read concurrently with it.
pub(in crate::cli::bytecode) struct OrdinaryCodeDirectory {
    retained: RefCell<RetainedEntries<TransferCode>>,
}

impl OrdinaryCodeDirectory {
    #[cfg(test)]
    pub(in crate::cli::bytecode) fn for_walk_test(codes: &[(Rc<TransferCode>, usize, usize)]) -> Self {
        let mut retained = RetainedEntries::default();
        for (code, start, len) in codes {
            // Test-only ownership registration. Legacy fixture entries are
            // never published or executed through the ordinary ABI.
            let descriptor = OrdinaryEntryDescriptor {
                kind: linkage::ORDINARY_X64_LISP_KIND,
                version: linkage::ORDINARY_X64_LISP_VERSION,
                revision: 0,
                entry: *start,
                code_start: *start,
                code_len: *len,
                arity: 0,
                environment: linkage::ORDINARY_ENV_NONE,
                owner: Rc::as_ptr(code) as usize,
                stack_required: 8,
            };
            assert!(retained.retain(Arc::new(CallCell::new(0, 11, 12)), descriptor, Rc::clone(code)).is_some());
        }
        Self { retained: RefCell::new(retained) }
    }

    pub(in crate::cli::bytecode) fn code_for_pc(&self, pc: usize) -> Option<Rc<TransferCode>> {
        self.retained.borrow().code_for_pc(pc)
    }
}

static DIRECTORY: egcl_rt::execution_local::ExecutionLocal<Rc<OrdinaryCodeDirectory>> = unsafe {
    egcl_rt::execution_local::ExecutionLocal::new(|| {
        Rc::new(OrdinaryCodeDirectory {
            retained: RefCell::new(RetainedEntries::default()),
        })
    })
};

/// Capture on the publishing execution, never during a foreign stack walk.
/// The enclosing segment lease keeps retired owners until every frame returns.
pub(in crate::cli::bytecode) fn active_ordinary_directory() -> Rc<OrdinaryCodeDirectory> {
    DIRECTORY.with(|directory| {
        assert!(directory.retained.borrow().segments > 0, "native publication needs a segment lease");
        Rc::clone(directory)
    })
}

/// Owns a lease for the whole segment, including suspension and Rust callbacks.
/// Drop uses the saved directory, never whichever execution is currently cached.
pub(in crate::cli::bytecode) struct OrdinarySegmentGuard {
    directory: Rc<OrdinaryCodeDirectory>,
}

impl OrdinarySegmentGuard {
    #[cfg_attr(not(test), expect(dead_code, reason = "ordinary entry consumers follow in bliss-t8a48"))]
    pub(in crate::cli::bytecode) fn directory(&self) -> Rc<OrdinaryCodeDirectory> {
        Rc::clone(&self.directory)
    }
}

impl Drop for OrdinarySegmentGuard {
    fn drop(&mut self) {
        self.directory.retained.borrow_mut().finish_segment();
    }
}

pub(in crate::cli::bytecode) fn enter_ordinary_segment() -> OrdinarySegmentGuard {
    DIRECTORY.with(|directory| {
        directory.retained.borrow_mut().begin_segment();
        OrdinarySegmentGuard {
            directory: Rc::clone(directory),
        }
    })
}

/// Install ownership before publishing the descriptor address. The caller has
/// already validated the emitted code and provided its true code range/entry.
/// Failed revision checks discard only unreferenced quiescent versions.
///
/// # Safety
/// `cell` identifies this execution's live CallCell; descriptor entry/range and
/// ABI metadata must describe `code` and have passed installation validation.
#[expect(dead_code, reason = "ordinary entry installation follows in bliss-t8a48")]
pub(in crate::cli::bytecode) unsafe fn publish_ordinary(
    cell: u64,
    descriptor: OrdinaryEntryDescriptor,
    code: Rc<TransferCode>,
) -> bool {
    let state = unsafe { super::state(cell) };
    DIRECTORY.with(|directory| {
        let pointer =
            directory
                .retained
                .borrow_mut()
                .retain(Arc::clone(&state.cell), descriptor, code);
        let Some(pointer) = pointer else { return false };
        // Retention and publication do not allocate Lisp objects or yield.
        let published = unsafe { linkage::publish_ordinary(&state.cell, &*pointer) };
        directory.retained.borrow_mut().sweep();
        published
    })
}

/// Run with all executions stopped, alongside call-table cache root scanning.
/// TransferCode owns its bytecode/constant roots; descriptors have no Lisp words.
pub(super) unsafe fn sweep_all() {
    unsafe {
        DIRECTORY.scan(|directory| directory.retained.borrow_mut().sweep());
    }
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn ordinary_segment_depth_for_test() -> usize {
    DIRECTORY.with(|directory| directory.retained.borrow().segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(owner: &Rc<()>, revision: u64, start: usize) -> OrdinaryEntryDescriptor {
        OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision,
            entry: start + 8,
            code_start: start,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner: Rc::as_ptr(owner) as usize,
            stack_required: 8,
        }
    }

    #[test]
    fn ordinary_explicit_directory_retains_the_exact_compiled_owner() {
        use crate::cli::{Env, reader};
        use egcl_rt::value::NIL;
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(forms = reader::read_from_string("(42)").unwrap().0);
        let body = Arc::new(
            crate::cli::bytecode::compile_function(
                "ORDINARY-DIRECTORY-OWNER",
                NIL,
                *forms,
                &env,
                false,
                false,
            )
            .unwrap(),
        );
        let code = Rc::new(TransferCode::compile(body).expect("retained code fixture"));
        let owner = Rc::as_ptr(&code) as usize;
        let weak = Rc::downgrade(&code);
        let guard = enter_ordinary_segment();
        let directory = guard.directory();
        // Synthetic PC geometry tests ownership lookup only. This descriptor
        // is never published or executed: the fixture still has the old ABI.
        let descriptor = OrdinaryEntryDescriptor {
            kind: linkage::ORDINARY_X64_LISP_KIND,
            version: linkage::ORDINARY_X64_LISP_VERSION,
            revision: 0,
            entry: 0x1010,
            code_start: 0x1000,
            code_len: 64,
            arity: 0,
            environment: linkage::ORDINARY_ENV_NONE,
            owner,
            stack_required: 8,
        };
        assert!(
            directory
                .retained
                .borrow_mut()
                .retain(Arc::new(CallCell::new(0, 11, 12)), descriptor, code,)
                .is_some()
        );
        let unrelated = OrdinaryCodeDirectory {
            retained: RefCell::new(RetainedEntries::default()),
        };
        assert!(unrelated.code_for_pc(0x1010).is_none());
        assert_eq!(
            Rc::as_ptr(&directory.code_for_pc(0x1010).unwrap()) as usize,
            owner
        );
        assert!(weak.upgrade().is_some());
        drop(guard);
        assert!(directory.code_for_pc(0x1010).is_none());
        assert!(
            weak.upgrade().is_none(),
            "directory handle alone does not retain retired code"
        );
    }

    #[test]
    fn ordinary_guard_restores_depth_after_nested_rust_unwind() {
        let _thread = egcl_rt::thread::current_thread_id();
        assert_eq!(ordinary_segment_depth_for_test(), 0);
        let outer = enter_ordinary_segment();
        let directory = outer.directory();
        assert_eq!(ordinary_segment_depth_for_test(), 1);
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let inner = enter_ordinary_segment();
            assert!(Rc::ptr_eq(&directory, &inner.directory()));
            assert_eq!(ordinary_segment_depth_for_test(), 2);
            panic!("selected Rust error");
        }));
        assert!(result.is_err());
        assert_eq!(ordinary_segment_depth_for_test(), 1);
        drop(outer);
        assert_eq!(ordinary_segment_depth_for_test(), 0);
    }

    #[test]
    fn ordinary_retired_owner_survives_suspension_and_nested_segments() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let mut entries = RetainedEntries::default();
        entries.begin_segment();
        // A failed/invalidated publication is retired, but may still be in a
        // loaded native frame. The lease remains live while a fiber is parked.
        assert!(
            entries
                .retain(Arc::clone(&cell), descriptor(&owner, 0, 0x1000), owner)
                .is_some()
        );
        entries.sweep();
        assert!(
            weak.upgrade().is_some(),
            "suspended outer segment retains code"
        );
        entries.begin_segment();
        entries.finish_segment();
        assert!(
            weak.upgrade().is_some(),
            "nested callback cannot retire outer code"
        );
        assert!(entries.code_for_pc(0x1010).is_some());
        assert!(entries.code_for_pc(0x1040).is_none());
        entries.finish_segment();
        assert!(
            weak.upgrade().is_none(),
            "last segment retires invalidated owners"
        );
    }

    #[test]
    fn ordinary_published_descriptor_remains_live_when_execution_is_quiescent() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let owner = Rc::new(());
        let weak = Rc::downgrade(&owner);
        let mut entries = RetainedEntries::default();
        let pointer = entries
            .retain(Arc::clone(&cell), descriptor(&owner, 0, 0x1000), owner)
            .unwrap();
        // The runtime publication tests exercise the locked revision check;
        // here the same atomic slot controls the owner's retirement decision.
        unsafe { &*cell.ordinary_entry_address() }.store(pointer as usize, Ordering::Release);
        entries.sweep();
        assert!(weak.upgrade().is_some());
        entries.begin_segment();
        entries.finish_segment();
        assert!(weak.upgrade().is_some());
        unsafe { &*cell.ordinary_entry_address() }.store(0, Ordering::Release);
        entries.sweep();
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn ordinary_directory_keeps_multiple_code_owners_and_validates_identity() {
        let cell = Arc::new(CallCell::new(0, 11, 12));
        let old = Rc::new(());
        let replacement = Rc::new(());
        let mut entries = RetainedEntries::default();
        entries.begin_segment();
        let wrong = descriptor(&old, 0, 0x1000);
        assert!(
            entries
                .retain(Arc::clone(&cell), wrong, Rc::clone(&replacement))
                .is_none()
        );
        assert!(
            entries
                .retain(
                    Arc::clone(&cell),
                    descriptor(&old, 0, 0x1000),
                    Rc::clone(&old)
                )
                .is_some()
        );
        assert!(
            entries
                .retain(
                    cell,
                    descriptor(&replacement, 1, 0x2000),
                    Rc::clone(&replacement)
                )
                .is_some()
        );
        assert!(Rc::ptr_eq(&entries.code_for_pc(0x1010).unwrap(), &old));
        assert!(Rc::ptr_eq(
            &entries.code_for_pc(0x2010).unwrap(),
            &replacement
        ));
        assert!(entries.code_for_pc(0x3000).is_none());
        entries.finish_segment();
    }
}
