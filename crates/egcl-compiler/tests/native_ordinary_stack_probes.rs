// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{emit::*, frame_state::*, ir::*};
use iced_x86::{Decoder, DecoderOptions, Mnemonic, Register};

fn instruction(opcode: Opcode, args: Vec<Value>, aux: AuxData) -> InstData {
    InstData {
        opcode,
        args,
        results: vec![],
        aux,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

#[test]
fn large_ordinary_frames_and_outgoing_areas_touch_each_stack_page() {
    let mut f = Function::new("ordinary-large-outgoing-area");
    let block = f.entry();
    let argument = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
    let mut guard = instruction(
        Opcode::Guard,
        vec![argument],
        AuxData::TypeTag(IRType::of(TypeBits::FIXNUM)),
    );
    guard.flags = InstFlags {
        guard: true,
        effectful: true,
        ..Default::default()
    };
    guard.frame_state = Some(f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 76,
            bcp: 0,
            locals: vec![
                ValueSource::Value {
                    value: argument,
                    repr: ValueRepresentation::Tagged
                };
                1030
            ],
            stack: vec![],
        }],
        remat: vec![],
    }));
    f.push_inst(
        block,
        guard,
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    let (_, values) = f.push_inst(
        block,
        instruction(Opcode::ConstFixnum, vec![], AuxData::FixnumImm(42)),
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    let mut call = instruction(Opcode::Call, vec![values[0]; 1030], AuxData::CallTarget(77));
    call.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    call.frame_state = Some(f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 76,
            bcp: 0,
            locals: vec![],
            stack: vec![],
        }],
        remat: vec![],
    }));
    let (site, result) = f.push_inst(
        block,
        call,
        &[(IRType::of(TypeBits::TOP), ValueRepresentation::Tagged)],
    );
    f.set_terminator(block, instruction(Opcode::Return, result, AuxData::None));
    let code = emit_framed_ordinary_function(
        &f,
        OrdinaryLispEntryOptions {
            require_callable_home: false,
            arity: 1,
            arity_error_veneer: 1,
            binding: None,
        },
        &[(
            site,
            OrdinaryCallTarget {
                symbol: 77,
                descriptor_slot: 1,
                revision_address: 1,
                heat_slot: 0,
                function_cell: 0,
            },
        )],
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
    )
    .unwrap();
    let mut decoder = Decoder::new(64, &code.code, DecoderOptions::NONE);
    let mut pending_page = false;
    let mut probes = 0;
    while decoder.can_decode() {
        let instruction = decoder.decode();
        assert!(!instruction.is_invalid());
        if pending_page {
            assert_eq!(
                instruction.mnemonic(),
                Mnemonic::Test,
                "page reservation must immediately touch its new stack home"
            );
            assert_eq!(instruction.memory_base(), Register::RSP);
            assert_eq!(instruction.memory_displacement64(), 0);
            probes += 1;
            pending_page = false;
        }
        if instruction.mnemonic() == Mnemonic::Sub && instruction.op0_register() == Register::RSP {
            let bytes = instruction.immediate(1);
            assert!(
                bytes <= 4096,
                "ordinary stack reservation skips guard pages: {bytes} bytes"
            );
            pending_page = bytes == 4096;
        }
    }
    assert!(!pending_page);
    assert!(
        probes >= 6,
        "native frame, outgoing arguments, and guard recovery each span multiple pages"
    );
}
