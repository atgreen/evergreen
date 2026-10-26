// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
#![cfg(all(target_arch = "x86_64", target_os = "linux"))]

use egcl_compiler::t2::{
    emit::emit_framed,
    frame_state::{FrameScope, FrameState, ValueSource},
    ir::*,
    lower::lower,
    regalloc::allocate_framed,
    x64_frame::select_frame_homes,
};
use egcl_rt::{EgclVal, jit::JitBuffer};
use std::cell::Cell;

thread_local! { static DEOPTED: Cell<bool> = const { Cell::new(false) }; }
extern "C" fn deopt(_: u64, _: u64, _: *const u64) -> u64 {
    DEOPTED.with(|flag| flag.set(true));
    egcl_rt::value::NIL.0
}
extern "C" fn clear(_: u64, _: *mut EgclVal, _: u64) {}

fn instruction(opcode: Opcode, args: Vec<Value>) -> InstData {
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
fn frame(
    f: &mut Function,
    values: &[Value],
    bcp: u32,
) -> egcl_compiler::t2::frame_state::FrameStateId {
    f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp,
            locals: vec![],
            stack: values
                .iter()
                .map(|&value| ValueSource::Value {
                    value,
                    repr: ValueRepresentation::Tagged,
                })
                .collect(),
        }],
        remat: vec![],
    })
}

fn fixture(opcode: Opcode, adjacent: bool, boxes: usize) -> (Function, Value, Value, Vec<Value>) {
    let mut f = Function::new("comparison-with-boxed-edge-value");
    let entry = f.entry();
    let args: Vec<_> = (0..4)
        .map(|_| f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged))
        .collect();
    let fx = IRType::of(TypeBits::FIXNUM);
    let boolean = IRType::of(TypeBits::SYMBOL.join(TypeBits::NULL));
    let fs = frame(&mut f, &args, 0);
    let mut unbox = instruction(Opcode::UnboxFixnum, vec![args[3]]);
    unbox.frame_state = Some(fs);
    unbox.flags.guard = true;
    let raw = f
        .push_inst(entry, unbox, &[(fx, ValueRepresentation::UnboxedFixnum)])
        .1[0];
    let mut min = instruction(Opcode::FixnumMin, vec![args[0], args[1]]);
    min.frame_state = Some(fs);
    min.flags = InstFlags {
        guard: true,
        effectful: true,
        ..Default::default()
    };
    let bound = f
        .push_inst(entry, min, &[(fx, ValueRepresentation::Tagged)])
        .1[0];
    let mut compare = instruction(opcode, vec![args[2], bound]);
    if opcode != Opcode::GenericEq {
        compare.frame_state = Some(frame(&mut f, &[args[2], bound], 1));
        compare.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
    }
    let (cmp, flag) = f.push_inst(entry, compare, &[(boolean, ValueRepresentation::Tagged)]);
    let mut boxed = Vec::new();
    for _ in 0..boxes {
        let (box_inst, result) = f.push_inst(
            entry,
            instruction(Opcode::BoxFixnum, vec![raw]),
            &[(fx, ValueRepresentation::Tagged)],
        );
        boxed.push(result[0]);
        if adjacent {
            let list = &mut f.block_mut(entry).insts;
            assert_eq!(list.pop(), Some(box_inst));
            let index = list.iter().position(|&inst| inst == cmp).unwrap();
            list.insert(index, box_inst);
        }
    }
    let yes = f.make_block();
    let no = f.make_block();
    let yes_values: Vec<_> = (0..boxes)
        .map(|_| f.add_block_param(yes, fx, ValueRepresentation::Tagged))
        .collect();
    let mut branch = instruction(Opcode::Brif, flag);
    branch.targets = vec![
        BlockCall {
            block: yes,
            args: boxed.clone(),
        },
        BlockCall {
            block: no,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    let mut sum = yes_values[0];
    for &value in &yes_values[1..] {
        let mut add = instruction(Opcode::FixnumAdd, vec![sum, value]);
        add.flags = InstFlags {
            guard: true,
            effectful: true,
            ..Default::default()
        };
        add.frame_state = Some(frame(&mut f, &[sum, value], 2));
        sum = f
            .push_inst(yes, add, &[(fx, ValueRepresentation::Tagged)])
            .1[0];
    }
    f.set_terminator(yes, instruction(Opcode::Return, vec![sum]));
    // The false continuation still needs the raw source. The true exit needs
    // the interposed box, as in a raw loop whose exit crosses a tagged phi.
    let raw_state = f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 0,
            bcp: 2,
            locals: vec![],
            stack: vec![ValueSource::Value {
                value: raw,
                repr: ValueRepresentation::UnboxedFixnum,
            }],
        }],
        remat: vec![],
    });
    let mut neg = instruction(Opcode::FixnumNeg, vec![raw]);
    neg.flags = InstFlags {
        guard: true,
        effectful: true,
        ..Default::default()
    };
    neg.frame_state = Some(raw_state);
    let negated = f
        .push_inst(no, neg, &[(fx, ValueRepresentation::UnboxedFixnum)])
        .1[0];
    let answer = f
        .push_inst(
            no,
            instruction(Opcode::BoxFixnum, vec![negated]),
            &[(fx, ValueRepresentation::Tagged)],
        )
        .1;
    f.set_terminator(no, instruction(Opcode::Return, answer));
    egcl_compiler::t2::verify::verify(&f).unwrap();
    (f, args[2], bound, boxed)
}

fn compile(f: &Function) -> (JitBuffer, Vec<u8>) {
    let framed = emit_framed(
        f,
        deopt as *const () as u64,
        deopt as *const () as u64,
        0,
        0,
        0,
        0,
        0,
        clear as *const () as u64,
        None,
    )
    .unwrap();
    (JitBuffer::new(&framed.code).unwrap(), framed.code)
}
fn run(code: &JitBuffer, index: i64) -> i64 {
    let entry: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(code.as_ptr()) };
    let mut frame = [egcl_rt::value::NIL.0; 128];
    for (slot, n) in frame.iter_mut().zip([11, 6, index, 100]) {
        *slot = EgclVal::from_fixnum(n).0;
    }
    DEOPTED.with(|flag| flag.set(false));
    let answer = EgclVal(entry(frame.as_mut_ptr()));
    assert!(
        !DEOPTED.with(Cell::get),
        "the comparison must execute natively"
    );
    assert!(answer.is_fixnum());
    answer.as_fixnum()
}
fn materializes_true(code: &[u8]) -> bool {
    iced_x86::Decoder::new(64, code, iced_x86::DecoderOptions::NONE)
        .into_iter()
        .any(|inst| {
            inst.mnemonic() == iced_x86::Mnemonic::Mov
                && inst.op1_kind() == iced_x86::OpKind::Immediate64
                && inst.immediate64() == egcl_rt::value::T.0
        })
}

#[test]
fn interposed_box_cannot_defer_fixnum_comparison_past_operand_home_reuse() {
    let (f, _, bound, boxed) = fixture(Opcode::FixnumCmpGe, false, 2);
    let mut machine = lower(&f);
    allocate_framed(&mut machine).unwrap();
    let homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    assert_eq!(
        homes.values[&bound], homes.values[&boxed[1]],
        "fixture must overwrite the actual comparison operand home"
    );
    let (code, bytes) = compile(&f);
    assert!(
        materializes_true(&bytes),
        "nonadjacent comparison must retain its Boolean value"
    );
    for index in [5, 6, 7, 100] {
        assert_eq!(run(&code, index), if index >= 6 { 200 } else { -100 });
    }
}

#[test]
fn interposed_box_preserves_generic_eq_boolean_before_branch() {
    // GenericEq is conservatively modeled as CALL_RUNTIME in LIR. Its
    // operands have stable spill/callee-saved homes in this fixture, so this
    // case does not claim the same physical alias as the fixnum test. It
    // independently requires Boolean materialization before the intervening
    // box, covering the shared fusion predicate without corrupting mappings.
    let (f, _, _, _) = fixture(Opcode::GenericEq, false, 1);
    let (code, bytes) = compile(&f);
    assert!(
        materializes_true(&bytes),
        "nonadjacent EQ must retain its Boolean value"
    );
    for index in [5, 6, 7, 100] {
        assert_eq!(run(&code, index), if index == 6 { 100 } else { -100 });
    }
}

#[test]
fn adjacent_comparison_still_uses_direct_cmp_branch_fusion() {
    for opcode in [Opcode::FixnumCmpGe, Opcode::GenericEq] {
        let (f, _, _, _) = fixture(opcode, true, 1);
        let (code, bytes) = compile(&f);
        assert!(
            !materializes_true(&bytes),
            "adjacent comparison should not materialize T/NIL"
        );
        for index in [5, 6, 7, 100] {
            let yes = if opcode == Opcode::GenericEq {
                index == 6
            } else {
                index >= 6
            };
            assert_eq!(run(&code, index), if yes { 100 } else { -100 });
        }
    }
}
