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
    let decline = |reason| {
        t2_log_write(format_args!(
            "{}: mapped T2 declined ({reason}); keep mapped T1 or interpreter",
            display_fn_name(&input.body.name)
        ));
        None
    };
    if !fixed_tagged_parameters(&input.body) || !local_control_native_body(&input.body, true, true, false) {
        return decline("unsupported parameter or control-scope shape");
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
    let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&input.body).ok()?;
    let mut mapped = optimized.clone();
    if has_raw {
        // Only this full-entry clone has raw homes. The worker retains its
        // tagged artifact and T1 OSR path; raw imports require separate recipes.
        mapped.osr_entries.clear();
    }
    egcl_compiler::t2::build::legalize_transfer_calls(&mut mapped, &scopes).ok()?;
    egcl_compiler::t2::verify::verify(&mapped).ok()?;
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
        _roots: egcl_rt::CrossThreadRoot::new(T2InstalledBodies { bodies: owners }),
        deopt_bodies,
    });
    let roots = retain_native_body(body);
    let Some(mut code) = TransferCode::emit_prepared_with_ordinary(
        Arc::clone(body),
        roots,
        &prepared.ir,
        Some(&prepared.ordinary_ir),
        Some(Arc::clone(&metadata)),
        &prepared.call_cells,
        true,
        CompilationTier::T2,
    ) else {
        t2_log_write(format_args!(
            "{}: mapped T2 emission declined; keep mapped T1 or interpreter",
            display_fn_name(&body.name)
        ));
        return None;
    };
    code.installed_symbol = Some(symbol);
    let code = Rc::new(code);
    let entry = code.code.as_ptr();
    // Report the actual mapped bytes and positions, never the checked fallback.
    let bytes = unsafe { std::slice::from_raw_parts(entry, code.code_len) };
    maybe_write_perf_map(entry as usize, code.code_len, symbol);
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
        t2_log_write(format_args!(
            "{}: mapped T2 emitted; clear_mv_resets={clear_mv_resets}, raw_f64_values={raw_f64_values}",
            display_fn_name(&body.name)
        ));
    }
    Some(native)
}
