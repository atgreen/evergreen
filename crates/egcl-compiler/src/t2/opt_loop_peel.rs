// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Peel a profitable first iteration and version the remainder by type.
//!
//! A mixed integer profile is not permission to assume fixnums at loop entry.
//! For a loop-carried arithmetic right shift, a fixnum value stays a fixnum,
//! while a wide initial integer can become one. Execute the first iteration
//! with generic arithmetic, then test the carried value once. The fast copy
//! has a proved fixnum phi; the original loop handles larger integers and OSR.
//! Zero-trip/early-exit conditions, effects and deopt bytecode positions are
//! cloned unchanged. Exit SSA (including FrameState-only uses) is repaired.

use std::collections::HashMap;

use super::frame_state::ValueSource;
use super::ir::{
    AuxData, Block, BlockCall, Function, IRType, InstData, InstFlags, Opcode, Range, TypeBits,
    Value, ValueDef,
};
use super::region_clone::{clone_region, repair_outside_uses};

#[path = "bounded_octet_fallback.rs"]
mod bounded_octet_fallback;

/// Bounded growth: duplicate only small innermost loops with useful feedback.
#[derive(Clone, Copy, Debug)]
pub struct PeelOptions {
    pub max_loop_instructions: usize,
    pub max_added_instructions: usize,
}
impl Default for PeelOptions {
    fn default() -> Self {
        Self {
            max_loop_instructions: 64,
            max_added_instructions: 192,
        }
    }
}

/// Successful transformations and their explicit code-growth cost.
#[derive(Default, Debug)]
pub struct PeelStats {
    pub loops: usize,
    pub added_instructions: usize,
    pub declined: Vec<String>,
}

/// `benefit(bcp)` must name a mixed integer site whose fixnum observations
/// outnumber wide ones. It predicts benefit, never supplies a type proof.
/// Every transformation is verified transactionally before being published.
pub fn peel_for_numeric_types(
    f: &mut Function,
    benefit: &impl Fn(u32) -> bool,
    options: PeelOptions,
) -> PeelStats {
    let dom = f.dominators();
    let loops = super::opt_licm::detect_loops(f, &dom);
    let mut stats = PeelStats::default();
    for lp in &loops {
        if lp.header == f.entry()
            || loops
                .iter()
                .any(|inner| inner.header != lp.header && lp.body.contains(&inner.header))
        {
            continue;
        }
        let cost: usize = lp.body.iter().map(|&b| f.block(b).insts.len()).sum();
        if cost > options.max_loop_instructions
            || stats.added_instructions + cost * 2 + 4 > options.max_added_instructions
        {
            continue;
        }
        let Some((parameter, latch, range)) = candidate(f, lp.header, &lp.body, benefit) else {
            continue;
        };
        if known_tiny_loop(f, lp.header, &lp.body, latch) {
            continue;
        }
        if std::env::var_os("EGCL_DISABLE_BOUNDED_FALLBACK_PRUNING").is_none()
            && bounded_octet_fallback::bounded(f, lp.header, &lp.body, latch)
        {
            stats.declined.push(format!(
                "header {}: bounded octet scalar fallback",
                lp.header.0
            ));
            continue;
        }
        let mut trial = f.clone();
        let before = trial.num_insts();
        if let Err(error) = transform(&mut trial, lp.header, &lp.body, parameter, latch, range) {
            stats
                .declined
                .push(format!("header {}: {error}", lp.header.0));
            continue;
        }
        if let Err(error) = super::verify::verify(&trial) {
            stats
                .declined
                .push(format!("header {}: {error:?}", lp.header.0));
            continue;
        }
        let added = trial.num_insts() - before;
        if stats.added_instructions + added > options.max_added_instructions {
            continue;
        }
        *f = trial;
        stats.loops += 1;
        stats.added_instructions += added;
    }
    stats
}

fn fixnum(f: &Function, value: Value) -> bool {
    let bits = f.value(value).ty.bits;
    !bits.is_bottom() && bits.meet(TypeBits::FIXNUM) == bits
}

/// Collapse single-predecessor forwarding parameters without treating a loop
/// phi as an alias. Invoke normal-edge results are common bytecode builders.
fn alias(f: &Function, mut value: Value) -> Value {
    for _ in 0..64 {
        let ValueDef::Param { block, num } = f.value(value).def else {
            break;
        };
        let preds = f.preds(block);
        if preds.len() != 1 {
            break;
        }
        let Some(term) = f.terminator(preds[0]) else {
            break;
        };
        let mut incoming = f
            .inst(term)
            .targets
            .iter()
            .filter(|edge| edge.block == block);
        let Some(edge) = incoming.next() else { break };
        if incoming.next().is_some() {
            break;
        }
        let Some(&next) = edge.args.get(num as usize) else {
            break;
        };
        if next == value {
            break;
        }
        value = next;
    }
    value
}

fn candidate(
    f: &Function,
    header: Block,
    body: &[Block],
    benefit: &impl Fn(u32) -> bool,
) -> Option<(usize, Block, Option<Range>)> {
    let tails: Vec<_> = body
        .iter()
        .copied()
        .filter(|&b| f.succs(b).contains(&header))
        .collect();
    let back_edges = body
        .iter()
        .filter_map(|&block| f.terminator(block))
        .flat_map(|inst| &f.inst(inst).targets)
        .filter(|edge| edge.block == header)
        .count();
    if tails.len() != 1
        || back_edges != 1
        || !f.osr_entries.iter().any(|entry| entry.block == header)
    {
        return None;
    }
    // The region must be single-entry, with no nested loop or control landing.
    if body
        .iter()
        .any(|&b| b != header && f.preds(b).iter().any(|p| !body.contains(p)))
    {
        return None;
    }
    let latch = tails[0];
    let term = f.terminator(latch)?;
    let edge = f
        .inst(term)
        .targets
        .iter()
        .find(|edge| edge.block == header)?;
    for (index, &phi) in f.block(header).params.iter().enumerate() {
        if fixnum(f, phi) || f.value(phi).repr != super::ir::ValueRepresentation::Tagged {
            continue;
        }
        let carried = alias(f, *edge.args.get(index)?);
        let ValueDef::Result { inst, .. } = f.value(carried).def else {
            continue;
        };
        let data = f.inst(inst);
        if !matches!(data.opcode, Opcode::Call | Opcode::Invoke) || data.args.len() != 2 {
            continue;
        }
        let AuxData::CallTarget(symbol) = data.aux else {
            continue;
        };
        let Some(name) = egcl_rt::symbols::registry_key(symbol) else {
            continue;
        };
        if super::builtin_binding::snapshot(symbol)
            != Some(super::builtin_binding::Binding::FixedStandard)
        {
            continue;
        }
        let other = if alias(f, data.args[0]) == phi {
            data.args[1]
        } else if name == "LOGAND" && alias(f, data.args[1]) == phi {
            data.args[0]
        } else {
            continue;
        };
        let Some(amount) = integer_constant(f, other) else {
            continue;
        };
        // These operations close over fixnums and either reduce integer width
        // or normalize into a bounded range. Left shift/addition do not.
        let range = match name.as_str() {
            "ASH" if (-63..=-1).contains(&amount) => None,
            "LOGAND" if (0..=(1i64 << 60) - 1).contains(&amount) => {
                Some(Range { lo: 0, hi: amount })
            }
            "MOD" if amount > 0 => Some(Range {
                lo: 0,
                hi: amount - 1,
            }),
            "REM" if amount > 0 => Some(Range {
                lo: 1 - amount,
                hi: amount - 1,
            }),
            _ => continue,
        };
        let state = f.frame_states.get(data.frame_state?);
        if benefit(state.scopes.last()?.bcp) {
            return Some((index, latch, range));
        }
    }
    None
}

fn integer_constant(f: &Function, value: Value) -> Option<i64> {
    let ValueDef::Result { inst, .. } = f.value(alias(f, value)).def else {
        return None;
    };
    match (&f.inst(inst).opcode, &f.inst(inst).aux) {
        (Opcode::ConstFixnum, AuxData::FixnumImm(value)) => Some(*value),
        _ => None,
    }
}

/// Reject a statically zero/one-trip monotone counter loop. Other trip counts
/// rely on observed type benefit rather than pretending dynamic bounds prove
/// a minimum number of iterations.
fn known_tiny_loop(f: &Function, header: Block, body: &[Block], latch: Block) -> bool {
    let Some(tail) = f.terminator(latch) else {
        return false;
    };
    let Some(back) = f
        .inst(tail)
        .targets
        .iter()
        .find(|edge| edge.block == header)
    else {
        return false;
    };
    let dom = f.dominators();
    for (index, &phi) in f.block(header).params.iter().enumerate() {
        let Some(&next) = back.args.get(index) else {
            continue;
        };
        let ValueDef::Result { inst, .. } = f.value(alias(f, next)).def else {
            continue;
        };
        let step_op = f.inst(inst);
        if step_op.args.len() != 2 || alias(f, step_op.args[0]) != phi {
            continue;
        }
        let Some(delta) = integer_constant(f, step_op.args[1]) else {
            continue;
        };
        let step = match (&step_op.opcode, &step_op.aux) {
            (Opcode::FixnumAdd, _) => delta,
            (Opcode::FixnumSub, _) => match delta.checked_neg() {
                Some(step) => step,
                None => continue,
            },
            (Opcode::Call | Opcode::Invoke, AuxData::CallTarget(s)) => {
                match egcl_rt::symbols::registry_key(*s).as_deref() {
                    Some("+") => delta,
                    Some("-") => match delta.checked_neg() {
                        Some(step) => step,
                        None => continue,
                    },
                    _ => continue,
                }
            }
            _ => continue,
        };
        if step != 1 && step != -1 {
            continue;
        }
        let mut starts = Vec::new();
        let mut unknown = false;
        for pred in f.preds(header).into_iter().filter(|p| !body.contains(p)) {
            if let Some(term) = f.terminator(pred) {
                for edge in &f.inst(term).targets {
                    if edge.block == header {
                        match edge.args.get(index).and_then(|v| integer_constant(f, *v)) {
                            Some(start) => starts.push(start),
                            None => unknown = true,
                        }
                    }
                }
            }
        }
        if unknown || starts.is_empty() {
            continue;
        }
        for &block in body {
            if !dom.dominates(block, latch) {
                continue;
            }
            let Some(term) = f.terminator(block) else {
                continue;
            };
            let branch = f.inst(term);
            if branch.opcode != Opcode::Brif || branch.targets.len() != 2 {
                continue;
            }
            let exits = [
                !body.contains(&branch.targets[0].block),
                !body.contains(&branch.targets[1].block),
            ];
            if exits[0] == exits[1] {
                continue;
            }
            let ValueDef::Result { inst, .. } = f.value(alias(f, branch.args[0])).def else {
                continue;
            };
            let cmp = f.inst(inst);
            if cmp.args.len() != 2 || alias(f, cmp.args[0]) != phi {
                continue;
            }
            let Some(bound) = integer_constant(f, cmp.args[1]) else {
                continue;
            };
            let name = match (&cmp.opcode, &cmp.aux) {
                (Opcode::FixnumCmpGe, _) => ">=",
                (Opcode::FixnumCmpGt, _) => ">",
                (Opcode::FixnumCmpLe, _) => "<=",
                (Opcode::FixnumCmpLt, _) => "<",
                (Opcode::Call | Opcode::Invoke, AuxData::CallTarget(s)) => {
                    match egcl_rt::symbols::registry_key(*s).as_deref() {
                        Some(">=") => ">=",
                        Some(">") => ">",
                        Some("<=") => "<=",
                        Some("<") => "<",
                        _ => continue,
                    }
                }
                _ => continue,
            };
            let offset = match (step, name, exits[0]) {
                (1, ">=", true) | (1, "<", false) | (-1, "<=", true) | (-1, ">", false) => 0,
                (1, ">", true) | (1, "<=", false) | (-1, "<", true) | (-1, ">=", false) => 1,
                _ => continue,
            };
            if starts.iter().all(|&start| {
                (i128::from(bound) - i128::from(start)) * i128::from(step) + offset <= 1
            }) {
                return true;
            }
        }
    }
    false
}

fn jump(f: &mut Function, from: Block, to: Block, args: Vec<Value>) {
    f.set_terminator(
        from,
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![BlockCall { block: to, args }],
            frame_state: None,
            source_pos: 0,
        },
    );
}

fn transform(
    f: &mut Function,
    header: Block,
    body: &[Block],
    parameter: usize,
    latch: Block,
    range: Option<Range>,
) -> Result<(), &'static str> {
    let outside: Vec<_> = f
        .preds(header)
        .into_iter()
        .filter(|b| !body.contains(b))
        .collect();
    if outside.is_empty() {
        return Err("no ordinary entry to peel");
    }
    let params = f.block(header).params.clone();
    let state_id = f
        .osr_entries
        .iter()
        .find(|entry| entry.block == header)
        .ok_or("missing loop state")?
        .frame_state;
    let peeled = clone_region(f, body)?;
    let fast = clone_region(f, body)?;
    let fast_blocks: Vec<_> = body.iter().map(|b| fast.block_map[b]).collect();
    let bridge = f.make_block();
    let bridge_params: Vec<_> = params
        .iter()
        .map(|&p| {
            let data = f.value(p).clone();
            f.add_block_param(bridge, data.ty, data.repr)
        })
        .collect();
    let narrowed_entry = f.make_block();
    let fix = IRType::of(TypeBits::FIXNUM);
    let (_, checked) = f.push_inst(
        bridge,
        InstData {
            opcode: Opcode::TypeCheck,
            args: vec![bridge_params[parameter]],
            results: vec![],
            aux: AuxData::TypeTag(fix),
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: 0,
        },
        &[(
            IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
            super::ir::ValueRepresentation::Tagged,
        )],
    );
    f.set_terminator(
        bridge,
        InstData {
            opcode: Opcode::Brif,
            args: vec![checked[0]],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![
                BlockCall {
                    block: narrowed_entry,
                    args: vec![],
                },
                BlockCall {
                    block: header,
                    args: bridge_params.clone(),
                },
            ],
            frame_state: None,
            source_pos: 0,
        },
    );
    let map: HashMap<_, _> = params
        .iter()
        .copied()
        .zip(bridge_params.iter().copied())
        .collect();
    let mut state = f.frame_states.get(state_id).clone();
    for source in state
        .scopes
        .iter_mut()
        .flat_map(|scope| scope.locals.iter_mut().chain(&mut scope.stack))
        .chain(state.remat.iter_mut().flat_map(|recipe| &mut recipe.inputs))
    {
        if let ValueSource::Value { value, .. } = source {
            if let Some(new) = map.get(value) {
                *value = *new;
            }
        }
    }
    let guard_state = f.frame_states.add(state);
    let (_, narrowed) = f.push_inst(
        narrowed_entry,
        InstData {
            opcode: Opcode::Guard,
            args: vec![bridge_params[parameter]],
            results: vec![],
            aux: AuxData::TypeTag(fix),
            flags: InstFlags {
                guard: true,
                effectful: true,
                ..InstFlags::default()
            },
            targets: vec![],
            frame_state: Some(guard_state),
            source_pos: 0,
        },
        &[(fix, super::ir::ValueRepresentation::Tagged)],
    );
    let mut arguments = bridge_params.clone();
    arguments[parameter] = narrowed[0];
    jump(f, narrowed_entry, fast.block_map[&header], arguments);
    // Range facts here follow from the canonical normalization operation, not
    // from the profile or the tag-only branch. Right shifts retain a general
    // fixnum fact unless an unsigned input bound was separately established.
    f.refine_type(fast.value_map[&params[parameter]], IRType { range, ..fix });
    for block in outside {
        let term = f.terminator(block).ok_or("entry missing terminator")?;
        for edge in &mut f.inst_mut(term).targets {
            if edge.block == header {
                edge.block = peeled.block_map[&header];
            }
        }
    }
    let peeled_tail = f
        .terminator(peeled.block_map[&latch])
        .ok_or("peeled latch missing terminator")?;
    for edge in &mut f.inst_mut(peeled_tail).targets {
        if edge.block == peeled.block_map[&header] {
            edge.block = bridge;
        }
    }
    super::speculate::speculate_blocks(f, &fast_blocks);
    let fast_term = f
        .terminator(fast.block_map[&latch])
        .ok_or("fast latch missing terminator")?;
    let edge = f
        .inst(fast_term)
        .targets
        .iter()
        .find(|edge| edge.block == fast.block_map[&header])
        .ok_or("fast latch missing edge")?;
    let forwarded = edge.args[parameter];
    let result = alias(f, forwarded);
    if !fixnum(f, result) {
        return Err("typed remainder does not close its carried type");
    }
    f.refine_type(forwarded, fix);
    repair_outside_uses(f, body, &[peeled, fast])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState};
    use crate::t2::ir::{OsrEntry, ValueRepresentation};

    fn data(opcode: Opcode, args: Vec<Value>) -> InstData {
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
    fn constant(f: &mut Function, b: Block, n: i64) -> Value {
        let mut d = data(Opcode::ConstFixnum, vec![]);
        d.aux = AuxData::FixnumImm(n);
        f.push_inst(
            b,
            d,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0]
    }
    fn state(
        f: &mut Function,
        bcp: u32,
        values: &[Value],
    ) -> super::super::frame_state::FrameStateId {
        f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp,
                locals: values
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        })
    }
    fn loop_ir(amount: i64) -> Function {
        let mut f = Function::new("integer-transition");
        let entry = f.entry();
        let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        let n = f.add_block_param(
            entry,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
        f.mark_entry_param_checked(n);
        let zero = constant(&mut f, entry, 0);
        let one = constant(&mut f, entry, 1);
        let shift = constant(&mut f, entry, amount);
        let header = f.make_block();
        let body = f.make_block();
        let exit = f.make_block();
        let remaining = f.add_block_param(
            header,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        );
        let lane = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
        jump(&mut f, entry, header, vec![n, input]);
        let condition = f
            .push_inst(
                header,
                data(Opcode::FixnumCmpGt, vec![remaining, zero]),
                &[(
                    IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL)),
                    ValueRepresentation::Tagged,
                )],
            )
            .1[0];
        let mut branch = data(Opcode::Brif, vec![condition]);
        branch.targets = vec![
            BlockCall {
                block: body,
                args: vec![],
            },
            BlockCall {
                block: exit,
                args: vec![],
            },
        ];
        f.set_terminator(header, branch);
        let fs = state(&mut f, 7, &[remaining, lane]);
        let mut call = data(Opcode::Call, vec![lane, shift]);
        call.aux = AuxData::CallTarget(egcl_rt::symbols::intern("ASH"));
        call.frame_state = Some(fs);
        call.flags = InstFlags {
            call: true,
            effectful: true,
            safepoint: true,
            ..InstFlags::default()
        };
        let next = f
            .push_inst(body, call, &[(IRType::TOP, ValueRepresentation::Tagged)])
            .1[0];
        let mut decrement = data(Opcode::FixnumSub, vec![remaining, one]);
        decrement.frame_state = Some(fs);
        decrement.flags = InstFlags {
            guard: true,
            effectful: true,
            ..InstFlags::default()
        };
        let left = f
            .push_inst(
                body,
                decrement,
                &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
            )
            .1[0];
        jump(&mut f, body, header, vec![left, next]);
        f.set_terminator(exit, data(Opcode::Return, vec![lane]));
        let fs = state(&mut f, 4, &[remaining, lane]);
        f.osr_entries.push(OsrEntry {
            control_scopes: vec![],
            bcp: 4,
            block: header,
            frame_state: fs,
            checks: vec![],
        });
        crate::t2::verify::verify(&f).unwrap();
        f
    }
    #[test]
    fn mixed_right_shift_peels_only_the_first_iteration_and_types_the_remainder() {
        let mut f = loop_ir(-8);
        let osr = f.osr_entries[0].block;
        let stats = peel_for_numeric_types(&mut f, &|bcp| bcp == 7, PeelOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        assert!(stats.declined.is_empty());
        crate::t2::verify::verify(&f).unwrap();
        let ops: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter())
            .map(|&i| f.inst(i).opcode)
            .collect();
        assert_eq!(
            ops.iter().filter(|&&op| op == Opcode::Call).count(),
            2,
            "generic first iteration and fallback"
        );
        assert_eq!(ops.iter().filter(|&&op| op == Opcode::FixnumShl).count(), 1);
        assert_eq!(ops.iter().filter(|&&op| op == Opcode::TypeCheck).count(), 1);
        assert_eq!(f.osr_entries[0].block, osr, "OSR retains its generic route");
    }
    #[test]
    fn no_type_benefit_or_excessive_growth_leaves_the_loop_unchanged() {
        let mut f = loop_ir(-8);
        let before = format!("{f:?}");
        assert_eq!(
            peel_for_numeric_types(&mut f, &|_| false, PeelOptions::default()).loops,
            0
        );
        assert_eq!(format!("{f:?}"), before);
        assert_eq!(
            peel_for_numeric_types(
                &mut f,
                &|_| true,
                PeelOptions {
                    max_loop_instructions: 0,
                    ..PeelOptions::default()
                }
            )
            .loops,
            0
        );
        assert_eq!(format!("{f:?}"), before);
        let mut expanding = loop_ir(8);
        let before = format!("{expanding:?}");
        assert_eq!(
            peel_for_numeric_types(&mut expanding, &|_| true, PeelOptions::default()).loops,
            0,
            "left shift does not preserve fixnum inputs"
        );
        assert_eq!(format!("{expanding:?}"), before);
    }
    #[test]
    fn bounded_normalizer_exposes_a_range_without_assuming_the_initial_type() {
        let mut f = loop_ir(255);
        let calls: Vec<_> = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .filter(|&i| matches!(f.inst(i).aux, AuxData::CallTarget(_)))
            .collect();
        for call in calls {
            f.inst_mut(call).aux = AuxData::CallTarget(egcl_rt::symbols::intern("LOGAND"));
        }
        let stats = peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default());
        assert_eq!(stats.loops, 1, "{stats:?}");
        crate::t2::verify::verify(&f).unwrap();
        assert!(
            (0..f.num_values()).any(|i| {
                let value = f.value(Value(i as u32));
                matches!(value.def, ValueDef::Param { .. })
                    && value.ty.range == Some(Range { lo: 0, hi: 255 })
            }),
            "the normalized remainder phi has byte bounds"
        );
    }

    #[test]
    fn known_one_trip_loops_are_not_profitable() {
        let mut f = loop_ir(-8);
        let entry = f.entry();
        let one = f
            .block(entry)
            .insts
            .iter()
            .flat_map(|&i| &f.inst(i).results)
            .find(|&&value| integer_constant(&f, value) == Some(1))
            .copied()
            .unwrap();
        let term = f.terminator(entry).unwrap();
        f.inst_mut(term).targets[0].args[0] = one;
        let before = format!("{f:?}");
        assert_eq!(
            peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default()).loops,
            0
        );
        assert_eq!(format!("{f:?}"), before);
    }

    #[test]
    fn two_backedges_from_the_same_latch_are_declined() {
        let mut f = loop_ir(-8);
        let header = f.osr_entries[0].block;
        let body = f.succs(header)[0];
        let branch = f.inst(f.terminator(header).unwrap()).args[0];
        let old = f.inst(f.terminator(body).unwrap()).clone();
        let mut twice = data(Opcode::Brif, vec![branch]);
        twice.targets = vec![old.targets[0].clone(), old.targets[0].clone()];
        f.block_mut(body).insts.pop();
        f.set_terminator(body, twice);
        crate::t2::verify::verify(&f).unwrap();
        let before = format!("{f:?}");
        assert_eq!(
            peel_for_numeric_types(&mut f, &|_| true, PeelOptions::default()).loops,
            0
        );
        assert_eq!(format!("{f:?}"), before);
    }
}
