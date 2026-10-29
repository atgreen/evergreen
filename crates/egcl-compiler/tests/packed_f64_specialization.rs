// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
use egcl_compiler::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, Value, ValueRepresentation as Repr,
};
use egcl_compiler::t2::packed_f64::specialize;
use std::sync::Mutex;

static SYMBOLS: Mutex<()> = Mutex::new(());

fn inst(opcode: Opcode, args: Vec<Value>) -> InstData {
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

fn kernel(aref: u32, escape: bool) -> Function {
    let mut f = Function::new("packed-closed-expression");
    let entry = f.entry();
    let args: Vec<_> = (0..4)
        .map(|_| f.add_block_param(entry, IRType::TOP, Repr::Tagged))
        .collect();
    let source = |value| ValueSource::Value {
        value,
        repr: Repr::Tagged,
    };
    let call = |f: &mut Function, symbol, operands, bcp, stack| {
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp,
                locals: args.iter().copied().map(source).collect(),
                stack,
            }],
            remat: vec![],
        });
        let mut data = inst(Opcode::Call, operands);
        data.aux = AuxData::CallTarget(symbol);
        data.flags = InstFlags {
            effectful: true,
            call: true,
            safepoint: true,
            ..InstFlags::default()
        };
        data.frame_state = Some(state);
        f.push_inst(entry, data, &[(IRType::TOP, Repr::Tagged)]).1[0]
    };
    let value = call(&mut f, aref, vec![args[0], args[2]], 1, vec![]);
    let product = call(
        &mut f,
        egcl_rt::symbols::intern("*"),
        vec![value, args[3]],
        2,
        vec![source(value)],
    );
    let sum = call(
        &mut f,
        egcl_rt::symbols::intern("+"),
        vec![product, value],
        3,
        vec![source(product), source(value)],
    );
    call(
        &mut f,
        egcl_rt::symbols::intern("EGCL::SET-AREF"),
        vec![args[1], args[2], sum],
        4,
        vec![source(sum)],
    );
    f.set_terminator(
        entry,
        inst(Opcode::Return, vec![if escape { value } else { args[1] }]),
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
    f
}

fn count(f: &Function, opcode: Opcode) -> usize {
    f.block_order()
        .iter()
        .flat_map(|&block| &f.block(block).insts)
        .filter(|&&instruction| f.inst(instruction).opcode == opcode)
        .count()
}

#[test]
fn closed_array_expression_uses_raw_homes_and_explicit_recovery() {
    let _lock = SYMBOLS.lock().unwrap();
    let mut f = kernel(egcl_rt::symbols::intern("AREF"), false);
    assert_eq!(specialize(&mut f), 4);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    for opcode in [
        Opcode::PackedF64Ref,
        Opcode::PackedF64Set,
        Opcode::FloatAdd,
        Opcode::FloatMul,
        Opcode::UnboxFloat,
    ] {
        assert_eq!(count(&f, opcode), 1, "{opcode:?}");
    }
    assert_eq!(count(&f, Opcode::Call), 0);
    assert_eq!(count(&f, Opcode::ClearMv), 4);
    assert_eq!(
        count(&f, Opcode::Guard),
        2,
        "mutable array bindings remain guarded"
    );
    for (_, state) in f.frame_states.iter() {
        for source in state
            .scopes
            .iter()
            .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
        {
            if let ValueSource::Value { value, repr } = source {
                assert_eq!(*repr, f.value(*value).repr);
            }
        }
    }
    assert!(
        f.block(f.entry())
            .params
            .iter()
            .all(|&value| f.value(value).repr == Repr::Tagged)
    );
}

#[test]
fn raw_result_cannot_escape_the_closed_expression() {
    let _lock = SYMBOLS.lock().unwrap();
    let mut f = kernel(egcl_rt::symbols::intern("AREF"), true);
    let before = format!("{f:?}");
    assert_eq!(specialize(&mut f), 0);
    assert_eq!(format!("{f:?}"), before);
}

#[test]
fn shadowed_array_primitive_is_not_specialized_by_printed_name() {
    let _lock = SYMBOLS.lock().unwrap();
    let uninterned = egcl_rt::symbols::make_uninterned("AREF").as_symbol_index();
    assert_eq!(
        egcl_rt::symbols::symbol_name(uninterned).as_deref(),
        Some("AREF")
    );
    for symbol in [egcl_rt::symbols::intern("OTHER::AREF"), uninterned] {
        let mut f = kernel(symbol, false);
        let before = format!("{f:?}");
        assert_eq!(specialize(&mut f), 0);
        assert_eq!(format!("{f:?}"), before);
    }
}

#[test]
fn rebound_mutable_array_primitive_retains_the_original_calls() {
    let _lock = SYMBOLS.lock().unwrap();
    let aref = egcl_rt::symbols::intern("AREF");
    let mut f = kernel(aref, false);
    let old = egcl_rt::symbols::symbol_function(aref).unwrap();
    struct Restore(u32, egcl_rt::value::EgclVal);
    impl Drop for Restore {
        fn drop(&mut self) {
            egcl_rt::symbols::set_symbol_function(self.0, self.1);
        }
    }
    let _restore = Restore(aref, old);
    egcl_rt::symbols::set_symbol_function(aref, egcl_rt::value::EgclVal::from_fixnum(123));
    let before = format!("{f:?}");
    assert_eq!(specialize(&mut f), 0);
    assert_eq!(format!("{f:?}"), before);
}

// Boot SETF binds its store value through MULTIPLE-VALUE-BIND even for one
// variable. Match the resulting bytecode/SSA transport, including recovery uses.
fn with_store_transport(mut f: Function, nvars: u16, escape: bool) -> Function {
    let block = f.entry();
    let store = *f.block(block).insts.iter().find(|&&i| matches!(f.inst(i).aux,
        AuxData::CallTarget(s) if egcl_rt::symbols::registry_key(s).as_deref() == Some("EGCL::SET-AREF"))).unwrap();
    let primary = f.inst(store).args[2];
    let state = f.inst(store).frame_state.unwrap();
    let mut data = inst(Opcode::TakeValuesToLocals, vec![primary]);
    data.aux = AuxData::ValuesLocals {
        nvars,
        slot_base: 0,
    };
    data.frame_state = Some(state);
    data.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    let types = vec![(IRType::TOP, Repr::Tagged); nvars as usize];
    let (transport, results) = f.push_inst(block, data, &types);
    assert_eq!(f.block_mut(block).insts.pop(), Some(transport));
    let position = f
        .block(block)
        .insts
        .iter()
        .position(|&i| i == store)
        .unwrap();
    f.block_mut(block).insts.insert(position, transport);
    f.inst_mut(store).args[2] = results[0];
    let mut after = f.frame_states.get(state).clone();
    after.scopes[0].stack = vec![ValueSource::Value {
        value: results[0],
        repr: Repr::Tagged,
    }];
    after.scopes[0].locals.push(ValueSource::Value {
        value: results[0],
        repr: Repr::Tagged,
    });
    let after = f.frame_states.add(after);
    f.inst_mut(store).frame_state = Some(after);
    if escape {
        let ret = f.terminator(block).unwrap();
        f.inst_mut(ret).args[0] = results[0];
    }
    egcl_compiler::t2::verify::verify(&f).unwrap();
    f
}

#[test]
fn boot_setf_single_value_transport_keeps_raw_graph_and_recovery() {
    let _lock = SYMBOLS.lock().unwrap();
    let mut f = with_store_transport(kernel(egcl_rt::symbols::intern("AREF"), false), 1, false);
    let transported = f
        .block(f.entry())
        .insts
        .iter()
        .find_map(|&i| {
            (f.inst(i).opcode == Opcode::TakeValuesToLocals).then(|| f.inst(i).results[0])
        })
        .unwrap();
    assert_eq!(specialize(&mut f), 4);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    assert_eq!(count(&f, Opcode::TakeValuesToLocals), 0);
    assert_eq!(
        count(&f, Opcode::ClearMv),
        4,
        "producer resets retained; binder does not add one"
    );
    assert_eq!(count(&f, Opcode::PackedF64Set), 1);
    for (_, state) in f.frame_states.iter() {
        for source in state
            .scopes
            .iter()
            .flat_map(|s| s.locals.iter().chain(&s.stack))
        {
            if let ValueSource::Value { value, repr } = source {
                assert_ne!(
                    *value, transported,
                    "recovery must use the dominating primary"
                );
                assert_eq!(*repr, f.value(*value).repr);
            }
        }
    }
}

#[test]
fn nonadjacent_store_transport_stays_outside_the_bounded_normalization() {
    let _lock = SYMBOLS.lock().unwrap();
    let mut f = with_store_transport(kernel(egcl_rt::symbols::intern("AREF"), false), 1, false);
    let block = f.entry();
    let position = f
        .block(block)
        .insts
        .iter()
        .position(|&i| f.inst(i).opcode == Opcode::TakeValuesToLocals)
        .unwrap();
    let state = f.inst(f.block(block).insts[position]).frame_state;
    let mut call = inst(Opcode::Call, vec![]);
    call.aux = AuxData::CallTarget(egcl_rt::symbols::intern("UNKNOWN-MV-PRODUCER"));
    call.flags = InstFlags {
        call: true,
        safepoint: true,
        effectful: true,
        ..Default::default()
    };
    call.frame_state = state;
    let (intervening, _) = f.push_inst(block, call, &[(IRType::TOP, Repr::Tagged)]);
    assert_eq!(f.block_mut(block).insts.pop(), Some(intervening));
    f.block_mut(block).insts.insert(position, intervening);
    egcl_compiler::t2::verify::verify(&f).unwrap();
    let before = format!("{f:?}");
    assert_eq!(specialize(&mut f), 0);
    assert_eq!(format!("{f:?}"), before);
}

#[test]
fn store_transport_declines_multiple_values_unknown_producers_and_raw_escape() {
    let _lock = SYMBOLS.lock().unwrap();
    for (nvars, escape, unknown) in [(2, false, false), (1, true, false), (1, false, true)] {
        let mut f = kernel(egcl_rt::symbols::intern("AREF"), false);
        if unknown {
            let add = *f
                .block(f.entry())
                .insts
                .iter()
                .find(|&&i| {
                    matches!(f.inst(i).aux,
                AuxData::CallTarget(s) if egcl_rt::symbols::registry_key(s).as_deref() == Some("+"))
                })
                .unwrap();
            f.inst_mut(add).aux =
                AuxData::CallTarget(egcl_rt::symbols::intern("UNKNOWN-MV-PRODUCER"));
        }
        let mut f = with_store_transport(f, nvars, escape);
        let before = format!("{f:?}");
        assert_eq!(specialize(&mut f), 0);
        assert_eq!(
            format!("{f:?}"),
            before,
            "declines must preserve exact tagged IR"
        );
    }
}

#[test]
fn failed_family_rejects_entire_closed_packed_expression_without_changing_state() {
    let _lock = SYMBOLS.lock().unwrap();
    for bcp in [1, 2, 3, 4] {
        let mut f = kernel(egcl_rt::symbols::intern("AREF"), false);
        let original = format!("{f:?}");
        assert_eq!(
            egcl_compiler::t2::packed_f64::specialize_with_feedback(&mut f, &|site, kind| !(site
                == bcp
                && kind == egcl_compiler::t2::speculate::SpecType::DoubleFloat)),
            0
        );
        assert_eq!(
            format!("{f:?}"),
            original,
            "no partial mutation, including FrameStates, at vetoed site {bcp}"
        );
        egcl_compiler::t2::verify::verify(&f).unwrap();
    }
    let mut f = kernel(egcl_rt::symbols::intern("AREF"), false);
    assert_eq!(
        egcl_compiler::t2::packed_f64::specialize_with_feedback(&mut f, &|bcp, _| bcp != 999),
        4
    );
    egcl_compiler::t2::verify::verify(&f).unwrap();
}
