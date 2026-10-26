// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bounded ordinary calls with explicit continuation and final-result policies.
use super::*;
use crate::t2::ir::{AuxData, Inst, InstData, Opcode, TypeBits, Value, ValueRepresentation};
use egcl_rt::call_table::{
    OrdinaryCallHeat, ORDINARY_ENV_NONE, OrdinaryEntryDescriptor,
};
use std::collections::HashMap;

#[derive(Clone, Copy)]
pub struct OrdinaryCallTarget {
    pub symbol: u32,
    /// Address of a slot populated only by checked ordinary publication.
    /// Descriptors are immutable and retained with their code through every
    /// active native segment. Publication validates ABI kind/version, owner,
    /// code bounds and stack requirement; callers check mutable admission.
    pub descriptor_slot: u64,
    pub revision_address: u64,
    /// Atomic pointer to retained profiling metadata; zero disables profiling.
    pub heat_slot: u64,
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
    Call(Inst),
    Continuation,
    Calls(&'a [Inst]),
}

/// Execution invariants proved by the owner of a compiled artifact.
/// Public standalone emission retains conservative defaults.
#[derive(Clone, Copy, Debug, Default)]
pub struct OrdinarySelfCallFacts {
    /// Every ingress establishes a live control belonging to R13; callbacks
    /// restore it before native resumption. The raw ordinary ABI alone does
    /// not establish this precondition (runtime adapters may have no control).
    pub trusted_control: bool,
    /// The artifact is its function's final tier, so its loop polls need not
    /// sample back-edge heat. Each poll then first tests the R13 execution's
    /// pending-work slot, exactly as an ordinary call admits, and skips the
    /// published runtime poll when nothing is armed (bliss-6m6ca).
    pub top_tier: bool,
}

#[derive(Clone, Copy)]
pub(super) struct OrdinaryCallEmission<'a> {
    target: OrdinaryCallTarget,
    targets: &'a [(Inst, OrdinaryCallTarget)],
    second_target: Option<(Inst, OrdinaryCallTarget)>,
    pub(super) return_policy: OrdinaryReturnPolicy<'a>,
    cold: u64,
    tail: bool,
    pub(super) guard: Option<OrdinaryGuardOptions>,
    pub(super) poll_veneer: Option<u64>,
    pub(super) self_calls: &'a [Inst],
    direct_self: bool,
    pub(super) self_facts: OrdinarySelfCallFacts,
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
                    | Opcode::ConstFloat
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
                Opcode::ConstFixnum | Opcode::ConstFloat | Opcode::ConstNil | Opcode::ConstT
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
            return_policy: if guard.is_some() { OrdinaryReturnPolicy::Continuation }
                else { OrdinaryReturnPolicy::Call(call_inst) },
            cold,
            // Under the bounded 0–4 signature, rounded capacity is 0 or 16.
            tail: allow_tail && (call.args.len() <= 3 || entry.arity == 4),
            guard,
            poll_veneer: None, self_calls: &[], direct_self: false, self_facts: OrdinarySelfCallFacts::default(),
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
        Opcode::ConstFixnum | Opcode::ConstFloat | Opcode::ConstNil | Opcode::ConstT
            | Opcode::ConstSymbol | Opcode::ConstChar);
    if insts[..first_position].iter().any(|&inst| !immediate(inst))
        || insts[first_position + 1..last_position].iter()
            .any(|&inst| !immediate(inst) && f.inst(inst).opcode != Opcode::ClearMv)
        || !crate::t2::opt_clear_mv::single_value_state(f, true).contains(&last)
    { return Err(bad()); }
    super::emit_framed_inner(f, 0, 0, 0, 0, 0, 0, 0, multiple_values, 0, 0,
        Some(entry.arity as u16), None, &[], 0, EnvironmentCalls::default(), None,
        Some(entry), Some(OrdinaryCallEmission {
            target: targets[0], targets: &[], second_target: Some((last, targets[1])),
            return_policy: OrdinaryReturnPolicy::Call(last), cold, tail: false, guard: None, poll_veneer: None, self_calls: &[], direct_self: false, self_facts: OrdinarySelfCallFacts::default(),
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
    emit_framed_ordinary_function_with_poll(f, entry, targets, flags, cold, guard, 0)
}

/// Also publish native-only root maps at loop and straight-line polls. The
/// veneer receives a null activation pointer and must capture/publish native
/// homes before collecting, yielding or delivering a nonlocal transfer.
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_ordinary_function_with_poll(
    f: &Function, entry: OrdinaryLispEntryOptions,
    targets: &[(Inst, OrdinaryCallTarget)], flags: OrdinaryCallFlags,
    cold: u64, guard: OrdinaryGuardOptions, poll_veneer: u64,
) -> Result<FramedCode, EmitError> {
    emit_framed_ordinary_function_with_self_calls(f, entry, targets, flags, cold, guard, poll_veneer, &[])
}

/// Emit source-proven self calls to this artifact's ordinary entry. The caller
/// must exclude NOTINLINE and lexically shadowed calls, and preserve the same
/// callable identity through cold admission and guard recovery. These sites
/// need no global binding lookup or invocation heat (the body is already T2).
#[allow(clippy::too_many_arguments)]
pub fn emit_framed_ordinary_function_with_self_calls(
    f: &Function, entry: OrdinaryLispEntryOptions,
    targets: &[(Inst, OrdinaryCallTarget)], flags: OrdinaryCallFlags,
    cold: u64, guard: OrdinaryGuardOptions, poll_veneer: u64, self_calls: &[Inst],
) -> Result<FramedCode, EmitError> {
    emit_framed_ordinary_function_with_self_call_facts(f, entry, targets, flags,
        cold, guard, poll_veneer, self_calls, OrdinarySelfCallFacts::default())
}

/// Emit using invariants established at every ingress by the artifact owner.
/// Facts apply only to source-proven self calls, never ordinary named calls.
#[allow(clippy::too_many_arguments)]
/// Copy a join's bare `return p` into each edge that reaches it, so a branch
/// that ends by calling returns the call's result directly.
///
/// The ordinary ABI forwards a callee's result descriptor only from a call
/// immediately followed by its return, and a return of a merged value needs
/// every path to have proved a single value. A tail call inside an IF reaches
/// its return through the join's block parameter, so it satisfied neither, and
/// the whole function took the mapped ABI: `(if (< n 2) (leaf n) (+ ...))`.
/// A `jump B(a)` becomes `return a`; a conditional edge into the join gets a
/// new block holding `return a`, as when an arm is just a variable:
/// `(if (listp x) x (list x))`. Every other instruction and frame state is
/// kept, so call sites and their targets are unchanged. Returns `None` when no
/// join qualifies.
///
/// Repeated until no join remains: rewriting one join turns a block that only
/// forwarded its parameter to it into a new `return p` join, as in a nested
/// `(if a (if b (call) x) y)` or `(and a b (call))`, whose arms reach the
/// return through two jumps.
fn duplicate_join_returns(f: &Function) -> Option<Function> {
    let mut rewritten: Option<Function> = None;
    // Each round removes the joins it rewrites; the bound only guards
    // against a malformed graph.
    for _ in 0..16 {
        match duplicate_join_returns_once(rewritten.as_ref().unwrap_or(f)) {
            Some(next) => rewritten = Some(next),
            None => break,
        }
    }
    rewritten
}

fn duplicate_join_returns_once(f: &Function) -> Option<Function> {
    let mut joins = Vec::new();
    for &block in f.block_order() {
        let data = f.block(block);
        if block == f.entry() || data.insts.len() != 1 { continue; }
        let ret = f.inst(data.insts[0]);
        let [returned] = ret.args[..] else { continue };
        if ret.opcode != Opcode::Return { continue; }
        // A returned block parameter takes each edge's argument. A value
        // defined elsewhere dominates the block, and so every predecessor:
        // a jump returns it directly (`(call x)` followed by an empty jump).
        let param = data.params.iter().position(|&p| p == returned);
        joins.push((block, param, returned, data.insts[0]));
    }
    let mut copy: Option<Function> = None;
    for (join, param, returned, ret) in joins {
        let source = copy.as_ref().unwrap_or(f);
        // Every (terminator, edge index) that enters the join. A join that
        // returns a dominating value is copied only into jumps: a branch
        // edge would gain a block identical to the join itself.
        let edges: Vec<(Inst, usize)> = source.preds(join).into_iter().filter_map(|pred| {
            source.terminator(pred)
        }).filter(|&term| param.is_some() || source.inst(term).opcode == Opcode::Jump)
        .flat_map(|term| {
            source.inst(term).targets.iter().enumerate()
                .filter(|(_, edge)| edge.block == join)
                .map(move |(index, _)| (term, index))
                .collect::<Vec<_>>()
        }).collect();
        if edges.is_empty() { continue; }
        let g = copy.get_or_insert_with(|| f.clone());
        for (term, index) in edges {
            let value = match param {
                Some(param) => g.inst(term).targets[index].args[param],
                None => returned,
            };
            if g.inst(term).opcode == Opcode::Jump {
                let source_pos = g.inst(ret).source_pos;
                let data = g.inst_mut(term);
                data.opcode = Opcode::Return;
                data.args = vec![value];
                data.targets.clear();
                data.source_pos = source_pos;
            } else {
                let block = g.make_block();
                let mut data = g.inst(ret).clone();
                data.args = vec![value];
                g.set_terminator(block, data);
                let edge = &mut g.inst_mut(term).targets[index];
                edge.block = block;
                edge.args.clear();
            }
        }
        if g.preds(join).is_empty() {
            g.remove_from_layout(join);
            if param.is_none() {
                // The verifier still checks the unlisted block: give its
                // orphaned return a parameter of its own to name, since the
                // returned value no longer dominates a block nothing reaches.
                let (ty, repr) = (g.value(returned).ty, g.value(returned).repr);
                let own = g.add_block_param(join, ty, repr);
                g.inst_mut(ret).args = vec![own];
            }
        }
    }
    copy
}

#[allow(clippy::too_many_arguments)]
pub fn emit_framed_ordinary_function_with_self_call_facts(
    f: &Function, entry: OrdinaryLispEntryOptions,
    targets: &[(Inst, OrdinaryCallTarget)], flags: OrdinaryCallFlags,
    cold: u64, guard: OrdinaryGuardOptions, poll_veneer: u64, self_calls: &[Inst],
    self_facts: OrdinarySelfCallFacts,
) -> Result<FramedCode, EmitError> {
    let declined = EmitError::Declined;
    let duplicated = duplicate_join_returns(f);
    let f = duplicated.as_ref().unwrap_or(f);
    if f.is_variadic() { return Err(declined("variadic")); }
    if !f.osr_entries.is_empty() { return Err(declined("OSR entries")); }
    if !f.handler_cases.is_empty() { return Err(declined("handler cases")); }
    if entry.arity > u16::MAX as usize || entry.arity_error_veneer == 0
        || f.block(f.entry()).params.len() != entry.arity
    { return Err(declined("entry arity")); }
    if [cold, guard.veneer, guard.multiple_values, flags.gc_requested,
        flags.sigusr1_pending, flags.posted_signal, flags.native_errors,
        flags.signal_epoch].contains(&0)
    { return Err(declined("missing veneer or flag address")); }
    if crate::t2::verify::verify(f).is_err() { return Err(declined("IR verification")); }
    // Ordinary calls retain the same authoritative native homes, precise cold
    // reconstruction and GC maps as the mapped backend. Admit raw fixnums only
    // through that shared validator: speculative type annotations alone never
    // authorize a pure unbox, and entry/call/return boundaries stay tagged.
    if !super::native_representations_supported(f, NativeRootPublication::PublishedHomes,
        NativeCallStorage::NativeStack, true)
    {
        return Err(declined("unsupported native representation contract"));
    }
    if std::env::var_os("EGCL_DISABLE_RAW_ORDINARY_ABI").is_some()
        && (0..f.num_values()).any(|v|
            f.value(Value(v as u32)).repr == ValueRepresentation::UnboxedFixnum)
    {
        return Err(declined("raw ordinary ABI disabled"));
    }
    if f.frame_states.iter().any(|(_, state)| {
        // Retained self expansion shares one body's scope-free recovery
        // protocol. Cross-function logical frames need separate admission.
        state.scopes.first().is_none_or(|outer|
            state.scopes.iter().any(|scope| scope.function != outer.function))
    }) { return Err(declined("frame state spans inlined functions")); }
    let single = crate::t2::opt_clear_mv::single_value_state(f, true);
    let mut returns = Vec::new();
    let mut call_count = 0;
    for &block in f.block_order() {
        let insts = &f.block(block).insts;
        for (position, &inst) in insts.iter().enumerate() {
            let data = f.inst(inst);
            match data.opcode {
                Opcode::Call => {
                    if data.args.iter().chain(&data.results).any(|&value|
                        f.value(value).repr != ValueRepresentation::Tagged) {
                        return Err(declined("raw ordinary call boundary"));
                    }
                    call_count += 1;
                    let mut matches = targets.iter().filter(|(site, _)| *site == inst);
                    let (_, target) = matches.next()
                        .ok_or(declined("call site without a call cell"))?;
                    if matches.next().is_some() || target.descriptor_slot == 0
                        || target.revision_address == 0 || data.args.len() > u16::MAX as usize
                        || data.results.len() > 1 || !data.flags.call || !data.flags.safepoint
                        || !matches!(data.aux, AuxData::CallTarget(symbol) if symbol == target.symbol)
                    { return Err(declined("call site shape")); }
                    if position + 2 == insts.len() {
                        let ret = f.inst(insts[position + 1]);
                        if ret.opcode == Opcode::Return && ret.args == data.results {
                            returns.push(inst);
                        }
                    }
                }
                Opcode::Return => {
                    if data.args.iter().any(|&value| f.value(value).repr != ValueRepresentation::Tagged) {
                        return Err(declined("raw ordinary return"));
                    }
                    if data.args.len() != 1 || (!single.contains(&inst)
                        && (position == 0 || !returns.contains(&insts[position - 1])))
                    { return Err(declined("return of possibly multiple values")); }
                }
                Opcode::FixnumMin | Opcode::FixnumMax => {
                    if data.args.len() != 2 || data.results.len() != 1
                        || data.args.iter().chain(&data.results).any(|&value|
                            f.value(value).repr != ValueRepresentation::Tagged)
                        || f.value(data.results[0]).ty.bits != TypeBits::FIXNUM
                        || data.flags != (crate::t2::ir::InstFlags {
                            guard: true, effectful: true, ..Default::default() })
                        || data.frame_state.is_none()
                    { return Err(declined("guarded fixnum selection shape")); }
                }
                Opcode::PackedF64Ref => {
                    // Shared representation validation and the IR verifier
                    // establish exact packed/generic-double layouts. Retain
                    // ordered type/bounds recovery before exposing a raw word.
                    if data.args.len() != 2 || data.results.len() != 1
                        || data.args.iter().any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
                        || f.value(data.results[0]).repr != ValueRepresentation::UnboxedF64
                        || f.value(data.results[0]).ty.bits != TypeBits::DOUBLE_FLOAT
                        || data.flags != (crate::t2::ir::InstFlags {
                            guard: true, effectful: true, ..Default::default() })
                        || data.frame_state.is_none()
                    { return Err(declined("guarded packed-double read shape")); }
                }
                // Raw double operations. Their shapes, including the deopt
                // states of sqrt and division (bliss-lhy89), are checked by
                // the raw representation validator.
                Opcode::FloatAdd | Opcode::FloatSub | Opcode::FloatMul | Opcode::FloatDiv
                | Opcode::FloatSqrt | Opcode::FloatNeg
                | Opcode::FloatCmpEq | Opcode::FloatCmpLt | Opcode::FloatCmpLe
                | Opcode::FloatCmpGt | Opcode::FloatCmpGe
                    if data.args.iter().all(|&v| f.value(v).repr == ValueRepresentation::UnboxedF64) => {}
                Opcode::ConstDouble | Opcode::BoxFloat | Opcode::BoxFixnum => {}
                Opcode::UnboxFixnum if !data.flags.guard => {
                    if data.args.len() != 1 || !crate::t2::unbox_fixnum::proven_tagged_fixnum(f, data.args[0]) {
                        return Err(declined("unproven pure fixnum unbox"));
                    }
                }
                Opcode::UnboxFixnum | Opcode::IntegerLowBits
                | Opcode::VecRef | Opcode::VecSet | Opcode::ArrayReadDiscarded => {
                    if data.frame_state.is_none() {
                        return Err(declined("guarded numeric or array operation without state"));
                    }
                }
                Opcode::PackedF64P => {}
                Opcode::Guard | Opcode::FixnumAdd | Opcode::FixnumSub | Opcode::FixnumMul
                | Opcode::FixnumNeg | Opcode::FixnumShl | Opcode::FixnumShr
                | Opcode::FloatAdd | Opcode::FloatSub | Opcode::FloatMul
                | Opcode::FloatCmpEq | Opcode::FloatCmpLt | Opcode::FloatCmpLe
                | Opcode::FloatCmpGt | Opcode::FloatCmpGe | Opcode::UnboxFloat => {
                    if data.frame_state.is_none() {
                        // Name the operation: an unguarded one is a shape
                        // this emitter does not model, not a missing state.
                        return Err(EmitError::UnsupportedOp(super::op_tag(data.opcode)));
                    }
                }
                Opcode::ConstFixnum | Opcode::ConstFloat | Opcode::ConstNil | Opcode::ConstT
                | Opcode::ConstSymbol | Opcode::ConstChar | Opcode::ClearMv
                | Opcode::FixnumCmpEq | Opcode::FixnumCmpLt | Opcode::FixnumCmpLe
                | Opcode::FixnumCmpGt | Opcode::FixnumCmpGe | Opcode::GenericEq
                | Opcode::TypeCheck | Opcode::LogAnd | Opcode::LogOr | Opcode::LogXor
                | Opcode::LogNot | Opcode::Jump | Opcode::Brif
                | Opcode::Car | Opcode::Cdr | Opcode::ConstHeapObj
                | Opcode::StringByteLength | Opcode::StringAsciiCharAt | Opcode::StringCharOrNil
                | Opcode::MemoryFence | Opcode::TryIntegerShr => {}
                _ => return Err(EmitError::UnsupportedOp(super::op_tag(data.opcode))),
            }
        }
    }
    if targets.len() != call_count { return Err(declined("call cells do not match calls")); }
    if self_calls.iter().any(|site| !targets.iter().any(|(inst, target)| {
        if inst != site { return false; }
        let data = f.inst(*inst);
        data.args.len() == entry.arity && data.frame_state.is_some_and(|state| {
            let scopes = &f.frame_states.get(state).scopes;
            !scopes.is_empty() && scopes.iter().all(|scope| scope.function == target.symbol)
        })
    })) { return Err(declined("self call shape")); }

    // Calls contain the complete lifecycle poll. A cycle without a call would
    // otherwise suppress signals, collection and fiber yielding indefinitely.
    // Remove call-containing blocks and require the remaining graph acyclic.
    let mut pending: std::collections::HashSet<_> = f.block_order().iter().copied()
        .filter(|&block| !f.block(block).insts.iter().any(|&inst| f.inst(inst).opcode == Opcode::Call))
        .collect();
    while poll_veneer == 0 && !pending.is_empty() {
        let ready = pending.iter().copied().find(|&block|
            !pending.iter().any(|&predecessor| f.terminator(predecessor).is_some_and(|inst|
                f.inst(inst).targets.iter().any(|target| target.block == block))));
        let Some(block) = ready else { return Err(declined("call-free loop without a poll")); };
        pending.remove(&block);
    }
    // A call-free guarded function still needs native homes and guard control.
    let unused = OrdinaryCallTarget { symbol: 0, descriptor_slot: 0, revision_address: 0 , heat_slot: 0};
    super::emit_framed_inner(f, 0, 0, 0, 0, 0, 0, 0, guard.multiple_values, 0, 0,
        Some(entry.arity as u16), None, &[], 0, EnvironmentCalls::default(), None,
        Some(entry), Some(OrdinaryCallEmission {
            target: unused, targets, second_target: None,
            return_policy: OrdinaryReturnPolicy::Calls(&returns), cold,
            tail: false, guard: Some(guard), poll_veneer: (poll_veneer != 0).then_some(poll_veneer),
            self_calls, direct_self: false, self_facts,
        }))
}

/// Reserve ordinary stack space without jumping over a guard page.
pub(super) fn reserve_ordinary_stack(a: &mut Asm, mut bytes: usize) {
    while bytes != 0 {
        let step = bytes.min(4096);
        alu_r_imm(a, 5, 4, step as i32);
        a.extend_from_slice(&[0xf6, 0x04, 0x24, 0x00]); // test byte [rsp], 0
        bytes -= step;
    }
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

/// Compare a pending flag at its declared width without a scratch-register
/// load. All admission observations still occur before the ordinary call.
fn compare_zero_memory(a: &mut Asm, base: u8, displacement: u8, byte: bool) {
    debug_assert_ne!(base & 7, 4, "rsp/r12 requires SIB addressing");
    if !byte || base >= 8 { a.push(if byte { 0x40 } else { 0x48 } | (base >> 3)); }
    a.extend_from_slice(&[if byte { 0x80 } else { 0x83 },
        0x78 | (base & 7), displacement, 0]); // CMP r/m8 or r/m64, imm8=0
}

/// The frame storage and its matching callee-save retirement sequence.
pub(super) struct OrdinaryCallFrame<'a> {
    pub function: &'a Function,
    pub inference: &'a crate::t2::infer::InferenceResult,
    pub outgoing: NativeValueArea,
    pub control_home: FramedHome,
    /// Must-state at this call, after actual return normalization and resets.
    /// Unknown tuples still require the runtime admission check.
    pub mv_known_empty: bool,
    pub self_entry: egcl_rt::asm::Label,
    pub self_prefix: Option<&'a ordinary_prefix::Prefix>,
    pub self_specializations: &'a [ordinary_prefix::Specialization],
    pub self_stack_fixups: &'a mut Vec<usize>,
    pub emit_epilogue: &'a dyn Fn(&mut Asm),
}

impl OrdinaryCallEmission<'_> {
    pub(super) fn returns_at(self, inst: Inst) -> bool {
        match self.return_policy {
            OrdinaryReturnPolicy::Call(call) => call == inst,
            OrdinaryReturnPolicy::Calls(calls) => calls.contains(&inst),
            OrdinaryReturnPolicy::Continuation => false,
        }
    }

    pub(super) fn for_inst(mut self, inst: Inst) -> Self {
        self.direct_self = self.self_calls.contains(&inst);
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
            function,
            inference,
            outgoing,
            control_home,
            mv_known_empty,
            self_entry,
            self_prefix,
            self_specializations,
            self_stack_fixups,
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
        if !(self.direct_self && self.self_facts.trusted_control) {
            alu_r_imm(a, 7, 7, 0);
            a.jcc(Cc::E, cold);
            cmp_mem64_disp8(a, 13, 7, 0);
            a.jcc(Cc::Ne, cold);
        }
        load_mem64_disp(a, 11, 13, egcl_rt::exec_context::offsets::POLL_SLOT as i32);
        compare_zero_memory(a, 11, 0, false);
        a.jcc(Cc::Ne, cold);
        if !mv_known_empty {
            load_mem64_disp(a, 11, 7, 24);
            compare_zero_memory(a, 11, 0, true);
            a.jcc(Cc::Ne, cold);
        }
        // Ordinary machine frames are bounded by their exact stack budget.
        // The legacy depth counter bounds Rust/native reentry only.

        if self.direct_self {
            // Source identity does not prove the next argument contract. Check
            // it before even a pure return shortcut: those shortcuts also
            // bypass the shared checked entry. A miss dispatches the retained
            // definition through that entry, preserving its TYPE-ERROR.
            let parameters = &function.block(function.entry()).params;
            if parameters.len() != data.args.len() {
                return Err(EmitError::UnsupportedOp(0xFA));
            }
            for (index, (&parameter, &argument)) in parameters.iter().zip(&data.args).enumerate() {
                if !function.is_entry_param_checked(parameter) { continue; }
                let expected = function.value(parameter).ty;
                let actual = inference.ty(argument);
                let tag_proven = !actual.bits.is_bottom() && expected.bits.contains(actual.bits);
                let actual_range = if tag_proven { actual.range } else { None };
                let range_proven = expected.range.is_none_or(|range| actual_range.is_some_and(
                    |got| got.lo >= range.lo && got.hi <= range.hi));
                if tag_proven && range_proven { continue; }
                load_home(a, RAX, FramedHome::Stack(base + index as u32), 0);
                if expected.bits == TypeBits::FIXNUM {
                    if !tag_proven { guard_fixnum(a, RAX, cold); }
                    if let Some(range) = expected.range {
                        // Declared fixnum intervals must fit the tagged word.
                        if range.lo < FIXNUM_MIN || range.hi > FIXNUM_MAX || range.lo > range.hi {
                            return Err(EmitError::UnsupportedOp(0xFA));
                        }
                        if actual_range.is_none_or(|got| got.lo < range.lo) {
                            mov_imm64(a, SCRATCH, range.lo << 3);
                            cmp_rr(a, RAX, SCRATCH);
                            a.jcc(Cc::L, cold);
                        }
                        if actual_range.is_none_or(|got| got.hi > range.hi) {
                            mov_imm64(a, SCRATCH, range.hi << 3);
                            cmp_rr(a, RAX, SCRATCH);
                            a.jcc(Cc::G, cold);
                        }
                    }
                } else if expected.bits == TypeBits::SINGLE_FLOAT && expected.range.is_none() {
                    guard_single_float(a, RAX, cold);
                } else {
                    return Err(EmitError::UnsupportedOp(0xFA));
                }
            }
        }

        if self.direct_self && !self.tail {
            if let Some(prefix) = self_prefix {
                // All sources were snapshotted before any ABI register changes.
                // The pure prefix cannot collect or deopt. On success there is
                // no child frame to admit; pending polls already went cold above.
                for (index, reg) in [7, 6, 2].into_iter().take(data.args.len()).enumerate() {
                    load_home(a, reg, FramedHome::Stack(base + index as u32), 0);
                }
                ordinary_prefix::emit_selected(a, prefix, Some(done));
                ordinary_prefix::emit_specializations(a, self_specializations, done);
                // Failed prefix tests leave argument registers live, so restore
                // the control pointer used by normal child-stack admission.
                load_home(a, 7, control_home, 0);
            }
        }

        if self.direct_self {
            // Patched from the final published bound, including guard/cold
            // temporary storage and argument moves, after maps are finalized.
            let at = a.here();
            mov_imm64(a, SCRATCH, 0);
            self_stack_fixups.push(at + 2);
        } else {
        mov_imm64(a, 11, self.target.descriptor_slot as i64);
        load_mem64_disp(a, 11, 11, 0);
        alu_r_imm(a, 7, 11, 0);
        a.jcc(Cc::E, cold);
        for (offset, expected) in [
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
            SCRATCH,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, stack_required) as i32,
        );
        }
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
        cmp_mem64_disp8(a, RAX, 7, 88);
        unsigned_branch(a, true, cold);
        if !self.direct_self && self.target.heat_slot != 0 {
            let no_heat = a.label();
            mov_imm64(a, 10, self.target.heat_slot as i64);
            load_mem64_disp(a, 10, 10, 0);
            alu_r_imm(a, 7, 10, 0);
            a.jcc(Cc::E, no_heat);
            load_mem64_disp(a, RAX, 10, std::mem::offset_of!(OrdinaryCallHeat, descriptor) as i32);
            cmp_rr(a, RAX, 11);
            a.jcc(Cc::Ne, no_heat);
            // No collecting operation separates descriptor validation from
            // the atomic increment. Its retained owner roots both counters.
            for (counter, threshold, invocation) in [
                (std::mem::offset_of!(OrdinaryCallHeat, invocation_counter),
                 std::mem::offset_of!(OrdinaryCallHeat, invocation_threshold), true),
                (std::mem::offset_of!(OrdinaryCallHeat, back_edge_counter),
                 std::mem::offset_of!(OrdinaryCallHeat, back_edge_threshold), false),
            ] {
                let disabled = a.label();
                a.extend_from_slice(&[0x41, 0x8b, 0x52, threshold as u8]); // edx=threshold
                alu_r_imm(a, 7, SCRATCH, 0);
                a.jcc(Cc::E, disabled);
                if invocation { alu_r_imm(a, 5, SCRATCH, 1); }
                load_mem64_disp(a, RAX, 10, counter as i32);
                a.extend_from_slice(&[0x8b, 0x00]); // eax=*counter
                cmp_rr(a, SCRATCH, RAX);
                unsigned_branch(a, true, cold); // threshold <= count
                a.bind(disabled);
            }
            load_mem64_disp(a, RAX, 10, std::mem::offset_of!(OrdinaryCallHeat, invocation_counter) as i32);
            alu_r_imm(a, 7, RAX, 0);
            a.jcc(Cc::E, no_heat);
            a.extend_from_slice(&[0xf0, 0xff, 0x00]); // lock inc dword [rax]
            a.bind(no_heat);
        }
        if !self.direct_self {
        load_mem64_disp(
            a,
            11,
            11,
            std::mem::offset_of!(OrdinaryEntryDescriptor, entry) as i32,
        );
        }
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
                reserve_ordinary_stack(a, overflow as usize);
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
            if self.direct_self {
                a.call(self_entry);
            } else {
                a.extend_from_slice(&[0x41, 0xff, 0xd3]); // call r11, genuine ordinary ABI
            }
            let direct = CallReturn {
                return_offset: a.here(),
                stack_adjust: overflow as u32,
            };
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
            (56, u64::from(self.direct_self)),
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

#[cfg(test)]
mod join_return_tests {
    use super::*;
    use crate::t2::ir::{BlockCall, IRType, InstFlags};

    const TAGGED: (IRType, ValueRepresentation) = (IRType::TOP, ValueRepresentation::Tagged);

    fn inst(opcode: Opcode, args: Vec<Value>, targets: Vec<BlockCall>) -> InstData {
        InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags {
                call: opcode == Opcode::Call,
                ..InstFlags::default()
            },
            targets,
            frame_state: None,
            source_pos: 0,
        }
    }

    fn edge(block: crate::t2::ir::Block, args: Vec<Value>) -> BlockCall {
        BlockCall { block, args }
    }

    /// `(and (stringp s) (plusp (length s)) (char s 0))`: a nested branch whose
    /// arms reach a join returning its parameter, one arm by calling.
    #[test]
    fn a_join_return_is_copied_into_every_arm() {
        let mut f = Function::new("first-char");
        let entry = f.entry();
        let s = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (inner, call_arm, nil_arm, join, outer_nil) =
            (f.make_block(), f.make_block(), f.make_block(), f.make_block(), f.make_block());
        let (_, nil) = f.push_inst(entry, inst(Opcode::ConstNil, vec![], vec![]), &[TAGGED]);
        let (_, test) = f.push_inst(entry, inst(Opcode::Call, vec![s], vec![]), &[TAGGED]);
        f.set_terminator(entry, inst(Opcode::Brif, vec![test[0]],
            vec![edge(inner, vec![]), edge(outer_nil, vec![])]));
        let (_, inner_test) = f.push_inst(inner, inst(Opcode::Call, vec![s], vec![]), &[TAGGED]);
        f.set_terminator(inner, inst(Opcode::Brif, vec![inner_test[0]],
            vec![edge(call_arm, vec![]), edge(nil_arm, vec![])]));
        let (call, result) = f.push_inst(call_arm, inst(Opcode::Call, vec![s], vec![]), &[TAGGED]);
        f.set_terminator(call_arm, inst(Opcode::Jump, vec![], vec![edge(join, vec![result[0]])]));
        f.set_terminator(nil_arm, inst(Opcode::Jump, vec![], vec![edge(join, vec![nil[0]])]));
        let merged = f.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(join, inst(Opcode::Return, vec![merged], vec![]));
        f.set_terminator(outer_nil, inst(Opcode::Return, vec![nil[0]], vec![]));

        let g = duplicate_join_returns(&f).expect("the join qualifies");
        assert!(!g.block_order().contains(&join), "the emptied join leaves the layout");
        let tail = |block| g.inst(g.terminator(block).unwrap()).clone();
        assert_eq!(tail(call_arm).opcode, Opcode::Return);
        assert_eq!(tail(call_arm).args, result);
        assert_eq!(*g.block(call_arm).insts.first().unwrap(), call, "the call is kept");
        assert_eq!(tail(nil_arm).opcode, Opcode::Return);
        assert_eq!(tail(nil_arm).args, nil);
        assert!(crate::t2::verify::verify(&g).is_ok());
    }

    /// An arm that is just a variable leaves the branch entering the join
    /// directly; that edge gets its own returning block.
    #[test]
    fn a_conditional_edge_into_a_join_gets_its_own_return() {
        let mut f = Function::new("ensure-list");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (call_arm, join) = (f.make_block(), f.make_block());
        let (_, test) = f.push_inst(entry, inst(Opcode::Call, vec![x], vec![]), &[TAGGED]);
        f.set_terminator(entry, inst(Opcode::Brif, vec![test[0]],
            vec![edge(join, vec![x]), edge(call_arm, vec![])]));
        let (_, list) = f.push_inst(call_arm, inst(Opcode::Call, vec![x], vec![]), &[TAGGED]);
        f.set_terminator(call_arm, inst(Opcode::Jump, vec![], vec![edge(join, vec![list[0]])]));
        let merged = f.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(join, inst(Opcode::Return, vec![merged], vec![]));

        let g = duplicate_join_returns(&f).expect("the join qualifies");
        let branch = g.inst(g.terminator(entry).unwrap());
        let taken = branch.targets[0].block;
        assert!(branch.targets[0].args.is_empty());
        assert_ne!(taken, join);
        assert_eq!(g.inst(g.terminator(taken).unwrap()).args, vec![x]);
        assert!(!g.block_order().contains(&join));
        assert!(crate::t2::verify::verify(&g).is_ok());
    }

    /// `(and a b (call))`: the call's arm jumps to an inner join that only
    /// forwards its parameter to the returning join. Both levels are copied,
    /// so the call is followed directly by its return.
    #[test]
    fn a_join_reached_through_a_forwarding_join_is_copied_too() {
        let mut f = Function::new("and-call");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (second, call_arm, nil_arm, inner_join, outer_nil, join) = (f.make_block(),
            f.make_block(), f.make_block(), f.make_block(), f.make_block(), f.make_block());
        let (_, nil) = f.push_inst(entry, inst(Opcode::ConstNil, vec![], vec![]), &[TAGGED]);
        f.set_terminator(entry, inst(Opcode::Brif, vec![x],
            vec![edge(second, vec![]), edge(outer_nil, vec![])]));
        f.set_terminator(second, inst(Opcode::Brif, vec![x],
            vec![edge(call_arm, vec![]), edge(nil_arm, vec![])]));
        let (call, result) = f.push_inst(call_arm, inst(Opcode::Call, vec![x], vec![]), &[TAGGED]);
        f.set_terminator(call_arm, inst(Opcode::Jump, vec![], vec![edge(inner_join, vec![result[0]])]));
        f.set_terminator(nil_arm, inst(Opcode::Jump, vec![], vec![edge(inner_join, vec![nil[0]])]));
        let forwarded = f.add_block_param(inner_join, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(inner_join, inst(Opcode::Jump, vec![], vec![edge(join, vec![forwarded])]));
        f.set_terminator(outer_nil, inst(Opcode::Jump, vec![], vec![edge(join, vec![nil[0]])]));
        let merged = f.add_block_param(join, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(join, inst(Opcode::Return, vec![merged], vec![]));

        let g = duplicate_join_returns(&f).expect("the joins qualify");
        assert!(!g.block_order().contains(&join));
        assert!(!g.block_order().contains(&inner_join));
        let tail = |block| g.inst(g.terminator(block).unwrap()).clone();
        assert_eq!(tail(call_arm).opcode, Opcode::Return);
        assert_eq!(tail(call_arm).args, result);
        assert_eq!(*g.block(call_arm).insts.first().unwrap(), call);
        assert_eq!(tail(outer_nil).opcode, Opcode::Return);
        assert!(crate::t2::verify::verify(&g).is_ok());
    }

    /// A call whose block jumps to a block returning the call's own result
    /// (not a parameter) returns it directly. A branch into that block is left
    /// alone rather than given an identical copy.
    #[test]
    fn a_jump_to_a_return_of_a_dominating_value_returns_directly() {
        let mut f = Function::new("flet-call");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let ret_block = f.make_block();
        let (call, result) = f.push_inst(entry, inst(Opcode::Call, vec![x], vec![]), &[TAGGED]);
        f.set_terminator(entry, inst(Opcode::Jump, vec![], vec![edge(ret_block, vec![])]));
        f.set_terminator(ret_block, inst(Opcode::Return, vec![result[0]], vec![]));

        let g = duplicate_join_returns(&f).expect("the jump qualifies");
        let tail = g.inst(g.terminator(entry).unwrap()).clone();
        assert_eq!(tail.opcode, Opcode::Return);
        assert_eq!(tail.args, result);
        assert_eq!(*g.block(entry).insts.first().unwrap(), call);
        assert!(!g.block_order().contains(&ret_block));
        assert_eq!(crate::t2::verify::verify(&g).map_err(|e| format!("{e:?}")), Ok(()));

        let mut branching = Function::new("branch-to-return");
        let entry = branching.entry();
        let x = branching.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let (ret_block, other) = (branching.make_block(), branching.make_block());
        let (_, result) =
            branching.push_inst(entry, inst(Opcode::Call, vec![x], vec![]), &[TAGGED]);
        branching.set_terminator(entry, inst(Opcode::Brif, vec![x],
            vec![edge(ret_block, vec![]), edge(other, vec![])]));
        branching.set_terminator(ret_block, inst(Opcode::Return, vec![result[0]], vec![]));
        branching.set_terminator(other, inst(Opcode::Return, vec![x], vec![]));
        assert!(duplicate_join_returns(&branching).is_none());
    }

    #[test]
    fn a_function_without_a_bare_join_return_is_left_alone() {
        let mut f = Function::new("leaf");
        let entry = f.entry();
        let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        f.set_terminator(entry, inst(Opcode::Return, vec![x], vec![]));
        assert!(duplicate_join_returns(&f).is_none());
    }
}
