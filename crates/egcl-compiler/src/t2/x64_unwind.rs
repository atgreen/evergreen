// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Physical SysV frame geometry, independent of GC and debugger encodings.
//!
//! Addresses returned here are writable-location provenance, not copied register
//! values. A runtime must separately establish live stack ownership before it
//! reads or writes them. These routines perform no pointer dereferences.

use super::x64_calls::{NativeCallSite, NativeStackBase};
use std::ops::Range;

/// Machine register encodings in capture/register-location array order.
pub const SYSV_PRESERVED_REGISTERS: [u8; 6] = [3, 5, 12, 13, 14, 15];

/// Physical state at a suspended call in a known code owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFrameCursor {
    pub pc: usize,
    /// RSP immediately before CALL, excluding CALL's return-address word.
    pub call_sp: usize,
    /// Writable saved-word addresses, ordered by `SYSV_PRESERVED_REGISTERS`.
    /// An unsaved register inherits its younger frame's location.
    pub registers: [Option<usize>; 6],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NativeFrameStep {
    /// Absent after the epilogue has already retired this frame.
    pub body_sp: Option<usize>,
    pub caller: NativeFrameCursor,
}

/// Geometry derived from the exact prologue's push order and allocations.
#[derive(Clone, Debug)]
pub struct NativeUnwindRecipe {
    frame_bytes: u32,
    saved_offsets: [Option<u32>; 6],
}

impl NativeUnwindRecipe {
    #[cfg(any(test, not(all(target_arch = "x86_64", windows))))]
    pub(crate) fn from_prologue(saved: &[u8], pad: bool, spill_bytes: usize) -> Option<Self> {
        // The emitter uses a sign-extended imm32 for its spill subtraction.
        if spill_bytes % 16 != 0 || spill_bytes > i32::MAX as usize {
            return None;
        }
        let frame_bytes = u32::try_from(
            spill_bytes
                .checked_add(usize::from(pad) * 8)?
                .checked_add(saved.len().checked_mul(8)?)?,
        )
        .ok()?;
        let mut saved_offsets = [None; 6];
        for (push_index, register) in saved.iter().enumerate() {
            let index = SYSV_PRESERVED_REGISTERS
                .iter()
                .position(|r| r == register)?;
            if saved_offsets[index].is_some() {
                return None;
            }
            saved_offsets[index] =
                Some(frame_bytes.checked_sub(u32::try_from((push_index + 1) * 8).ok()?)?);
        }
        Some(Self {
            frame_bytes,
            saved_offsets,
        })
    }

    pub fn frame_bytes(&self) -> u32 {
        self.frame_bytes
    }

    /// Body-RSP-relative save offset for a machine register, if this frame saves it.
    pub fn saved_offset(&self, register: u8) -> Option<u32> {
        self.saved_offsets[SYSV_PRESERVED_REGISTERS
            .iter()
            .position(|&r| r == register)?]
    }

    pub(crate) fn unwind(
        &self,
        site: &NativeCallSite,
        cursor: &NativeFrameCursor,
        bounds: Range<usize>,
        mut read_word: impl FnMut(usize) -> Option<usize>,
    ) -> Option<NativeFrameStep> {
        let base_sp = cursor.call_sp.checked_add(site.stack_adjust as usize)?;
        let body_sp = (site.stack_base == NativeStackBase::Body).then_some(base_sp);
        let entry_sp = base_sp.checked_add(if body_sp.is_some() {
            self.frame_bytes as usize
        } else {
            0
        })?;
        let caller_sp = entry_sp.checked_add(8)?;
        if cursor.call_sp % 8 != 0 || cursor.call_sp < bounds.start || caller_sp > bounds.end {
            return None;
        }
        let mut registers = cursor.registers;
        if let Some(body_sp) = body_sp {
            for (location, offset) in registers.iter_mut().zip(self.saved_offsets) {
                if let Some(offset) = offset {
                    *location = Some(body_sp.checked_add(offset as usize)?);
                }
            }
        }
        for &location in registers.iter().flatten() {
            if location % 8 != 0 || location < bounds.start || location.checked_add(8)? > bounds.end
            {
                return None;
            }
            read_word(location)?;
        }
        let pc = read_word(entry_sp)?;
        Some(NativeFrameStep {
            body_sp,
            caller: NativeFrameCursor {
                pc,
                call_sp: caller_sp,
                registers,
            },
        })
    }
}
