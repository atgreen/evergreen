//! Resolve exception capture at the throwing call, before physical frames retire.
//! These maps name machine instruction indices and location keys, NOT
//! emitted PCs or hardware save offsets. Emission must translate both and retain
//! the executing definitions before installation can publish an unwind site.
//! The framed variant resolves keys from the rich emitter's final stable homes.

use crate::control_scope::ControlScope;
use crate::t2::deopt::{self, LoweredScope, Rebox, SlotDescriptor};
use crate::t2::ir::{AuxData, Function, Inst, Opcode, Value};
use crate::t2::lower::op;
use crate::t2::mach::{Location, MachFunc};

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
            || roots.iter().any(|root| !stack_map.live_refs.contains(root))
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
    use crate::t2::frame_state::ValueSource;
    let mut maps = lower_transfer_maps(f, machine)?;
    for map in &mut maps {
        let mut state = f
            .frame_states
            .get(f.inst(map.call).frame_state.unwrap())
            .clone();
        let substitute = |source: &mut ValueSource| {
            if let ValueSource::Value { value, .. } = source {
                if let Some(&bits) = constants.get(value) {
                    *source = ValueSource::Const(egcl_rt::value::EgclVal(bits));
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
        map.frames = deopt::lower_one(0, &state, &|value| {
            homes.values.get(&value).and_then(|home| home.location())
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
