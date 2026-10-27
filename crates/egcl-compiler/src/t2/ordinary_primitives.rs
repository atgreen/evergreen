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
}

impl OrdinaryPrimitive {
    /// The interned name of the private symbol whose adapter implements this
    /// operation, e.g. `%EGCL-ORDINARY-LOADGLOBAL`.
    pub fn adapter_name(self) -> String {
        format!("%EGCL-ORDINARY-{self:?}").to_ascii_uppercase()
    }

    pub const ALL: [Self; 10] = [
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

pub(super) fn packed_u8_adapter(target: u32) -> Option<PackedU8AdapterSymbols> {
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
