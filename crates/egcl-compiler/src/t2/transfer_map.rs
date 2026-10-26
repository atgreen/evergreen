// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Per-call-site exception capture maps: which locations a transfer must read
//! at a throwing `Invoke`, resolved before any physical frame retires.
//!
//! # Place in the transfer pipeline
//!
//! This is the first compiler-side step of the native transfer ABI. For every
//! `Invoke` in a verified [`Function`] it produces a [`TransferCaptureMap`]
//! naming the machine instruction, the throwing bytecode origin, the ordered
//! control scopes active there, the lowered frame slots, and the subset of
//! locations that are tagged GC roots. transfer_sites.rs later binds each map
//! to an emitted return PC and physical access recipes; transfer_capture.rs
//! allocates the run-time snapshot from it. Maps name machine instruction
//! indices and `Location` keys only, never emitted PCs or hardware save
//! offsets, and nothing here retains code or bytecode definitions.
//!
//! # Algorithm (`lower_transfer_maps`)
//!
//! 1. Verify the IR; collect the `Invoke` instructions in block order and
//!    reject any machine `INVOKE` that does not trace to one of them.
//! 2. For each call, find its unique machine `INVOKE` (safepoint set, same
//!    frame state), require a single-scope frame state, and require the cold
//!    successor to end in an `NlxTransfer` carrying `AuxData::TransferSite`
//!    with the same frame state. That aux supplies `origin_bcp` and the
//!    control scopes.
//! 3. Read the call's own operand allocations from `inst_allocations`, taking
//!    the `deopt_uses` tail as the `Value → Location` binding. The coarse
//!    `MachFunc::allocation` table is never consulted: a split live range can
//!    sit in different locations at two calls of one function, and the cold
//!    continuation's allocations are not yet live at the throwing call.
//! 4. Lower the frame state through `deopt::lower_one` against that binding
//!    and collect the tagged roots (`InLocation` with `Rebox::None`, recursing
//!    into remat inputs).
//! 5. Require exactly one before-instruction stack map with the same frame
//!    state and correctly typed live values at valid Before locations. Validate
//!    late recovery locations independently against the allocation ranges.
//!    These phases share value identity, not necessarily physical addresses.
//!
//! Any failure is a `TransferMapError` and the function declines the transfer
//! emitter rather than shipping a partial map.
//!
//! `lower_framed_transfer_maps` is the variant the rich x86 emitter uses. It
//! runs the same checks, then rebinds every slot through the emitter's final
//! stable homes (`FrameHomes`) rather than regalloc2's transient locations,
//! substituting `Const` sources for values the emitter materialises as
//! constants; a movable heap literal is still rejected by shared descriptor
//! lowering. The emitter must publish roots from those final homes.
//!
//! # Limits
//!
//! Inlined scope composition is refused (`UnsupportedLogicalScopes`) until
//! each logical frame carries its own ordered control-scope map.

use crate::control_scope::ControlScope;
use crate::t2::deopt::{self, LoweredScope, Rebox, SlotDescriptor};
use crate::t2::ir::{AuxData, Function, Inst, Opcode, Value};
use crate::t2::lower::op;
use crate::t2::mach::{EditPosition, Location, MachFunc};

#[derive(Clone, Debug)]
pub struct TransferCaptureMap {
    pub call: Inst,
    pub machine_inst: usize,
    pub origin_bcp: u32,
    pub control_scopes: Vec<ControlScope>,
    /// Reuses deopt's slot descriptors, but `resume_pc` is the throwing origin.
    /// Consumers must begin unwind propagation, never replay that instruction.
    pub frames: Vec<LoweredScope>,
    /// Located tagged values, including inputs nested in remat recipes. Publish
    /// these roots before any reconstruction that can allocate or yield.
    pub roots: Vec<Location>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum TransferMapError {
    InvalidIr,
    MissingOrDuplicateCall(Inst),
    UnexpectedMachineCall,
    BadCallState(Inst),
    UnsupportedContinuation(Inst),
    UnsupportedLogicalScopes(Inst),
    MissingAllocations(Inst),
    MissingCallRoots(Inst),
    FrameState(deopt::LowerError),
    FrameHome(crate::t2::x64_frame::FrameMapError),
}

/// Resolve all Invoke sites using their own operand allocations. Never consult
/// MachFunc::allocation: split live ranges can have different locations at two
/// calls in one function. Reject incomplete maps rather than substituting a
/// cold continuation's allocations, which are not yet live at the throwing call.
/// Inlined scope composition is deliberately refused until each logical frame
/// has its own ordered control-scope map.
pub fn lower_transfer_maps(
    f: &Function,
    machine: &MachFunc,
) -> Result<Vec<TransferCaptureMap>, TransferMapError> {
    use TransferMapError::*;
    crate::t2::verify::verify(f).map_err(|_| InvalidIr)?;
    let calls: Vec<_> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| f.inst(i).opcode == Opcode::Invoke)
        .collect();
    if machine
        .insts
        .iter()
        .any(|i| i.op == op::INVOKE && !i.source_inst.is_some_and(|source| calls.contains(&source)))
    {
        return Err(UnexpectedMachineCall);
    }
    let mut maps = Vec::with_capacity(calls.len());
    for call in calls {
        let data = f.inst(call);
        let mut selected = machine
            .insts
            .iter()
            .enumerate()
            .filter(|(_, i)| i.op == op::INVOKE && i.source_inst == Some(call));
        let (index, inst) = selected.next().ok_or(MissingOrDuplicateCall(call))?;
        if selected.next().is_some() {
            return Err(MissingOrDuplicateCall(call));
        }
        if !inst.safepoint || inst.frame_state != data.frame_state {
            return Err(BadCallState(call));
        }
        let fs = f
            .frame_states
            .get(data.frame_state.ok_or(BadCallState(call))?);
        if fs.scopes.len() != 1 {
            return Err(UnsupportedLogicalScopes(call));
        }
        let cold = f
            .terminator(data.targets[1].block)
            .ok_or(UnsupportedContinuation(call))?;
        let continuation = f.inst(cold);
        let AuxData::TransferSite { origin_bcp, scopes } = &continuation.aux else {
            return Err(UnsupportedContinuation(call));
        };
        if continuation.opcode != Opcode::NlxTransfer
            || continuation.frame_state != data.frame_state
        {
            return Err(UnsupportedContinuation(call));
        }
        let allocations = machine
            .inst_allocations
            .get(index)
            .ok_or(MissingAllocations(call))?;
        let base = inst.defs.len() + inst.uses.len();
        if allocations.len() != base + inst.deopt_uses.len() {
            return Err(MissingAllocations(call));
        }
        let mut locations = std::collections::HashMap::new();
        for (value, &location) in inst.deopt_uses.iter().zip(&allocations[base..]) {
            // Recovery uses are late operands. A before-call GC map cannot
            // validate these addresses: the allocator may move the value.
            if value.num as usize >= f.num_values()
                || machine.value_reprs.get(value) != Some(&f.value(Value(value.num)).repr)
                || !machine
                    .locations_at(*value, index, EditPosition::After)
                    .any(|home| home == location)
            {
                return Err(BadCallState(call));
            }
            if locations
                .insert(Value(value.num), location)
                .is_some_and(|old| old != location)
            {
                return Err(BadCallState(call));
            }
        }
        let frames = deopt::lower_one(0, fs, &|value| locations.get(&value).copied())
            .map_err(FrameState)?
            .scopes;
        let mut roots = Vec::new();
        for frame in &frames {
            for slot in &frame.slots {
                collect_roots(slot, &mut roots);
            }
        }
        let mut stack_maps = machine
            .stack_maps
            .iter()
            .filter(|m| m.code_offset as usize == index);
        let stack_map = stack_maps.next().ok_or(MissingCallRoots(call))?;
        if stack_maps.next().is_some()
            || stack_map.frame_state != data.frame_state
            || inst.deopt_uses.iter().any(|vreg| {
                !stack_map.values.iter().any(|value| {
                    value.vreg == *vreg
                        && machine.value_reprs.get(vreg) == Some(&value.repr)
                        && machine
                            .locations_at(*vreg, index, EditPosition::Before)
                            .any(|home| home == value.location)
                })
            })
        {
            return Err(MissingCallRoots(call));
        }
        maps.push(TransferCaptureMap {
            call,
            machine_inst: index,
            origin_bcp: *origin_bcp,
            control_scopes: scopes.clone(),
            frames,
            roots,
        });
    }
    Ok(maps)
}

fn collect_roots(slot: &SlotDescriptor, roots: &mut Vec<Location>) {
    match slot {
        SlotDescriptor::InLocation(location, Rebox::None) => {
            if !roots.contains(location) {
                roots.push(*location);
            }
        }
        SlotDescriptor::Remat(recipe) => {
            for input in &recipe.inputs {
                collect_roots(input, roots);
            }
        }
        _ => {}
    }
}

/// Rebind validated call states to the rich emitter's final homes. Constants
/// omitted from the home table are materialized directly; a moving heap literal
/// is rejected by shared descriptor lowering. The emitter must publish roots
/// from these final homes, not the raw allocator's transient stack-map locations.
pub fn lower_framed_transfer_maps(
    f: &Function,
    machine: &MachFunc,
    homes: &crate::t2::x64_frame::FrameHomes,
    constants: &std::collections::HashMap<Value, u64>,
) -> Result<Vec<TransferCaptureMap>, TransferMapError> {
    let resolved = crate::t2::x64_frame::resolve_safepoint_maps(f, machine, homes, constants)
        .map_err(TransferMapError::FrameHome)?;
    lower_resolved_transfer_maps(f, machine, &resolved)
}

/// Consume the same checked home records used for root synchronization.
pub(crate) fn lower_resolved_transfer_maps(
    f: &Function,
    machine: &MachFunc,
    resolved: &crate::t2::x64_frame::FrameSafepoints,
) -> Result<Vec<TransferCaptureMap>, TransferMapError> {
    use crate::t2::frame_state::ValueSource;
    use crate::t2::x64_frame::FrameValueLocation;
    let mut maps = lower_transfer_maps(f, machine)?;
    for map in &mut maps {
        let point = resolved
            .get(map.machine_inst)
            .filter(|point| point.source_inst == map.call)
            .ok_or(TransferMapError::BadCallState(map.call))?;
        let mut state = f
            .frame_states
            .get(f.inst(map.call).frame_state.unwrap())
            .clone();
        let substitute = |source: &mut ValueSource| {
            if let ValueSource::Value { value, .. } = source {
                match point.value(*value).map(|entry| entry.location) {
                    Some(FrameValueLocation::Immediate(constant)) => *source = ValueSource::Const(constant),
                    Some(FrameValueLocation::RawImmediate(bits)) =>
                        *source = ValueSource::Const(egcl_rt::value::EgclVal::from_fixnum(bits as i64)),
                    _ => {}
                }
            }
        };
        for scope in &mut state.scopes {
            for source in scope.locals.iter_mut().chain(&mut scope.stack) {
                substitute(source);
            }
        }
        for recipe in &mut state.remat {
            for source in &mut recipe.inputs {
                substitute(source);
            }
        }
        map.frames = deopt::lower_one(0, &state, &|value| match point.value(value)?.location {
            FrameValueLocation::Home(home) => home.location(),
            FrameValueLocation::Immediate(_) | FrameValueLocation::RawImmediate(_) => None,
        })
        .map_err(TransferMapError::FrameState)?
        .scopes;
        map.roots.clear();
        for frame in &map.frames {
            for slot in &frame.slots {
                collect_roots(slot, &mut map.roots);
            }
        }
    }
    Ok(maps)
}

/// Reuse precise deopt descriptors for a cold OSR capture at an inserted poll.
/// The poll's resolved values are authoritative for liveness and physical
/// homes, including constants; absent dead locals have no import destination.
#[cfg_attr(
    not(all(target_arch = "x86_64", target_os = "linux")),
    allow(dead_code)
)]
pub(crate) fn lower_poll_transfer_map(
    f: &Function,
    block: crate::t2::ir::Block,
    machine_inst: usize,
    values: &crate::t2::x64_frame::FrameValues,
) -> Option<TransferCaptureMap> {
    use crate::t2::frame_state::ValueSource;
    use crate::t2::x64_frame::FrameValueLocation;
    let entry = f.osr_entries.iter().find(|entry| entry.block == block)?;
    let mut state = f.frame_states.get(entry.frame_state).clone();
    if state.scopes.len() != 1 || !state.scopes[0].stack.is_empty() {
        return None;
    }
    let substitute = |source: &mut ValueSource| {
        if let ValueSource::Value { value, .. } = source {
            match values.value(*value).map(|entry| entry.location) {
                Some(FrameValueLocation::Immediate(value)) => *source = ValueSource::Const(value),
                Some(FrameValueLocation::RawImmediate(bits)) =>
                    *source = ValueSource::Const(egcl_rt::value::EgclVal::from_fixnum(bits as i64)),
                None => *source = ValueSource::Const(egcl_rt::value::NIL),
                _ => {}
            }
        }
    };
    for source in &mut state.scopes[0].locals {
        substitute(source);
    }
    for recipe in &mut state.remat {
        for source in &mut recipe.inputs {
            substitute(source);
        }
    }
    let frames = deopt::lower_one(0, &state, &|value| match values.value(value)?.location {
        FrameValueLocation::Home(home) => home.location(),
        FrameValueLocation::Immediate(_) | FrameValueLocation::RawImmediate(_) => None,
    }).ok()?.scopes;
    let mut roots = Vec::new();
    for frame in &frames {
        for slot in &frame.slots {
            collect_roots(slot, &mut roots);
        }
    }
    Some(TransferCaptureMap {
        call: f.terminator(block)?,
        machine_inst,
        origin_bcp: entry.bcp,
        control_scopes: entry.control_scopes.clone(),
        frames,
        roots,
    })
}

#[cfg(test)]
mod poll_scope_tests {
    use super::lower_poll_transfer_map;
    use crate::control_scope::{Ownership, ScopeKind, ScopeMap};
    use crate::t2::{build, lower, regalloc, verify, x64_frame};
    use egcl_rt::bytecode::{BytecodeFunction, Instr};
    use egcl_rt::value::NIL;
    use std::collections::HashMap;

    #[test]
    fn osr_poll_preserves_exact_nested_catch_header_scopes() {
        let body = BytecodeFunction {
            self_call_sites: Vec::new(),
            name: "nested-catch-poll-scopes".into(),
            code: vec![
                Instr::LoadLocal(0),
                Instr::PushCatch {
                    resume_bcp: 15,
                    sp_restore: 0,
                },
                Instr::LoadLocal(1),
                Instr::PushCatch {
                    resume_bcp: 9,
                    sp_restore: 0,
                },
                Instr::LoadLocal(2),
                Instr::BrIfFalse(7),
                Instr::Br(4),
                Instr::Const(0),
                Instr::PopHandler,
                Instr::Pop,
                Instr::LoadLocal(2),
                Instr::BrIfFalse(13),
                Instr::Br(10),
                Instr::Const(0),
                Instr::PopHandler,
                Instr::Pop,
                Instr::LoadLocal(2),
                Instr::BrIfFalse(19),
                Instr::Br(16),
                Instr::Const(0),
                Instr::Return,
            ],
            constants: vec![NIL],
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            names: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals: 3,
            max_stack: 1,
            arity: 3,
            params_form: NIL,
            min_args: 3,
            max_args: Some(3),
            variadic: false,
            binder_cache: Default::default(),
        };
        let scopes = ScopeMap::analyze_function(&body).unwrap();
        let function = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
        verify::verify(&function).unwrap();
        let mut machine = lower::lower(&function);
        regalloc::allocate(&mut machine).unwrap();
        let homes = x64_frame::select_frame_homes(&function, &machine, |_| false).unwrap();
        let constants = HashMap::new();
        assert_eq!(function.osr_entries.len(), 3);
        for (bcp, pushes) in [(4, vec![1, 3]), (10, vec![1]), (16, vec![])] {
            let expected = scopes.before(bcp).unwrap();
            assert_eq!(
                expected
                    .iter()
                    .map(|scope| scope.push_bcp)
                    .collect::<Vec<_>>(),
                pushes
            );
            assert!(expected.iter().all(|scope| {
                scope.ownership == Ownership::Local && matches!(scope.kind, ScopeKind::Catch { .. })
            }));
            let entry = function
                .osr_entries
                .iter()
                .find(|entry| entry.bcp == bcp)
                .unwrap();
            let block_index = function
                .block_order()
                .iter()
                .position(|&block| block == entry.block)
                .unwrap();
            let point = machine.blocks[block_index].start;
            let values =
                x64_frame::resolve_values_before(&function, &machine, &homes, &constants, point)
                    .unwrap();
            let capture = lower_poll_transfer_map(&function, entry.block, point, &values).unwrap();
            assert_eq!(capture.origin_bcp, bcp);
            assert_eq!(capture.frames.len(), 1);
            assert_eq!(capture.frames[0].resume_pc, bcp);
            assert_eq!(
                capture.control_scopes, expected,
                "poll must describe the exact scope stack at header {bcp}"
            );
        }
    }
}
