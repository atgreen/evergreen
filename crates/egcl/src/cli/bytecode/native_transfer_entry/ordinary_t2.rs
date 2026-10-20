// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary optimizing-worker IR entering the existing mapped transfer emitter.
//! Unsupported shapes retain mapped T1, or remain interpreted when T1 declined.

use super::*;
use egcl_compiler::t2::ir::{Function, Value, ValueRepresentation};

pub(in crate::cli::bytecode) struct PreparedT2 {
    ir: Function,
    ordinary_ir: Function,
    call_cells: Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
}

pub(in crate::cli::bytecode) fn prepare_ordinary_t2(
    input: &T2CompileInput,
    optimized: &Function,
) -> Option<PreparedT2> {
    let decline = |reason: &str| {
        t2_log_write(format_args!(
            "{}: mapped T2 declined ({reason}); keep mapped T1 or interpreter",
            display_fn_name(&input.body.name)
        ));
        None
    };
    if !positional_tagged_parameters(&input.body) {
        return decline("unsupported parameter shape");
    }
    if !local_control_native_body(&input.body, true, true, false) {
        let op = first_refused_local_control_op(&input.body, true, true, false);
        return decline(&format!("unsupported instruction {}", op.as_deref().unwrap_or("?")));
    }
    // Transfer capture currently reconstructs one logical frame. In particular,
    // never attach the root body's scope map to an inlined callee's bytecode PC.
    if optimized
        .frame_states
        .iter()
        .any(|(_, state)| state.scopes.len() != 1 || state.scopes[0].function != input.sym)
    {
        return decline("multi-frame reconstruction not admitted");
    }
    let has_raw = (0..optimized.num_values())
        .any(|index| optimized.value(Value(index as u32)).repr != ValueRepresentation::Tagged);
    if has_raw && (!packed_f64_native_body(&input.body, optimized)
        || (0..optimized.num_values()).any(|index| !matches!(
            optimized.value(Value(index as u32)).repr,
            ValueRepresentation::Tagged | ValueRepresentation::UnboxedF64)))
    {
        return decline("unsupported raw representation or control scope");
    }
    let scopes = match egcl_compiler::control_scope::ScopeMap::analyze_function(&input.body) {
        Ok(scopes) => scopes,
        Err(error) => return decline(&format!("control-scope analysis: {error:?}")),
    };
    let mut mapped = optimized.clone();
    if has_raw {
        // Only this full-entry clone has raw homes. The worker retains its
        // tagged artifact and T1 OSR path; raw imports require separate recipes.
        mapped.osr_entries.clear();
    }
    if let Err(error) = egcl_compiler::t2::build::legalize_transfer_calls(&mut mapped, &scopes) {
        return decline(&format!("transfer-call legalization: {error:?}"));
    }
    if let Err(error) = egcl_compiler::t2::verify::verify(&mapped) {
        return decline(&format!("verify after legalization: {error:?}"));
    }
    Some(PreparedT2 {
        ir: mapped,
        ordinary_ir: optimized.clone(),
        call_cells: input.call_cells.clone(),
    })
}

pub(in crate::cli::bytecode) fn install_ordinary_t2(
    symbol: u32,
    body: &Arc<BytecodeFunction>,
    artifact: &mut T2Artifact,
) -> Option<Rc<NativeCode>> {
    let prepared = artifact.mapped.take()?;
    if !native_transfer::is_supported() {
        t2_log_write(format_args!(
            "{}: mapped T2 boundary unavailable; keep mapped T1 or interpreter",
            display_fn_name(&body.name)
        ));
        return None;
    }
    // Immutable cell owners arrive with the worker artifact. Installation must
    // not acquire the compiler-reader gate while running on a mutator: a
    // collector holding its write side may already be waiting for this thread.
    let mut owners = artifact.rooted_bodies.clone();
    if !owners.iter().any(|owner| Arc::ptr_eq(owner, body)) {
        owners.push(Arc::clone(body));
    }
    let mut deopt_bodies = artifact.deopt_bodies.clone();
    deopt_bodies.insert(symbol, Arc::clone(body));
    let metadata = Arc::new(T2InstalledMetadata {
        speculations: artifact.speculations.clone(),
        cold_guesses: artifact.cold_guesses.clone(),
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies { bodies: owners }),
        deopt_bodies,
    });
    let roots = retain_native_body(body);
    let mut code = match TransferCode::try_emit_prepared_with_ordinary(
        Arc::clone(body),
        roots,
        &prepared.ir,
        Some(&prepared.ordinary_ir),
        Some(Arc::clone(&metadata)),
        &prepared.call_cells,
        true,
        CompilationTier::T2,
    ) {
        Ok(code) => code,
        Err(decline) => {
            t2_log_write(format_args!(
                "{}: mapped T2 emission declined ({decline}); keep mapped T1 or interpreter",
                display_fn_name(&body.name)
            ));
            return None;
        }
    };
    code.set_installed_symbol(symbol);
    let code = Rc::new(code);
    let entry = code.code.as_ptr();
    // Report the actual mapped bytes and positions, never the checked fallback.
    let bytes = unsafe { std::slice::from_raw_parts(entry, code.code_len) };
    maybe_write_perf_map("T2", entry as usize, code.code_len, symbol);
    maybe_write_jitdump_code_load("T2", entry as usize, bytes, symbol);
    let native = Rc::new(NativeCode {
        _env_names: NativeEnvNames::new(body),
        _call_cells: artifact.call_cells.clone(),
        body: Some(Arc::clone(body)),
        transfer_abi_version: MAPPED_TRANSFER_ABI_VERSION,
        transfer_abi_arch: NATIVE_TRANSFER_ARCH,
        _direct_calls: Vec::new(),
        entry,
        code_len: code.code_len,
        is_t2: true,
        num_slots: code.slots,
        compiled_entry: 0,
        code_id: artifact.code_id,
        osr_entries: code.osr_entries.clone(),
        bcp_offsets: code.bcp_offsets.clone(),
        code_info: code.code_info,
        has_deopt: code.has_deopt,
        t2_metadata: Some(metadata),
        _storage: NativeCodeStorage::Mapped(code),
    });
    if t2_log_target().is_some() {
        let clear_mv_resets = prepared.ir.block_order().iter()
            .flat_map(|&block| prepared.ir.block(block).insts.iter())
            .filter(|&&inst| prepared.ir.inst(inst).opcode == egcl_compiler::t2::ir::Opcode::ClearMv)
            .count();
        let raw_f64_values = (0..prepared.ir.num_values()).filter(|&index|
            prepared.ir.value(Value(index as u32)).repr == ValueRepresentation::UnboxedF64
        ).count();
        let abi = match &native._storage {
            NativeCodeStorage::Mapped(code) if code.ordinary_entry.is_some() => "ordinary x86-64 Lisp",
            _ => "mapped",
        };
        t2_log_write(format_args!(
            "{}: {abi} T2 emitted; clear_mv_resets={clear_mv_resets}, raw_f64_values={raw_f64_values}",
            display_fn_name(&body.name)
        ));
    }
    Some(native)
}

/// Only source-proven calls in this retained body's logical frame chain can
/// bind directly. Each suspended parent must itself be a proven self expansion;
/// matching names in arbitrary inlined frames do not establish identity.
pub(super) fn proven_self_calls(ir: &Function, body: &BytecodeFunction, symbol: u32) -> Vec<egcl_compiler::t2::ir::Inst> {
    use egcl_compiler::t2::ir::{AuxData, Opcode};
    let Ok(depths) = verified_operand_stack_depths(body) else { return Vec::new() };
    let proven_site = |pc: u32| {
        body.self_call_sites.binary_search(&pc).is_ok()
            && matches!(body.code.get(pc as usize), Some(Instr::CallNamed { sym, nargs })
                if *sym == symbol && *nargs == body.arity)
    };
    ir.block_order().iter().flat_map(|&block| ir.block(block).insts.iter().copied())
        .filter(|&inst| {
            let data = ir.inst(inst);
            if data.opcode != Opcode::Call || !matches!(data.aux, AuxData::CallTarget(target) if target == symbol) {
                return false;
            }
            let Some(state) = data.frame_state else { return false };
            let scopes = &ir.frame_states.get(state).scopes;
            !scopes.is_empty() && data.args.len() == usize::from(body.arity)
                && scopes.iter().enumerate().all(|(index, scope)| {
                    let suspended = index + 1 < scopes.len();
                    if scope.function != symbol || scope.locals.len() != usize::from(body.n_locals)
                        || depths.get(scope.bcp as usize).copied().flatten()
                            != Some(scope.stack.len() as u32 + u32::from(suspended))
                    { return false; }
                    if suspended {
                        scope.bcp.checked_sub(1).is_some_and(|pc| proven_site(pc)
                            && depths.get(pc as usize).copied().flatten()
                                .and_then(|depth| depth.checked_sub(u32::from(body.arity)))
                                == Some(scope.stack.len() as u32))
                    } else {
                        proven_site(scope.bcp)
                    }
                })
        }).collect()
}
