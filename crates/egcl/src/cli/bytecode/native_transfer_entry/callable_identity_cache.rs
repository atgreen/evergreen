// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Reusable identity handles are performance caches, not semantic owners.
//! Private caches are dropped before every root pass, including snapshots.
use crate::cli::bytecode::call_table::RootIdentity;
use egcl_rt::{gc::CrossThreadRoot, value::EgclVal};
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::{Arc, Once},
};

#[derive(Default)]
pub(super) struct CallableIdentityCache {
    // Separate cells are essential: pinned inspection borrows last_callable
    // while taking a spare identity through its callback.
    pub(super) last_callable: RefCell<Option<Arc<CrossThreadRoot<EgclVal>>>>,
    pub(super) last_callable_pinned: Cell<u64>,
    pub(super) spare_root_identity: RefCell<Option<Rc<RootIdentity>>>,
    queued: Cell<bool>,
}

static DIRTY: egcl_rt::execution_local::ExecutionLocal<RefCell<Vec<Weak<CallableIdentityCache>>>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Vec::new())) };

impl CallableIdentityCache {
    /// Call after the last operation that may park, before publishing a cached
    /// handle. A steady hit performs no registration, allocation or Rc clone.
    pub(super) fn enqueue(cache: &Rc<Self>) {
        if cache.queued.get() {
            return;
        }
        static INSTALL: Once = Once::new();
        INSTALL.call_once(|| egcl_rt::gc::register_root_preparer(prepare));
        DIRTY.with(|dirty| dirty.borrow_mut().push(Rc::downgrade(cache)));
        cache.queued.set(true);
    }

    fn clear(&self) {
        self.queued.set(false);
        self.last_callable_pinned.set(0);
        // Release both borrows before destructors unregister cross-thread
        // roots. Active directory guards own independent Arc/Rc references.
        let last = self.last_callable.borrow_mut().take();
        let spare = self.spare_root_identity.borrow_mut().take();
        drop(last);
        drop(spare);
    }
}

fn prepare() {
    // SAFETY: the RT invokes preparers only with all executions stopped, under
    // collection/snapshot exclusion, before any external scanner holds locks.
    unsafe {
        DIRTY.scan(|dirty| {
            let pending = std::mem::take(&mut *dirty.borrow_mut());
            for token in pending {
                if let Some(cache) = token.upgrade() {
                    cache.clear();
                }
            }
        });
    }
}
