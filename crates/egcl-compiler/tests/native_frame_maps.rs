// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, Value, ValueRepresentation,
};
use egcl_compiler::t2::lower::lower;
use egcl_compiler::t2::mach::{Location, MachFunc, StackSlot};
use egcl_compiler::t2::regalloc::allocate_framed;
use egcl_compiler::t2::x64_frame::{
    FrameMapError, FrameValueLocation, ValueHome, resolve_safepoint_maps, select_frame_homes,
};
use egcl_rt::value::EgclVal;
use std::collections::HashMap;

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

fn fixture() -> (Function, MachFunc, [Value; 4]) {
    let mut f = Function::new("resolved-home-probe");
    let entry = f.entry();
    let held = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let argument = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let raw = f.add_block_param(entry, IRType::TOP, ValueRepresentation::UnboxedFixnum);
    let mut call = instruction(Opcode::Call, vec![argument]);
    call.aux = AuxData::CallTarget(42);
    call.flags.call = true;
    call.flags.safepoint = true;
    let (call, _) = f.push_inst(entry, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    let result = f.inst(call).results[0];
    f.set_terminator(entry, instruction(Opcode::Return, vec![held, raw, result]));
    let mut machine = lower(&f);
    allocate_framed(&mut machine).unwrap();
    (f, machine, [held, argument, raw, result])
}

#[test]
fn split_values_use_final_homes_with_complete_typed_call_inputs() {
    let (f, mut machine, [held, argument, raw, result]) = fixture();
    let mut split = *machine
        .value_locations
        .iter()
        .find(|range| range.vreg.num == held.0)
        .unwrap();
    let allocator_home = split.location;
    split.location = Location::Stack(StackSlot(machine.num_spill_slots));
    machine.num_spill_slots += 1;
    machine.value_locations.push(split);
    let homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    let ValueHome::Stack(slot) = homes.values[&held] else {
        panic!("split value needs a stable spill")
    };
    assert!(slot >= machine.num_spill_slots);
    assert_ne!(homes.values[&held].location(), Some(allocator_home));
    let maps = resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()).unwrap();
    let map = maps.iter().next().unwrap();
    assert_eq!(map.values().len(), 3);
    assert_eq!(
        map.value(held).unwrap().location,
        FrameValueLocation::Home(ValueHome::Stack(slot))
    );
    assert_eq!(
        map.value(held).unwrap().gc_home(),
        Some(ValueHome::Stack(slot))
    );
    assert!(
        map.value(argument).unwrap().gc_home().is_some(),
        "dying argument survives the call"
    );
    assert_eq!(
        map.value(raw).unwrap().repr,
        ValueRepresentation::UnboxedFixnum
    );
    assert!(
        map.value(raw).unwrap().gc_home().is_none(),
        "raw word remains debug-visible, never a root"
    );
    assert!(map.value(result).is_none(), "result does not yet exist");
}

#[test]
fn invalid_final_homes_and_live_aliases_are_rejected() {
    let (f, machine, [held, argument, _, _]) = fixture();
    let mut homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    let original = homes.values[&held];
    homes.values.remove(&held);
    assert!(
        matches!(resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()), Err(FrameMapError::MissingHome(v)) if v == held)
    );
    for invalid in [
        ValueHome::Reg(0),
        ValueHome::Reg(2),
        ValueHome::Reg(11),
        ValueHome::Stack(homes.stack_slots),
    ] {
        homes.values.insert(held, invalid);
        assert!(
            matches!(resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()), Err(FrameMapError::InvalidHome(h)) if h == invalid)
        );
    }
    homes.stack_slots = u32::MAX;
    homes
        .values
        .insert(held, ValueHome::Stack(i32::MAX as u32 / 8 + 1));
    assert!(matches!(
        resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()),
        Err(FrameMapError::InvalidHome(_))
    ));
    homes.values.insert(held, original);
    homes.values.insert(argument, original);
    assert!(matches!(
        resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()),
        Err(FrameMapError::ConflictingHome(_))
    ));
}

#[test]
fn constants_are_retained_for_debug_without_becoming_roots() {
    let mut f = Function::new("constant-home-probe");
    let entry = f.entry();
    let raw = f.add_block_param(entry, IRType::TOP, ValueRepresentation::UnboxedFixnum);
    let mut constant = instruction(Opcode::ConstFixnum, vec![]);
    constant.aux = AuxData::FixnumImm(19);
    let (_, values) = f.push_inst(
        entry,
        constant,
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let held = values[0];
    let mut call = instruction(Opcode::Call, vec![held]);
    call.aux = AuxData::CallTarget(42);
    call.flags.call = true;
    call.flags.safepoint = true;
    f.push_inst(entry, call, &[]);
    f.set_terminator(entry, instruction(Opcode::Return, vec![held, raw]));
    let mut machine = lower(&f);
    allocate_framed(&mut machine).unwrap();
    let homes = select_frame_homes(&f, &machine, |value| value == held).unwrap();
    let constant = EgclVal::from_fixnum(19);
    let mut constants = HashMap::from([(held, constant.to_raw())]);
    let maps = resolve_safepoint_maps(&f, &machine, &homes, &constants).unwrap();
    let value = maps.iter().next().unwrap().value(held).unwrap();
    assert_eq!(value.location, FrameValueLocation::Immediate(constant));
    assert!(value.gc_home().is_none());
    constants.insert(held, EgclVal::from_fixnum(20).to_raw());
    assert!(matches!(
        resolve_safepoint_maps(&f, &machine, &homes, &constants),
        Err(FrameMapError::InvalidConstant(v)) if v == held
    ));
    constants.insert(held, 0x1001);
    assert!(
        matches!(resolve_safepoint_maps(&f, &machine, &homes, &constants), Err(FrameMapError::InvalidConstant(v)) if v == held)
    );
    constants.insert(held, constant.to_raw());
    constants.insert(raw, constant.to_raw());
    assert!(
        matches!(resolve_safepoint_maps(&f, &machine, &homes, &constants), Err(FrameMapError::InvalidConstant(v)) if v == raw)
    );
}

#[test]
fn a_parameter_cannot_be_disguised_as_a_constant() {
    let (f, machine, [held, _, _, _]) = fixture();
    let homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    let constants = HashMap::from([(held, EgclVal::from_fixnum(19).to_raw())]);
    assert_eq!(
        resolve_safepoint_maps(&f, &machine, &homes, &constants).err(),
        Some(FrameMapError::InvalidConstant(held))
    );
}

#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn comparison_live_across_call_keeps_a_materialized_home() {
    use egcl_compiler::t2::ir::BlockCall;
    let mut f = Function::new("comparison-across-call");
    let entry = f.entry();
    let yes = f.make_block();
    let no = f.make_block();
    let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let y = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let (_, results) = f.push_inst(
        entry,
        instruction(Opcode::GenericEq, vec![x, y]),
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let mut call = instruction(Opcode::Call, vec![]);
    call.aux = AuxData::CallTarget(42);
    call.flags.call = true;
    call.flags.safepoint = true;
    f.push_inst(entry, call, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    let mut branch = instruction(Opcode::Brif, vec![results[0]]);
    branch.targets = vec![
        BlockCall {
            block: yes,
            args: vec![],
        },
        BlockCall {
            block: no,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    for (block, answer) in [(yes, 111), (no, 222)] {
        let mut constant = instruction(Opcode::ConstFixnum, vec![]);
        constant.aux = AuxData::FixnumImm(answer);
        let (_, values) = f.push_inst(
            block,
            constant,
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(block, instruction(Opcode::Return, values));
    }
    extern "C" fn helper(_: u64, _: u64, _: u64, _: u64, _: u64, _: u64) -> u64 {
        egcl_rt::value::NIL.to_raw()
    }
    let emitted = egcl_compiler::t2::emit::emit_framed(
        &f,
        0,
        0,
        helper as *const () as u64,
        0,
        0,
        0,
        0,
        0,
        None,
    )
    .expect("a comparison live at a safepoint must have a materialized home");
    let buffer = egcl_rt::jit::JitBuffer::new(&emitted.code).unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    for (x, y, expected) in [(7, 7, 111), (7, 9, 222)] {
        let mut frame = [
            EgclVal::from_fixnum(x).to_raw(),
            EgclVal::from_fixnum(y).to_raw(),
        ];
        assert_eq!(
            run(frame.as_mut_ptr()),
            EgclVal::from_fixnum(expected).to_raw()
        );
    }
}

#[test]
fn inserted_poll_uses_typed_before_values_without_an_ir_safepoint() {
    use egcl_compiler::t2::x64_frame::resolve_values_before;
    let (f, machine, [held, argument, raw, result]) = fixture();
    let homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    let point = machine.insts.len() - 1;
    assert!(!machine.insts[point].safepoint);
    let values = resolve_values_before(&f, &machine, &homes, &HashMap::new(), point).unwrap();
    assert_eq!(values.values().len(), 3);
    assert!(values.value(held).unwrap().gc_home().is_some());
    assert!(values.value(result).unwrap().gc_home().is_some());
    assert!(values.value(raw).unwrap().gc_home().is_none());
    assert!(values.value(argument).is_none());
    let ordinary = resolve_safepoint_maps(&f, &machine, &homes, &HashMap::new()).unwrap();
    let call = ordinary.iter().next().unwrap();
    let before_call =
        resolve_values_before(&f, &machine, &homes, &HashMap::new(), call.machine_inst).unwrap();
    assert_eq!(before_call.values(), call.values());
    assert!(
        before_call.value(argument).is_some(),
        "early dying argument remains live"
    );
    assert!(
        before_call.value(result).is_none(),
        "late result does not exist yet"
    );
    let mut anonymous = machine.clone();
    anonymous.insts[point].source_inst = None;
    assert_eq!(
        resolve_values_before(&f, &anonymous, &homes, &HashMap::new(), point)
            .unwrap()
            .values(),
        values.values(),
        "an inserted poll does not need a source annotation"
    );
    let mut broken = homes;
    broken.values.remove(&held);
    assert!(matches!(
        resolve_values_before(&f, &machine, &broken, &HashMap::new(), point),
        Err(FrameMapError::MissingHome(v)) if v == held
    ));
}

#[test]
fn loop_header_poll_precedes_source_less_edge_moves() {
    use egcl_compiler::t2::ir::BlockCall;
    use egcl_compiler::t2::x64_frame::resolve_values_before;
    let mut f = Function::new("poll-before-edge-move");
    let entry = f.entry();
    let header = f.make_block();
    let body = f.make_block();
    let exit = f.make_block();
    let input = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let carried = f.add_block_param(header, IRType::TOP, ValueRepresentation::Tagged);
    let next = f.add_block_param(body, IRType::TOP, ValueRepresentation::Tagged);
    let output = f.add_block_param(exit, IRType::TOP, ValueRepresentation::Tagged);
    for (block, target, value) in [(entry, header, input), (header, body, carried)] {
        let mut jump = instruction(Opcode::Jump, vec![]);
        jump.targets = vec![BlockCall {
            block: target,
            args: vec![value],
        }];
        f.set_terminator(block, jump);
    }
    let mut branch = instruction(Opcode::Brif, vec![next]);
    branch.targets = vec![
        BlockCall {
            block: header,
            args: vec![next],
        },
        BlockCall {
            block: exit,
            args: vec![next],
        },
    ];
    f.set_terminator(body, branch);
    f.set_terminator(exit, instruction(Opcode::Return, vec![output]));
    let mut machine = lower(&f);
    allocate_framed(&mut machine).unwrap();
    let point = machine.blocks[header.index()].start;
    assert!(machine.insts[point].source_inst.is_none());
    assert!(
        machine.insts[point]
            .uses
            .iter()
            .any(|vreg| vreg.num == carried.0)
    );
    let homes = select_frame_homes(&f, &machine, |_| false).unwrap();
    let values = resolve_values_before(&f, &machine, &homes, &HashMap::new(), point).unwrap();
    assert!(values.value(carried).unwrap().gc_home().is_some());
    assert!(
        values.value(next).is_none(),
        "edge destination has not been written yet"
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
#[test]
fn comparison_live_across_inserted_poll_has_a_real_home() {
    use egcl_compiler::t2::ir::BlockCall;
    extern "C" fn poll(_: *mut u64) -> u64 {
        unsafe {
            core::arch::asm!("xor rcx,rcx", "xor r8,r8",
                out("rcx") _, out("r8") _, options(nomem, nostack));
        }
        egcl_rt::value::NIL.to_raw()
    }
    let mut f = Function::new("comparison-across-inserted-poll");
    let entry = f.entry();
    let yes = f.make_block();
    let no = f.make_block();
    let x = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let y = f.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
    let (_, comparison) = f.push_inst(
        entry,
        instruction(Opcode::GenericEq, vec![x, y]),
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    for _ in 0..65 {
        f.push_inst(
            entry,
            instruction(Opcode::GenericEq, vec![x, y]),
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
    }
    let mut branch = instruction(Opcode::Brif, comparison);
    branch.targets = vec![
        BlockCall {
            block: yes,
            args: vec![],
        },
        BlockCall {
            block: no,
            args: vec![],
        },
    ];
    f.set_terminator(entry, branch);
    for (block, answer) in [(yes, 111), (no, 222)] {
        let mut constant = instruction(Opcode::ConstFixnum, vec![]);
        constant.aux = AuxData::FixnumImm(answer);
        let (_, values) = f.push_inst(
            block,
            constant,
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(block, instruction(Opcode::Return, values));
    }
    let helper = poll as *const () as u64;
    let (emitted, _) = egcl_compiler::t2::emit::emit_framed_native_handlers_with_poll(
        &f, helper, 2, helper, helper, helper, helper, helper, helper,
    )
    .expect("poll-live comparison must not be fused away");
    let buffer = egcl_rt::jit::JitBuffer::new(&emitted.code).unwrap();
    let run: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buffer.as_ptr()) };
    for (x, y, expected) in [(7, 7, 111), (7, 9, 222)] {
        let mut frame =
            vec![egcl_rt::value::NIL.to_raw(); 2 + usize::from(emitted.shadow_root_slots)];
        frame[0] = EgclVal::from_fixnum(x).to_raw();
        frame[1] = EgclVal::from_fixnum(y).to_raw();
        assert_eq!(
            run(frame.as_mut_ptr()),
            EgclVal::from_fixnum(expected).to_raw()
        );
    }
}
