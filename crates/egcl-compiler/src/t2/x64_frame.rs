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

use crate::t2::ir::{AuxData, Function, Inst, Opcode, Value, ValueDef, ValueRepresentation};
use crate::t2::mach::{Location, MachFunc, PhysReg, RegClass, StackSlot, VReg};
use egcl_rt::value::EgclVal;
use std::collections::{HashMap, HashSet};

/// Allocator indices to hardware encodings; RSP/RBP are reserved.
pub(crate) const GPR_X86: [u8; 14] = [0, 1, 2, 6, 7, 8, 9, 10, 11, 3, 12, 13, 14, 15];

/// Allocator indices usable as stable homes; the first four form the reduced
/// pool. Keep allocation and final-home validation on the same register set.
pub(crate) const FRAME_GPRS: [usize; 8] = [1, 5, 3, 9, 10, 11, 12, 13];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ValueHome {
    /// Hardware register encoding, not an allocator index.
    Reg(u8),
    /// Eight-byte slot relative to the function's body RSP.
    Stack(u32),
}

impl ValueHome {
    /// A stable home whose value survives a SysV call. Native publication must
    /// still expose its writable slot/save word before it can replace a shadow.
    pub(crate) fn survives_sysv_call(self) -> bool {
        matches!(self, Self::Stack(_) | Self::Reg(3 | 12..=15))
    }

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

/// A value in the rich emitter's physical representation, before an instruction
/// template starts changing registers or reserving temporary call space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameValueLocation {
    Home(ValueHome),
    /// A nonmoving tagged word materialized by the emitter when needed.
    Immediate(EgclVal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameValue {
    pub value: Value,
    pub repr: ValueRepresentation,
    pub location: FrameValueLocation,
}

impl FrameValue {
    /// Raw words remain described for debugging/recovery, but only located
    /// tagged values can require GC synchronization.
    pub fn gc_home(&self) -> Option<ValueHome> {
        match (self.repr, self.location) {
            (ValueRepresentation::Tagged, FrameValueLocation::Home(home)) => Some(home),
            _ => None,
        }
    }
}

/// Checked final homes at one native template boundary.
#[derive(Clone, Debug)]
pub struct FrameValues {
    values: Vec<FrameValue>,
}

impl FrameValues {
    pub fn values(&self) -> &[FrameValue] {
        &self.values
    }
    /// Poll calls are invisible to register allocation. Preserve raw registers
    /// in private native slots; raw stack homes already survive the callback.
    pub fn raw_registers(&self) -> impl Iterator<Item = ValueHome> + '_ {
        self.values
            .iter()
            .filter_map(|entry| match (entry.repr, entry.location) {
                (ValueRepresentation::Tagged, _) => None,
                (_, FrameValueLocation::Home(home @ ValueHome::Reg(_))) => Some(home),
                _ => None,
            })
    }

    pub fn value(&self, value: Value) -> Option<&FrameValue> {
        self.values
            .binary_search_by_key(&value.0, |entry| entry.value.0)
            .ok()
            .map(|index| &self.values[index])
    }
}

/// Resolve a poll inserted before any allocated instruction. No source IR
/// safepoint annotation is required, including at edge-move-only block entries.
pub fn resolve_values_before(
    f: &Function,
    machine: &MachFunc,
    homes: &FrameHomes,
    constants: &HashMap<Value, u64>,
    machine_inst: usize,
) -> Result<FrameValues, FrameMapError> {
    let live = machine
        .live_vregs_before(machine_inst, &machine.read_vregs())
        .ok_or(FrameMapError::InvalidSource(machine_inst))?;
    resolve_frame_values(f, machine, homes, constants, live)
}

/// Final body homes at a machine safepoint. These are not yet locations at a
/// native return PC: call setup can clobber homes or make a shadow authoritative.
#[derive(Clone, Debug)]
pub struct FrameSafepoint {
    pub machine_inst: usize,
    pub source_inst: Inst,
    values: FrameValues,
}

impl FrameSafepoint {
    pub fn values(&self) -> &[FrameValue] {
        self.values.values()
    }
    pub fn value(&self, value: Value) -> Option<&FrameValue> {
        self.values.value(value)
    }
}

/// Checked, immutable maps shared by root synchronization and frame recovery.
pub struct FrameSafepoints {
    maps: Vec<FrameSafepoint>,
}

impl FrameSafepoints {
    pub fn iter(&self) -> impl Iterator<Item = &FrameSafepoint> {
        self.maps.iter()
    }

    pub fn get(&self, machine_inst: usize) -> Option<&FrameSafepoint> {
        self.maps
            .binary_search_by_key(&machine_inst, |map| map.machine_inst)
            .ok()
            .map(|index| &self.maps[index])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameMapError {
    SafepointCoverage,
    LiveSetMismatch(usize),
    InvalidSource(usize),
    InvalidValue(Value),
    MissingHome(Value),
    InvalidHome(ValueHome),
    ConflictingHome(ValueHome),
    InvalidConstant(Value),
}

/// Resolve allocator value identities through the homes actually used by the
/// instruction templates. Never substitute a transient allocator location for
/// a missing final home, or silently omit a potentially moving value.
pub fn resolve_safepoint_maps(
    f: &Function,
    machine: &MachFunc,
    homes: &FrameHomes,
    constants: &HashMap<Value, u64>,
) -> Result<FrameSafepoints, FrameMapError> {
    if machine.stack_maps.len() != machine.insts.iter().filter(|i| i.safepoint).count() {
        return Err(FrameMapError::SafepointCoverage);
    }
    let read_vregs = machine.read_vregs();
    let mut maps = Vec::with_capacity(machine.stack_maps.len());
    for (machine_inst, instruction) in machine.insts.iter().enumerate() {
        if !instruction.safepoint {
            continue;
        }
        let mut candidates = machine
            .stack_maps
            .iter()
            .filter(|map| map.code_offset as usize == machine_inst);
        let map = candidates.next().ok_or(FrameMapError::SafepointCoverage)?;
        if candidates.next().is_some() || map.frame_state != instruction.frame_state {
            return Err(FrameMapError::SafepointCoverage);
        }
        // Validate coverage against the allocator's authoritative split ranges.
        // Consumers below use the checked table, never their own liveness scan.
        let expected = machine
            .live_vregs_before(machine_inst, &read_vregs)
            .ok_or(FrameMapError::InvalidSource(machine_inst))?;
        let described: HashSet<_> = map.values.iter().map(|entry| entry.vreg).collect();
        if expected != described {
            return Err(FrameMapError::LiveSetMismatch(machine_inst));
        }
        let source_inst = instruction
            .source_inst
            .filter(|source| source.index() < f.num_insts())
            .ok_or(FrameMapError::InvalidSource(machine_inst))?;
        for allocated in &map.values {
            if machine.value_reprs.get(&allocated.vreg) != Some(&allocated.repr) {
                return Err(FrameMapError::InvalidValue(Value(allocated.vreg.num)));
            }
        }
        let values = resolve_frame_values(f, machine, homes, constants, expected)?;
        maps.push(FrameSafepoint {
            machine_inst,
            source_inst,
            values,
        });
    }
    Ok(FrameSafepoints { maps })
}

fn resolve_frame_values(
    f: &Function,
    machine: &MachFunc,
    homes: &FrameHomes,
    constants: &HashMap<Value, u64>,
    live: HashSet<VReg>,
) -> Result<FrameValues, FrameMapError> {
    let mut values = Vec::new();
    let mut home_values = HashMap::new();
    for vreg in live {
        let value = Value(vreg.num);
        let repr = *machine
            .value_reprs
            .get(&vreg)
            .ok_or(FrameMapError::InvalidValue(value))?;
        if value.index() >= f.num_values()
            || f.value(value).repr != repr
            || crate::t2::lower::class_of(repr) != vreg.class
        {
            return Err(FrameMapError::InvalidValue(value));
        }
        let location = if let Some(&bits) = constants.get(&value) {
            let constant = EgclVal(bits);
            if repr != ValueRepresentation::Tagged || immediate_constant(f, value) != Some(constant)
            {
                return Err(FrameMapError::InvalidConstant(value));
            }
            FrameValueLocation::Immediate(constant)
        } else {
            let home = *homes
                .values
                .get(&value)
                .ok_or(FrameMapError::MissingHome(value))?;
            let valid = match home {
                ValueHome::Reg(reg) => {
                    FRAME_GPRS.iter().any(|&index| GPR_X86[index] == reg)
                        && crate::t2::lower::class_of(repr) == RegClass::Gpr
                }
                ValueHome::Stack(slot) => slot < homes.stack_slots && slot <= i32::MAX as u32 / 8,
            };
            if !valid {
                return Err(FrameMapError::InvalidHome(home));
            }
            if home_values
                .insert(home, value)
                .is_some_and(|old| old != value)
            {
                return Err(FrameMapError::ConflictingHome(home));
            }
            FrameValueLocation::Home(home)
        };
        let entry = FrameValue {
            value,
            repr,
            location,
        };
        if !values.contains(&entry) {
            values.push(entry);
        }
    }
    values.sort_by_key(|entry| entry.value.0);
    Ok(FrameValues { values })
}

// Only IR-defined immediates may replace a live physical root.
fn immediate_constant(f: &Function, value: Value) -> Option<EgclVal> {
    let ValueDef::Result { inst, .. } = f.value(value).def else {
        return None;
    };
    let instruction = f.inst(inst);
    match (instruction.opcode, &instruction.aux) {
        (Opcode::ConstFixnum, AuxData::FixnumImm(v)) => Some(EgclVal::from_fixnum(*v)),
        (Opcode::ConstFloat, AuxData::FloatImm(v)) => Some(EgclVal::from_single_float(*v)),
        (Opcode::ConstChar, AuxData::CharImm(v)) => Some(EgclVal::from_char(*v)),
        (Opcode::ConstSymbol, AuxData::SymbolRef(v)) => Some(EgclVal::from_symbol_index(*v)),
        (Opcode::ConstNil, _) => Some(egcl_rt::value::NIL),
        (Opcode::ConstT, _) => Some(egcl_rt::value::T),
        _ => None,
    }
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
