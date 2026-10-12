// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bounded ordinary calls with explicit continuation and final-result policies.
use super::*;
use crate::t2::ir::{AuxData, Inst, InstData, Opcode, Value, ValueRepresentation};
use egcl_rt::call_table::{
    ORDINARY_ENV_NONE, ORDINARY_X64_LISP_KIND, ORDINARY_X64_LISP_VERSION, OrdinaryEntryDescriptor,
};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct OrdinaryCallTarget {
    pub symbol: u32,
    pub descriptor_slot: u64,
    pub revision_address: u64,
}

/// Process-wide atomic cells, independent of each target's cell revision.
#[derive(Clone, Copy)]
pub struct OrdinaryCallFlags {
    pub gc_requested: u64,
    pub sigusr1_pending: u64,
    pub posted_signal: u64,
    pub native_errors: u64,
    pub signal_epoch: u64,
}

/// Cold continuation and noncollecting MV snapshot/clear helper for a bounded
/// ordinary activation which continues after its single call.
#[derive(Clone, Copy)]
pub struct OrdinaryGuardOptions {
    pub veneer: u64,
    pub multiple_values: u64,
}

/// Serialized single-scope guard payload, still owned by the native frame.
#[repr(C)]
pub struct OrdinaryGuardRequest {
    pub n_scopes: u64,
    pub n_words: u64,
    pub words: *const u64,
    pub control: *mut NativeRecursiveControl,
}

const _: () = assert!(std::mem::size_of::<OrdinaryGuardRequest>() == 32);

/// The cold veneer publishes the native caller before collecting or unwinding.
/// Arguments remain in the caller's exact mapped slice. The veneer returns a
/// completed ordinary result in RAX/RDX/RCX/CF; it must not request a retry.
#[repr(C)]
pub struct OrdinaryColdRequest {
    pub symbol: u64,
    pub nargs: usize,
    pub args: *mut egcl_rt::value::EgclVal,
    pub closure: egcl_rt::value::EgclVal,
    pub control: *mut NativeRecursiveControl,
    pub descriptor_slot: u64,
    pub revision_address: u64,
    pub reserved: u64,
}

const _: () = {
    assert!(std::mem::size_of::<OrdinaryColdRequest>() == 64);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, closure) == 24);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, control) == 32);
    assert!(std::mem::offset_of!(OrdinaryColdRequest, revision_address) == 48);
};

#[derive(Clone, Copy)]
pub(super) enum OrdinaryReturnPolicy<'a> {
    FromCall(Inst),
    FromContinuation,
    FromCalls(&'a [Inst]),
}

#[derive(Clone, Copy)]
pub(super) struct OrdinaryCallEmission<'a> {
    target: OrdinaryCallTarget,
    targets: &'a [(Inst, OrdinaryCallTarget)],
    second_target: Option<(Inst, OrdinaryCallTarget)>,
    pub(super) return_policy: OrdinaryReturnPolicy<'a>,
    flags: OrdinaryCallFlags,
    cold: u64,
    tail: bool,
    pub(super) guard: Option<OrdinaryGuardOptions>,
}

/// Emit a single named ordinary call followed immediately by returning its
/// complete result. Only tagged fixed 0–4 argument, environment-free callers
/// with immediate constants are accepted. This artifact has ONLY an ordinary
/// entry; offset zero traps. A retained segment, rooted arity veneer and valid
/// R13 execution pointer are required before invocation. This does not install
/// or publish code. Cold helpers must implement `OrdinaryColdRequest` above.
pub fn emit_framed_ordinary_call(
    f: &Function,
    entry: OrdinaryLispEntryOptions,
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
) -> Result<FramedCode, EmitError> {
    emit_ordinary_call_body(f, entry, target, flags, cold, false, None)
}

/// Opt into a tail handoff for the same bounded, scope-free forwarding shape.
/// A tail transfer reuses the incoming overflow capacity and return address;
/// capacity misses retain an ordinary call and return. Admission failures use
/// the existing rooted cold handoff. The enclosing segment must retain both
/// code owners until quiescence, including after this frame is retired.
pub fn emit_framed_ordinary_tail_call(
    f: &Function,
    entry: OrdinaryLispEntryOptions,
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
) -> Result<FramedCode, EmitError> {
    emit_ordinary_call_body(f, entry, target, flags, cold, true, None)
}

/// Admit only a linear tagged call followed by guarded arithmetic and return.
/// Entry guards resume before the call; continuation guards resume strictly
/// after the completed call. Native callers stay
/// suspended while the failing activation completes in T0; no tail retirement
/// or whole-chain continuation reconstruction is implied by this entry.
pub fn emit_framed_ordinary_guarded_call(
    f: &Function,
    entry: OrdinaryLispEntryOptions,
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
    guard: OrdinaryGuardOptions,
) -> Result<FramedCode, EmitError> {
    if guard.veneer == 0 || guard.multiple_values == 0 {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    emit_ordinary_call_body(f, entry, target, flags, cold, false, Some(guard))
}

#[allow(clippy::too_many_arguments)]
fn emit_ordinary_call_body(
    f: &Function,
    entry: OrdinaryLispEntryOptions,
    target: OrdinaryCallTarget,
    flags: OrdinaryCallFlags,
    cold: u64,
    allow_tail: bool,
    guard: Option<OrdinaryGuardOptions>,
) -> Result<FramedCode, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    validate_entry(f, entry, &[target], flags, cold)?;
    let insts = &f.block(f.entry()).insts;
    if insts.len() < 2 || insts.len() > 64 {
        return Err(bad());
    }
    let calls: Vec<_> = insts.iter().enumerate()
        .filter(|(_, inst)| f.inst(**inst).opcode == Opcode::Call).collect();
    if calls.len() != 1 { return Err(bad()); }
    let (call_position, &call_inst) = calls[0];
    let call = f.inst(call_inst);
    let ret = f.inst(insts[insts.len() - 1]);
    if call.opcode != Opcode::Call
        || call.args.len() > 4
        || call.results.len() != 1
        || !matches!(call.aux, AuxData::CallTarget(symbol) if symbol == target.symbol)
        || !call.flags.call
        || !call.flags.safepoint
        || ret.opcode != Opcode::Return
        || ret.args.len() != 1
        || (guard.is_none() && (call_position != insts.len() - 2 || ret.args != call.results))
        || insts[..call_position].iter().any(|&i| {
            !(matches!(
                f.inst(i).opcode,
                Opcode::ConstFixnum
                    | Opcode::ConstNil
                    | Opcode::ConstT
                    | Opcode::ConstSymbol
                    | Opcode::ConstChar
            ) || guard.is_some() && f.inst(i).opcode == Opcode::Guard)
        })
    {
        return Err(bad());
    }
    if guard.is_some() {
        // A continuing ordinary call consumes only the primary. Until a
        // distinct tuple-to-runtime bridge exists, the fast continuation must
        // explicitly normalize MV before its final return.
        if !crate::t2::opt_clear_mv::single_value_state(f, true)
            .contains(insts.last().ok_or_else(bad)?)
        { return Err(bad()); }
        let state = f.frame_states.get(call.frame_state.ok_or_else(bad)?);
        if state.scopes.len() != 1 || state.scopes[0].function == target.symbol {
            // Direct recursion is outside this first continuation slice.
            return Err(bad());
        }
        let before = &state.scopes[0];
        // The optimizer may hoist a pure type guard on an entry argument.
        // Its continuation still precedes the call, so T0 must execute it.
        for &inst in &insts[..call_position] {
            let data = f.inst(inst);
            if data.opcode == Opcode::Guard {
                let state = f.frame_states.get(data.frame_state.ok_or_else(bad)?);
                if state.scopes.len() != 1
                    || state.scopes[0].function != before.function
                    || state.scopes[0].bcp > before.bcp
                { return Err(bad()); }
            }
        }
        let mut guards = 0;
        for &inst in &insts[call_position + 1..insts.len() - 1] {
            let data = f.inst(inst);
            match data.opcode {
                Opcode::ConstFixnum | Opcode::ConstNil | Opcode::ConstT
                | Opcode::ConstSymbol | Opcode::ConstChar | Opcode::ClearMv => {}
                Opcode::Guard | Opcode::FixnumAdd | Opcode::FixnumSub | Opcode::FixnumMul => {
                    let state = f.frame_states.get(data.frame_state.ok_or_else(bad)?);
                    if state.scopes.len() != 1
                        || state.scopes[0].function != before.function
                        || state.scopes[0].bcp <= before.bcp
                    { return Err(bad()); }
                    guards += 1;
                }
                _ => return Err(bad()),
            }
        }
        if guards == 0 { return Err(bad()); }
    }
    super::emit_framed_inner(
        f,
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        guard.map_or(0, |guard| guard.multiple_values),
        0,
        0,
        Some(entry.arity as u16),
        None,
        &[],
        0,
        EnvironmentCalls::default(),
        None,
        Some(entry),
        Some(OrdinaryCallEmission {
            target,
            targets: &[],
            second_target: None,
            return_policy: if guard.is_some() { OrdinaryReturnPolicy::FromContinuation }
                else { OrdinaryReturnPolicy::FromCall(call_inst) },
            flags,
            cold,
            // Under the bounded 0–4 signature, rounded capacity is 0 or 16.
            tail: allow_tail && (call.args.len() <= 3 || entry.arity == 4),
            guard,
        }),
    )
}

fn validate_entry(
    f: &Function, entry: OrdinaryLispEntryOptions, targets: &[OrdinaryCallTarget],
    flags: OrdinaryCallFlags, cold: u64,
) -> Result<(), EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    if f.block_order().len() != 1
        || f.is_variadic()
        || !f.osr_entries.is_empty()
        || !f.handler_cases.is_empty()
        || entry.arity > 4
        || entry.arity_error_veneer == 0
        || f.block(f.entry()).params.len() != entry.arity
        || f.block(f.entry())
            .params
            .iter()
            .any(|&v| f.is_entry_param_checked(v))
        || [
            cold,
            flags.gc_requested,
            flags.sigusr1_pending,
            flags.posted_signal,
            flags.native_errors,
            flags.signal_epoch,
        ]
        .contains(&0)
        || targets.iter().any(|target| target.descriptor_slot == 0 || target.revision_address == 0)
        || crate::t2::verify::verify(f).is_err()
        || (0..f.num_values()).any(|v| f.value(Value(v as u32)).repr != ValueRepresentation::Tagged)
    {
        return Err(bad());
    }
    Ok(())
}

/// Two straight-line ordinary calls with an explicit single-value reset between
/// them. Targets correspond to call order, even when both name the same symbol.
/// Only the final call forwards its tuple. No guards, OSR, raw values, branches
/// or tail retirement are admitted. The MV helper's zero-count arm must be a
/// nonallocating, nonpolling, nonsignaling clear operation.
pub fn emit_framed_ordinary_two_calls(
    f: &Function, entry: OrdinaryLispEntryOptions, targets: [OrdinaryCallTarget; 2],
    flags: OrdinaryCallFlags, cold: u64, multiple_values: u64,
) -> Result<FramedCode, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    validate_entry(f, entry, &targets, flags, cold)?;
    if multiple_values == 0 { return Err(bad()); }
    let insts = &f.block(f.entry()).insts;
    if insts.len() < 4 || insts.len() > 64 { return Err(bad()); }
    let calls: Vec<_> = insts.iter().enumerate()
        .filter(|(_, inst)| f.inst(**inst).opcode == Opcode::Call).collect();
    if calls.len() != 2 { return Err(bad()); }
    let (first_position, &first) = calls[0];
    let (last_position, &last) = calls[1];
    for (&inst, target) in [first, last].iter().zip(targets) {
        let call = f.inst(inst);
        if call.args.len() > 4 || call.results.len() != 1 || !call.flags.call || !call.flags.safepoint
            || !matches!(call.aux, AuxData::CallTarget(symbol) if symbol == target.symbol)
        { return Err(bad()); }
    }
    let ret = f.inst(*insts.last().ok_or_else(bad)?);
    if last_position != insts.len() - 2 || ret.opcode != Opcode::Return
        || ret.args != f.inst(last).results
    { return Err(bad()); }
    let immediate = |inst: Inst| matches!(f.inst(inst).opcode,
        Opcode::ConstFixnum | Opcode::ConstNil | Opcode::ConstT | Opcode::ConstSymbol | Opcode::ConstChar);
    if insts[..first_position].iter().any(|&inst| !immediate(inst))
        || insts[first_position + 1..last_position].iter()
            .any(|&inst| !immediate(inst) && f.inst(inst).opcode != Opcode::ClearMv)
        || !crate::t2::opt_clear_mv::single_value_state(f, true).contains(&last)
    { return Err(bad()); }
    super::emit_framed_inner(f, 0, 0, 0, 0, 0, 0, 0, multiple_values, 0, 0,
        Some(entry.arity as u16), None, &[], 0, EnvironmentCalls::default(), None,
        Some(entry), Some(OrdinaryCallEmission {
            target: targets[0], targets: &[], second_target: Some((last, targets[1])),
            return_policy: OrdinaryReturnPolicy::FromCall(last), flags, cold, tail: false, guard: None,
        }))
}

/// Compile an ordinary fixed-arity function with per-instruction named targets.
/// All recursive and cross-function sites share the descriptor-selected ABI.
/// The caller must retain code owners and provide rooted cold/guard veneers.
/// Control-region operations and raw values require their own native transition
/// protocols and are rejected here rather than invoking a zero helper address.
pub fn emit_framed_ordinary_function(
    f: &Function, entry: OrdinaryLispEntryOptions,
    targets: &[(Inst, OrdinaryCallTarget)], flags: OrdinaryCallFlags,
    cold: u64, guard: OrdinaryGuardOptions,
) -> Result<FramedCode, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    if f.is_variadic() || !f.osr_entries.is_empty() || !f.handler_cases.is_empty()
        || entry.arity > u16::MAX as usize || entry.arity_error_veneer == 0
        || f.block(f.entry()).params.len() != entry.arity
        || f.block(f.entry()).params.iter().any(|&v| f.is_entry_param_checked(v))
        || [cold, guard.veneer, guard.multiple_values, flags.gc_requested,
            flags.sigusr1_pending, flags.posted_signal, flags.native_errors,
            flags.signal_epoch].contains(&0)
        || crate::t2::verify::verify(f).is_err()
        || (0..f.num_values()).any(|v| f.value(Value(v as u32)).repr != ValueRepresentation::Tagged)
        || f.frame_states.iter().any(|(_, state)| state.scopes.len() != 1)
    { return Err(bad()); }
    let single = crate::t2::opt_clear_mv::single_value_state(f, true);
    let mut returns = Vec::new();
    let mut call_count = 0;
    for &block in f.block_order() {
        let insts = &f.block(block).insts;
        for (position, &inst) in insts.iter().enumerate() {
            let data = f.inst(inst);
            match data.opcode {
                Opcode::Call => {
                    call_count += 1;
                    let mut matches = targets.iter().filter(|(site, _)| *site == inst);
                    let (_, target) = matches.next().ok_or_else(bad)?;
                    if matches.next().is_some() || target.descriptor_slot == 0
                        || target.revision_address == 0 || data.args.len() > u16::MAX as usize
                        || data.results.len() != 1 || !data.flags.call || !data.flags.safepoint
                        || !matches!(data.aux, AuxData::CallTarget(symbol) if symbol == target.symbol)
                    { return Err(bad()); }
                    if position + 2 == insts.len() {
                        let ret = f.inst(insts[position + 1]);
                        if ret.opcode == Opcode::Return && ret.args == data.results {
                            returns.push(inst);
                        }
                    }
                }
                Opcode::Return => {
                    if data.args.len() != 1 || (!single.contains(&inst)
                        && (position == 0 || !returns.contains(&insts[position - 1])))
                    { return Err(bad()); }
                }
                Opcode::Guard | Opcode::FixnumAdd | Opcode::FixnumSub | Opcode::FixnumMul
                | Opcode::FixnumNeg | Opcode::FixnumShl | Opcode::FixnumShr => {
                    if data.frame_state.is_none() { return Err(bad()); }
                }
                Opcode::ConstFixnum | Opcode::ConstNil | Opcode::ConstT
                | Opcode::ConstSymbol | Opcode::ConstChar | Opcode::ClearMv
                | Opcode::FixnumCmpEq | Opcode::FixnumCmpLt | Opcode::FixnumCmpLe
                | Opcode::FixnumCmpGt | Opcode::FixnumCmpGe | Opcode::GenericEq
                | Opcode::TypeCheck | Opcode::LogAnd | Opcode::LogOr | Opcode::LogXor
                | Opcode::LogNot | Opcode::Jump | Opcode::Brif => {}
                _ => return Err(EmitError::UnsupportedOp(super::op_tag(data.opcode))),
            }
        }
    }
    if targets.len() != call_count { return Err(bad()); }
    // Calls contain the complete lifecycle poll. A cycle without a call would
    // otherwise suppress signals, collection and fiber yielding indefinitely.
    // Remove call-containing blocks and require the remaining graph acyclic.
    let mut pending: std::collections::HashSet<_> = f.block_order().iter().copied()
        .filter(|&block| !f.block(block).insts.iter().any(|&inst| f.inst(inst).opcode == Opcode::Call))
        .collect();
    while !pending.is_empty() {
        let ready = pending.iter().copied().find(|&block|
            !pending.iter().any(|&predecessor| f.terminator(predecessor).is_some_and(|inst|
                f.inst(inst).targets.iter().any(|target| target.block == block))));
        let Some(block) = ready else { return Err(bad()); };
        pending.remove(&block);
    }
    // A call-free guarded function still needs native homes and guard control.
    let unused = OrdinaryCallTarget { symbol: 0, descriptor_slot: 0, revision_address: 0 };
    super::emit_framed_inner(f, 0, 0, 0, 0, 0, 0, 0, guard.multiple_values, 0, 0,
        Some(entry.arity as u16), None, &[], 0, EnvironmentCalls::default(), None,
        Some(entry), Some(OrdinaryCallEmission {
            target: unused, targets, second_target: None,
            return_policy: OrdinaryReturnPolicy::FromCalls(&returns), flags, cold,
            tail: false, guard: Some(guard),
        }))
}

fn unsigned_branch(a: &mut Asm, inclusive: bool, target: egcl_rt::asm::Label) {
    let at = a.here();
    a.jcc(Cc::E, target);
    // Shared Cc deliberately only models portable signed conditions. x86
    // admission uses unsigned addresses and explicit borrow tests.
    a.patch_u8(at + 1, if inclusive { 0x86 } else { 0x82 }); // JBE / JB
}

fn retire(a: &mut Asm, bytes: u32) {
    if bytes != 0 {
        a.extend_from_slice(&[0x48, 0x8d, 0xa4, 0x24]);
        a.extend_from_slice(&bytes.to_le_bytes()); // lea rsp,[rsp+bytes], preserve CF
    }
}

/// The frame storage and its matching callee-save retirement sequence.
pub(super) struct OrdinaryCallFrame<'a> {
    pub outgoing: NativeValueArea,
    pub control_home: FramedHome,
    pub emit_epilogue: &'a dyn Fn(&mut Asm),
}

impl OrdinaryCallEmission<'_> {
    pub(super) fn returns_at(self, inst: Inst) -> bool {
        match self.return_policy {
            OrdinaryReturnPolicy::FromCall(call) => call == inst,
            OrdinaryReturnPolicy::FromCalls(calls) => calls.contains(&inst),
            OrdinaryReturnPolicy::FromContinuation => false,
        }
    }

    pub(super) fn for_inst(mut self, inst: Inst) -> Self {
        if let Some((_, target)) = self.targets.iter().find(|(site, _)| *site == inst) {
            self.target = *target;
        }
        if let Some((second, target)) = self.second_target
            && second == inst { self.target = target; }
        self
    }

    pub(super) fn emit(
        self,
        a: &mut Asm,
        data: &InstData,
        homes: &HashMap<Value, FramedHome>,
        constants: &HashMap<Value, u64>,
        frame: OrdinaryCallFrame<'_>,
    ) -> Result<Vec<CallReturn>, EmitError> {
        let OrdinaryCallFrame {
            outgoing,
            control_home,
            emit_epilogue,
        } = frame;
        let NativeValueArea::NativeStack(base) = outgoing else {
            return Err(EmitError::UnsupportedOp(0xFD));
        };
        let cold = a.label();
        let done = a.label();
        let overflow = ((data.args.len().saturating_sub(3) * 8 + 15) & !15) as i32;
        for (i, value) in data.args.iter().enumerate() {
            if let Some(&bits) = constants.get(value) {
                mov_imm64(a, RAX, bits as i64);
            } else {
                load_home(a, RAX, homes[value], 0);
            }
            store_home(a, FramedHome::Stack(base + i as u32), RAX, 0);
        }
        load_home(a, 7, control_home, 0);
        alu_r_imm(a, 7, 7, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(a, RAX, 7, 0);
        cmp_rr(a, RAX, 13);
        a.jcc(Cc::Ne, cold);
        for (address, byte) in [
            (self.flags.gc_requested, true),
            (self.flags.sigusr1_pending, true),
            (self.flags.posted_signal, false),
            (self.flags.native_errors, false),
        ] {
            mov_imm64(a, 11, address as i64);
            if byte {
                movzx_mem8(a, RAX, 11);
            } else {
                load_mem64_disp(a, RAX, 11, 0);
            }
            alu_r_imm(a, 7, RAX, 0);
            a.jcc(Cc::Ne, cold);
        }
        mov_imm64(a, 11, self.flags.signal_epoch as i64);
        load_mem64_disp(a, RAX, 11, 0);
        cmp_mem64_disp8(a, RAX, 7, 48);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            13,
            egcl_rt::exec_context::offsets::CARRIER_ID as i32,
        );
        cmp_mem64_disp8(a, RAX, 7, 40);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            13,
            egcl_rt::exec_context::offsets::YIELD_REQUESTED as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        mov_rr(a, 11, 7);
        alu_r_imm(a, 0, 11, 64);
        movzx_mem8(a, RAX, 11);
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(a, 11, 7, 24);
        movzx_mem8(a, RAX, 11);
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::Ne, cold);
        if !self.tail {
            load_mem64_disp(a, 11, 7, 8);
            a.extend_from_slice(&[0x41, 0x8b, 0x03, 0x8b, 0x57, 56]); // eax=*depth; edx=limit
            cmp_rr(a, RAX, SCRATCH);
            a.jcc(Cc::Ge, cold);
        }

        mov_imm64(a, 11, self.target.descriptor_slot as i64);
        load_mem64_disp(a, 11, 11, 0);
        alu_r_imm(a, 7, 11, 0);
        a.jcc(Cc::E, cold);
        for (offset, expected) in [
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, kind),
                ORDINARY_X64_LISP_KIND,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, version),
                ORDINARY_X64_LISP_VERSION,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, arity),
                data.args.len() as u64,
            ),
            (
                std::mem::offset_of!(OrdinaryEntryDescriptor, environment),
                ORDINARY_ENV_NONE,
            ),
        ] {
            load_mem64_disp(a, RAX, 11, offset as i32);
            mov_imm64(a, SCRATCH, expected as i64);
            if offset == std::mem::offset_of!(OrdinaryEntryDescriptor, arity) {
                let accepted = a.label();
                cmp_rr(a, RAX, SCRATCH);
                a.jcc(Cc::E, accepted);
                alu_r_imm(a, 7, RAX, -1);
                a.jcc(Cc::Ne, cold);
                a.bind(accepted);
            } else {
                cmp_rr(a, RAX, SCRATCH);
                a.jcc(Cc::Ne, cold);
            }
        }
        mov_imm64(a, SCRATCH, self.target.revision_address as i64);
        load_mem64_disp(a, RAX, SCRATCH, 0);
        cmp_mem64_disp8(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, revision) as u8,
        );
        a.jcc(Cc::Ne, cold);
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, owner) as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_start) as i32,
        );
        alu_r_imm(a, 7, RAX, 0);
        a.jcc(Cc::E, cold);
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_len) as i32,
        );
        alu_rr(a, 0x01, RAX, SCRATCH);
        unsigned_branch(a, false, cold); // reject range overflow
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, entry) as i32,
        );
        cmp_rr(a, RAX, SCRATCH);
        unsigned_branch(a, true, cold); // entry >= end
        load_mem64_disp(
            a,
            RAX,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, code_start) as i32,
        );
        cmp_rr(a, SCRATCH, RAX);
        unsigned_branch(a, false, cold); // entry < start
        load_mem64_disp(
            a,
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, stack_required) as i32,
        );
        alu_r_imm(a, 7, SCRATCH, 8);
        a.jcc(Cc::L, cold); // also rejects high-bit bounds
        if self.tail {
            // Ordinary RBP names the first saved word. Retiring this frame
            // restores entry RSP = RBP+8; no new return word is pushed.
            mov_rr(a, RAX, 5);
            alu_r_imm(a, 0, RAX, 8);
            unsigned_branch(a, false, cold);
        } else {
            mov_rr(a, RAX, 4);
            alu_r_imm(a, 5, RAX, overflow + 8);
            unsigned_branch(a, false, cold);
        }
        alu_rr(a, 0x29, RAX, SCRATCH);
        unsigned_branch(a, false, cold);
        load_mem64_disp(a, SCRATCH, 7, 88);
        cmp_rr(a, RAX, SCRATCH);
        unsigned_branch(a, true, cold);
        if !self.tail {
            load_mem64_disp(a, SCRATCH, 7, 8);
            a.extend_from_slice(&[0xff, 0x02]); // inc dword[rdx]
        }
        load_mem64_disp(
            a,
            11,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, entry) as i32,
        );
        let direct = if self.tail {
            // Every source was copied above before any incoming area is
            // overwritten. The handoff has no safepoint: the target takes
            // ownership before its first collecting boundary.
            if overflow != 0 {
                load_home(a, RAX, FramedHome::Stack(base + 3), 0);
                a.extend_from_slice(&[0x48, 0x89, 0x45, 0x10]); // [rbp+16]=arg4
            }
            for (index, reg) in [7, 6, 2].into_iter().take(data.args.len()).enumerate() {
                load_home(a, reg, FramedHome::Stack(base + index as u32), 0);
            }
            mov_imm64(a, 1, data.args.len() as i64);
            mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
            emit_epilogue(a);
            a.extend_from_slice(&[0x41, 0xff, 0xe3]); // jmp r11; original return PC
            None
        } else {
            if overflow != 0 {
                alu_r_imm(a, 5, 4, overflow);
                for index in 3..data.args.len() {
                    load_home(a, RAX, FramedHome::Stack(base + index as u32), overflow);
                    store_to_rsp(a, RAX, ((index - 3) * 8) as i32);
                }
            }
            for (index, reg) in [7, 6, 2].into_iter().take(data.args.len()).enumerate() {
                load_home(a, reg, FramedHome::Stack(base + index as u32), overflow);
            }
            mov_imm64(a, 1, data.args.len() as i64);
            mov_imm64(a, RAX, egcl_rt::value::NIL.0 as i64);
            a.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11, genuine ordinary ABI
            let direct = CallReturn {
                return_offset: a.here(),
                stack_adjust: overflow as u32,
            };
            load_home(a, 11, control_home, overflow);
            load_mem64_disp(a, 11, 11, 8);
            a.extend_from_slice(&[0x41, 0xff, 0x0b]); // DEC preserves CF and all result registers
            retire(a, overflow as u32);
            a.jmp(done);
            Some(direct)
        };

        a.bind(cold);
        alu_r_imm(a, 5, 4, 64);
        for (offset, bits) in [
            (0, self.target.symbol as u64),
            (8, data.args.len() as u64),
            (24, egcl_rt::value::NIL.0),
            (40, self.target.descriptor_slot),
            (48, self.target.revision_address),
            (56, 0),
        ] {
            mov_imm64(a, RAX, bits as i64);
            store_to_rsp(a, RAX, offset);
        }
        mov_rr(a, RAX, 4);
        alu_r_imm(a, 0, RAX, (base * 8 + 64) as i32);
        store_to_rsp(a, RAX, 16);
        load_home(a, RAX, control_home, 64);
        store_to_rsp(a, RAX, 32);
        mov_rr(a, 7, 4);
        mov_imm64(a, 11, self.cold as i64);
        a.extend_from_slice(&[0x41, 0xff, 0xd3]);
        let slow = CallReturn {
            return_offset: a.here(),
            stack_adjust: 64,
        };
        retire(a, 64);
        a.bind(done);
        Ok(direct.into_iter().chain(std::iter::once(slow)).collect())
    }
}
