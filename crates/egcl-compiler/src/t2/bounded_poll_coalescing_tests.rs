// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::ir::{
    AuxData, Block, BlockCall, BoundedPollScope, IRType, InstData, InstFlags, Opcode, OsrEntry,
    ValueRepresentation,
};
use std::collections::HashSet;

struct Envelope {
    f: Function,
    blocks: Vec<Block>,
    outer: Block,
    pre: Block,
    probe: Block,
    success: Block,
    post: Block,
    scalar: Block,
}
fn inst(opcode: Opcode) -> InstData {
    InstData {
        opcode,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        frame_state: None,
        targets: vec![],
        source_pos: 0,
    }
}
fn edge(
    f: &mut Function,
    from: Block,
    opcode: Opcode,
    args: Vec<crate::t2::ir::Value>,
    to: &[Block],
) {
    let mut d = inst(opcode);
    d.args = args;
    d.targets = to
        .iter()
        .map(|&block| BlockCall {
            block,
            args: vec![],
        })
        .collect();
    f.set_terminator(from, d);
}
fn envelope() -> Envelope {
    let mut f = Function::new("bounded-poll-envelope-nested-loop");
    let entry = f.entry();
    let values: Vec<_> = (0..4)
        .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    let outer = f.make_block();
    let pre = f.make_block();
    let probe = f.make_block();
    let success = f.make_block();
    let post = f.make_block();
    let slow = f.make_block();
    let scalar = f.make_block();
    let exit = f.make_block();
    let symbol = egcl_rt::symbols::intern("BOUNDED-POLL-ENVELOPE-NESTED-LOOP");
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: symbol,
            bcp: 7,
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
    });
    for (block, done) in [(pre, false), (post, true)] {
        let mut d = inst(Opcode::NativePoll);
        d.aux = AuxData::BoundedPoll {
            exit: done,
            scope: BoundedPollScope {
                function: symbol,
                bcp: 7,
                control_scopes: vec![],
            },
        };
        d.flags = InstFlags {
            effectful: true,
            safepoint: true,
            ..Default::default()
        };
        d.frame_state = Some(state);
        f.push_inst(block, d, &[]);
    }
    edge(&mut f, entry, Opcode::Jump, vec![], &[outer]);
    edge(&mut f, outer, Opcode::Brif, vec![values[0]], &[pre, exit]);
    edge(&mut f, pre, Opcode::Jump, vec![], &[probe]);
    let mut d = inst(Opcode::TryStoreIntegerOctets8);
    d.args = values.clone();
    d.flags.effectful = true;
    let (_, outputs) = f.push_inst(
        probe,
        d,
        &[
            (
                IRType::of(crate::t2::ir::TypeBits::SYMBOL.join(crate::t2::ir::TypeBits::NULL)),
                ValueRepresentation::Tagged,
            ),
            (
                IRType::of(crate::t2::ir::TypeBits::FIXNUM),
                ValueRepresentation::Tagged,
            ),
        ],
    );
    edge(
        &mut f,
        probe,
        Opcode::Brif,
        vec![outputs[0]],
        &[success, slow],
    );
    let mut clear = inst(Opcode::ClearMv);
    clear.flags = InstFlags {
        call: true,
        effectful: true,
        safepoint: true,
        ..Default::default()
    };
    clear.frame_state = Some(state);
    f.push_inst(success, clear, &[]);
    edge(&mut f, success, Opcode::Jump, vec![], &[post]);
    edge(&mut f, post, Opcode::Jump, vec![], &[outer]);
    edge(&mut f, slow, Opcode::Jump, vec![], &[scalar]);
    edge(
        &mut f,
        scalar,
        Opcode::Brif,
        vec![values[0]],
        &[scalar, outer],
    );
    edge(&mut f, exit, Opcode::Return, vec![values[0]], &[]);
    let blocks = f.reverse_postorder();
    crate::t2::verify::verify(&f).unwrap();
    Envelope {
        f,
        blocks,
        outer,
        pre,
        probe,
        success,
        post,
        scalar,
    }
}
fn candidates(e: &Envelope, heat: &[Block]) -> HashSet<Block> {
    redundant_bounded_cycle_polls(
        &e.f,
        &e.blocks,
        &e.blocks.iter().copied().collect(),
        &heat.iter().copied().collect(),
    )
}

#[test]
fn nested_bounded_envelope_removes_only_implicit_intermediate_polls() {
    let e = envelope();
    assert_eq!(
        candidates(&e, &[e.outer, e.scalar]),
        HashSet::from([e.probe, e.success])
    );
    let code = emit_framed_ordinary_function_with_poll(
        &e.f,
        OrdinaryLispEntryOptions {
            require_callable_home: false,
            arity: 4,
            arity_error_veneer: 1,
            binding: None,
        },
        &[],
        OrdinaryCallFlags {
            gc_requested: 1,
            sigusr1_pending: 1,
            posted_signal: 1,
            native_errors: 1,
            signal_epoch: 1,
        },
        1,
        OrdinaryGuardOptions {
            veneer: 1,
            multiple_values: 1,
        },
        1,
    )
    .unwrap();
    let origins: Vec<_> = code
        .native_calls
        .as_ref()
        .unwrap()
        .iter()
        .map(|site| site.origin)
        .collect();
    use crate::t2::x64_calls::NativeCallOrigin;
    for block in [e.probe, e.success] {
        assert!(
            !origins.contains(&NativeCallOrigin::LoopPoll(block)),
            "redundant implicit poll retained at {block:?}: {origins:?}"
        );
    }
    for block in [e.outer, e.scalar] {
        assert!(
            origins.contains(&NativeCallOrigin::LoopPoll(block)),
            "required cycle poll lost: {origins:?}"
        );
    }
    assert_eq!(
        origins
            .iter()
            .filter(|origin| matches!(origin, NativeCallOrigin::BoundedLoopEntry(_)))
            .count(),
        1
    );
    assert_eq!(
        origins
            .iter()
            .filter(|origin| matches!(origin, NativeCallOrigin::BoundedLoopExit(_)))
            .count(),
        1
    );
    assert_eq!(code.native_poll_transfer_sites.len(), 2);
}

#[test]
fn bounded_poll_coalescing_preserves_heat_and_osr_sites() {
    let mut e = envelope();
    assert_eq!(
        candidates(&e, &[e.outer, e.scalar, e.probe]),
        HashSet::from([e.success])
    );
    let state = e.f.inst(e.f.block(e.pre).insts[0]).frame_state.unwrap();
    e.f.osr_entries.push(OsrEntry {
        control_scopes: vec![],
        bcp: 7,
        block: e.success,
        frame_state: state,
        checks: vec![],
    });
    assert_eq!(
        candidates(&e, &[e.outer, e.scalar]),
        HashSet::from([e.probe])
    );
}

#[test]
fn bounded_poll_coalescing_rejects_escape_call_extra_effect_and_scope_mismatch() {
    for variant in 0..6 {
        let mut e = envelope();
        match variant {
            0 => {
                // Alternate entry bypassing the exact PRE boundary.
                let extra = e.f.make_block();
                edge(&mut e.f, extra, Opcode::Jump, vec![], &[e.probe]);
                e.blocks.push(extra);
            }
            1 | 2 => {
                let mut d = inst(if variant == 1 {
                    Opcode::Call
                } else {
                    Opcode::ClearMv
                });
                d.flags.effectful = true;
                d.flags.call = variant == 1;
                let (added, _) = e.f.push_inst(e.probe, d, &[]);
                let body = &mut e.f.block_mut(e.probe).insts;
                assert_eq!(body.pop(), Some(added));
                body.insert(0, added);
            }
            3 => {
                let i = e.f.block(e.post).insts[0];
                let AuxData::BoundedPoll { scope, .. } = &mut e.f.inst_mut(i).aux else {
                    unreachable!()
                };
                scope.bcp += 1;
            }
            4 => {
                // Fallback can execute work before any retained poll.
                let polls = HashSet::from([e.pre, e.probe, e.success, e.post]);
                assert!(
                    redundant_bounded_cycle_polls(&e.f, &e.blocks, &polls, &HashSet::new())
                        == HashSet::from([e.success])
                );
                continue;
            }
            5 => {
                let d = inst(Opcode::ClearMv);
                let (added, _) = e.f.push_inst(e.success, d, &[]);
                let body = &mut e.f.block_mut(e.success).insts;
                assert_eq!(body.pop(), Some(added));
                body.insert(0, added);
            }
            _ => unreachable!(),
        }
        let expected = if variant == 1 || variant == 2 {
            HashSet::from([e.success])
        } else {
            HashSet::new()
        };
        assert_eq!(
            candidates(&e, &[e.outer, e.scalar]),
            expected,
            "variant {variant}"
        );
    }
}

#[test]
fn bounded_poll_coalescing_handles_already_removed_mv_reset() {
    let mut e = envelope();
    e.f.block_mut(e.success).insts.remove(0);
    assert_eq!(
        candidates(&e, &[e.outer, e.scalar]),
        HashSet::from([e.probe, e.success])
    );
}
