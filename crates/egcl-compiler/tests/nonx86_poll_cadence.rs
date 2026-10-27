// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Finite loops distinguish every-selected-backedge polling from the sampled
//! cadence used on System Z. Execute this test on each target (QEMU proves
//! cadence, not native latency).

use egcl_compiler::t2::{
    emit::FramedCode,
    frame_state::{FrameScope, FrameState, ValueSource},
    ir::{
        AuxData, BlockCall, Function, IRType, InstData, InstFlags, Opcode, OsrEntry, TypeBits,
        ValueRepresentation,
    },
};
use egcl_rt::value::{EgclVal, NIL};
use std::cell::Cell;

const INITIAL_ROOT: u64 = 0x1001;
const MOVED_BY: u64 = 0x10000;
const ITERATIONS: usize = 128;
const S390X_POLL_INTERVAL: usize = 64;
const ROOTS: usize = 20;
const ACTIVATION_SLOTS: usize = ROOTS + 2;

fn instruction(opcode: Opcode, args: Vec<egcl_compiler::t2::ir::Value>) -> InstData {
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

fn finite_loop() -> Function {
    let mut f = Function::new("finite-poll-cadence");
    let entry = f.entry();
    let header = f.make_block();
    let body = f.make_block();
    let exit = f.make_block();
    let same = f.make_block();
    let different = f.make_block();
    let mut params = |block| {
        let mut values = vec![f.add_block_param(
            block,
            IRType::of(TypeBits::FIXNUM),
            ValueRepresentation::Tagged,
        )];
        values.extend(
            (0..ROOTS).map(|_| f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged)),
        );
        values
    };
    let inputs = params(entry);
    let carried = params(header);
    let sources = params(body);
    // This value stays live without appearing in a backward-edge argument list.
    let through = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let result = f.add_block_param(exit, IRType::TOP, ValueRepresentation::Tagged);
    let mut state = |values: &[egcl_compiler::t2::ir::Value]| {
        f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 73,
                bcp: 12,
                locals: values
                    .iter()
                    .copied()
                    .chain([through])
                    .map(|value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
                stack: vec![],
            }],
            remat: vec![],
        })
    };
    let header_state = state(&carried);
    let body_state = state(&sources);
    f.osr_entries.push(OsrEntry {
        control_scopes: vec![],
        block: header,
        bcp: 12,
        frame_state: header_state,
        checks: vec![],
    });
    let mut jump = instruction(Opcode::Jump, vec![]);
    jump.targets.push(BlockCall {
        block: header,
        args: inputs,
    });
    f.set_terminator(entry, jump);
    let mut constant = instruction(Opcode::ConstFixnum, vec![]);
    constant.aux = AuxData::FixnumImm(1);
    let (_, one) = f.push_inst(
        header,
        constant,
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    let mut decrement = instruction(Opcode::FixnumSub, vec![sources[0], one[0]]);
    decrement.frame_state = Some(body_state);
    let (_, next) = f.push_inst(
        body,
        decrement,
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    let mut compare = instruction(Opcode::FixnumCmpGe, vec![carried[0], one[0]]);
    compare.frame_state = Some(header_state);
    let (_, more) = f.push_inst(
        header,
        compare,
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let mut branch = instruction(Opcode::Brif, vec![more[0]]);
    branch.targets = vec![
        BlockCall {
            block: body,
            args: carried.clone(),
        },
        BlockCall {
            block: exit,
            args: vec![carried[1]],
        },
    ];
    f.set_terminator(header, branch);
    // Distinct source/destination phi values and a rotating parallel copy force
    // the poll map to describe homes BEFORE the lowered anonymous edge moves.
    let mut rotated = vec![next[0]];
    rotated.extend_from_slice(&sources[2..]);
    rotated.push(sources[1]);
    let mut jump = instruction(Opcode::Jump, vec![]);
    jump.targets.push(BlockCall {
        block: header,
        args: rotated,
    });
    f.set_terminator(body, jump);
    let mut branch = instruction(Opcode::Brif, vec![through]);
    branch.targets = vec![
        BlockCall {
            block: same,
            args: vec![],
        },
        BlockCall {
            block: different,
            args: vec![],
        },
    ];
    f.set_terminator(exit, branch);
    f.set_terminator(same, instruction(Opcode::Return, vec![result]));
    f.set_terminator(different, instruction(Opcode::Return, vec![through]));
    f
}

thread_local! {
    static STATE: Cell<(*mut u64, usize, usize, bool)> = const {
        Cell::new((std::ptr::null_mut(), 0, 0, false))
    };
}

extern "C" fn poll() -> u64 {
    STATE.with(|state| {
        let (slots, length, count, exit_first) = state.get();
        let expected = INITIAL_ROOT + count as u64 * MOVED_BY;
        let mut found = [false; ROOTS + 1];
        // Simulate relocation of every published root, including the root live
        // through the loop. Stale/undefined destination homes must not be scanned.
        for index in ACTIVATION_SLOTS..length {
            let slot = unsafe { &mut *slots.add(index) };
            if *slot & 7 != 1 {
                continue;
            }
            let delta = slot
                .checked_sub(expected)
                .expect("stale phi destination published");
            assert!(
                delta % 16 == 0 && delta / 16 <= ROOTS as u64,
                "unexpected root home published: {slot:#x}"
            );
            found[(delta / 16) as usize] = true;
            *slot += MOVED_BY;
        }
        assert!(
            found.into_iter().all(|present| present),
            "each poll must publish edge sources and the live-through root"
        );
        state.set((slots, length, count + 1, exit_first));
        u64::from(exit_first)
    })
}

extern "C" fn unexpected_deopt(_: u64, _: u64, _: *const u64, _: u64) {
    panic!("bounded fixnum loop must not deoptimize");
}

fn verify(compiled: FramedCode, execute: bool, poll_interval: usize) {
    assert_eq!(compiled.root_sync_sites.len(), 1);
    // System Z omits proven-immediate counter word. Every backend
    // must exclude the not-yet-defined destination phi homes.
    assert!(
        [ROOTS + 1, ROOTS + 2].contains(&usize::from(compiled.root_sync_sites[0].live_roots)),
        "poll map includes extra destination phi homes: {}",
        compiled.root_sync_sites[0].live_roots
    );
    assert_eq!(compiled.osr_entries.len(), 1);
    if !execute {
        return;
    }
    let buffer = egcl_rt::jit::JitBuffer::new(&compiled.code).unwrap();
    for offset in [0, compiled.osr_entries[0].1] {
        let entry: extern "C" fn(*mut u64) -> u64 =
            unsafe { std::mem::transmute(buffer.as_ptr().add(offset)) };
        for exit_first in [false, true] {
            let mut slots = vec![NIL.0; ACTIVATION_SLOTS + usize::from(compiled.shadow_root_slots)];
            slots[0] = EgclVal::from_fixnum(ITERATIONS as i64).0;
            for (index, slot) in slots[1..ACTIVATION_SLOTS].iter_mut().enumerate() {
                *slot = INITIAL_ROOT + index as u64 * 16;
            }
            STATE.with(|state| state.set((slots.as_mut_ptr(), slots.len(), 0, exit_first)));
            let result = entry(slots.as_mut_ptr());
            let count = STATE.with(|state| state.replace((std::ptr::null_mut(), 0, 0, false)).2);
            let expected_polls = ITERATIONS / poll_interval;
            assert_eq!(
                count,
                if exit_first { 1 } else { expected_polls },
                "the loop must poll at the backend's selected-edge cadence"
            );
            assert_eq!(
                result,
                if exit_first {
                    NIL.0
                } else {
                    INITIAL_ROOT
                        + (ITERATIONS % ROOTS) as u64 * 16
                        + expected_polls as u64 * MOVED_BY
                },
                "normal edges reload rewritten roots; a pending request exits on the first edge"
            );
        }
    }
}

macro_rules! backend {
    ($name:ident, $module:ident, $native:expr, $poll_interval:expr) => {
        #[test]
        fn $name() {
            use egcl_compiler::t2::$module::{RuntimeCalls, emit_framed_with_runtime};
            let compiled = emit_framed_with_runtime(
                &finite_loop(),
                unexpected_deopt as *const () as u64,
                ACTIVATION_SLOTS as u16,
                RuntimeCalls {
                    poll: poll as *const () as u64,
                    ..RuntimeCalls::default()
                },
            )
            .expect("finite poll loop must compile");
            verify(compiled, $native, $poll_interval);
        }
    };
}
backend!(
    aarch64_finite_loop_polls_every_selected_edge,
    emit_a64,
    cfg!(target_arch = "aarch64"),
    1
);
backend!(
    ppc64le_finite_loop_polls_every_selected_edge,
    emit_ppc64le,
    cfg!(all(target_arch = "powerpc64", target_endian = "little")),
    1
);
backend!(
    riscv64_finite_loop_polls_every_selected_edge,
    emit_riscv64,
    cfg!(target_arch = "riscv64"),
    1
);
backend!(
    s390x_finite_loop_polls_on_sampled_cadence,
    emit_s390x,
    cfg!(target_arch = "s390x"),
    S390X_POLL_INTERVAL
);
