// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exact returning-call positions and authoritative suspended value copies.

use super::frame_state::FrameStateId;
use super::ir::{Block, Inst};
use super::x64_value_maps::NativeFrameValues;
use std::collections::HashMap;
use std::sync::Arc;

/// State of native values at the exact primary CALL return PC.
#[derive(Clone, Debug)]
pub enum NativeCallValues {
    /// This emission path has not supplied authoritative value recipes.
    Unavailable,
    /// The epilogue removed this native frame before calling restart deopt.
    Retired,
    /// The helper reconstructs managed frames before any collection or yield.
    /// At GC points those frames own the live values; the old serialization
    /// buffer must not be scanned. Errors abandon it without collecting.
    /// Asynchronous debug samples during reconstruction have unavailable values.
    Deoptimizing {
        native_slots: u32,
    },
    Frame(Arc<NativeFrameValues>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeCallOrigin {
    Instruction(Inst),
    LoopPoll(Block),
    StraightPoll(Inst),
    Deopt(FrameStateId),
    RestartDeopt,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NativeStackBase {
    /// RSP after the native prologue, before any call's temporary area.
    Body,
    /// RSP on entry, after the native epilogue has already removed the frame.
    Entry,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeCallSite {
    /// Offset immediately after CALL, before any restoration or leaf callback.
    pub return_offset: usize,
    /// Bytes subtracted from `stack_base` at CALL; excludes CALL's return word.
    pub stack_adjust: u32,
    pub stack_base: NativeStackBase,
    pub origin: NativeCallOrigin,
}

/// Immutable, exact-offset index. Absence is never resolved to a nearby call.
#[derive(Clone, Debug)]
pub struct NativeCallSites {
    sites: Vec<NativeCallSite>,
    values: Vec<NativeCallValues>,
}

impl NativeCallSites {
    pub(crate) fn new(code_len: usize, mut sites: Vec<NativeCallSite>) -> Option<Self> {
        sites.sort_unstable_by_key(|site| site.return_offset);
        if sites.iter().any(|site| {
            site.return_offset == 0 || site.return_offset >= code_len || site.stack_adjust % 8 != 0
        }) || sites
            .windows(2)
            .any(|pair| pair[0].return_offset == pair[1].return_offset)
        {
            return None;
        }
        let values = vec![NativeCallValues::Unavailable; sites.len()];
        Some(Self { sites, values })
    }

    pub(crate) fn with_value_maps(
        mut self,
        bindings: HashMap<usize, NativeCallValues>,
    ) -> Option<Self> {
        for (offset, values) in bindings {
            let index = self
                .sites
                .binary_search_by_key(&offset, |site| site.return_offset)
                .ok()?;
            let site = &self.sites[index];
            match &values {
                NativeCallValues::Frame(map) => map.validate_at(site).ok()?,
                NativeCallValues::Deoptimizing { .. }
                    if site.stack_base != NativeStackBase::Body
                        || !matches!(site.origin, NativeCallOrigin::Deopt(_)) =>
                {
                    return None;
                }
                NativeCallValues::Retired
                    if site.stack_base != NativeStackBase::Entry
                        || site.origin != NativeCallOrigin::RestartDeopt =>
                {
                    return None;
                }
                _ => {}
            }
            self.values[index] = values;
        }
        Some(self)
    }

    pub fn value_map(&self, return_offset: usize) -> Option<&NativeCallValues> {
        let index = self
            .sites
            .binary_search_by_key(&return_offset, |site| site.return_offset)
            .ok()?;
        Some(&self.values[index])
    }

    pub fn iter(&self) -> impl Iterator<Item = &NativeCallSite> {
        self.sites.iter()
    }

    pub fn get(&self, return_offset: usize) -> Option<&NativeCallSite> {
        self.sites
            .binary_search_by_key(&return_offset, |site| site.return_offset)
            .ok()
            .map(|index| &self.sites[index])
    }
}

/// One template's primary call, before enclosing-template stack adjustment.
#[derive(Clone, Copy, Debug)]
pub(crate) struct CallReturn {
    pub return_offset: usize,
    pub stack_adjust: u32,
}

impl CallReturn {
    pub(crate) fn site(
        self,
        origin: NativeCallOrigin,
        stack_base: NativeStackBase,
    ) -> NativeCallSite {
        NativeCallSite {
            return_offset: self.return_offset,
            stack_adjust: self.stack_adjust,
            stack_base,
            origin,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bindings_reject_unknown_pcs_and_incompatible_frame_states() {
        let instruction = NativeCallSite {
            return_offset: 12,
            stack_adjust: 0,
            stack_base: NativeStackBase::Body,
            origin: NativeCallOrigin::Instruction(Inst(1)),
        };
        let table = NativeCallSites::new(32, vec![instruction]).unwrap();
        for (pc, values) in [
            (11, NativeCallValues::Unavailable),
            (12, NativeCallValues::Retired),
            (12, NativeCallValues::Deoptimizing { native_slots: 3 }),
        ] {
            assert!(
                table
                    .clone()
                    .with_value_maps(HashMap::from([(pc, values)]))
                    .is_none()
            );
        }
        let deopt = NativeCallSite {
            origin: NativeCallOrigin::Deopt(FrameStateId(0)),
            ..instruction
        };
        let restart = NativeCallSite {
            return_offset: 24,
            origin: NativeCallOrigin::RestartDeopt,
            stack_base: NativeStackBase::Entry,
            ..instruction
        };
        let table = NativeCallSites::new(32, vec![deopt, restart])
            .unwrap()
            .with_value_maps(HashMap::from([
                (12, NativeCallValues::Deoptimizing { native_slots: 3 }),
                (24, NativeCallValues::Retired),
            ]))
            .unwrap();
        assert!(matches!(
            table.value_map(12),
            Some(NativeCallValues::Deoptimizing { native_slots: 3 })
        ));
        assert!(matches!(
            table.value_map(24),
            Some(NativeCallValues::Retired)
        ));
        assert!(table.value_map(13).is_none());
    }

    #[test]
    fn index_rejects_ambiguous_or_out_of_range_return_pcs() {
        let site = NativeCallSite {
            return_offset: 12,
            stack_adjust: 64,
            stack_base: NativeStackBase::Body,
            origin: NativeCallOrigin::Instruction(Inst(1)),
        };
        for invalid in [
            NativeCallSite {
                return_offset: 0,
                ..site
            },
            NativeCallSite {
                return_offset: 20,
                ..site
            },
            NativeCallSite {
                stack_adjust: 7,
                ..site
            },
        ] {
            assert!(NativeCallSites::new(20, vec![invalid]).is_none());
        }
        assert!(NativeCallSites::new(20, vec![site, site]).is_none());
        let earlier = NativeCallSite {
            return_offset: 4,
            ..site
        };
        let table = NativeCallSites::new(20, vec![site, earlier]).unwrap();
        assert_eq!(
            table.iter().copied().collect::<Vec<_>>(),
            vec![earlier, site]
        );
        assert_eq!(table.get(12), Some(&site));
        assert_eq!(table.get(11), None);
        assert_eq!(table.get(13), None);
        assert_eq!(table.get(usize::MAX), None);
    }
}
