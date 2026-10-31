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
    MakeArray,
    LoadEnvironment,
    StoreEnvironment,
    /// `(symbol value)`: establish a special binding on the execution's
    /// native binding stack (bliss-gg3i1.7.4).
    BindSpecial,
    /// `(count)`: restore the `count` newest native special bindings.
    UnbindSpecial,
    /// `BindSpecial` leaving the multiple values alone, where the incoming
    /// tuple may still be read: the interpreter's binding never touches it.
    BindSpecialPreservingValues,
    /// `UnbindSpecial` leaving the multiple values alone.
    UnbindSpecialPreservingValues,
}

impl OrdinaryPrimitive {
    /// The interned name of the private symbol whose adapter implements this
    /// operation, e.g. `%EGCL-ORDINARY-LOADGLOBAL`.
    pub fn adapter_name(self) -> String {
        format!("%EGCL-ORDINARY-{self:?}").to_ascii_uppercase()
    }

    pub const ALL: [Self; 16] = [
        Self::LoadGlobal,
        Self::StoreGlobal,
        Self::LoadFunction,
        Self::Cons,
        Self::Add,
        Self::Subtract,
        Self::Multiply,
        Self::Divide,
        Self::Equal,
        Self::MakeArray,
        Self::LoadEnvironment,
        Self::StoreEnvironment,
        Self::BindSpecial,
        Self::UnbindSpecial,
        Self::BindSpecialPreservingValues,
        Self::UnbindSpecialPreservingValues,
    ];
}

/// Immutable immediate identities captured by the owner that registers the
/// private adapter. Background emission never looks up or interns symbols.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PackedU8AdapterSymbols {
    pub element_type: u32,
    pub unsigned_byte: u32,
}

fn packed_u8_adapters()
-> &'static std::sync::Mutex<std::collections::HashMap<u32, Option<PackedU8AdapterSymbols>>> {
    static ADAPTERS: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u32, Option<PackedU8AdapterSymbols>>>,
    > = std::sync::OnceLock::new();
    ADAPTERS.get_or_init(|| std::sync::Mutex::new(std::collections::HashMap::new()))
}

/// Register an exact runtime-owned semantic identity, never a public name or
/// inferred body shape. Conflicting registration permanently declines inline
/// emission for that identity; its unchanged collecting call remains valid.
pub fn register_packed_u8_adapter(target: u32, symbols: PackedU8AdapterSymbols) -> bool {
    let mut adapters = packed_u8_adapters().lock().unwrap();
    match adapters.entry(target) {
        std::collections::hash_map::Entry::Vacant(entry) => {
            entry.insert(Some(symbols));
            true
        }
        std::collections::hash_map::Entry::Occupied(mut entry) => {
            if *entry.get() == Some(symbols) {
                true
            } else {
                *entry.get_mut() = None;
                false
            }
        }
    }
}

pub(crate) fn packed_u8_adapter(target: u32) -> Option<PackedU8AdapterSymbols> {
    packed_u8_adapters()
        .lock()
        .unwrap()
        .get(&target)
        .copied()
        .flatten()
}

fn array_constructor_candidate(f: &Function, data: &InstData) -> bool {
    use crate::t2::ir::ValueDef;
    let AuxData::CallTarget(symbol) = data.aux else {
        return false;
    };
    if !matches!(data.opcode, Opcode::Call | Opcode::Invoke)
        || !data.flags.call
        || !data.flags.effectful
        || !data.flags.safepoint
        || data.args.len() != 3
        || data.results.len() != 1
        || data.frame_state.is_none()
        || data
            .args
            .iter()
            .chain(&data.results)
            .any(|&v| f.value(v).repr != ValueRepresentation::Tagged)
        || egcl_rt::symbols::registry_key(symbol).as_deref() != Some("MAKE-ARRAY")
        || egcl_rt::symbols::find_index("MAKE-ARRAY") != Some(symbol)
    {
        return false;
    }
    let ValueDef::Result { inst, .. } = f.value(data.args[1]).def else {
        return false;
    };
    let keyword = f.inst(inst);
    let AuxData::SymbolRef(index) = keyword.aux else {
        return false;
    };
    keyword.opcode == Opcode::ConstSymbol
        && egcl_rt::symbols::registry_key(index).as_deref() == Some("KEYWORD:ELEMENT-TYPE")
        && egcl_rt::symbols::find_index("KEYWORD:ELEMENT-TYPE") == Some(index)
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
    let captures = f.block_order().iter().any(|&block| {
        f.block(block).insts.iter().any(|&inst| {
            matches!(
                f.inst(inst).opcode,
                Opcode::EnvironmentValue | Opcode::SetEnvironmentValue
            )
        })
    });
    if !captures {
        return lower_ordinary_primitives_inner(f, &mut resolve);
    }
    // Failure must leave the original mapped/checked candidate intact.
    if crate::t2::verify::verify(f).is_err() {
        return Err(EmitError::UnsupportedOp(0xFA));
    }
    let mut candidate = f.clone();
    crate::t2::opt_lexical::forward(&mut candidate);
    let count = lower_environment(&mut candidate, &mut resolve)?;
    let count = count + lower_ordinary_primitives_inner(&mut candidate, &mut resolve)?;
    *f = candidate;
    Ok(count)
}

/// `lower_ordinary_primitives`, first lowering every plain `SpecialBinding`
/// call (bliss-gg3i1.7.4) to the BindSpecial/UnbindSpecial adapters.
/// `operand(bcp, enter)` names the binding's symbol index for an entry, or
/// the number of bindings an exit restores; the bytecode at `bcp` owns both.
/// All-or-nothing: on failure `f` is unchanged.
pub fn lower_ordinary_primitives_with_specials(
    f: &mut Function,
    mut resolve: impl FnMut(OrdinaryPrimitive) -> Option<u32>,
    operand: impl Fn(u32, bool) -> Option<u32>,
) -> Result<usize, EmitError> {
    if !has_special_bindings(f) {
        return lower_ordinary_primitives(f, resolve);
    }
    let mut candidate = f.clone();
    let specials = lower_special_bindings(&mut candidate, &mut resolve, &operand)?;
    let others = lower_ordinary_primitives(&mut candidate, resolve)?;
    *f = candidate;
    Ok(specials + others)
}

fn has_special_bindings(f: &Function) -> bool {
    f.block_order().iter().any(|&block| {
        f.block(block)
            .insts
            .iter()
            .any(|&inst| matches!(f.inst(inst).aux, AuxData::SpecialBinding { .. }))
    })
}

/// Whether `inst` is an unbinding exit that only forwards the complete result
/// of the call before it to the function's return: the block ends
/// `call, unbind..., return call-result`. The ordinary emitter restores those
/// bindings between the call and the return without touching the call's
/// multiple values (`forwarded_unbinds`).
fn forwarded_unbind(f: &Function, block: crate::t2::ir::Block, inst: crate::t2::ir::Inst) -> bool {
    let unbind = |site: crate::t2::ir::Inst| {
        let data = f.inst(site);
        data.opcode == Opcode::Call
            && matches!(data.aux, AuxData::SpecialBinding { enter: false, .. })
    };
    let insts = &f.block(block).insts;
    let Some(position) = insts.iter().position(|&site| site == inst) else {
        return false;
    };
    let mut first = position;
    while first > 0 && unbind(insts[first - 1]) {
        first -= 1;
    }
    let mut last = position;
    while last + 1 < insts.len() && unbind(insts[last + 1]) {
        last += 1;
    }
    let (Some(&call), Some(&ret)) = (
        first.checked_sub(1).and_then(|index| insts.get(index)),
        insts.get(last + 1),
    ) else {
        return false;
    };
    let call = f.inst(call);
    let ret = f.inst(ret);
    last + 2 == insts.len()
        && call.opcode == Opcode::Call
        && matches!(
            call.aux,
            AuxData::CallTarget(_) | AuxData::OpaqueCallTarget(_)
        )
        && call.results.len() == 1
        && ret.opcode == Opcode::Return
        && ret.args == call.results
}

fn insert_before(
    f: &mut Function,
    block: crate::t2::ir::Block,
    anchor: crate::t2::ir::Inst,
    data: InstData,
    results: &[(IRType, ValueRepresentation)],
) -> Vec<crate::t2::ir::Value> {
    let (inst, values) = f.push_inst(block, data, results);
    let layout = &mut f.block_mut(block).insts;
    assert_eq!(layout.pop(), Some(inst));
    let position = layout.iter().position(|&site| site == anchor).unwrap();
    layout.insert(position, inst);
    values
}

/// How one special-binding call treats the multiple values it receives.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingValues {
    /// The incoming tuple is empty (`normalize` false) or never read again
    /// (`normalize` true, cleared first): the adapter returns one value, as
    /// an ordinary call, and a reset records it.
    Single { normalize: bool },
    /// An exit between a call and the return of its complete result.
    Forwarded,
    /// The tuple may still be read: the binding leaves it alone, as the
    /// interpreter's does, and what follows sees it unchanged.
    Preserved,
}

fn lower_special_bindings(
    f: &mut Function,
    resolve: &mut impl FnMut(OrdinaryPrimitive) -> Option<u32>,
    operand: &impl Fn(u32, bool) -> Option<u32>,
) -> Result<usize, EmitError> {
    // A binding leaves the multiple values alone in the interpreter. Model
    // the bindings as transparent to find where their incoming tuple matters.
    let mut model = f.clone();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            if matches!(f.inst(inst).aux, AuxData::SpecialBinding { .. }) {
                let data = model.inst_mut(inst);
                data.opcode = Opcode::ConstNil;
                data.flags = InstFlags::default();
            }
        }
    }
    let single = crate::t2::opt_clear_mv::single_value_state(&model, true);
    let unobserved = crate::t2::opt_clear_mv::unobserved_multiple_value_state(&model);
    let mut sites = Vec::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            let AuxData::SpecialBinding { bcp, enter } = data.aux else {
                continue;
            };
            if data.opcode != Opcode::Call
                || !data.results.is_empty()
                || data.args.len() != usize::from(enter)
                || data
                    .args
                    .iter()
                    .any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
            {
                return Err(EmitError::Declined("special binding call shape"));
            }
            let operand = operand(bcp, enter).ok_or(EmitError::Declined(
                "special binding without a bytecode operand",
            ))?;
            let values = if single.contains(&inst) {
                BindingValues::Single { normalize: false }
            } else if unobserved.contains(&inst) {
                BindingValues::Single { normalize: true }
            } else if !enter && forwarded_unbind(f, block, inst) {
                BindingValues::Forwarded
            } else {
                BindingValues::Preserved
            };
            let primitive = match (enter, values) {
                (true, BindingValues::Preserved) => OrdinaryPrimitive::BindSpecialPreservingValues,
                (false, BindingValues::Preserved) => {
                    OrdinaryPrimitive::UnbindSpecialPreservingValues
                }
                (true, _) => OrdinaryPrimitive::BindSpecial,
                (false, _) => OrdinaryPrimitive::UnbindSpecial,
            };
            sites.push((
                block,
                inst,
                enter,
                operand,
                resolve(primitive).ok_or(EmitError::Declined("special binding adapter"))?,
                values,
            ));
        }
    }
    let clear = |source_pos| InstData {
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
    };
    for &(block, inst, enter, operand, target, values) in &sites {
        let source_pos = f.inst(inst).source_pos;
        // A forwarded exit keeps the call, its unbinds and the return
        // adjacent: its operand goes before the whole sequence.
        let mut anchor = inst;
        if values == BindingValues::Forwarded {
            // Earlier unbinds of the same run may already be lowered.
            let unbind = |site: crate::t2::ir::Inst| match f.inst(site).aux {
                AuxData::SpecialBinding { enter: false, .. } => true,
                AuxData::CallTarget(symbol) => symbol == target,
                _ => false,
            };
            let insts = &f.block(block).insts;
            let mut position = insts.iter().position(|&site| site == inst).unwrap();
            while position > 0 && unbind(insts[position - 1]) {
                position -= 1;
            }
            anchor = insts[position
                .checked_sub(1)
                .ok_or(EmitError::Declined("forwarded special-binding exit"))?];
        }
        let (opcode, aux, ty) = if enter {
            (
                Opcode::ConstSymbol,
                AuxData::SymbolRef(operand),
                IRType::of(TypeBits::SYMBOL),
            )
        } else {
            (
                Opcode::ConstFixnum,
                AuxData::FixnumImm(i64::from(operand)),
                IRType::of(TypeBits::FIXNUM),
            )
        };
        let constant = insert_before(
            f,
            block,
            anchor,
            InstData {
                opcode,
                args: vec![],
                results: vec![],
                aux,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos,
            },
            &[(ty, ValueRepresentation::Tagged)],
        );
        if values == (BindingValues::Single { normalize: true }) {
            insert_before(f, block, inst, clear(source_pos), &[]);
        }
        let data = f.inst_mut(inst);
        data.args.insert(0, constant[0]);
        data.aux = AuxData::CallTarget(target);
        data.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        if matches!(values, BindingValues::Single { .. }) {
            // The adapter returns one value, as an ordinary call; record it.
            let (reset, _) = f.push_inst(block, clear(source_pos), &[]);
            let layout = &mut f.block_mut(block).insts;
            assert_eq!(layout.pop(), Some(reset));
            let position = layout.iter().position(|&site| site == inst).unwrap();
            layout.insert(position + 1, reset);
        }
    }
    Ok(sites.len())
}

fn lower_ordinary_primitives_inner(
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
    let mut constructors = Vec::new();
    static CONSTRUCTORS_ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *CONSTRUCTORS_ENABLED
        .get_or_init(|| std::env::var_os("EGCL_DISABLE_NATIVE_ARRAY_CONSTRUCTOR").is_none())
    {
        for &block in f.block_order() {
            for &inst in &f.block(block).insts {
                if array_constructor_candidate(f, f.inst(inst)) {
                    constructors
                        .push((inst, resolve(OrdinaryPrimitive::MakeArray).ok_or_else(bad)?));
                }
            }
        }
    }
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
            replacements.push((
                block,
                inst,
                resolve(primitive).ok_or_else(bad)?,
                symbol,
                !single.contains(&inst),
            ));
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
                    source_pos: f.inst(inst).source_pos,
                },
                &[],
            );
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
    // Public MAKE-ARRAY has one result and clears argument multiple values.
    // Keep its original collecting call, argument homes and exact continuation;
    // only the target changes. The runtime adapter validates mutable objects.
    for &(inst, target) in &constructors {
        f.inst_mut(inst).aux = AuxData::CallTarget(target);
    }
    static CONSTRUCTOR_STATS: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !constructors.is_empty()
        && *CONSTRUCTOR_STATS
            .get_or_init(|| std::env::var_os("EGCL_NATIVE_ARRAY_CONSTRUCTOR_STATS").is_some())
    {
        eprintln!(
            "[native-array-constructor] lowered {} sites={}",
            f.name(),
            constructors.len()
        );
    }
    Ok(replacements.len() + constructors.len())
}

/// Only the full ordinary entry supplies this hidden argument. A mapped/OSR
/// entry never executes the rewritten candidate. No captured value is frozen:
/// the runtime adapter resolves the exact closure's shared frame each time.
fn lower_environment(
    f: &mut Function,
    resolve: &mut impl FnMut(OrdinaryPrimitive) -> Option<u32>,
) -> Result<usize, EmitError> {
    let bad = || EmitError::UnsupportedOp(0xFA);
    if !f.osr_entries.is_empty() {
        return Err(bad());
    }
    let mut model = f.clone();
    let mut sites = Vec::new();
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            let primitive = match data.opcode {
                Opcode::EnvironmentValue => OrdinaryPrimitive::LoadEnvironment,
                Opcode::SetEnvironmentValue => OrdinaryPrimitive::StoreEnvironment,
                _ => continue,
            };
            let AuxData::EnvironmentName(index) = data.aux else {
                return Err(bad());
            };
            let store = primitive == OrdinaryPrimitive::StoreEnvironment;
            if data.args.len() != usize::from(store)
                || data.results.len() != usize::from(!store)
                || !data.targets.is_empty()
                || data.frame_state.is_none()
                || data
                    .args
                    .iter()
                    .chain(&data.results)
                    .any(|&value| f.value(value).repr != ValueRepresentation::Tagged)
            {
                return Err(bad());
            }
            sites.push((block, inst, index, resolve(primitive).ok_or_else(bad)?));
            model.inst_mut(inst).flags.call = false;
        }
    }
    let single = crate::t2::opt_clear_mv::single_value_state(&model, true);
    let unobserved = crate::t2::opt_clear_mv::unobserved_multiple_value_state(&model);
    if sites
        .iter()
        .any(|(_, inst, _, _)| !single.contains(inst) && !unobserved.contains(inst))
    {
        return Err(bad());
    }
    let entry = f.entry();
    let (read, result) = f.push_inst(
        entry,
        InstData {
            opcode: Opcode::CurrentCallable,
            args: vec![],
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos: Default::default(),
        },
        &[(IRType::TOP, ValueRepresentation::Tagged)],
    );
    let layout = &mut f.block_mut(entry).insts;
    assert_eq!(layout.pop(), Some(read));
    layout.insert(0, read);
    let callable = result[0];
    for &(block, inst, index, target) in &sites {
        let source_pos = f.inst(inst).source_pos;
        let (constant, result) = f.push_inst(
            block,
            InstData {
                opcode: Opcode::ConstFixnum,
                args: vec![],
                results: vec![],
                aux: AuxData::FixnumImm(i64::from(index)),
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos,
            },
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        );
        let layout = &mut f.block_mut(block).insts;
        assert_eq!(layout.pop(), Some(constant));
        let position = layout.iter().position(|&site| site == inst).unwrap();
        layout.insert(position, constant);
        let data = f.inst_mut(inst);
        data.args.splice(0..0, [callable, result[0]]);
        data.opcode = Opcode::Call;
        data.aux = AuxData::CallTarget(target);
        data.flags = InstFlags {
            call: true,
            safepoint: true,
            effectful: true,
            ..Default::default()
        };
        // Like global-variable adapters, the semantic adapter returns exactly
        // one primary. Clear an unobservable incoming tuple before entry and
        // make the outgoing state explicit to later ordinary admission.
        for before in [true, false] {
            if before && single.contains(&inst) {
                continue;
            }
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
            let layout = &mut f.block_mut(block).insts;
            assert_eq!(layout.pop(), Some(clear));
            let position = layout.iter().position(|&site| site == inst).unwrap();
            layout.insert(position + usize::from(!before), clear);
        }
    }
    Ok(sites.len())
}

#[cfg(test)]
mod array_constructor_tests {
    use super::*;
    use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
    use crate::t2::ir::{BlockCall, Inst, Value};

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
    fn fixture(callee: &str, key: &str) -> (Function, Inst, Value) {
        let mut f = Function::new("ARRAY-CONSTRUCTOR-TEST");
        let block = f.entry();
        let dimensions = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let descriptor = f.add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
        let (_, keyword) = f.push_inst(
            block,
            InstData {
                aux: AuxData::SymbolRef(egcl_rt::symbols::intern(key)),
                ..plain(Opcode::ConstSymbol)
            },
            &[(IRType::of(TypeBits::SYMBOL), ValueRepresentation::Tagged)],
        );
        let args = vec![dimensions, keyword[0], descriptor];
        let state = f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: egcl_rt::symbols::intern("ARRAY-CONSTRUCTOR-TEST"),
                bcp: 7,
                locals: vec![],
                stack: args
                    .iter()
                    .map(|&value| ValueSource::Value {
                        value,
                        repr: ValueRepresentation::Tagged,
                    })
                    .collect(),
            }],
            remat: vec![],
        });
        let (site, result) = f.push_inst(
            block,
            InstData {
                aux: AuxData::CallTarget(egcl_rt::symbols::intern(callee)),
                args,
                flags: InstFlags {
                    effectful: true,
                    call: true,
                    safepoint: true,
                    ..InstFlags::default()
                },
                frame_state: Some(state),
                source_pos: 91,
                ..plain(Opcode::Call)
            },
            &[(IRType::TOP, ValueRepresentation::Tagged)],
        );
        f.set_terminator(
            block,
            InstData {
                args: vec![result[0]],
                ..plain(Opcode::Return)
            },
        );
        (f, site, descriptor)
    }

    #[test]
    fn constructor_requires_original_collecting_call_contract() {
        for flag in 0..3 {
            let (mut f, site, _) = fixture("MAKE-ARRAY", "KEYWORD:ELEMENT-TYPE");
            match flag {
                0 => f.inst_mut(site).flags.call = false,
                1 => f.inst_mut(site).flags.effectful = false,
                _ => f.inst_mut(site).flags.safepoint = false,
            }
            assert!(!array_constructor_candidate(&f, f.inst(site)));
            let before = format!("{f:?}");
            // Invalid IR may be rejected by the verifier; otherwise it must
            // remain untouched, without resolving a collecting adapter.
            let result = lower_ordinary_primitives(&mut f, |_| panic!("must not resolve"));
            assert!(result.is_err() || result == Ok(0));
            assert_eq!(format!("{f:?}"), before);
        }
    }

    #[test]
    fn constructor_adapter_keeps_original_operands_state_and_effects() {
        let (mut f, site, descriptor) = fixture("MAKE-ARRAY", "KEYWORD:ELEMENT-TYPE");
        let before = f.inst(site).clone();
        let adapter = egcl_rt::symbols::intern("TEST-PRIVATE-ARRAY-ADAPTER");
        let count = lower_ordinary_primitives(&mut f, |kind| {
            assert_eq!(kind, OrdinaryPrimitive::MakeArray);
            Some(adapter)
        })
        .unwrap();
        assert_eq!(count, 1);
        let after = f.inst(site);
        assert!(matches!(after.aux, AuxData::CallTarget(symbol) if symbol == adapter));
        assert_eq!(before.args, after.args);
        assert_eq!(before.results, after.results);
        assert_eq!(before.frame_state, after.frame_state);
        assert_eq!(before.flags, after.flags);
        assert_eq!(before.source_pos, after.source_pos);
        assert_eq!(
            f.value(descriptor).ty,
            IRType::TOP,
            "mutable descriptor remains opaque"
        );
        assert!(crate::t2::verify::verify(&f).is_ok());
    }

    #[test]
    fn constructor_aliases_keywords_and_unresolved_adapter_leave_ir_unchanged() {
        for (callee, key) in [
            ("OTHER::MAKE-ARRAY", "KEYWORD:ELEMENT-TYPE"),
            ("MAKE-ARRAY", "OTHER::ELEMENT-TYPE"),
            ("MAKE-ARRAY", "KEYWORD:INITIAL-ELEMENT"),
            ("FUNCALL", "KEYWORD:ELEMENT-TYPE"),
        ] {
            let (mut f, _, _) = fixture(callee, key);
            let before = format!("{f:?}");
            assert_eq!(
                lower_ordinary_primitives(&mut f, |_| panic!("must not resolve")).unwrap(),
                0
            );
            assert_eq!(format!("{f:?}"), before);
        }
        let (mut f, _, _) = fixture("MAKE-ARRAY", "KEYWORD:ELEMENT-TYPE");
        let before = format!("{f:?}");
        assert!(lower_ordinary_primitives(&mut f, |_| None).is_err());
        assert_eq!(format!("{f:?}"), before);
    }

    #[test]
    fn constructor_invoke_keeps_its_normal_and_exceptional_edges() {
        let (mut f, site, _) = fixture("MAKE-ARRAY", "KEYWORD:ELEMENT-TYPE");
        let cold = f.make_block();
        f.set_terminator(cold, plain(Opcode::Trap));
        f.make_call_exceptional(
            site,
            BlockCall {
                block: cold,
                args: vec![],
            },
        )
        .unwrap();
        let before = f.inst(site).clone();
        let adapter = egcl_rt::symbols::intern("TEST-PRIVATE-ARRAY-ADAPTER");
        assert_eq!(
            lower_ordinary_primitives(&mut f, |_| Some(adapter)).unwrap(),
            1
        );
        assert_eq!(f.inst(site).opcode, Opcode::Invoke);
        assert_eq!(f.inst(site).targets.len(), before.targets.len());
        for (a, b) in f.inst(site).targets.iter().zip(&before.targets) {
            assert_eq!(a.block, b.block);
            assert_eq!(a.args, b.args);
        }
        assert!(crate::t2::verify::verify(&f).is_ok());
    }
}

#[cfg(test)]
#[path = "ordinary_closure_lowering_tests.rs"]
mod closure_lowering_tests;
