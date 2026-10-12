// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Preserve runtime-operation semantics while routing their calls through the
//! ordinary ABI. These targets are private semantic adapters, not redefinable
//! public CL function cells (a variable read is not a call to SYMBOL-VALUE).
use super::EmitError;
use crate::t2::ir::{
    AuxData, Function, IRType, InstData, InstFlags, Opcode, TypeBits, ValueRepresentation,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum OrdinaryPrimitive {
    LoadGlobal,
    StoreGlobal,
    LoadFunction,
    Cons,
    Add,
    Subtract,
    Multiply,
    Divide,
    Equal,
}

impl OrdinaryPrimitive {
    pub const ALL: [Self; 9] = [
        Self::LoadGlobal,
        Self::StoreGlobal,
        Self::LoadFunction,
        Self::Cons,
        Self::Add,
        Self::Subtract,
        Self::Multiply,
        Self::Divide,
        Self::Equal,
    ];
}

/// Replace semantic helper operations with calls to runtime-owned adapters.
/// `resolve` must return stable private symbol IDs whose adapters implement the
/// exact operation, including errors and dynamic binding; public function cells
/// are not equivalent. Loads receive a tagged symbol; StoreGlobal receives the
/// tagged symbol followed by its value. Other operations keep operand order.
///
/// Original instruction/result identities, source positions and continuation
/// states stay intact. Stores can discard the adapter's returned primary.
/// Each operation requires a proven single-value or unobservable incoming state.
/// Unobservable tuples are cleared before entry; the adapter's
/// guaranteed single-value completion is made explicit with ClearMv.
/// Resolution/shape failure leaves the IR unchanged. Captured environment and
/// control-region operations cannot be lowered without their context protocol.
/// SetCar/SetCdr stay unlowered until their IR operand order is made consistent.
pub fn lower_ordinary_primitives(
    f: &mut Function,
    mut resolve: impl FnMut(OrdinaryPrimitive) -> Option<u32>,
) -> Result<usize, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    if crate::t2::verify::verify(f).is_err() {
        return Err(bad());
    }
    // These semantic operations are transparent to the pre-existing MV
    // buffer. Model that fact rather than their old C-call clobber flags. An
    // adapter's single-value return is equivalent when the incoming MV state
    // is already empty or unobservable. Never erase a tuple a later consumer needs.
    let mut state_model = f.clone();
    for block in f.block_order() {
        for &inst in &f.block(*block).insts {
            if matches!(
                f.inst(inst).opcode,
                Opcode::SymbolValue
                    | Opcode::SetSymbolValue
                    | Opcode::SymbolFunction
                    | Opcode::AllocCons
                    | Opcode::GenericAdd
                    | Opcode::GenericSub
                    | Opcode::GenericMul
                    | Opcode::GenericDiv
                    | Opcode::GenericEqual
            ) {
                state_model.inst_mut(inst).flags.call = false;
            }
        }
    }
    let single = crate::t2::opt_clear_mv::single_value_state(&state_model, true);
    let unobserved = crate::t2::opt_clear_mv::unobserved_multiple_value_state(&state_model);
    let mut replacements = Vec::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            let (primitive, arguments, results, symbol) = match data.opcode {
                Opcode::SymbolValue => (OrdinaryPrimitive::LoadGlobal, 0, 1, true),
                Opcode::SetSymbolValue => (OrdinaryPrimitive::StoreGlobal, 1, 0, true),
                Opcode::SymbolFunction => (OrdinaryPrimitive::LoadFunction, 0, 1, true),
                Opcode::AllocCons => (OrdinaryPrimitive::Cons, 2, 1, false),
                Opcode::GenericAdd => (OrdinaryPrimitive::Add, 2, 1, false),
                Opcode::GenericSub => (OrdinaryPrimitive::Subtract, 2, 1, false),
                Opcode::GenericMul => (OrdinaryPrimitive::Multiply, 2, 1, false),
                Opcode::GenericDiv => (OrdinaryPrimitive::Divide, 2, 1, false),
                Opcode::GenericEqual => (OrdinaryPrimitive::Equal, 2, 1, false),
                _ => continue,
            };
            if !single.contains(&inst) && !unobserved.contains(&inst) {
                return Err(bad());
            }
            if data.args.len() != arguments
                || data.results.len() != results
                || data
                    .args
                    .iter()
                    .chain(&data.results)
                    .any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
            {
                return Err(bad());
            }
            let symbol = if symbol {
                let AuxData::SymbolRef(symbol) = data.aux else {
                    return Err(bad());
                };
                Some(symbol)
            } else {
                if !matches!(data.aux, AuxData::None) {
                    return Err(bad());
                }
                None
            };
            replacements.push((block, inst, resolve(primitive).ok_or_else(bad)?, symbol, !single.contains(&inst)));
        }
    }
    for &(block, inst, target, symbol, normalize) in &replacements {
        if let Some(symbol) = symbol {
            let source_pos = f.inst(inst).source_pos;
            let (constant, values) = f.push_inst(
                block,
                InstData {
                    opcode: Opcode::ConstSymbol,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::SymbolRef(symbol),
                    flags: InstFlags::default(),
                    targets: vec![],
                    frame_state: None,
                    source_pos,
                },
                &[(IRType::of(TypeBits::SYMBOL), ValueRepresentation::Tagged)],
            );
            // push_inst allocates stable SSA IDs. Move its layout position in
            // front of the original instruction before exposing the new use.
            let block_data = f.block_mut(block);
            assert_eq!(block_data.insts.pop(), Some(constant));
            let position = block_data
                .insts
                .iter()
                .position(|&site| site == inst)
                .unwrap();
            block_data.insts.insert(position, constant);
            f.inst_mut(inst).args.insert(0, values[0]);
        }
        if normalize {
            let (clear, _) = f.push_inst(block, InstData {
                opcode: Opcode::ClearMv, args: vec![], results: vec![], aux: AuxData::None,
                flags: InstFlags { call: true, safepoint: true, effectful: true, ..Default::default() },
                targets: vec![], frame_state: None, source_pos: f.inst(inst).source_pos,
            }, &[]);
            let layout = &mut f.block_mut(block).insts;
            assert_eq!(layout.pop(), Some(clear));
            let position = layout.iter().position(|&site| site == inst).unwrap();
            layout.insert(position, clear);
        }
        let data = f.inst_mut(inst);
        data.opcode = Opcode::Call;
        data.aux = AuxData::CallTarget(target);
        data.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        let source_pos = data.source_pos;
        // Record the adapter's guaranteed one-value completion explicitly so
        // later guards/returns, including a result-less store, retain that fact.
        let (clear, _) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::ClearMv,
                args: vec![],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags {
                    call: true,
                    safepoint: true,
                    effectful: true,
                    ..Default::default()
                },
                targets: vec![],
                frame_state: None,
                source_pos,
            },
            &[],
        );
        let block_data = f.block_mut(block);
        assert_eq!(block_data.insts.pop(), Some(clear));
        let position = block_data
            .insts
            .iter()
            .position(|&site| site == inst)
            .unwrap();
        block_data.insts.insert(position + 1, clear);
    }
    Ok(replacements.len())
}
