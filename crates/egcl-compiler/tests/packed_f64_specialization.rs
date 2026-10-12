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
