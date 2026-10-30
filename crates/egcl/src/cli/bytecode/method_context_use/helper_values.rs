// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Elide unreachable lexical continuation values, retaining dynamic context.
//! CallNamed does not export the VM activation's EnvFrame/function_vars to
//! ordinary Env. Source/host evaluation and any user closure remain opaque.
use super::*;

#[derive(Clone)]
pub(super) struct Sites {
    pub initializer: usize,
    pub creations: [usize; 2],
}

pub(super) fn elide(body: &mut BytecodeFunction, spec: &Spec, sites: &Sites) {
    macro_rules! decline {
        ($reason:literal) => {{
            #[cfg(test)]
            if std::env::var_os("EGCL_METHOD_HELPER_PROOF_DIAGNOSTICS").is_some() {
                eprintln!("[method-helper-proof] {} decline={}", body.name, $reason);
            }
            return;
        }};
    }
    // Deliberately bounded to the actual fixed-argument generated prologue.
    // Private values are consumed by these three exact stores, leaving an
    // empty operand stack; none can enter user dataflow without a key read.
    if body.variadic
        || sites.initializer != 0
        || sites.creations != [3, 4]
        || body.nested_functions.len() != 2
        || body.code.len() < 9
        || !body.handler_cases.is_empty()
        || !body.handler_binds.is_empty()
        || !body.restart_cases.is_empty()
    {
        decline!("shape/sites/variadic/handler metadata");
    }
    let key = |index: u16| body.names.get(usize::from(index)).map(String::as_str);
    let cnm = closure_fn_binding_key("CALL-NEXT-METHOD");
    let nmp = closure_fn_binding_key("NEXT-METHOD-P");
    match body.code[..8] {
        [
            Instr::CallNamed { sym, nargs: 0 },
            Instr::PushEnvChild,
            Instr::DefineEnvVar(context),
            Instr::MakeClosure {
                func: 0,
                capture_env: true,
            },
            Instr::MakeClosure {
                func: 1,
                capture_env: true,
            },
            Instr::PushEnvChild,
            Instr::DefineEnvVar(next),
            Instr::DefineEnvVar(call),
        ] if sym == spec.initializer
            && key(context) == Some(spec.context_key.as_str())
            && key(next) == Some(nmp.as_str())
            && key(call) == Some(cnm.as_str()) => {}
        _ => decline!("exact generated prologue/binding keys"),
    }
    for instruction in &body.code[8..] {
        match *instruction {
            Instr::EvalHost(_)
            | Instr::MakeClosureEnv(_)
            | Instr::MakeClosure { .. }
            | Instr::PushRestartCase { .. } => decline!("opaque opcode or user closure"),
            Instr::LoadEnvVar(index) | Instr::StoreEnvVar(index) | Instr::DefineEnvVar(index) => {
                let Some(name) = key(index) else {
                    decline!("invalid environment name index");
                };
                if name == spec.context_key || name == cnm || name == nmp {
                    decline!("helper/context environment access");
                }
            }
            Instr::LoadFunction(symbol)
                if ["CALL-NEXT-METHOD", "NEXT-METHOD-P"]
                    .iter()
                    .any(|name| reader::find_symbol_index(name) == Some(symbol)) =>
            {
                decline!("helper function value load");
            }
            _ => {}
        }
    }
    let nil = if let Some(index) = body.constants.iter().position(|value| value.is_nil()) {
        let Ok(index) = u16::try_from(index) else {
            decline!("constant index exceeds u16");
        };
        index
    } else {
        let Ok(index) = u16::try_from(body.constants.len()) else {
            decline!("constant table exceeds u16");
        };
        body.constants.push(NIL);
        index
    };
    // Only these two compiler-marker-authorized allocations change. Keep all
    // environment bindings, initializer calls, checks, stores and context proof.
    for pc in sites.creations {
        body.code[pc] = Instr::Const(nil);
    }
}

#[cfg(test)]
thread_local! {
    static CREATIONS: std::cell::Cell<([usize; 2], usize)> = const { std::cell::Cell::new(([0, 0], 0)) };
}

#[cfg(test)]
pub(in crate::cli::bytecode) fn record_creation(body: &Arc<BytecodeFunction>) {
    CREATIONS.with(|observed| {
        let (templates, count) = observed.get();
        if templates.contains(&(Arc::as_ptr(body) as usize)) {
            observed.set((templates, count + 1));
        }
    });
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(in crate::cli::bytecode) fn observe(templates: &[Arc<BytecodeFunction>]) -> impl Drop {
    assert_eq!(templates.len(), 2);
    struct Observation(([usize; 2], usize));
    impl Drop for Observation {
        fn drop(&mut self) {
            CREATIONS.with(|observed| observed.set(self.0));
        }
    }
    Observation(CREATIONS.with(|observed| {
        observed.replace((
            [
                Arc::as_ptr(&templates[0]) as usize,
                Arc::as_ptr(&templates[1]) as usize,
            ],
            0,
        ))
    }))
}

#[cfg(all(test, target_arch = "x86_64", target_os = "linux"))]
pub(in crate::cli::bytecode) fn creations() -> usize {
    CREATIONS.with(|observed| observed.get().1)
}
