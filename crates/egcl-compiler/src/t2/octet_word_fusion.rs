// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Version exact integer-to-octet counted loops before scalar type peeling.
//!
//! The kernel is a success-only bounded operation. Its two results establish
//! the final index/end and ASH64 value; nothing assumes that deopt-only locals
//! are dead. Unreconstructible escaping temporaries decline transactionally.

use super::builtin_binding::{self, Binding};
use super::frame_state::{FrameState, ValueSource};
use super::inlining::{InlineOptions, InlinePolicy};
use super::ir::{
    AuxData, Block, BlockCall, BoundedPollScope, Function, IRType, Inst, InstData, InstFlags,
    Opcode, TypeBits, Value, ValueDef, ValueRepresentation as Repr,
};
use super::opt_licm::{NaturalLoop, detect_loops};
use super::region_clone::{clone_region, repair_outside_uses};
use std::collections::{HashMap, HashSet};

/// Local lexical scopes can be reconstructed at an explicit poll from the
/// exact original OSR chain. Escaping targets, inherited records, handlers,
/// cleanups and dynamic bindings need a broader native transfer protocol.
pub(crate) fn bounded_poll_scopes_supported(scopes: &[crate::control_scope::ControlScope]) -> bool {
    use crate::control_scope::{Ownership, ScopeKind};
    scopes.iter().all(|scope| {
        scope.ownership == Ownership::Local
            && matches!(
                scope.kind,
                ScopeKind::Block {
                    register: false,
                    ..
                } | ScopeKind::Tagbody { .. }
                    | ScopeKind::LexicalEnvironment
            )
    })
}

#[derive(Default, Debug)]
pub struct OctetWordStats {
    pub loops: usize,
    pub declined: Vec<String>,
}

struct Pattern {
    header: Block,
    body: Vec<Block>,
    preheader: Block,
    entry: Vec<Value>,
    exit: BlockCall,
    index: Value,
    lane: Value,
    end: Value,
    array: Value,
    increment: Value,
    shifted: Value,
    prefix: Vec<Inst>,
    constants: Vec<Inst>,
    aliases: HashMap<Value, Value>,
    post_aliases: HashMap<Value, Value>,
    bindings: Vec<Binding>,
    state: FrameState,
    poll_scope: BoundedPollScope,
}

fn alias(mut value: Value, aliases: &HashMap<Value, Value>) -> Value {
    for _ in 0..=aliases.len() {
        let Some(&next) = aliases.get(&value) else {
            break;
        };
        if next == value {
            break;
        }
        value = next;
    }
    value
}

fn result(f: &Function, value: Value) -> Option<Inst> {
    match f.value(value).def {
        ValueDef::Result { inst, num: 0 } => Some(inst),
        _ => None,
    }
}

fn literal(f: &Function, value: Value, aliases: &HashMap<Value, Value>) -> Option<i64> {
    let d = f.inst(result(f, alias(value, aliases))?);
    match (d.opcode, &d.aux) {
        (Opcode::ConstFixnum, AuxData::FixnumImm(n)) => Some(*n),
        _ => None,
    }
}

fn bcp(f: &Function, d: &InstData, options: &InlineOptions) -> Option<u32> {
    let state = f.frame_states.get(d.frame_state?);
    let [scope] = state.scopes.as_slice() else {
        return None;
    };
    (options.policy_at(scope.bcp) != InlinePolicy::NotInline).then_some(scope.bcp)
}

fn canonical(f: &Function, inst: Inst, name: &str, options: &InlineOptions) -> Option<Binding> {
    let d = f.inst(inst);
    if d.opcode != Opcode::Call
        || d.results.len() != 1
        || !d.targets.is_empty()
        || d.args
            .iter()
            .chain(&d.results)
            .any(|&v| f.value(v).repr != Repr::Tagged)
    {
        return None;
    }
    bcp(f, d, options)?;
    let AuxData::CallTarget(symbol) = d.aux else {
        return None;
    };
    if egcl_rt::symbols::registry_key(symbol).as_deref() != Some(name)
        || egcl_rt::symbols::find_index(name) != Some(symbol)
    {
        return None;
    }
    let binding = builtin_binding::snapshot(symbol)?;
    // Only the implementation setter uses the existing mutable-binding guard.
    if name != "EGCL::SET-AREF" && binding != Binding::FixedStandard {
        return None;
    }
    Some(binding)
}

fn defining_block(f: &Function, v: Value) -> Option<Block> {
    match f.value(v).def {
        ValueDef::Param { block, .. } => Some(block),
        ValueDef::Result { inst, .. } => f
            .block_order()
            .iter()
            .copied()
            .find(|&b| f.block(b).insts.contains(&inst)),
    }
}

fn coalescible_mv_reset(d: &InstData) -> bool {
    d.opcode == Opcode::ClearMv
        && d.args.is_empty()
        && d.results.is_empty()
        && d.targets.is_empty()
        && matches!(d.aux, AuxData::None)
        && d.flags.effectful
        && !d.flags.guard
        && !d.flags.terminator
}

fn infallible_constant(f: &Function, d: &InstData) -> bool {
    d.results.len() == 1
        && d.args.is_empty()
        && d.targets.is_empty()
        && d.frame_state.is_none()
        && d.flags == InstFlags::default()
        && f.value(d.results[0]).repr == Repr::Tagged
        && match (d.opcode, &d.aux) {
            (Opcode::ConstFixnum, AuxData::FixnumImm(value)) => {
                (egcl_rt::bignum::FIXNUM_MIN..=egcl_rt::bignum::FIXNUM_MAX).contains(value)
            }
            (Opcode::ConstNil | Opcode::ConstT, AuxData::None) => true,
            _ => false,
        }
}

fn simple_guard(d: &InstData) -> bool {
    d.opcode == Opcode::Guard
        && d.args.len() == 1
        && d.results.len() <= 1
        && matches!(d.aux, AuxData::TypeTag(ty) if ty.bits == TypeBits::FIXNUM && ty.range.is_none() && ty.class_id.is_none())
        && d.flags.guard
        && d.frame_state.is_some()
}

fn recognize(
    f: &Function,
    lp: &NaturalLoop,
    options: &InlineOptions,
) -> Result<Pattern, &'static str> {
    if lp
        .body
        .iter()
        .map(|&b| f.block(b).insts.len())
        .sum::<usize>()
        > 64
    {
        return Err("loop too large");
    }
    if lp
        .body
        .iter()
        .any(|&b| b != lp.header && f.preds(b).iter().any(|p| !lp.body.contains(p)))
    {
        return Err("secondary loop entry");
    }
    let outside: Vec<_> = f
        .preds(lp.header)
        .into_iter()
        .filter(|b| !lp.body.contains(b))
        .collect();
    let [preheader] = outside.as_slice() else {
        return Err("ordinary entry is not unique");
    };
    // A previous fusion deliberately leaves a Jump stub for scalar peeling.
    // Recognize its success-only kernel predecessor rather than fusing the
    // same checked fallback again on a later optimization invocation.
    if f.preds(*preheader).iter().any(|&pred| {
        f.terminator(pred).is_some_and(|inst| {
            let d = f.inst(inst);
            d.opcode == Opcode::Brif
                && d.targets.len() == 2
                && d.targets[1].block == *preheader
                && d.args
                    .first()
                    .and_then(|&v| result(f, v))
                    .is_some_and(|i| f.inst(i).opcode == Opcode::TryStoreIntegerOctets8)
        })
    }) {
        return Err("already versioned checked fallback");
    }
    let initial_jump = f.inst(f.terminator(*preheader).ok_or("missing entry")?);
    if initial_jump.opcode != Opcode::Jump || initial_jump.targets.len() != 1 {
        return Err("entry is not an unconditional jump");
    }
    let entry = initial_jump.targets[0].args.clone();
    let params = &f.block(lp.header).params;
    if entry.len() != params.len() {
        return Err("entry arity");
    }
    let osr = f
        .osr_entries
        .iter()
        .find(|e| e.block == lp.header)
        .ok_or("missing exact header state")?;
    if !bounded_poll_scopes_supported(&osr.control_scopes) {
        return Err("native control scopes are unsupported");
    }
    let state = f.frame_states.get(osr.frame_state).clone();
    if params
        .iter()
        .chain(&entry)
        .any(|&v| f.value(v).repr != Repr::Tagged)
    {
        return Err("raw loop parameters");
    }
    if state.scopes.len() != 1
        || f.osr_entries
            .iter()
            .any(|e| lp.body.contains(&e.block) && e.block != lp.header)
    {
        return Err("unsupported logical scope or interior OSR");
    }
    let mut aliases = HashMap::new();
    for &b in &lp.body {
        if b == lp.header {
            continue;
        }
        let preds = f.preds(b);
        let [pred] = preds.as_slice() else {
            return Err("body is not linear");
        };
        let term = f.inst(
            f.terminator(*pred)
                .ok_or("missing predecessor terminator")?,
        );
        let incoming: Vec<_> = term.targets.iter().filter(|e| e.block == b).collect();
        let [edge] = incoming.as_slice() else {
            return Err("ambiguous forwarding edge");
        };
        for (&p, &v) in f.block(b).params.iter().zip(&edge.args) {
            aliases.insert(p, v);
        }
    }
    for &b in &lp.body {
        for &i in &f.block(b).insts {
            let d = f.inst(i);
            if simple_guard(d) && d.results.len() == 1 {
                aliases.insert(d.results[0], d.args[0]);
            }
        }
    }
    let term = f.inst(f.terminator(lp.header).ok_or("header terminator")?);
    if term.opcode != Opcode::Brif
        || term.args.len() != 1
        || term.targets.len() != 2
        || lp.body.contains(&term.targets[0].block)
        || !lp.body.contains(&term.targets[1].block)
    {
        return Err("header must compare then exit on true");
    }
    let comparison = result(f, alias(term.args[0], &aliases)).ok_or("comparison result")?;
    canonical(f, comparison, ">=", options).ok_or("comparison binding/policy")?;
    // A failing comparison is ordered before any later guard/arithmetic.
    // Do not hoist a post-comparison operation ahead of that failure.
    let header_insts = &f.block(lp.header).insts;
    let cmp_position = header_insts
        .iter()
        .position(|&i| i == comparison)
        .ok_or("comparison is not in header")?;
    if header_insts[cmp_position + 1..].iter().any(|&i| {
        let d = f.inst(i);
        !d.opcode.is_terminator() && !infallible_constant(f, d) && !coalescible_mv_reset(d)
    }) {
        return Err("ordered header operation follows comparison");
    }
    let cmp = f.inst(comparison);
    if cmp.args.len() != 2 {
        return Err("comparison arity");
    }
    let index = alias(cmp.args[0], &aliases);
    if !params.contains(&index) {
        return Err("index is not a header phi");
    }
    let end = alias(cmp.args[1], &aliases);
    let mut ordered = Vec::new();
    let mut block = term.targets[1].block;
    let latch;
    loop {
        if block == lp.header || ordered.contains(&block) {
            return Err("nonlinear body");
        }
        ordered.push(block);
        let t = f.inst(f.terminator(block).ok_or("body terminator")?);
        if t.opcode != Opcode::Jump || t.targets.len() != 1 {
            return Err("body contains control transfer");
        }
        if t.targets[0].block == lp.header {
            latch = t.targets[0].clone();
            break;
        }
        block = t.targets[0].block;
        if !lp.body.contains(&block) {
            return Err("early body exit");
        }
    }
    if ordered.len() + 1 != lp.body.len() {
        return Err("nonlinear region");
    }
    let instructions: Vec<_> = ordered
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| !f.inst(i).opcode.is_terminator())
        .collect();
    let mut low = None;
    let mut store = None;
    let mut shift = None;
    let mut increment = None;
    let mut constants = Vec::new();
    let mut bindings = Vec::new();
    let mut stage = 0;
    for inst in instructions {
        let d = f.inst(inst);
        if infallible_constant(f, d) {
            constants.push(inst);
            continue;
        }
        if d.opcode == Opcode::ClearMv && d.args.is_empty() && d.results.is_empty() {
            continue;
        }
        if d.opcode == Opcode::IntegerLowBits
            && stage == 0
            && d.args.len() == 1
            && d.results.len() == 1
            && matches!(d.aux, AuxData::FixnumImm(255))
            && d.flags.guard
        {
            let pc = bcp(f, d, options).ok_or("LDB/BYTE policy")?;
            if options.policy_at(pc.saturating_add(2)) == InlinePolicy::NotInline {
                return Err("LDB policy");
            }
            low = Some(inst);
            stage = 1;
            continue;
        }
        if stage == 1 && canonical(f, inst, "EGCL::SET-AREF", options).is_some() {
            if d.args.len() != 3
                || alias(d.args[1], &aliases) != index
                || alias(d.args[2], &aliases) != f.inst(low.unwrap()).results[0]
            {
                return Err("store dataflow");
            }
            bindings.push(canonical(f, inst, "EGCL::SET-AREF", options).unwrap());
            store = Some(inst);
            stage = 2;
            continue;
        }
        if stage == 2 && canonical(f, inst, "ASH", options).is_some() {
            if d.args.len() != 2 || literal(f, d.args[1], &aliases) != Some(-8) {
                return Err("shift count");
            }
            shift = Some(inst);
            stage = 3;
            continue;
        }
        if stage == 3 {
            let binary = canonical(f, inst, "+", options).is_some();
            let unary = canonical(f, inst, "1+", options).is_some();
            if binary || unary {
                if (binary && (d.args.len() != 2 || literal(f, d.args[1], &aliases) != Some(1)))
                    || (unary && d.args.len() != 1)
                    || alias(d.args[0], &aliases) != index
                {
                    return Err("increment dataflow");
                }
                increment = Some(inst);
                stage = 4;
                continue;
            }
        }
        return Err("unsupported body instruction/effect");
    }
    if stage != 4 {
        return Err("incomplete octet idiom");
    }
    let lane = alias(f.inst(low.unwrap()).args[0], &aliases);
    if !params.contains(&lane)
        || lane == index
        || alias(f.inst(shift.unwrap()).args[0], &aliases) != lane
    {
        return Err("lane phi/dataflow");
    }
    let increment = f.inst(increment.unwrap()).results[0];
    let shifted = f.inst(shift.unwrap()).results[0];
    for (n, &p) in params.iter().enumerate() {
        let incoming = alias(*latch.args.get(n).ok_or("latch arity")?, &aliases);
        if incoming
            != if p == index {
                increment
            } else if p == lane {
                shifted
            } else {
                p
            }
        {
            return Err("unreconstructible carried temporary");
        }
    }
    let array = alias(f.inst(store.unwrap()).args[0], &aliases);
    if lp
        .body
        .contains(&defining_block(f, array).ok_or("array definition")?)
        && !params.contains(&array)
    {
        return Err("variant output array");
    }
    // The entire header prefix must be reconstructible once at first entry;
    // end itself must remain invariant over all scalar iterations.
    let mut invariant: HashSet<_> = f
        .block_order()
        .iter()
        .filter(|b| !lp.body.contains(b))
        .flat_map(|&b| {
            f.block(b).params.iter().copied().chain(
                f.block(b)
                    .insts
                    .iter()
                    .flat_map(|&i| f.inst(i).results.iter().copied()),
            )
        })
        .collect();
    invariant.extend(params.iter().copied().filter(|p| *p != index && *p != lane));
    let mut prefix = Vec::new();
    for (position, &inst) in f.block(lp.header).insts.iter().enumerate() {
        if inst == comparison || f.inst(inst).opcode.is_terminator() {
            continue;
        }
        let d = f.inst(inst);
        // IF-test lowering resets secondary values after its comparison. Do
        // not reorder that reset or its FrameState into the probe prefix:
        // the checked fallback keeps it, and success has its own final reset.
        if coalescible_mv_reset(d) {
            // Earlier resets may be observed by canonical bound calls (calls
            // are MV readers under opt_clear_mv's contract). Preserve their
            // order and recovery state; only the post-comparison reset is
            // coalesced into the successful path's final reset.
            if position < cmp_position {
                prefix.push(inst);
            }
            continue;
        }
        if infallible_constant(f, d) {
            invariant.extend(d.results.iter().copied());
            prefix.push(inst);
            continue;
        }
        if simple_guard(d) {
            if invariant.contains(&alias(d.args[0], &aliases)) {
                invariant.extend(d.results.iter().copied());
            }
            prefix.push(inst);
            continue;
        }
        let allowed = ["+", "MIN", "MAX"]
            .iter()
            .any(|name| canonical(f, inst, name, options).is_some());
        if !allowed
            || d.args
                .iter()
                .any(|v| !invariant.contains(&alias(*v, &aliases)))
        {
            return Err("variant/opaque header bound");
        }
        invariant.extend(d.results.iter().copied());
        prefix.push(inst);
    }
    if !invariant.contains(&end) {
        return Err("loop bound varies");
    }
    // A forwarding parameter used in the last BODY iteration does not have
    // the final header phi's value: its index is end-1 and its lane is ASH56.
    // Two kernel outputs cannot recreate those temporaries, so never collapse
    // such body aliases to the final index/lane while repairing exit states.
    let post_aliases = aliases
        .iter()
        .filter_map(|(&source, &target)| {
            let resolved = alias(source, &aliases);
            let owner = defining_block(f, source)?;
            (!(owner != lp.header
                && lp.body.contains(&owner)
                && (resolved == index || resolved == lane)))
                .then_some((source, target))
        })
        .collect();
    let poll_scope = BoundedPollScope {
        function: state.scopes[0].function,
        bcp: state.scopes[0].bcp,
        control_scopes: osr.control_scopes.clone(),
    };
    Ok(Pattern {
        header: lp.header,
        body: lp.body.clone(),
        preheader: *preheader,
        entry,
        exit: term.targets[0].clone(),
        index,
        lane,
        end,
        array,
        increment,
        shifted,
        prefix,
        constants,
        aliases,
        post_aliases,
        bindings,
        state,
        poll_scope,
    })
}

fn mapped(
    f: &Function,
    value: Value,
    map: &HashMap<Value, Value>,
    aliases: &HashMap<Value, Value>,
    region: &[Block],
) -> Result<Value, &'static str> {
    if let Some(&value) = map.get(&value) {
        return Ok(value);
    }
    let value = alias(value, aliases);
    if let Some(&value) = map.get(&value) {
        return Ok(value);
    }
    if !region.contains(&defining_block(f, value).ok_or("missing value definition")?) {
        return Ok(value);
    }
    Err("unreconstructible deopt-live temporary")
}

fn remap_state(
    f: &Function,
    state: &FrameState,
    map: &HashMap<Value, Value>,
    p: &Pattern,
    first_entry: bool,
) -> Result<FrameState, &'static str> {
    let aliases = if first_entry {
        &p.aliases
    } else {
        &p.post_aliases
    };
    let mut state = state.clone();
    for scope in &mut state.scopes {
        for source in scope.locals.iter_mut().chain(&mut scope.stack) {
            if let ValueSource::Value { value, .. } = source {
                *value = mapped(f, *value, map, aliases, &p.body)?;
            }
        }
    }
    for recipe in &mut state.remat {
        for source in &mut recipe.inputs {
            if let ValueSource::Value { value, .. } = source {
                *value = mapped(f, *value, map, aliases, &p.body)?;
            }
        }
    }
    Ok(state)
}

fn plain(opcode: Opcode, args: Vec<Value>) -> InstData {
    InstData {
        opcode,
        args,
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

fn jump(f: &mut Function, from: Block, to: Block, args: Vec<Value>) {
    let mut d = plain(Opcode::Jump, vec![]);
    d.targets = vec![BlockCall { block: to, args }];
    f.set_terminator(from, d);
}

fn poll(f: &mut Function, block: Block, exit: bool, state: FrameState, scope: &BoundedPollScope) {
    let state = f.frame_states.add(state);
    let mut d = plain(Opcode::NativePoll, vec![]);
    d.aux = AuxData::BoundedPoll {
        exit,
        scope: scope.clone(),
    };
    d.flags = InstFlags {
        effectful: true,
        safepoint: true,
        ..Default::default()
    };
    d.frame_state = Some(state);
    f.push_inst(block, d, &[]);
}

fn transform(f: &mut Function, p: &Pattern) -> Result<(), &'static str> {
    // Allocate identities with the shared cloner, then replace only the fast
    // region. Every surviving value-map entry below has an explicit proof.
    let mut fast = clone_region(f, &p.body)?;
    let post = fast.block_map[&p.header];
    let mut initial: HashMap<_, _> = f
        .block(p.header)
        .params
        .iter()
        .copied()
        .zip(p.entry.iter().copied())
        .collect();
    let pre_state = remap_state(f, &p.state, &initial, p, true)?;
    let pre = f.make_block();
    let probe = f.make_block();
    let success = f.make_block();
    let slow_entry = f.make_block();
    poll(f, pre, false, pre_state.clone(), &p.poll_scope);
    jump(f, pre, probe, vec![]);
    let old_jump = f
        .block_mut(p.preheader)
        .insts
        .pop()
        .ok_or("entry terminator")?;
    if f.inst(old_jump).opcode != Opcode::Jump {
        return Err("changed preheader");
    }
    jump(f, p.preheader, pre, vec![]);
    for &inst in p.prefix.iter().chain(&p.constants) {
        let mut d = f.inst(inst).clone();
        d.args = d
            .args
            .iter()
            .map(|&v| mapped(f, v, &initial, &p.aliases, &p.body))
            .collect::<Result<_, _>>()?;
        if let Some(id) = d.frame_state {
            let state = remap_state(f, f.frame_states.get(id), &initial, p, true)?;
            d.frame_state = Some(f.frame_states.add(state));
        }
        let old_results = d.results.clone();
        d.results.clear();
        let types: Vec<_> = old_results
            .iter()
            .map(|&v| (f.value(v).ty, f.value(v).repr))
            .collect();
        let (_, new_results) = f.push_inst(probe, d, &types);
        initial.extend(old_results.into_iter().zip(new_results));
    }
    for &binding in &p.bindings {
        let fs = f.frame_states.add(pre_state.clone());
        let pos = f.block(probe).insts.len();
        builtin_binding::insert(f, probe, pos, fs, binding);
    }
    let array = mapped(f, p.array, &initial, &p.aliases, &p.body)?;
    let start = initial[&p.index];
    let end = mapped(f, p.end, &initial, &p.aliases, &p.body)?;
    let lane = initial[&p.lane];
    let mut d = plain(
        Opcode::TryStoreIntegerOctets8,
        vec![array, start, end, lane],
    );
    d.flags.effectful = true;
    let (_, outputs) = f.push_inst(
        probe,
        d,
        &[
            (
                IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                Repr::Tagged,
            ),
            (IRType::of(TypeBits::FIXNUM), Repr::Tagged),
        ],
    );
    let mut branch = plain(Opcode::Brif, vec![outputs[0]]);
    branch.targets = vec![
        BlockCall {
            block: success,
            args: vec![],
        },
        BlockCall {
            block: slow_entry,
            args: vec![],
        },
    ];
    f.set_terminator(probe, branch);
    // Preserve a unique unconditional entry for later scalar type peeling.
    jump(f, slow_entry, p.header, p.entry.clone());
    let mut final_values = initial.clone();
    final_values.insert(p.index, end);
    final_values.insert(p.lane, outputs[1]);
    final_values.insert(p.increment, end);
    final_values.insert(p.shifted, outputs[1]);
    for &inst in &p.prefix {
        let d = f.inst(inst);
        if simple_guard(d) && d.results.len() == 1 {
            let source = alias(d.args[0], &p.aliases);
            if let Some(&value) = final_values.get(&source) {
                final_values.insert(d.results[0], value);
            }
        }
    }
    let mut clear = plain(Opcode::ClearMv, vec![]);
    clear.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    let success_state = remap_state(f, &p.state, &final_values, p, false)?;
    clear.frame_state = Some(f.frame_states.add(success_state));
    f.push_inst(success, clear, &[]);
    let post_args = f
        .block(p.header)
        .params
        .iter()
        .map(|&v| mapped(f, v, &final_values, &p.aliases, &p.body))
        .collect::<Result<Vec<_>, _>>()?;
    jump(f, success, post, post_args);
    // Discard the cloned scalar body; the original checked/OSR loop is intact.
    for (&old, &cloned) in &fast.block_map {
        if old != p.header {
            // Layout removal is not arena deletion: verification and some
            // metadata walkers still inspect every allocated block. Retire
            // the orphaned scalar copy exactly like integer extraction's
            // unreachable-block cleanup, without retaining stale phi uses or
            // backedges into the replacement post block.
            f.block_mut(cloned).params.clear();
            f.block_mut(cloned).insts.clear();
            f.set_terminator(cloned, plain(Opcode::Trap, vec![]));
            f.remove_from_layout(cloned);
        }
    }
    f.block_mut(post).insts.clear();
    let mut post_map = final_values.clone();
    for (&old, &new) in f.block(p.header).params.iter().zip(&f.block(post).params) {
        post_map.insert(old, new);
    }
    post_map.insert(p.increment, post_map[&p.index]);
    post_map.insert(p.shifted, post_map[&p.lane]);
    let post_state = remap_state(f, &p.state, &post_map, p, false)?;
    poll(f, post, true, post_state, &p.poll_scope);
    let exit_args = p
        .exit
        .args
        .iter()
        .map(|&v| mapped(f, v, &post_map, &p.post_aliases, &p.body))
        .collect::<Result<_, _>>()?;
    jump(f, post, p.exit.block, exit_args);
    let originals: Vec<_> = fast.value_map.keys().copied().collect();
    fast.value_map.clear();
    for original in originals {
        if let Ok(value) = mapped(f, original, &post_map, &p.post_aliases, &p.body) {
            fast.value_map.insert(original, value);
        }
    }
    repair_outside_uses(f, &p.body, &[fast])
}

/// Run immediately after integer extraction and before numeric loop peeling.
/// Each declined transaction leaves the checked loop byte-for-byte unchanged.
pub fn fuse_octet_words(f: &mut Function, options: &InlineOptions) -> OctetWordStats {
    let mut stats = OctetWordStats::default();
    if super::verify::verify(f).is_err() {
        stats.declined.push("input IR is malformed".into());
        return stats;
    }
    let loops = detect_loops(f, &f.dominators());
    for lp in &loops {
        if loops
            .iter()
            .any(|inner| inner.header != lp.header && lp.body.contains(&inner.header))
        {
            continue;
        }
        if stats.loops == 2 {
            break;
        }
        // Avoid stale body/exit operands after an earlier loop's SSA repair.
        let current = detect_loops(f, &f.dominators());
        let Some(lp) = current.iter().find(|l| l.header == lp.header) else {
            continue;
        };
        let p = match recognize(f, lp, options) {
            Ok(p) => p,
            Err(reason) => {
                stats
                    .declined
                    .push(format!("header {}: {reason}", lp.header.0));
                continue;
            }
        };
        if std::env::var_os("EGCL_OCTET_WORD_LOG").is_some() {
            // EgclVal's Debug prints tags/addresses, never heap contents. This
            // diagnostic is safe under the compiler's no-heap worker guard.
            eprintln!(
                "[octet-word] {} header {} entry {:?} state {:?}",
                f.name(),
                p.header.0,
                p.entry,
                p.state
            );
            for &block in &p.body {
                for &inst in &f.block(block).insts {
                    if let Some(state) = f.inst(inst).frame_state {
                        eprintln!(
                            "[octet-word] inst {} {:?} state {:?}",
                            inst.0,
                            f.inst(inst).opcode,
                            f.frame_states.get(state)
                        );
                    }
                }
            }
        }
        let mut trial = f.clone();
        if let Err(reason) = transform(&mut trial, &p) {
            stats
                .declined
                .push(format!("header {}: {reason}", lp.header.0));
            continue;
        }
        if let Err(reason) = super::verify::verify(&trial) {
            stats
                .declined
                .push(format!("header {}: {reason:?}", lp.header.0));
            continue;
        }
        *f = trial;
        stats.loops += 1;
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, RematOp, RematRecipe, RematRecipeId};
    use crate::t2::ir::OsrEntry;

    struct Fixture {
        f: Function,
        header: Block,
        body: Block,
        exit: Block,
        start: Value,
        lane: Value,
        index: Value,
        array: Value,
        initial_lane: Value,
        cmp: Inst,
        shift: Inst,
        store: Inst,
    }
    fn constant(f: &mut Function, b: Block, n: i64) -> Value {
        let mut d = plain(Opcode::ConstFixnum, vec![]);
        d.aux = AuxData::FixnumImm(n);
        f.push_inst(b, d, &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)])
            .1[0]
    }
    fn state(
        f: &mut Function,
        pc: u32,
        index: Value,
        lane: Value,
        args: &[Value],
    ) -> super::super::frame_state::FrameStateId {
        f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: pc,
                locals: vec![
                    ValueSource::Value {
                        value: index,
                        repr: Repr::Tagged,
                    },
                    ValueSource::Value {
                        value: lane,
                        repr: Repr::Tagged,
                    },
                ],
                stack: args
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: Repr::Tagged,
                    })
                    .collect(),
            }],
            remat: vec![],
        })
    }
    fn call(
        f: &mut Function,
        b: Block,
        pc: u32,
        name: &str,
        args: Vec<Value>,
        index: Value,
        lane: Value,
    ) -> (Inst, Value) {
        let fs = state(f, pc, index, lane, &args);
        let mut d = plain(Opcode::Call, args);
        d.aux = AuxData::CallTarget(egcl_rt::symbols::intern(name));
        d.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        d.frame_state = Some(fs);
        let (i, v) = f.push_inst(b, d, &[(IRType::TOP, Repr::Tagged)]);
        (i, v[0])
    }
    fn fixture() -> Fixture {
        let mut f = Function::new("octet-fusion-fixture");
        let entry = f.entry();
        let array = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let start = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let end = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let initial_lane = f.add_block_param(entry, IRType::TOP, Repr::Tagged);
        let minus8 = constant(&mut f, entry, -8);
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        let index = f.add_block_param(header, IRType::TOP, Repr::Tagged);
        let lane = f.add_block_param(header, IRType::TOP, Repr::Tagged);
        jump(&mut f, entry, header, vec![start, initial_lane]);
        let header_state = state(&mut f, 10, index, lane, &[]);
        f.osr_entries.push(OsrEntry {
            control_scopes: vec![],
            bcp: 10,
            block: header,
            frame_state: header_state,
            checks: vec![],
        });
        let (cmp, cond) = call(&mut f, header, 12, ">=", vec![index, end], index, lane);
        let mut branch = plain(Opcode::Brif, vec![cond]);
        branch.targets = vec![
            BlockCall {
                block: exit,
                args: vec![],
            },
            BlockCall {
                block: body,
                args: vec![],
            },
        ];
        f.set_terminator(header, branch);
        let fs = state(&mut f, 20, index, lane, &[lane]);
        let mut low = plain(Opcode::IntegerLowBits, vec![lane]);
        low.aux = AuxData::FixnumImm(255);
        low.flags = InstFlags {
            effectful: true,
            guard: true,
            ..Default::default()
        };
        low.frame_state = Some(fs);
        let (_, byte) = f.push_inst(body, low, &[(IRType::of(TypeBits::FIXNUM), Repr::Tagged)]);
        let (store, _) = call(
            &mut f,
            body,
            24,
            "EGCL::SET-AREF",
            vec![array, index, byte[0]],
            index,
            lane,
        );
        let mut clear = plain(Opcode::ClearMv, vec![]);
        clear.flags = InstFlags {
            effectful: true,
            call: true,
            safepoint: true,
            ..Default::default()
        };
        clear.frame_state = Some(state(&mut f, 25, index, lane, &[]));
        f.push_inst(body, clear, &[]);
        let (shift, next_lane) = call(&mut f, body, 26, "ASH", vec![lane, minus8], index, lane);
        let (_, next_index) = call(&mut f, body, 28, "1+", vec![index], index, next_lane);
        jump(&mut f, body, header, vec![next_index, next_lane]);
        // Direct deopt-only uses require actual SSA exit repair, not just
        // assigning the visible return value from a hand-built edge.
        let fs = state(&mut f, 30, index, lane, &[]);
        let mut guard = plain(Opcode::Guard, vec![index]);
        guard.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
        guard.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        guard.frame_state = Some(fs);
        f.push_inst(exit, guard, &[]);
        f.set_terminator(exit, plain(Opcode::Return, vec![lane]));
        Fixture {
            f,
            header,
            body,
            exit,
            start,
            lane,
            index,
            array,
            initial_lane,
            cmp,
            shift,
            store,
        }
    }
    #[test]
    fn versions_full_word_preserves_scalar_osr_and_repairs_liveouts_and_polls() {
        let mut x = fixture();
        super::super::verify::verify(&x.f).unwrap();
        let original_osr = format!("{:?}", x.f.osr_entries);
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        super::super::verify::verify(&x.f).unwrap();
        assert_eq!(format!("{:?}", x.f.osr_entries), original_osr);
        assert_eq!(x.f.inst(x.store).opcode, Opcode::Call);
        assert_eq!(x.f.inst(x.shift).opcode, Opcode::Call);
        for number in 0..x.f.num_blocks() {
            let block = Block(number as u32);
            if x.f.block_order().contains(&block) {
                continue;
            }
            assert!(x.f.block(block).params.is_empty());
            assert_eq!(x.f.block(block).insts.len(), 1);
            assert_eq!(x.f.inst(x.f.block(block).insts[0]).opcode, Opcode::Trap);
        }
        let kernels: Vec<_> =
            x.f.block_order()
                .iter()
                .flat_map(|&b| &x.f.block(b).insts)
                .filter(|&&i| x.f.inst(i).opcode == Opcode::TryStoreIntegerOctets8)
                .copied()
                .collect();
        assert_eq!(kernels.len(), 1);
        assert_eq!(x.f.inst(kernels[0]).args[0], x.array);
        assert_eq!(x.f.inst(kernels[0]).args[1], x.start);
        assert_eq!(x.f.inst(kernels[0]).args[3], x.initial_lane);
        let polls: Vec<_> =
            x.f.block_order()
                .iter()
                .flat_map(|&b| x.f.block(b).insts.iter().map(move |&i| (b, i)))
                .filter(|&(_, i)| x.f.inst(i).opcode == Opcode::NativePoll)
                .collect();
        assert_eq!(polls.len(), 2);
        for (b, i) in polls {
            assert_eq!(x.f.block(b).insts[0], i);
            let fs = x.f.frame_states.get(x.f.inst(i).frame_state.unwrap());
            assert_eq!(fs.scopes[0].bcp, 10);
            for source in &fs.scopes[0].locals {
                assert!(
                    !matches!(source, ValueSource::Value { value, .. } if *value == x.index || *value == x.lane)
                );
            }
        }
        let returned = x.f.inst(x.f.terminator(x.exit).unwrap()).args[0];
        assert_ne!(returned, x.lane);
        assert!(
            matches!(x.f.value(returned).def, ValueDef::Param { block, .. } if block == x.exit)
        );
        assert_eq!(
            fuse_octet_words(&mut x.f, &InlineOptions::default()).loops,
            0
        );
        let outside: Vec<_> =
            x.f.preds(x.header)
                .into_iter()
                .filter(|&b| b != x.body)
                .collect();
        assert_eq!(outside.len(), 1);
        let term = x.f.inst(x.f.terminator(outside[0]).unwrap());
        assert_eq!(term.opcode, Opcode::Jump);
        assert_eq!(term.targets.len(), 1);
        assert_eq!(term.targets[0].block, x.header);
        let mut peeled = x.f.clone();
        let stats = super::super::opt_loop_peel::peel_for_numeric_types(
            &mut peeled,
            &|_| true,
            super::super::opt_loop_peel::PeelOptions::default(),
        );
        assert_eq!(
            stats.loops, 1,
            "scalar fallback must remain peelable: {stats:?}"
        );
        super::super::verify::verify(&peeled).unwrap();
    }
    #[test]
    fn clones_invariant_header_bound_prefix_with_first_entry_recovery() {
        let mut x = fixture();
        let entry = x.f.entry();
        let eight = constant(&mut x.f, entry, 8);
        // Keep the entry terminator last after adding the new literal.
        let term_pos =
            x.f.block(entry)
                .insts
                .iter()
                .position(|&i| x.f.inst(i).opcode == Opcode::Jump)
                .unwrap();
        let term = x.f.block_mut(entry).insts.remove(term_pos);
        x.f.block_mut(entry).insts.push(term);
        let (add, sum) = call(
            &mut x.f,
            x.header,
            11,
            "+",
            vec![x.start, eight],
            x.index,
            x.lane,
        );
        let limit = x.f.inst(x.cmp).args[1];
        let (min, bound) = call(
            &mut x.f,
            x.header,
            11,
            "MIN",
            vec![sum, limit],
            x.index,
            x.lane,
        );
        x.f.inst_mut(x.cmp).args[1] = bound;
        let state = x.f.inst(x.cmp).frame_state.unwrap();
        x.f.frame_states.get_mut(state).scopes[0].stack[1] = ValueSource::Value {
            value: bound,
            repr: Repr::Tagged,
        };
        let list = &mut x.f.block_mut(x.header).insts;
        list.retain(|&i| i != add && i != min);
        list.insert(0, min);
        list.insert(0, add);
        super::super::verify::verify(&x.f).unwrap();
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        super::super::verify::verify(&x.f).unwrap();
        assert!(x.f.block(x.header).insts.contains(&add));
        assert!(x.f.block(x.header).insts.contains(&min));
    }
    #[test]
    fn if_test_mv_reset_is_coalesced_without_moving_its_comparison_state() {
        let mut x = fixture();
        let condition = x.f.inst(x.cmp).results[0];
        let mut clear = plain(Opcode::ClearMv, vec![]);
        clear.flags = InstFlags {
            effectful: true,
            call: true,
            safepoint: true,
            ..Default::default()
        };
        clear.frame_state = Some(state(&mut x.f, 13, x.index, x.lane, &[condition]));
        let (clear, _) = x.f.push_inst(x.header, clear, &[]);
        let list = &mut x.f.block_mut(x.header).insts;
        assert_eq!(list.pop(), Some(clear));
        list.insert(list.len() - 1, clear);
        // The false IF arm also pushes NIL before discarding its value.
        // This is real lower_if bytecode, not a special benchmark form.
        let (nil, nil_value) = x.f.push_inst(
            x.body,
            plain(Opcode::ConstNil, vec![]),
            &[(IRType::of(TypeBits::NULL), Repr::Tagged)],
        );
        let mut body_clear = plain(Opcode::ClearMv, vec![]);
        body_clear.flags = InstFlags {
            effectful: true,
            call: true,
            safepoint: true,
            ..Default::default()
        };
        body_clear.frame_state = Some(state(&mut x.f, 19, x.index, x.lane, &nil_value));
        let (body_clear, _) = x.f.push_inst(x.body, body_clear, &[]);
        let list = &mut x.f.block_mut(x.body).insts;
        assert_eq!(list.pop(), Some(body_clear));
        assert_eq!(list.pop(), Some(nil));
        list.insert(0, body_clear);
        list.insert(0, nil);
        super::super::verify::verify(&x.f).unwrap();
        let original_state = x.f.inst(clear).frame_state.unwrap();
        let original_state_debug = format!("{:?}", x.f.frame_states.get(original_state));
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        super::super::verify::verify(&x.f).unwrap();
        assert!(x.f.block(x.header).insts.contains(&clear));
        assert_eq!(x.f.inst(clear).frame_state, Some(original_state));
        assert_eq!(
            format!("{:?}", x.f.frame_states.get(original_state)),
            original_state_debug
        );
        let kernels: Vec<_> =
            x.f.block_order()
                .iter()
                .copied()
                .filter(|&b| {
                    x.f.block(b)
                        .insts
                        .iter()
                        .any(|&i| x.f.inst(i).opcode == Opcode::TryStoreIntegerOctets8)
                })
                .collect();
        assert_eq!(kernels.len(), 1);
        assert!(
            !x.f.block(kernels[0])
                .insts
                .iter()
                .any(|&i| x.f.inst(i).opcode == Opcode::ClearMv)
        );
    }

    #[test]
    fn failing_header_operations_after_comparison_are_not_reordered() {
        for arithmetic in [false, true] {
            let mut x = fixture();
            let late = if arithmetic {
                call(
                    &mut x.f,
                    x.header,
                    13,
                    "MIN",
                    vec![x.start, x.initial_lane],
                    x.index,
                    x.lane,
                )
                .0
            } else {
                let mut guard = plain(Opcode::Guard, vec![x.initial_lane]);
                guard.flags = InstFlags {
                    effectful: true,
                    guard: true,
                    ..Default::default()
                };
                guard.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
                guard.frame_state = Some(state(&mut x.f, 13, x.index, x.lane, &[]));
                x.f.push_inst(x.header, guard, &[]).0
            };
            let list = &mut x.f.block_mut(x.header).insts;
            assert_eq!(list.pop(), Some(late));
            list.insert(list.len() - 1, late);
            super::super::verify::verify(&x.f).unwrap();
            let before = format!("{:?}", x.f);
            let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
            assert_eq!(stats.loops, 0, "{stats:?}");
            assert_eq!(format!("{:?}", x.f), before);
        }
    }

    #[test]
    fn prefix_mv_reset_stays_before_bound_calls_with_first_entry_state() {
        let mut x = fixture();
        let entry = x.f.entry();
        let eight = constant(&mut x.f, entry, 8);
        let term_position =
            x.f.block(entry)
                .insts
                .iter()
                .position(|&i| x.f.inst(i).opcode == Opcode::Jump)
                .unwrap();
        let term = x.f.block_mut(entry).insts.remove(term_position);
        x.f.block_mut(entry).insts.push(term);
        let (add, sum) = call(
            &mut x.f,
            x.header,
            11,
            "+",
            vec![x.start, eight],
            x.index,
            x.lane,
        );
        let limit = x.f.inst(x.cmp).args[1];
        let (min, bound) = call(
            &mut x.f,
            x.header,
            11,
            "MIN",
            vec![sum, limit],
            x.index,
            x.lane,
        );
        x.f.inst_mut(x.cmp).args[1] = bound;
        let cmp_state = x.f.inst(x.cmp).frame_state.unwrap();
        x.f.frame_states.get_mut(cmp_state).scopes[0].stack[1] = ValueSource::Value {
            value: bound,
            repr: Repr::Tagged,
        };
        let mut reset = plain(Opcode::ClearMv, vec![]);
        reset.flags = InstFlags {
            effectful: true,
            call: true,
            safepoint: true,
            ..Default::default()
        };
        let original_state = state(&mut x.f, 10, x.index, x.lane, &[]);
        reset.frame_state = Some(original_state);
        let (reset, _) = x.f.push_inst(x.header, reset, &[]);
        let list = &mut x.f.block_mut(x.header).insts;
        list.retain(|&i| i != reset && i != add && i != min);
        list.insert(0, min);
        list.insert(0, add);
        list.insert(0, reset);
        super::super::verify::verify(&x.f).unwrap();
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        super::super::verify::verify(&x.f).unwrap();
        assert_eq!(x.f.inst(reset).frame_state, Some(original_state));
        let probe =
            x.f.block_order()
                .iter()
                .copied()
                .find(|&b| {
                    x.f.block(b)
                        .insts
                        .iter()
                        .any(|&i| x.f.inst(i).opcode == Opcode::TryStoreIntegerOctets8)
                })
                .unwrap();
        let instructions = &x.f.block(probe).insts;
        let cloned_reset = instructions[0];
        assert_eq!(x.f.inst(cloned_reset).opcode, Opcode::ClearMv);
        let cloned_state =
            x.f.frame_states
                .get(x.f.inst(cloned_reset).frame_state.unwrap());
        assert!(
            matches!(cloned_state.scopes[0].locals[0], ValueSource::Value { value, .. } if value == x.start)
        );
        assert!(
            matches!(cloned_state.scopes[0].locals[1], ValueSource::Value { value, .. } if value == x.initial_lane)
        );
        assert_eq!(cloned_state.scopes[0].bcp, 10);
        let numeric_calls: Vec<_> = instructions
            .iter()
            .enumerate()
            .filter_map(|(pos, &i)| {
                let d = x.f.inst(i);
                if d.opcode != Opcode::Call {
                    return None;
                }
                let AuxData::CallTarget(symbol) = d.aux else {
                    return None;
                };
                let name = egcl_rt::symbols::registry_key(symbol)?;
                matches!(name.as_str(), "+" | "MIN").then_some((pos, name))
            })
            .collect();
        assert_eq!(numeric_calls.len(), 2);
        assert!(numeric_calls[0].0 > 0);
        assert_eq!(numeric_calls[0].1, "+");
        assert_eq!(numeric_calls[1].1, "MIN");
        assert!(numeric_calls[0].0 < numeric_calls[1].0);
    }

    #[test]
    fn notinline_or_shadowed_operators_decline_without_changes() {
        for pc in [12, 20, 22, 24, 26, 28] {
            let mut x = fixture();
            let before = format!("{:?}", x.f);
            let options = InlineOptions::default().with_policy(pc, InlinePolicy::NotInline);
            assert_eq!(fuse_octet_words(&mut x.f, &options).loops, 0, "BCP {pc}");
            assert_eq!(format!("{:?}", x.f), before);
        }
        let mut x = fixture();
        x.f.inst_mut(x.shift).aux = AuxData::CallTarget(egcl_rt::symbols::intern("USER::ASH"));
        let before = format!("{:?}", x.f);
        assert_eq!(
            fuse_octet_words(&mut x.f, &InlineOptions::default()).loops,
            0
        );
        assert_eq!(format!("{:?}", x.f), before);
    }
    #[test]
    fn changing_extra_phi_cannot_be_dropped_even_when_only_deopt_live() {
        let mut x = fixture();
        let zero = constant(&mut x.f, x.body, 0);
        let term = x.f.terminator(x.body); // appended literal temporarily follows the old terminator
        if term.is_none() {
            let pos =
                x.f.block(x.body)
                    .insts
                    .iter()
                    .position(|&i| x.f.inst(i).opcode == Opcode::Jump)
                    .unwrap();
            let i = x.f.block_mut(x.body).insts.remove(pos);
            x.f.block_mut(x.body).insts.push(i);
        }
        let extra = x.f.add_block_param(x.header, IRType::TOP, Repr::Tagged);
        let entry = x.f.entry();
        let initial = x.initial_lane;
        let t = x.f.terminator(entry).unwrap();
        x.f.inst_mut(t).targets[0].args.push(initial);
        let t = x.f.terminator(x.body).unwrap();
        x.f.inst_mut(t).targets[0].args.push(zero);
        let id = x.f.osr_entries[0].frame_state;
        x.f.frame_states.get_mut(id).scopes[0]
            .locals
            .push(ValueSource::Value {
                value: extra,
                repr: Repr::Tagged,
            });
        super::super::verify::verify(&x.f).unwrap();
        let before = format!("{:?}", x.f);
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 0, "{stats:?}");
        assert_eq!(format!("{:?}", x.f), before);
    }
    #[test]
    fn rematerialization_inputs_are_mapped_at_both_boundaries() {
        let mut x = fixture();
        let id = x.f.osr_entries[0].frame_state;
        let state = x.f.frame_states.get_mut(id);
        state.remat.push(RematRecipe {
            op: RematOp::FixnumAdd,
            inputs: vec![
                ValueSource::Value {
                    value: x.index,
                    repr: Repr::Tagged,
                },
                ValueSource::Const(egcl_rt::EgclVal::from_fixnum(0)),
            ],
            result_repr: Repr::Tagged,
        });
        state.scopes[0]
            .stack
            .push(ValueSource::Remat(RematRecipeId(0)));
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        for &b in x.f.block_order() {
            for &i in &x.f.block(b).insts {
                if x.f.inst(i).opcode != Opcode::NativePoll {
                    continue;
                }
                let state = x.f.frame_states.get(x.f.inst(i).frame_state.unwrap());
                assert!(
                    matches!(state.remat[0].inputs[0], ValueSource::Value { value, .. } if value != x.index)
                );
            }
        }
    }
    #[test]
    fn transparent_local_scope_chain_is_preserved_without_synthetic_osr() {
        use crate::control_scope::{ControlScope, Ownership, ScopeKind};
        let mut x = fixture();
        x.f.osr_entries[0].control_scopes = vec![
            ControlScope {
                push_bcp: 1,
                ownership: Ownership::Local,
                sp_restore: 0,
                kind: ScopeKind::LexicalEnvironment,
            },
            ControlScope {
                push_bcp: 2,
                ownership: Ownership::Local,
                sp_restore: 0,
                kind: ScopeKind::Block {
                    id: 1,
                    resume_bcp: 40,
                    register: false,
                },
            },
            ControlScope {
                push_bcp: 3,
                ownership: Ownership::Local,
                sp_restore: 0,
                kind: ScopeKind::Tagbody { id: 2 },
            },
        ];
        let scopes = x.f.osr_entries[0].control_scopes.clone();
        let stats = fuse_octet_words(&mut x.f, &InlineOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        assert_eq!(x.f.osr_entries.len(), 1);
        assert_eq!(x.f.osr_entries[0].control_scopes, scopes);
        super::super::verify::verify(&x.f).unwrap();
        // Ordinary primitive lowering removes OSR imports before verifying
        // the artifact. Poll recovery must keep the original source anchor.
        let mut ordinary = x.f.clone();
        ordinary.osr_entries.clear();
        super::super::verify::verify(&ordinary).unwrap();
        let mut polls = 0;
        for &block in ordinary.block_order() {
            for &inst in &ordinary.block(block).insts {
                if ordinary.inst(inst).opcode != Opcode::NativePoll {
                    continue;
                }
                let AuxData::BoundedPoll { scope, .. } = &ordinary.inst(inst).aux else {
                    panic!("missing persistent scope");
                };
                assert_eq!(scope.control_scopes, scopes);
                assert_eq!(scope.bcp, 10);
                assert_eq!(scope.function, 0);
                polls += 1;
            }
        }
        assert_eq!(polls, 2);
    }

    #[test]
    fn nontransparent_or_inherited_scopes_decline_without_changes() {
        use crate::control_scope::{ControlScope, Ownership, ScopeKind};
        for (ownership, kind) in [
            (Ownership::Inherited, ScopeKind::Tagbody { id: 2 }),
            (
                Ownership::Local,
                ScopeKind::Block {
                    id: 1,
                    resume_bcp: 40,
                    register: true,
                },
            ),
            (Ownership::Local, ScopeKind::SpecialBinding { symbol: 0 }),
            (Ownership::Local, ScopeKind::HandlerBind { table_index: 0 }),
        ] {
            let mut x = fixture();
            x.f.osr_entries[0].control_scopes = vec![ControlScope {
                push_bcp: 1,
                ownership,
                sp_restore: 0,
                kind,
            }];
            assert!(!bounded_poll_scopes_supported(
                &x.f.osr_entries[0].control_scopes
            ));
            let before = format!("{:?}", x.f);
            assert_eq!(
                fuse_octet_words(&mut x.f, &InlineOptions::default()).loops,
                0
            );
            assert_eq!(format!("{:?}", x.f), before);
        }
    }
}
