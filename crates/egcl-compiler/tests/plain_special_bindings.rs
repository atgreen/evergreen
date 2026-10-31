// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The plain special-binding build and its ordinary lowering
//! (bliss-gg3i1.7.4).

use egcl_compiler::control_scope::ScopeMap;
use egcl_compiler::t2::{
    build,
    inlining::InlineOptions,
    ir::{AuxData, Function, Opcode},
    verify,
};
use egcl_rt::EgclVal;
use egcl_rt::bytecode::{BytecodeFunction, Instr};

/// `(lambda (v) (let ((*plain-x* v)) (read-x)))`, optionally followed by a
/// second call after the binding ends.
fn binding_body(after: bool) -> BytecodeFunction {
    let special = egcl_rt::symbols::intern("*PLAIN-X*");
    let read = egcl_rt::symbols::intern("PLAIN-READ-X");
    let mut code = vec![
        Instr::LoadLocal(0),
        Instr::BindSpecial(special),
        Instr::CallNamed {
            sym: read,
            nargs: 0,
        },
        Instr::UnbindSpecial(1),
    ];
    if after {
        code.extend([
            Instr::Pop,
            Instr::CallNamed {
                sym: read,
                nargs: 0,
            },
        ]);
    }
    code.push(Instr::Return);
    BytecodeFunction {
        self_call_sites: Vec::new(),
        notinline_sites: Vec::new(),
        name: "plain-binding".into(),
        code,
        constants: vec![EgclVal::from_fixnum(0)],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 1,
        arity: 1,
        params_form: egcl_rt::value::NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
        binder_cache: Default::default(),
    }
}

fn plain(body: &BytecodeFunction) -> Function {
    let symbol = egcl_rt::symbols::intern("PLAIN-BINDING");
    build::build_for_special_binding_optimization_with_inline_options(
        body,
        symbol,
        InlineOptions::default().with_root_symbol(symbol),
    )
    .expect("a body whose only scopes are special bindings builds plainly")
}

fn special_calls(f: &Function) -> Vec<(Opcode, bool)> {
    f.block_order()
        .iter()
        .flat_map(|&block| f.block(block).insts.iter())
        .filter_map(|&inst| match f.inst(inst).aux {
            AuxData::SpecialBinding { enter, .. } => Some((f.inst(inst).opcode, enter)),
            _ => None,
        })
        .collect()
}

#[test]
fn bindings_build_as_plain_calls_and_legalize_to_invokes() {
    let body = binding_body(false);
    let mut f = plain(&body);
    assert_eq!(verify::verify(&f).map_err(|e| format!("{e:?}")), Ok(()));
    assert_eq!(
        special_calls(&f),
        vec![(Opcode::Call, true), (Opcode::Call, false)],
        "plain calls with no exceptional successor"
    );
    // The mapped clone gets the same transfer edges as any other call.
    let scopes = ScopeMap::analyze_function(&body).unwrap();
    build::legalize_transfer_calls(&mut f, &scopes).unwrap();
    assert_eq!(verify::verify(&f).map_err(|e| format!("{e:?}")), Ok(()));
    assert_eq!(
        special_calls(&f),
        vec![(Opcode::Invoke, true), (Opcode::Invoke, false)]
    );
}

#[test]
fn other_dynamic_scopes_and_unwinding_exits_are_refused() {
    let mut body = binding_body(false);
    body.code.insert(0, Instr::PushEnvChild);
    body.code.insert(body.code.len() - 1, Instr::PopEnvChild);
    let symbol = egcl_rt::symbols::intern("PLAIN-BINDING");
    assert!(
        build::build_for_special_binding_optimization_with_inline_options(
            &body,
            symbol,
            InlineOptions::default().with_root_symbol(symbol),
        )
        .is_err()
    );
}

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
mod lowering {
    use super::*;
    use egcl_compiler::t2::emit::{OrdinaryPrimitive, lower_ordinary_primitives_with_specials};

    fn lower(body: &BytecodeFunction) -> Function {
        let mut f = plain(body);
        lower_ordinary_primitives_with_specials(
            &mut f,
            |primitive| Some(egcl_rt::symbols::intern(&primitive.adapter_name())),
            |bcp, enter| match (enter, body.code.get(bcp as usize)?) {
                (true, Instr::BindSpecial(symbol)) => Some(*symbol),
                (false, Instr::UnbindSpecial(count)) => Some(u32::from(*count)),
                _ => None,
            },
        )
        .expect("lowered");
        assert_eq!(verify::verify(&f).map_err(|e| format!("{e:?}")), Ok(()));
        f
    }

    fn targets(f: &Function) -> Vec<String> {
        f.block_order()
            .iter()
            .flat_map(|&block| f.block(block).insts.iter())
            .filter_map(|&inst| match f.inst(inst).aux {
                AuxData::CallTarget(symbol) | AuxData::OpaqueCallTarget(symbol) => {
                    egcl_rt::symbols::registry_key(symbol)
                }
                _ => None,
            })
            .collect()
    }

    /// The exit between the call and the return of its complete result
    /// keeps the call's values: no reset follows it, so the emitter can
    /// forward the call's tuple across it.
    #[test]
    fn a_tail_exit_forwards_the_call_result() {
        let f = lower(&binding_body(false));
        assert_eq!(
            targets(&f),
            vec![
                OrdinaryPrimitive::BindSpecial.adapter_name(),
                "PLAIN-READ-X".to_string(),
                OrdinaryPrimitive::UnbindSpecial.adapter_name(),
            ]
        );
        let block = f.block_order()[0];
        let opcodes: Vec<_> = f
            .block(block)
            .insts
            .iter()
            .rev()
            .take(3)
            .map(|&inst| f.inst(inst).opcode)
            .collect();
        assert_eq!(
            opcodes,
            vec![Opcode::Return, Opcode::Call, Opcode::Call],
            "call, unbind, return stay adjacent"
        );
    }

    /// An exit whose incoming values a later call may still observe keeps
    /// them: the value-preserving adapter, with no reset around it.
    #[test]
    fn an_observed_exit_preserves_the_values() {
        let f = lower(&binding_body(true));
        assert!(
            targets(&f).contains(&OrdinaryPrimitive::UnbindSpecialPreservingValues.adapter_name()),
            "{:?}",
            targets(&f)
        );
    }
}
