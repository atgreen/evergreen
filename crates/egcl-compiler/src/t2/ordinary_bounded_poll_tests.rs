// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::ir::{
    AuxData, BlockCall, BoundedPollScope, Function, IRType, InstData, InstFlags, Opcode,
    ValueRepresentation,
};
use crate::t2::transfer_sites::SysvTransferTable;
use crate::t2::x64_calls::{NativeCallOrigin, NativeCallValues};

#[test]
fn ordinary_bounded_polls_retain_exact_recovery_and_native_roots_without_transfer_mode() {
    let mut function = Function::new("ordinary-bounded-poll-maps");
    let entry = function.entry();
    let exit = function.make_block();
    let argument = function.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let symbol = egcl_rt::symbols::intern("ORDINARY-BOUNDED-POLL-MAPS");
    let state = function.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: symbol,
            bcp: 7,
            locals: vec![ValueSource::Value {
                value: argument,
                repr: ValueRepresentation::Tagged,
            }],
            stack: vec![],
        }],
        remat: vec![],
    });
    for (block, done) in [(entry, false), (exit, true)] {
        function.push_inst(
            block,
            InstData {
                opcode: Opcode::NativePoll,
                args: vec![],
                results: vec![],
                aux: AuxData::BoundedPoll {
                    exit: done,
                    scope: BoundedPollScope {
                        function: symbol,
                        bcp: 7,
                        control_scopes: vec![],
                    },
                },
                flags: InstFlags {
                    effectful: true,
                    safepoint: true,
                    ..Default::default()
                },
                frame_state: Some(state),
                targets: vec![],
                source_pos: 0,
            },
            &[],
        );
    }
    function.set_terminator(
        entry,
        InstData {
            opcode: Opcode::Jump,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            frame_state: None,
            targets: vec![BlockCall {
                block: exit,
                args: vec![],
            }],
            source_pos: 0,
        },
    );
    function.set_terminator(
        exit,
        InstData {
            opcode: Opcode::Return,
            args: vec![argument],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            frame_state: None,
            targets: vec![],
            source_pos: 0,
        },
    );
    assert!(function.osr_entries.is_empty());
    crate::t2::verify::verify(&function).unwrap();
    // This API selects the ordinary-call path, where transfer_mode is false.
    // Addresses are emission-only placeholders; no generated code is executed.
    let mut code = emit_framed_ordinary_function_with_poll(
        &function,
        OrdinaryLispEntryOptions {
            require_callable_home: false,
            arity: 1,
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
    assert!(code.ordinary_entry.is_some());
    assert!(code.frame_entry.is_none());
    assert!(
        code.osr_entries.is_empty(),
        "explicit polls must not become OSR imports"
    );
    let calls = code.native_calls.as_ref().unwrap();
    let offsets = calls
        .iter()
        .filter_map(|site| match site.origin {
            NativeCallOrigin::BoundedLoopEntry(_) | NativeCallOrigin::BoundedLoopExit(_) => {
                assert!(matches!(
                    calls.value_map(site.return_offset),
                    Some(NativeCallValues::Frame(_))
                ));
                Some(site.return_offset)
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(offsets.len(), 2);
    assert_eq!(code.native_poll_transfer_sites.len(), 2);
    for site in &code.native_poll_transfer_sites {
        assert_eq!(site.activation_slots, 0, "ordinary polls own native homes");
        assert_eq!(site.map.origin_bcp, 7);
        assert_eq!(site.map.frames[0].function, symbol);
        assert_eq!(site.map.frames[0].num_locals, 1);
        assert!(
            !site.map.roots.is_empty(),
            "the live tagged input must have a recoverable root"
        );
    }
    // The runtime installs this exact table, rather than an empty ordinary one.
    let table = SysvTransferTable::new(
        code.code.len(),
        std::mem::take(&mut code.native_poll_transfer_sites),
    )
    .unwrap();
    assert_eq!(
        table
            .sites()
            .map(|site| site.return_offset() as usize)
            .collect::<Vec<_>>(),
        offsets
    );
}
