// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Heap spans prepared for a directly mappable image.

pub const IMAGE_PAGE_SIZE: usize = 4096;

use crate::{error::EgclError, gc::RegionKind};

const MAGIC: &[u8; 8] = b"EGCLRGN\0";
const HEADER_SIZE: usize = 24;
const ENTRY_SIZE: usize = 64;

fn invalid() -> EgclError {
    EgclError::InvalidImage("invalid heap region directory".into())
}

fn add(a: usize, b: usize) -> Result<usize, EgclError> {
    a.checked_add(b).ok_or_else(invalid)
}

fn page_round(n: usize) -> Result<usize, EgclError> {
    Ok(add(n, IMAGE_PAGE_SIZE - 1)? & !(IMAGE_PAGE_SIZE - 1))
}

fn word(bytes: &[u8], offset: usize) -> Result<usize, EgclError> {
    let data = bytes.get(offset..add(offset, 8)?).ok_or_else(invalid)?;
    usize::try_from(u64::from_le_bytes(data.try_into().unwrap())).map_err(|_| invalid())
}

fn put(bytes: &mut [u8], offset: usize, value: usize) {
    bytes[offset..offset + 8].copy_from_slice(&(value as u64).to_le_bytes());
}

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

impl HeapSnapshot {
    /// Encode a directory, precise fixup offsets, and page-aligned heap spans.
    /// All offsets are relative to this section; the containing image must also
    /// align the section for direct mapping. Directory words are little-endian;
    /// heap payloads retain the native layout of the image's platform.
    pub fn encode(&self) -> Result<Vec<u8>, crate::error::EgclError> {
        let directory_end = add(
            HEADER_SIZE,
            self.regions
                .len()
                .checked_mul(ENTRY_SIZE)
                .ok_or_else(invalid)?,
        )?;
        let mut metadata_end = directory_end;
        for region in &self.regions {
            metadata_end = add(
                metadata_end,
                region.fixups.len().checked_mul(8).ok_or_else(invalid)?,
            )?;
        }
        let mut payload = if self.regions.is_empty() {
            metadata_end
        } else {
            page_round(metadata_end)?
        };
        let mut total = payload;
        for region in &self.regions {
            total = add(total, region.bytes.len())?;
        }
        let mut bytes = vec![0; total];
        bytes[..8].copy_from_slice(MAGIC);
        put(&mut bytes, 8, self.region_size);
        put(&mut bytes, 16, self.regions.len());
        let mut fixups = directory_end;
        for (index, region) in self.regions.iter().enumerate() {
            let kind = match region.kind {
                RegionKind::Nursery => 1,
                RegionKind::Survivor => 2,
                RegionKind::OldGen => 3,
                RegionKind::LargeObject => 4,
                RegionKind::Free => return Err(invalid()),
            };
            let entry = HEADER_SIZE + index * ENTRY_SIZE;
            for (i, value) in [
                region.saved_start,
                region.used,
                kind,
                payload,
                region.bytes.len(),
                fixups,
                region.fixups.len(),
                0,
            ]
            .into_iter()
            .enumerate()
            {
                put(&mut bytes, entry + i * 8, value);
            }
            for &offset in &region.fixups {
                put(&mut bytes, fixups, offset);
                fixups += 8;
            }
            bytes[payload..payload + region.bytes.len()].copy_from_slice(&region.bytes);
            payload += region.bytes.len();
        }
        // Keep the writer and reader's structural invariants identical.
        HeapImageView::parse(&bytes)?;
        Ok(bytes)
    }
}

pub struct HeapImageView<'a> {
    pub region_size: usize,
    pub regions: Vec<RegionView<'a>>,
}

pub struct RegionView<'a> {
    pub saved_start: usize,
    pub kind: crate::gc::RegionKind,
    pub used: usize,
    pub data_offset: usize,
    pub bytes: &'a [u8],
    fixup_bytes: &'a [u8],
}

impl<'a> RegionView<'a> {
    pub fn fixups(&self) -> impl Iterator<Item = usize> + '_ {
        self.fixup_bytes
            .chunks_exact(8)
            .map(|word| u64::from_le_bytes(word.try_into().unwrap()) as usize)
    }
}

impl<'a> HeapImageView<'a> {
    /// Check object boundaries and ensure fixups cannot overwrite headers or
    /// extend past logical payloads before publishing a native heap mapping.
    pub fn validate_objects(&self) -> Result<(), EgclError> {
        use crate::object::{ObjectHeader, gc_bit};
        for region in &self.regions {
            let mut cursor = 0;
            let mut fixups = region.fixups().peekable();
            while cursor < region.used {
                let header = ObjectHeader(u64::from_ne_bytes(region.bytes[cursor..cursor + 8].try_into().unwrap()));
                let body_offset = if header.is_large_object() { 16 } else { 8 };
                let size = if header.is_large_object() {
                    usize::try_from(u64::from_ne_bytes(region.bytes[cursor + 8..cursor + 16].try_into().unwrap())).map_err(|_| invalid())?
                } else { header.size_units() as usize * 8 };
                let end = add(cursor, size)?;
                if size < 16 || size % 16 != 0 || end > region.used
                    || header.gc_bits() & (1 << gc_bit::FORWARDED) != 0
                    || (header.type_id() != 0 && header.gc_bits() & (1 << gc_bit::PINNED) == 0)
                    || header.hash() as usize > size - body_offset { return Err(invalid()); }
                let body = cursor + body_offset;
                let payload_end = if header.hash() == 0 { end } else { body + header.hash() as usize };
                while fixups.peek().is_some_and(|&offset| offset < end) {
                    let offset = fixups.next().unwrap();
                    if offset < body || offset + 8 > payload_end || header.type_id() == 0 {
                        return Err(invalid());
                    }
                    let raw = u64::from_ne_bytes(region.bytes[offset..offset + 8].try_into().unwrap());
                    if !crate::gc::is_heap_ref(crate::value::EgclVal(raw)) { return Err(invalid()); }
                }
                cursor = end;
            }
        }
        Ok(())
    }

    /// Check directory arithmetic and span boundaries without copying payloads.
    /// This validates the container, not the object layouts inside each span.
    /// Callers must validate the image's platform and runtime contract before
    /// interpreting those layouts or applying pointer fixups.
    pub fn parse(bytes: &'a [u8]) -> Result<Self, crate::error::EgclError> {
        if bytes.get(..8) != Some(MAGIC.as_slice()) {
            return Err(invalid());
        }
        let region_size = word(bytes, 8)?;
        if region_size == 0 || region_size % IMAGE_PAGE_SIZE != 0 {
            return Err(invalid());
        }
        let count = word(bytes, 16)?;
        let directory_end = add(
            HEADER_SIZE,
            count.checked_mul(ENTRY_SIZE).ok_or_else(invalid)?,
        )?;
        if directory_end > bytes.len() {
            return Err(invalid());
        }
        let mut fixup_end = directory_end;
        let mut regions = Vec::with_capacity(count);
        let mut previous_end = 0;
        for index in 0..count {
            let entry = HEADER_SIZE + index * ENTRY_SIZE;
            let saved_start = word(bytes, entry)?;
            let used = word(bytes, entry + 8)?;
            let kind = match word(bytes, entry + 16)? {
                1 => RegionKind::Nursery,
                2 => RegionKind::Survivor,
                3 => RegionKind::OldGen,
                4 => RegionKind::LargeObject,
                _ => return Err(invalid()),
            };
            let data_offset = word(bytes, entry + 24)?;
            let data_len = word(bytes, entry + 32)?;
            let fixup_offset = word(bytes, entry + 40)?;
            let fixup_count = word(bytes, entry + 48)?;
            if word(bytes, entry + 56)? != 0
                || saved_start == 0
                || saved_start % IMAGE_PAGE_SIZE != 0
                || saved_start < previous_end
                || used == 0
                || used % 16 != 0
                || (kind != RegionKind::LargeObject && used > region_size)
                || data_len != page_round(used)?
                || data_offset % IMAGE_PAGE_SIZE != 0
                || fixup_offset != fixup_end
            {
                return Err(invalid());
            }
            let capacity = add(used, region_size - 1)? / region_size;
            previous_end = add(
                saved_start,
                capacity.checked_mul(region_size).ok_or_else(invalid)?,
            )?;
            fixup_end = add(
                fixup_offset,
                fixup_count.checked_mul(8).ok_or_else(invalid)?,
            )?;
            let fixup_bytes = bytes.get(fixup_offset..fixup_end).ok_or_else(invalid)?;
            let mut previous = None;
            for offset in (0..fixup_bytes.len()).step_by(8) {
                let slot = word(fixup_bytes, offset)?;
                if slot % 8 != 0 || add(slot, 8)? > used || previous.is_some_and(|p| slot <= p) {
                    return Err(invalid());
                }
                previous = Some(slot);
            }
            let payload = bytes
                .get(data_offset..add(data_offset, data_len)?)
                .ok_or_else(invalid)?;
            regions.push(RegionView {
                saved_start,
                kind,
                used,
                data_offset,
                bytes: payload,
                fixup_bytes,
            });
        }
        let mut payload_end = if count == 0 {
            fixup_end
        } else {
            page_round(fixup_end)?
        };
        for region in &regions {
            if region.data_offset != payload_end {
                return Err(invalid());
            }
            payload_end = add(payload_end, region.bytes.len())?;
        }
        if payload_end != bytes.len() {
            return Err(invalid());
        }
        Ok(Self {
            region_size,
            regions,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gc::RegionKind;

    fn fixture() -> HeapSnapshot {
        let mut bytes = vec![0; IMAGE_PAGE_SIZE];
        bytes[..32].fill(0x55);
        HeapSnapshot {
            region_size: 8192,
            regions: vec![SnapshotRegion {
                saved_start: 0x10000,
                kind: RegionKind::Nursery,
                used: 32,
                bytes,
                fixups: vec![8, 24],
            }],
        }
    }

    #[test]
    fn region_section_round_trip_borrows_aligned_payload_and_fixups() {
        let snapshot = fixture();
        let encoded = snapshot.encode().unwrap();
        let view = HeapImageView::parse(&encoded).unwrap();
        assert_eq!(view.region_size, 8192);
        let region = &view.regions[0];
        assert_eq!(region.saved_start, 0x10000);
        assert_eq!(region.kind, RegionKind::Nursery);
        assert_eq!(region.used, 32);
        assert_eq!(region.data_offset % IMAGE_PAGE_SIZE, 0);
        assert_eq!(region.bytes, snapshot.regions[0].bytes);
        assert_eq!(
            region.bytes.as_ptr(),
            encoded[region.data_offset..].as_ptr()
        );
        assert_eq!(region.fixups().collect::<Vec<_>>(), vec![8, 24]);
        assert!(encoded.len() < 8192 + IMAGE_PAGE_SIZE);
    }

    #[test]
    fn empty_snapshot_round_trips_without_heap_capacity() {
        let encoded = HeapSnapshot {
            region_size: 8192,
            regions: Vec::new(),
        }
        .encode()
        .unwrap();
        assert_eq!(encoded.len(), 24);
        assert!(HeapImageView::parse(&encoded).unwrap().regions.is_empty());
    }

    #[test]
    fn object_validation_rejects_invalid_strides_and_header_fixups() {
        let mut snapshot = fixture();
        let mut header = crate::object::ObjectHeader::new(crate::object::type_id::CONS, 4);
        header.set_hash(16);
        header.set_pinned();
        snapshot.regions[0].bytes[..8].copy_from_slice(&header.0.to_ne_bytes());
        snapshot.regions[0].bytes[8..16].copy_from_slice(&(0x10008u64 | crate::value::TAG_CONS).to_ne_bytes());
        snapshot.regions[0].fixups = vec![8];
        let encoded = snapshot.encode().unwrap();
        HeapImageView::parse(&encoded).unwrap().validate_objects().unwrap();
        let data_offset = HeapImageView::parse(&encoded).unwrap().regions[0].data_offset;
        for raw_header in [0, header.0 & !0xffff | 0xffff, header.0 & !0xffff | 3, header.0 & !(1 << 52)] {
            let mut bad = encoded.clone();
            bad[data_offset..data_offset + 8].copy_from_slice(&raw_header.to_ne_bytes());
            assert!(HeapImageView::parse(&bad).unwrap().validate_objects().is_err());
        }
        snapshot.regions[0].fixups = vec![0];
        let encoded = snapshot.encode().unwrap();
        assert!(HeapImageView::parse(&encoded).unwrap().validate_objects().is_err());
    }

    #[test]
    fn multiple_spans_include_large_objects_and_reject_overlaps() {
        let mut snapshot = fixture();
        snapshot.regions.push(SnapshotRegion {
            saved_start: 0x20000,
            kind: RegionKind::LargeObject,
            used: 20000,
            bytes: vec![0x33; 5 * IMAGE_PAGE_SIZE],
            fixups: vec![],
        });
        let encoded = snapshot.encode().unwrap();
        let view = HeapImageView::parse(&encoded).unwrap();
        assert_eq!(view.regions.len(), 2);
        assert_eq!(view.regions[1].bytes, snapshot.regions[1].bytes);
        assert_eq!(
            view.regions[1].data_offset,
            view.regions[0].data_offset + IMAGE_PAGE_SIZE
        );
        assert_eq!(view.regions[1].fixups().count(), 0);

        snapshot.regions[1].saved_start = 0x11000;
        assert!(snapshot.encode().is_err());
        snapshot.regions[1].saved_start = 0x20000;
        snapshot.regions[1].kind = RegionKind::OldGen;
        assert!(snapshot.encode().is_err());
    }

    #[test]
    fn invalid_snapshots_are_rejected_by_the_writer() {
        for fixups in [vec![7], vec![8, 8], vec![24, 8], vec![32], vec![usize::MAX]] {
            let mut snapshot = fixture();
            snapshot.regions[0].fixups = fixups;
            assert!(snapshot.encode().is_err());
        }
        let mut snapshot = fixture();
        snapshot.regions[0].saved_start = usize::MAX & !(IMAGE_PAGE_SIZE - 1);
        assert!(snapshot.encode().is_err());
        snapshot = fixture();
        snapshot.region_size = 0;
        assert!(snapshot.encode().is_err());
        snapshot = fixture();
        snapshot.regions[0].bytes.pop();
        assert!(snapshot.encode().is_err());
    }

    #[test]
    fn malformed_offsets_lengths_and_fixups_are_rejected() {
        let encoded = fixture().encode().unwrap();
        for len in [0, 7, 23, 24, 87, encoded.len() - 1] {
            assert!(
                HeapImageView::parse(&encoded[..len]).is_err(),
                "accepted truncation at {len}"
            );
        }
        // Header count; directory used, kind, payload offset/length,
        // fixup offset/count; then an out-of-range fixup.
        for (offset, value) in [
            (16, u64::MAX),
            (32, 0),
            (40, 99),
            (48, 1),
            (56, u64::MAX),
            (64, 0),
            (72, u64::MAX),
            (88, 32),
        ] {
            let mut bad = encoded.clone();
            bad[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
            assert!(
                HeapImageView::parse(&bad).is_err(),
                "accepted corrupt word at {offset}"
            );
        }
    }
}
