// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::{BytecodeFunction, Instr, VarLoc};
use egcl_rt::jit_debug::NativeArguments;

/// Original actual arguments, not the current values of parameter bindings.
/// Only the canonical positional slot ABI has this lifetime-wide recipe. The
/// all-or-unavailable public argument list still retains missing locations in
/// DWARF, so future per-binding debugger APIs can distinguish them.
pub(super) fn arguments(
    body: &BytecodeFunction,
    has_register_entry: bool,
) -> Option<NativeArguments> {
    if has_register_entry
        || body.variadic
        || body.param_layout.len() != usize::from(body.arity)
        || body.min_args != body.arity
        || body.max_args != Some(body.arity)
        || egcl_rt::jit_debug::managed_entry_register().is_none()
        || body.param_layout.iter().enumerate().any(|(i, (_, loc))| {
            !matches!(loc, VarLoc::Slot(slot) if usize::from(*slot) == i && *slot < body.n_locals)
        })
    {
        return None;
    }
    let overwritten = |slot: u16| {
        body.code.iter().any(|instruction| match instruction {
            Instr::StoreLocal(target) => *target == slot,
            Instr::TakeValuesToLocals { slot_base, nvars } => {
                u32::from(slot) >= u32::from(*slot_base)
                    && u32::from(slot) < u32::from(*slot_base) + u32::from(*nvars)
            }
            _ => false,
        }) || body.handler_cases.iter().any(|cluster| {
            cluster
                .clauses
                .iter()
                .any(|clause| clause.var_slot == Some(slot))
        })
    };
    Some(NativeArguments {
        parameters: body
            .param_layout
            .iter()
            .enumerate()
            .map(|(i, (name, _))| {
                let slot = i as u16;
                (name.clone(), (!overwritten(slot)).then_some(slot))
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::super::*;

    #[cfg(all(target_arch = "x86_64", unix))]
    #[test]
    fn native_argument_homes_relocate_before_snapshot_capture() {
        let _lock = super::super::super::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(value)").unwrap().0);
        egcl_rt::rooted!(form = reader::read_from_string(
            "((eval '(%force-minor-gc-for-test)) (let ((snapshot (egcl::%debug-backtrace 32 0 nil))) (length value) snapshot))"
        ).unwrap().0);
        let symbol = egcl_rt::symbols::intern("NATIVE-ARGUMENT-MOVEMENT");
        let body = compile_function(
            "NATIVE-ARGUMENT-MOVEMENT",
            *params,
            *form,
            &env,
            true, // The private capture primitive uses a qualified forward call.
            false,
        )
        .unwrap_or_else(|| panic!("native fixture lowering: {:?}", last_bail_reason()));
        registry_put(symbol, Arc::new(body));
        for t2 in [false, true] {
            let native = if t2 {
                let input = snapshot_t2_input(symbol, 0).unwrap();
                let generation = input.generation;
                let input = egcl_rt::CrossThreadRoot::new(input);
                let artifact = input
                    .with_gc_stable(compile_t2_artifact)
                    .expect("compile real T2 collector");
                install_t2_completion(T2Completion {
                    sym: symbol,
                    generation,
                    artifact: Some(artifact),
                    input,
                })
                .unwrap()
            } else {
                try_promote_to_t1_with_speculation(symbol, false).unwrap()
            };
            assert_eq!(native.is_t2, t2);
            assert_eq!(native.compiled_entry, 0, "use the managed slot entry");
            egcl_rt::rooted!(datum = super::super::super::arena_str("moved native argument"));
            let before = datum.to_raw();
            egcl_rt::rooted!(snapshot = run_native(&native, symbol, &[*datum], &mut env).unwrap());
            assert_ne!(
                datum.to_raw(),
                before,
                "argument must move before the native capture"
            );
            let row = list_to_vec(*snapshot)
                .into_iter()
                .map(list_to_vec)
                .find(|row| super::super::super::val_as_str(row[0]) == "NATIVE-ARGUMENT-MOVEMENT")
                .expect("native frame survives return");
            assert_eq!(row[2], T);
            let args = row[1];
            assert_eq!(list_to_vec(args), vec![*datum]);
            assert_eq!(
                super::super::super::val_as_str(*datum),
                "moved native argument"
            );
        }
        registry_remove(symbol);
    }
}
