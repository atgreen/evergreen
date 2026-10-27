// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::cell::Cell;

/// Purpose of the current external-root scanner invocation. Unaware scanners
/// may always perform a full traversal. This is not a liveness or epoch cache.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ExternalRootScanKind {
    /// Major marking/relocation, verification, snapshots, and ordinary walks.
    #[default]
    Full,
    /// Actual minor-GC root marking, before nursery evacuation.
    MinorMark,
    /// Actual minor-GC root relocation, while forwarding information is live.
    MinorRelocate,
}

thread_local! {
    static KIND: Cell<ExternalRootScanKind> = const { Cell::new(ExternalRootScanKind::Full) };
}

/// Returns `Full` outside an explicitly scoped minor external-root scan.
/// Nested full walks override the outer kind until they return or unwind.
pub fn external_root_scan_kind() -> ExternalRootScanKind {
    KIND.with(Cell::get)
}

pub(super) struct Scope(ExternalRootScanKind);
impl Scope {
    pub(super) fn enter(kind: ExternalRootScanKind) -> Self {
        Self(KIND.with(|current| current.replace(kind)))
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        KIND.with(|current| current.set(self.0));
    }
}
