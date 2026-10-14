// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Ordinary optimizing-worker IR entering the existing mapped transfer emitter.
//! Unsupported shapes retain mapped T1, or remain interpreted when T1 declined.

use super::*;
use egcl_compiler::t2::ir::{Function, Value, ValueRepresentation};

pub(in crate::cli::bytecode) struct PreparedT2 {
    ir: Function,
    ordinary_ir: Function,
    ordinary_fallback: Option<Function>,
    self_specializations: Option<egcl_compiler::t2::emit::OrdinarySelfSpecializations>,
    call_cells: Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
}

#[cfg(not(egcl_no_t2))]
impl PreparedT2 {
    pub(in crate::cli::bytecode) fn ordinary_ir(&self) -> &Function { &self.ordinary_ir }

    pub(in crate::cli::bytecode) fn expand_self_calls(
        &mut self,
        input: &T2CompileInput,
        profile: &impl Fn(u32) -> Option<egcl_compiler::t2::speculate::SpecType>,
    ) {
        use egcl_compiler::t2::{build, inlining::InlineOptions, speculate, verify};
        if input.body.self_call_sites.is_empty() || !fixed_tagged_parameters(&input.body)
            || !scope_free_native_body(&input.body, false)
        { return; }
        // Tagged expansion must not displace a selected packed-float kernel.
        // Expanding raw local-call conventions has its own recovery contract.
        if (0..self.ir.num_values()).any(|index|
            self.ir.value(Value(index as u32)).repr != ValueRepresentation::Tagged)
        { return; }
        // Only this exact retained root can expand. Other callees keep named
        // dispatch; the existing mapped/OSR artifact remains unchanged.
        let mut options = InlineOptions::default().with_root_symbol(input.sym)
            .with_body(input.sym, Arc::clone(&input.body));
        options.config.self_inline_depth = 2;
        for saved in input.inline_bodies.iter().filter(|saved| saved.symbol == input.sym) {
            for &(bcp, calls) in &saved.call_sites {
                options = options.with_call_site_profile(input.sym, bcp, calls, saved.invocations);
            }
        }
        let Ok(mut candidate) = build::build_from_bytecode_with_inline_options(&input.body, options)
            else { return; };
        if !candidate.frame_states.iter().any(|(_, state)| state.scopes.len() > 1) { return; }
        speculate::speculate(&mut candidate, profile);
        optimize_t2_ir(&mut candidate);
        if verify::verify(&candidate).is_err()
            || candidate.frame_states.iter().any(|(_, state)| state.scopes.is_empty()
                || state.scopes.iter().any(|scope| scope.function != input.sym))
            || (0..candidate.num_values()).any(|index|
                candidate.value(Value(index as u32)).repr != ValueRepresentation::Tagged)
        { return; }
        let self_calls = proven_self_calls(&self.ordinary_ir, &input.body, input.sym);
        self.self_specializations = Some(egcl_compiler::t2::emit::OrdinarySelfSpecializations::prove(
            &self.ordinary_ir, usize::from(input.body.arity), &self_calls));
        self.ordinary_fallback = Some(std::mem::replace(&mut self.ordinary_ir, candidate));
    }
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
        ordinary_fallback: None,
        self_specializations: None,
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
    let emit = |ordinary, specializations| TransferCode::emit_prepared_with_ordinary(
        Arc::clone(body),
        Arc::clone(&roots),
        &prepared.ir,
        Some(ordinary),
        Some(Arc::clone(&metadata)),
        &prepared.call_cells,
        true,
        CompilationTier::T2,
        specializations,
    );
    let mut emitted = emit(&prepared.ordinary_ir, prepared.self_specializations);
    if !emitted.as_ref().is_some_and(|code| code.ordinary_entry.is_some()) {
        if let Some(fallback) = &prepared.ordinary_fallback {
            // Expansion is optional: an unsupported expanded shape must not
            // discard the ordinary ABI that the original candidate could use.
            emitted = emit(fallback, None).or(emitted);
        }
    }
    let Some(mut code) = emitted else {
        t2_log_write(format_args!(
            "{}: mapped T2 emission declined; keep mapped T1 or interpreter",
            display_fn_name(&body.name)
        ));
        return None;
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

#[cfg(test)]
mod worker_self_expansion_tests {
    use super::*;

    thread_local! {
        static OBSERVED_SCOPE_COUNT: Cell<u64> = const { Cell::new(0) };
        static OBSERVED_GUARD_COUNT: Cell<u64> = const { Cell::new(0) };
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn worker_self_expansion_preserves_selected_packed_f64() {
        let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let forms = "(declare (type fixnum n))
            (if (< n 1) 0
                (progn (setf (aref a 0) (aref a 0))
                    (+ 1 (worker-packed-self (- n 1) a))))";
        crate::cli::read_eval_all_env(
            &format!("(defun worker-packed-self (n a) {forms})"), &mut env,
        ).unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(n a)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(&format!("({forms})")).unwrap().0);
        let symbol = egcl_rt::symbols::intern("WORKER-PACKED-SELF");
        let body = Arc::new(compile_function("WORKER-PACKED-SELF", *params, *forms,
            &env, false, false).unwrap_or_else(|| panic!("packed fixture: {:?}", last_bail_reason())));
        assert!(!body.self_call_sites.is_empty());
        assert!(body.code.len() + body.param_types.iter().filter(|ty| !ty.is_any()).count()
            <= egcl_compiler::t2::inlining::InlineConfig::default().small_threshold as usize);
        registry_put(symbol, Arc::clone(&body));
        let input = snapshot_t2_input(symbol, 0).unwrap();
        let generation = input.generation;
        let input = egcl_rt::CrossThreadRoot::new(input);
        let artifact = input.with_gc_stable_mutator(compile_t2_artifact).unwrap();
        let prepared = artifact.mapped.as_ref().unwrap();
        let has_raw = |ir: &Function| (0..ir.num_values()).any(|index|
            ir.value(Value(index as u32)).repr == ValueRepresentation::UnboxedF64);
        assert!(has_raw(&prepared.ir), "worker must select the packed candidate");
        assert!(has_raw(&prepared.ordinary_ir), "expansion must preserve selected packed IR");
        assert!(prepared.ordinary_ir.frame_states.iter().all(|(_, state)| state.scopes.len() == 1));
        let code = install_t2_completion(T2Completion {
            sym: symbol, generation, artifact: Some(artifact), input,
        }).unwrap();
        let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("mapped owner") };
        let maps = mapped.value_maps_for_test();
        use egcl_compiler::t2::x64_calls::NativeCallValues;
        assert!(maps.iter().any(|site| match maps.value_map(site.return_offset) {
            Some(NativeCallValues::Frame(frame)) => frame.values().iter()
                .any(|value| value.repr == ValueRepresentation::UnboxedF64),
            Some(NativeCallValues::Deoptimizing { f64_slots, .. }) => !f64_slots.is_empty(),
            _ => false,
        }), "installed code must retain raw native homes");
        egcl_rt::rooted!(array = egcl_rt::packed_f64::make(1, 1.25).unwrap());
        egcl_rt::rooted!(result = run_native(&code, symbol,
            &[EgclVal::from_fixnum(1), *array], &mut env));
        assert_eq!(*result.as_ref().unwrap(), EgclVal::from_fixnum(1));
        assert_eq!(egcl_rt::packed_f64::get(*array, 0), Some(1.25));
    }

    #[test]
    #[ignore = "requires a platform-supported native segment transition"]
    fn worker_self_expansion_preserves_mapped_fallback_and_notinline() {
        let _lock = crate::cli::heap_test_lock().lock().unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        for (notinline, decline_expansion) in [(false, false), (true, false), (false, true)] {
            let policy = if notinline { "(notinline worker-self-expansion)" } else { "" };
            let forms = format!("(declare (type fixnum n) {policy})
                (if (< n 1) (+ (car x) 1)
                    (+ 1 (worker-self-expansion (- n 1) x)))");
            crate::cli::read_eval_all_env(
                &format!("(defun worker-self-expansion (n x) {forms})"), &mut env,
            ).unwrap();
            egcl_rt::rooted!(params = reader::read_from_string("(n x)").unwrap().0);
            egcl_rt::rooted!(forms = reader::read_from_string(&format!("({forms})")).unwrap().0);
            let symbol = egcl_rt::symbols::intern("WORKER-SELF-EXPANSION");
            let body = Arc::new(compile_function("WORKER-SELF-EXPANSION", *params, *forms,
                &env, false, false).unwrap());
            assert!(body.code.len() + body.param_types.iter().filter(|ty| !ty.is_any()).count()
                <= egcl_compiler::t2::inlining::InlineConfig::default().small_threshold as usize,
                "fixture must qualify under the real default cost threshold");
            registry_put(symbol, Arc::clone(&body));
            let input = snapshot_t2_input(symbol, 0).unwrap();
            let generation = input.generation;
            let input = egcl_rt::CrossThreadRoot::new(input);
            // No test-only inline-depth override: this is the actual worker.
            let mut artifact = input.with_gc_stable_mutator(compile_t2_artifact).unwrap();
            let prepared = artifact.mapped.as_ref().expect("mapped fallback retained");
            assert!(prepared.ir.frame_states.iter().all(|(_, state)| state.scopes.len() == 1),
                "mapped capture must remain single-scope");
            let deepest = prepared.ordinary_ir.frame_states.iter()
                .map(|(_, state)| state.scopes.len()).max().unwrap();
            assert_eq!(deepest, if notinline { 1 } else { 3 },
                "worker must expand only permitted self calls");
            if !notinline && !decline_expansion {
                // Exercise the separate mapped import protocol with the real
                // worker artifact. This is a synthetic entry import, not a
                // claim about a naturally hot loop's promotion policy.
                use egcl_compiler::t2::frame_state::{FrameScope, FrameState, ValueSource};
                use egcl_compiler::t2::ir::OsrEntry;
                let mapped = &mut artifact.mapped.as_mut().unwrap().ir;
                let entry = mapped.entry();
                let locals = mapped.block(entry).params.iter().map(|&value|
                    ValueSource::Value { value, repr: ValueRepresentation::Tagged }).collect();
                let state = mapped.frame_states.add(FrameState {
                    scopes: vec![FrameScope { function: symbol, bcp: 0, locals, stack: vec![] }],
                    remat: vec![],
                });
                mapped.osr_entries.push(OsrEntry { control_scopes: vec![], bcp: 0,
                    block: entry, frame_state: state, checks: vec![] });
                egcl_compiler::t2::verify::verify(mapped).unwrap();
            }
            if decline_expansion {
                // Fault injection: a valid IR call without a retained call cell
                // cannot be emitted through the ordinary ABI. The original
                // ordinary candidate must remain available for installation.
                use egcl_compiler::t2::ir::{AuxData, Inst, Opcode};
                let missing = egcl_rt::symbols::intern("WORKER-MISSING-CALL-CELL");
                assert!(!artifact.mapped.as_ref().unwrap().call_cells.iter()
                    .any(|(symbol, _)| *symbol == missing));
                let ordinary = &mut artifact.mapped.as_mut().unwrap().ordinary_ir;
                let mut changed = 0;
                for index in 0..ordinary.num_insts() {
                    let inst = ordinary.inst_mut(Inst(index as u32));
                    if inst.opcode == Opcode::Call {
                        inst.aux = AuxData::CallTarget(missing);
                        changed += 1;
                    }
                }
                assert!(changed > 0);
                egcl_compiler::t2::verify::verify(ordinary).unwrap();
            }
            let code = install_t2_completion(T2Completion {
                sym: symbol, generation, artifact: Some(artifact), input,
            }).unwrap();
            let NativeCodeStorage::Mapped(mapped) = &code._storage else { panic!("mapped owner") };
            assert!(mapped.ordinary_entry.is_some(), "install actual ordinary candidate");
            if !notinline && !decline_expansion {
                let destination = mapped.osr_destination();
                assert!(destination.ordinary_entry.is_none());
                let offset = *destination.osr_entries.get(&0).expect("mapped import retained");
                egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_fixnum(10), NIL));
                let before = (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp());
                let state = Box::new(ActivationState::prepare(destination, &mut env).unwrap());
                let frame = egcl_rt::current_stack().push_frame(EgclVal::from_symbol_index(symbol),
                    destination.code_info, destination.slots, FLAG_CALL).unwrap();
                unsafe {
                    slot_set(frame, 0, EgclVal::from_fixnum(0));
                    slot_set(frame, 1, *argument);
                }
                egcl_rt::rooted!(result = destination.run_activation(&[], &mut env,
                    Some(OsrResume { frame, offset, state }), NIL));
                assert_eq!(*result.as_ref().unwrap(), EgclVal::from_fixnum(11));
                assert_eq!((egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()), before);
            }
            if !notinline {
                egcl_rt::rooted!(argument = crate::cli::arena_cons(EgclVal::from_single_float(2.5), NIL));
                OBSERVED_SCOPE_COUNT.with(|seen| seen.set(0));
                OBSERVED_GUARD_COUNT.with(|seen| seen.set(0));
                let before = (egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp());
                egcl_rt::rooted!(result = deopt::with_ordinary_guard_resume_observer_for_test(
                    |count| {
                        OBSERVED_SCOPE_COUNT.with(|seen| seen.set(count));
                        OBSERVED_GUARD_COUNT.with(|seen| seen.set(seen.get() + 1));
                    },
                    || run_native(&code, symbol, &[EgclVal::from_fixnum(2), *argument], &mut env),
                ));
                assert_eq!(*result.as_ref().unwrap(), EgclVal::from_single_float(5.5));
                assert_eq!(OBSERVED_SCOPE_COUNT.with(Cell::get), if decline_expansion { 1 } else { 3 },
                    "installed code must use the selected candidate's recovery map");
                assert_eq!(OBSERVED_GUARD_COUNT.with(Cell::get), if decline_expansion { 3 } else { 1 },
                    "unexpanded recursion recovers separately at each numeric caller");
                assert_eq!((egcl_rt::current_stack().fp(), egcl_rt::current_stack().sp()), before);
            }
        }
    }
}
