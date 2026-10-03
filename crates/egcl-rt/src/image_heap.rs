// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Heap spans prepared for a directly mappable image.

pub const IMAGE_PAGE_SIZE: usize = 4096;

pub struct HeapSnapshot {
    pub region_size: usize,
    pub regions: Vec<SnapshotRegion>,
}

pub struct SnapshotRegion {
    pub saved_start: usize,
    pub kind: crate::gc::RegionKind,
    pub used: usize,
    pub bytes: Vec<u8>,
    /// Byte offsets of genuine tagged pointer fields within this span.
    pub fixups: Vec<usize>,
}
