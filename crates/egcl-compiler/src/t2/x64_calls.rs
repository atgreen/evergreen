// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exact returning-call positions from x86-64 templates. These attachment
//! points are not GC recipes: suspended values still need authoritative homes.

use super::frame_state::FrameStateId;
use super::ir::{Block, Inst};

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
pub struct NativeCallSites(Vec<NativeCallSite>);

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
        Some(Self(sites))
    }

    pub fn iter(&self) -> impl Iterator<Item = &NativeCallSite> {
        self.0.iter()
    }

    pub fn get(&self, return_offset: usize) -> Option<&NativeCallSite> {
        self.0
            .binary_search_by_key(&return_offset, |site| site.return_offset)
            .ok()
            .map(|index| &self.0[index])
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
