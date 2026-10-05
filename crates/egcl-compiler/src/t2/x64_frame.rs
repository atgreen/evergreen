// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Stable value homes for the framed x86-64 emitter and its transfer metadata.
//!
//! # Purpose
//!
//! The framed emitter (emit.rs) expands each IR instruction from a template that
//! addresses every operand by a single fixed home for the whole function. It
//! does not replay regalloc2's edit stream, so a value whose live range the
//! allocator split across several locations cannot be used as allocated. This
//! module is the policy that reconciles the two: it takes the allocator's
//! `value_locations` as *advice* and produces one [`ValueHome`] per SSA value.
//! Keep that distinction explicit. The allocator's locations are inputs here,
//! not the locations the templates use.
//!
//! # Contract
//!
//! [`select_frame_homes`] consumes the IR [`Function`], the allocated
//! [`MachFunc`], and an `excluded` predicate naming values the emitter never
//! materialises (tagged constants it folds into immediates, and comparison
//! results fused into a branch). It returns [`FrameHomes`]:
//!
//! * `values` — a home for every non-excluded SSA value. A GPR value whose
//!   every reported range agrees on one register keeps that register, mapped
//!   through `GPR_X86` to its hardware encoding. A value whose ranges agree
//!   on one stack slot keeps that slot. Anything else (split, or unreported
//!   because the allocator never saw a use) is given a fresh stack slot above
//!   the allocator's own.
//! * `stack_slots` — the slot count after those assignments, starting from
//!   `MachFunc::num_spill_slots`. The emitter adds its own scratch slots above
//!   this and sizes the frame from the total.
//!
//! Only GPR-class ranges are consulted; float values are handled by the
//! emitter's XMM path and get a home here only through the fallback slot rule.
//!
//! `GPR_X86` is the allocator-index → hardware-encoding table shared with
//! emit.rs. RSP and RBP are absent (reserved), and the final index maps to r15,
//! the allocator's edit scratch. [`ValueHome::location`] inverts the table so a
//! home can be expressed as a [`Location`] for deopt and transfer descriptors
//! without confusing hardware encodings with allocator indices.
//!
//! # Rationale
//!
//! Demoting a split value to a stable stack home costs a load per use but makes
//! the templates, the shadow-root sync at safepoints, the deopt stubs, and the
//! OSR entries all agree on where a value is without consulting program
//! points. Values the allocator kept in one register for their whole life, the
//! common case in hot loops, pay nothing.

use crate::t2::ir::{Function, Value};
use crate::t2::mach::{Location, MachFunc, PhysReg, RegClass, StackSlot};
use std::collections::HashMap;

/// Allocator indices to hardware encodings; RSP/RBP are reserved.
pub(crate) const GPR_X86: [u8; 14] = [0, 1, 2, 6, 7, 8, 9, 10, 11, 3, 12, 13, 14, 15];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueHome {
    /// Hardware register encoding, not an allocator index.
    Reg(u8),
    /// Eight-byte slot relative to the function's body RSP.
    Stack(u32),
}

impl ValueHome {
    /// Give descriptors a canonical Location key without confusing hardware
    /// registers with allocator indices. Physical access still needs a recipe.
    pub fn location(self) -> Option<Location> {
        match self {
            Self::Reg(register) => GPR_X86.iter().position(|r| *r == register).map(|index| {
                Location::Register(PhysReg {
                    class: RegClass::Gpr,
                    encoding: index as u8,
                })
            }),
            Self::Stack(slot) => Some(Location::Stack(StackSlot(slot))),
        }
    }
}

pub struct FrameHomes {
    pub values: HashMap<Value, ValueHome>,
    /// Slots consumed by allocation and stable homes, before emitter scratch.
    pub stack_slots: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameHomeError {
    InvalidRegister(PhysReg),
    SlotOverflow,
}

/// Preserve register-resident values only when every reported range agrees;
/// give split/unreported values stable spill homes. `excluded` is the emitter's
/// exact set of materialized constants and fused-away results.
pub fn select_frame_homes(
    f: &Function,
    machine: &MachFunc,
    mut excluded: impl FnMut(Value) -> bool,
) -> Result<FrameHomes, FrameHomeError> {
    let mut ranges: HashMap<Value, Vec<Location>> = HashMap::new();
    for range in &machine.value_locations {
        if range.vreg.class == RegClass::Gpr {
            ranges
                .entry(Value(range.vreg.num))
                .or_default()
                .push(range.location);
        }
    }
    let mut values = HashMap::new();
    let mut stack_slots = machine.num_spill_slots;
    for value_num in 0..f.num_values() as u32 {
        let value = Value(value_num);
        if excluded(value) {
            continue;
        }
        let locs = ranges.get(&value).map(Vec::as_slice).unwrap_or_default();
        let stable = locs
            .first()
            .copied()
            .filter(|first| locs.iter().all(|loc| loc == first));
        let home = match stable {
            Some(Location::Register(preg)) if preg.class == RegClass::Gpr => ValueHome::Reg(
                *GPR_X86
                    .get(preg.encoding as usize)
                    .ok_or(FrameHomeError::InvalidRegister(preg))?,
            ),
            Some(Location::Stack(slot)) => ValueHome::Stack(slot.0),
            _ => {
                let slot = stack_slots;
                stack_slots = stack_slots
                    .checked_add(1)
                    .ok_or(FrameHomeError::SlotOverflow)?;
                ValueHome::Stack(slot)
            }
        };
        values.insert(value, home);
    }
    Ok(FrameHomes {
        values,
        stack_slots,
    })
}
