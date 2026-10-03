// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
//! Address translation for mapped heap images and separately restored objects.
use crate::error::EgclError;

/// A saved heap span whose internal byte offsets survive restoration.
#[derive(Clone, Copy, Debug)]
pub struct RegionRelocation {
    pub saved_start: usize,
    pub restored_start: usize,
    pub len: usize,
}

#[derive(Clone, Default)]
pub struct ImageRelocations {
    regions: Vec<RegionRelocation>,
    objects: std::collections::HashMap<usize, usize, crate::fxhash::FxBuildHasher>,
}

impl ImageRelocations {
    /// Validate both address spaces before using any range for translation.
    pub fn new(mut regions: Vec<RegionRelocation>) -> Result<Self, EgclError> {
        for region in &regions {
            if region.len == 0
                || region.saved_start % 8 != 0
                || region.restored_start % 8 != 0
                || region.saved_start.checked_add(region.len).is_none()
                || region.restored_start.checked_add(region.len).is_none()
            {
                return Err(EgclError::InvalidImage(
                    "invalid heap relocation range".into(),
                ));
            }
        }
        regions.sort_unstable_by_key(|region| region.saved_start);
        if regions
            .windows(2)
            .any(|pair| pair[0].saved_start + pair[0].len > pair[1].saved_start)
        {
            return Err(EgclError::InvalidImage(
                "overlapping saved heap ranges".into(),
            ));
        }
        let mut restored: Vec<_> = regions.iter().collect();
        restored.sort_unstable_by_key(|region| region.restored_start);
        if restored
            .windows(2)
            .any(|pair| pair[0].restored_start + pair[0].len > pair[1].restored_start)
        {
            return Err(EgclError::InvalidImage(
                "overlapping restored heap ranges".into(),
            ));
        }
        Ok(Self {
            regions,
            objects: Default::default(),
        })
    }

    /// Reserve exact translations for objects restored outside mapped regions.
    pub fn with_object_capacity(capacity: usize) -> Self {
        Self {
            regions: Vec::new(),
            objects: std::collections::HashMap::with_capacity_and_hasher(
                capacity,
                Default::default(),
            ),
        }
    }

    /// Register a separately restored object's header-plus-eight identity.
    pub fn insert_object(&mut self, saved_body: usize, restored_body: usize) -> Option<usize> {
        self.objects.insert(saved_body, restored_body)
    }

    pub(crate) fn object_targets(&self) -> impl Iterator<Item = &usize> {
        self.objects.values()
    }

    /// Translate a known address, including interior addresses in mapped spans.
    /// Callers must identify pointer slots; arbitrary integer payloads are not addresses.
    pub fn translate_address(&self, address: usize) -> Option<usize> {
        if let Some(&target) = self.objects.get(&address) {
            return Some(target);
        }
        let index = self
            .regions
            .partition_point(|region| region.saved_start <= address);
        let region = self.regions.get(index.checked_sub(1)?)?;
        let offset = address - region.saved_start;
        (offset < region.len).then(|| region.restored_start + offset)
    }

    /// Translate Lisp pointer tags and bare addresses used by image registries.
    pub fn remap(&self, raw: u64) -> u64 {
        const OBJECT_HEADER_SIZE: usize = std::mem::size_of::<crate::object::ObjectHeader>();
        use crate::value::{TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_MASK};
        let tag = raw & TAG_MASK;
        let address = (raw & !TAG_MASK) as usize;
        match tag {
            TAG_CONS | 0 => self
                .translate_address(address)
                .map_or(raw, |target| target as u64 | tag),
            TAG_HEAP_OBJECT | TAG_FUNCTION => address
                .checked_add(OBJECT_HEADER_SIZE)
                .and_then(|body| self.translate_address(body))
                .and_then(|body| body.checked_sub(OBJECT_HEADER_SIZE))
                .map_or(raw, |header| header as u64 | tag),
            _ => raw,
        }
    }

    /// Relocate a tagged Lisp value, never an untagged integer or native handle.
    pub fn remap_value(&self, raw: u64) -> u64 {
        use crate::value::{TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT, TAG_MASK};
        match raw & TAG_MASK {
            TAG_CONS | TAG_FUNCTION | TAG_HEAP_OBJECT => self.remap(raw),
            _ => raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn translates_offsets_in_both_directions_and_preserves_boundaries() {
        let map = ImageRelocations::new(vec![
            RegionRelocation {
                saved_start: 0x8000,
                restored_start: 0x2000,
                len: 0x1000,
            },
            RegionRelocation {
                saved_start: 0x1000,
                restored_start: 0x4000,
                len: 0x1000,
            },
            RegionRelocation {
                saved_start: 0xa000,
                restored_start: 0xa000,
                len: 0x1000,
            },
        ])
        .unwrap();
        for (saved, restored) in [
            (0x1000, 0x4000),
            (0x1008, 0x4008),
            (0x1fff, 0x4fff),
            (0x8123, 0x2123),
            (0xa050, 0xa050),
        ] {
            assert_eq!(map.translate_address(saved), Some(restored));
        }
        for address in [0, 0xfff, 0x2000, 0x7fff, 0x9000, 0xb000, usize::MAX] {
            assert_eq!(map.translate_address(address), None);
        }
    }

    #[test]
    fn preserves_pointer_tags_and_resolves_offheap_exceptions() {
        use crate::value::{NIL, TAG_CONS, TAG_FUNCTION, TAG_HEAP_OBJECT};
        let mut map = ImageRelocations::new(vec![RegionRelocation {
            saved_start: 0x1000,
            restored_start: 0x5000,
            len: 0x1000,
        }])
        .unwrap();
        for tag in [TAG_CONS, TAG_HEAP_OBJECT, TAG_FUNCTION, 0] {
            assert_eq!(map.remap(0x1010 | tag), 0x5010 | tag);
        }
        assert_eq!(map.remap_value(0x1010), 0x1010, "fixnums are not addresses");
        assert_eq!(map.remap_value(0x1010 | TAG_CONS), 0x5010 | TAG_CONS);
        map.insert_object(0x8008, 0xc008);
        assert_eq!(
            map.remap(0x8000 | TAG_HEAP_OBJECT),
            0xc000 | TAG_HEAP_OBJECT
        );
        assert_eq!(map.remap(0x8000 | TAG_FUNCTION), 0xc000 | TAG_FUNCTION);
        assert_eq!(map.remap(0x8008), 0xc008);
        assert_eq!(map.remap(0x8010), 0x8010);
        assert_eq!(map.remap(NIL.0), NIL.0);
        assert_eq!(map.remap(0x1013), 0x1013);
        assert_eq!(map.remap(u64::MAX - 5), u64::MAX - 5);
    }

    #[test]
    fn object_only_translation_remains_exact() {
        let mut map = ImageRelocations::with_object_capacity(2);
        map.insert_object(0x1008, 0x5008);
        assert_eq!(map.translate_address(0x1008), Some(0x5008));
        assert_eq!(map.translate_address(0x1010), None);
        assert_eq!(
            map.object_targets().copied().collect::<Vec<_>>(),
            vec![0x5008]
        );
    }

    #[test]
    fn rejects_overlapping_empty_and_overflowing_spans() {
        for (saved_start, restored_start, len) in [
            (0x1000, 0x4000, 0),
            (usize::MAX - 7, 0x4000, 16),
            (0x1000, usize::MAX - 7, 16),
        ] {
            assert!(
                ImageRelocations::new(vec![RegionRelocation {
                    saved_start,
                    restored_start,
                    len,
                }])
                .is_err()
            );
        }
        for second in [
            RegionRelocation {
                saved_start: 0x1800,
                restored_start: 0x8000,
                len: 0x1000,
            },
            RegionRelocation {
                saved_start: 0x8000,
                restored_start: 0x4800,
                len: 0x1000,
            },
        ] {
            assert!(
                ImageRelocations::new(vec![
                    RegionRelocation {
                        saved_start: 0x1000,
                        restored_start: 0x4000,
                        len: 0x1000
                    },
                    second,
                ])
                .is_err()
            );
        }
        assert!(ImageRelocations::new(Vec::new()).is_ok());
    }
}
