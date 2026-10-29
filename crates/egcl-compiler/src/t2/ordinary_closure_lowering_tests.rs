// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};

fn plain(opcode: Opcode) -> InstData {
    InstData {
        opcode,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

fn environment_function() -> (Function, crate::t2::ir::Inst, crate::t2::ir::Inst) {
    let mut f = Function::new("ordinary-captured-context");
    let entry = f.entry();
    let next = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 42,
            bcp: 3,
            locals: vec![ValueSource::Value {
                value: next,
                repr: ValueRepresentation::Tagged,
            }],
            stack: vec![],
        }],
        remat: vec![],
    });
    let (store, _) = f.push_inst(
        entry,
        InstData {
            args: vec![next],
            aux: AuxData::EnvironmentName(7),
            frame_state: Some(state),
            flags: InstFlags {
                effectful: true,
                call: true,
                safepoint: true,
                ..Default::default()
            },
            ..plain(Opcode::SetEnvironmentValue)
        },
        &[],
    );
    let (read, value) = f.push_inst(
        entry,
        InstData {
            aux: AuxData::EnvironmentName(7),
            frame_state: Some(state),
            flags: InstFlags {
                effectful: true,
                call: true,
                safepoint: true,
                ..Default::default()
            },
            ..plain(Opcode::EnvironmentValue)
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    f.set_terminator(
        entry,
        InstData {
            args: vec![value[0]],
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            ..plain(Opcode::Return)
        },
    );
    (f, store, read)
}

#[test]
fn captured_reads_and_writes_share_exact_frame_callable_and_original_continuations() {
    let (mut f, store, read) = environment_function();
    crate::t2::verify::verify(&f).unwrap();
    let before = [f.inst(store).frame_state, f.inst(read).frame_state];
    assert_eq!(
        lower_ordinary_primitives(&mut f, |primitive| Some(match primitive {
            OrdinaryPrimitive::LoadEnvironment => 101,
            OrdinaryPrimitive::StoreEnvironment => 102,
            _ => 103,
        }))
        .unwrap(),
        2
    );
    crate::t2::verify::verify(&f).unwrap();
    let hidden: Vec<_> = f
        .block(f.entry())
        .insts
        .iter()
        .copied()
        .filter(|&inst| f.inst(inst).opcode == Opcode::CurrentCallable)
        .collect();
    assert_eq!(hidden.len(), 1);
    let callable = f.inst(hidden[0]).results[0];
    for (site, expected_args, target, original) in
        [(store, 3, 102, before[0]), (read, 2, 101, before[1])]
    {
        let data = f.inst(site);
        assert_eq!(data.opcode, Opcode::Call);
        assert_eq!(data.args.len(), expected_args);
        assert_eq!(data.args[0], callable);
        assert!(matches!(data.aux, AuxData::CallTarget(actual) if actual == target));
        assert_eq!(data.frame_state, original);
        assert!(data.flags.call && data.flags.safepoint && data.flags.effectful);
    }
}

#[test]
fn missing_environment_continuation_declines_transactionally() {
    let (mut f, store, _) = environment_function();
    f.inst_mut(store).frame_state = None;
    let before = format!("{f:?}");
    assert!(lower_ordinary_primitives(&mut f, |_| Some(101)).is_err());
    assert_eq!(format!("{f:?}"), before);
}

#[test]
fn hidden_callable_cannot_forge_a_numeric_fact() {
    let mut f = Function::new("bad-callable-proof");
    let entry = f.entry();
    let (_, values) = f.push_inst(
        entry,
        plain(Opcode::CurrentCallable),
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    f.set_terminator(
        entry,
        InstData {
            args: vec![values[0]],
            flags: InstFlags {
                terminator: true,
                ..Default::default()
            },
            ..plain(Opcode::Return)
        },
    );
    let errors = crate::t2::verify::verify(&f).unwrap_err();
    assert!(
        errors
            .iter()
            .any(|error| error.check == "V8 current-callable-contract")
    );
}
