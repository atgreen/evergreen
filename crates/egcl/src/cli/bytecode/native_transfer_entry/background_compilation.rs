// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;

pub(in crate::cli::bytecode) struct EmittedTransfer {
    installed_symbol: Option<u32>,
    is_t2: bool,
    code: JitBuffer,
    _native_calls: Arc<egcl_compiler::t2::x64_calls::NativeCallSites>,
    code_len: usize,
    frame_entry: Option<usize>,
    ordinary_entry: Option<usize>,
    allocation_extent_counts: (usize, usize, usize),
    ordinary_stack_required: Option<usize>,
    ordinary_bound_entry: Option<usize>,
    ordinary_checks_params: bool,
    _ordinary_keywords: Option<JitBuffer>,
    _ordinary_arity: Option<JitBuffer>,
    _ordinary_cold: Option<JitBuffer>,
    _primitive_cells: Vec<Arc<egcl_rt::call_table::CallCell>>,
    has_deopt: bool,
    bcp_offsets: Vec<u32>,
    osr_entries: HashMap<u32, usize>,
    osr_raw_doubles: HashMap<u32, Vec<u16>>,
    osr_companion: Option<Box<EmittedTransfer>>,
    osr_polls: HashMap<usize, u32>,
    _veneer: JitBuffer,
    _call_entries: Vec<(u32, Arc<egcl_rt::call_table::CallCell>, JitBuffer)>,
    _capture: JitBuffer,
    _poll: JitBuffer,
    _completion: JitBuffer,
    _recursive_prepare: Option<JitBuffer>,
    _deopt_capture: Option<JitBuffer>,
    _deopt_veneer: Option<JitBuffer>,
    recursive_symbol: Option<u32>,
    recursive_generation: u64,
    native_entry_kind_slot: Option<u32>,
    native_control_slot: Option<u32>,
    native_argument_base: Option<u32>,
    native_callable_slot: Option<u32>,
    native_argument_count: u16,
    landing: JitBuffer,
    sites: SysvTransferTable,
    slots: u16,
    cleanup_depths: std::collections::HashMap<u32, usize>,
    control_scopes: egcl_compiler::control_scope::ScopeMap,
    guard_handler_capacity: usize,
    special_binds: usize,
    catch_pushes: usize,
    arguments: Option<egcl_rt::jit_debug::NativeArguments>,
}

impl EmittedTransfer {
    pub(super) fn allocation_extent_counts(&self) -> (usize, usize, usize) {
        self.allocation_extent_counts
    }

    pub(super) fn install(
        mut self,
        body: Arc<BytecodeFunction>,
        roots: Arc<ActiveBytecodeRoot>,
        metadata: Option<Arc<T2InstalledMetadata>>,
    ) -> Result<TransferCode, EmitDecline> {
        let debug_info = self
            .code
            .install_debug_info_with_arguments(&body.name, self.arguments.as_ref())
            .map_err(|_| EmitDecline::DebugInfo)?;
        let code_info = install_stack_map(self.slots, debug_info).ok_or(EmitDecline::StackMap)?;
        let companion = self
            .osr_companion
            .take()
            .map(|companion| {
                companion
                    .install(Arc::clone(&body), Arc::clone(&roots), metadata.clone())
                    .map(Box::new)
            })
            .transpose()?;
        Ok(TransferCode {
            body,
            _body_roots: roots,
            code_info,
            deopt_metadata: metadata,
            deopts: Cell::new(0),
            recompile: Cell::new(false),
            private_probe_revision: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            private_probe_published: Cell::new(false),
            state_pool: RefCell::new(Vec::new()),
            callable_cache: Rc::new(CallableIdentityCache::default()),
            leaf_names: std::cell::OnceCell::new(),
            #[cfg(test)]
            unavailable_catch: None,
            #[cfg(test)]
            unavailable_handler: None,
            installed_symbol: self.installed_symbol,
            is_t2: self.is_t2,
            code: self.code,
            _native_calls: self._native_calls,
            code_len: self.code_len,
            frame_entry: self.frame_entry,
            ordinary_entry: self.ordinary_entry,
            ordinary_stack_required: self.ordinary_stack_required,
            ordinary_bound_entry: self.ordinary_bound_entry,
            ordinary_checks_params: self.ordinary_checks_params,
            _ordinary_keywords: self._ordinary_keywords,
            _ordinary_arity: self._ordinary_arity,
            _ordinary_cold: self._ordinary_cold,
            _primitive_cells: self._primitive_cells,
            has_deopt: self.has_deopt,
            bcp_offsets: self.bcp_offsets,
            osr_entries: self.osr_entries,
            osr_raw_doubles: self.osr_raw_doubles,
            osr_companion: companion,
            osr_polls: self.osr_polls,
            _veneer: self._veneer,
            _call_entries: self._call_entries,
            _capture: self._capture,
            _poll: self._poll,
            _completion: self._completion,
            _recursive_prepare: self._recursive_prepare,
            _deopt_capture: self._deopt_capture,
            _deopt_veneer: self._deopt_veneer,
            recursive_symbol: self.recursive_symbol,
            recursive_generation: self.recursive_generation,
            native_entry_kind_slot: self.native_entry_kind_slot,
            native_control_slot: self.native_control_slot,
            native_argument_base: self.native_argument_base,
            native_callable_slot: self.native_callable_slot,
            native_argument_count: self.native_argument_count,
            landing: self.landing,
            sites: self.sites,
            slots: self.slots,
            cleanup_depths: self.cleanup_depths,
            control_scopes: self.control_scopes,
            guard_handler_capacity: self.guard_handler_capacity,
            special_binds: self.special_binds,
            catch_pushes: self.catch_pushes,
        })
    }
    /// `emit_prepared_with_ordinary`, naming the step that refused the body
    /// (bliss-6tf0e). The ordinary and native-frame entries are optional: their
    /// refusals fall back to the mapped emitter, so only its refusal and the
    /// mandatory steps around it are reasons.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn try_emit_prepared_with_ordinary(
        body: &BytecodeFunction,
        ir: &egcl_compiler::t2::ir::Function,
        ordinary_ir: Option<&egcl_compiler::t2::ir::Function>,
        has_metadata: bool,
        call_cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
        context: &EmissionContext,
        tier: CompilationTier,
    ) -> Result<Self, EmitDecline> {
        let has_raw = (0..ir.num_values()).any(|index| {
            ir.value(egcl_compiler::t2::ir::Value(index as u32)).repr
                != egcl_compiler::t2::ir::ValueRepresentation::Tagged
        });
        if !has_raw || ir.osr_entries.is_empty() {
            return Self::try_emit_prepared_entries(
                body,
                ir,
                ordinary_ir,
                has_metadata,
                call_cells,
                context,
                tier,
                false,
            );
        }
        // Raw doubles (bliss-vwpaz): the full-call code is emitted exactly as
        // without loop headers, and a separate mapped owner with raw homes
        // imports a running lower-tier loop, unboxing its double locals. A
        // declined companion leaves a full-call-only version, as before.
        let mut primary_ir = ir.clone();
        primary_ir.osr_entries.clear();
        let mut code = Self::try_emit_prepared_entries(
            body,
            &primary_ir,
            ordinary_ir,
            has_metadata,
            call_cells,
            context,
            tier,
            false,
        )?;
        match Self::try_emit_prepared_entries(
            body,
            ir,
            None,
            has_metadata,
            call_cells,
            context,
            tier,
            true,
        ) {
            Ok(companion) if !companion.osr_entries.is_empty() => {
                t2_log_write(format_args!(
                    "{}: raw double OSR companion emitted; headers={:?}",
                    display_fn_name(&body.name),
                    companion.osr_entries.keys().collect::<Vec<_>>()
                ));
                code.osr_companion = Some(Box::new(companion));
            }
            Ok(_) => t2_log_write(format_args!(
                "{}: raw double OSR companion has no emittable loop header",
                display_fn_name(&body.name)
            )),
            Err(decline) => t2_log_write(format_args!(
                "{}: raw double OSR companion declined ({decline})",
                display_fn_name(&body.name)
            )),
        }
        Ok(code)
    }

    /// `raw_osr` admits raw double values together with OSR entries: the
    /// mapped-only companion of a raw-double version (bliss-vwpaz).
    #[allow(clippy::too_many_arguments)]
    fn try_emit_prepared_entries(
        body: &BytecodeFunction,
        ir: &egcl_compiler::t2::ir::Function,
        ordinary_ir: Option<&egcl_compiler::t2::ir::Function>,
        has_metadata: bool,
        call_cells: &[(u32, Arc<egcl_rt::call_table::CallCell>)],
        context: &EmissionContext,
        tier: CompilationTier,
        raw_osr: bool,
    ) -> Result<Self, EmitDecline> {
        let exec = |buffer: Option<JitBuffer>| buffer.ok_or(EmitDecline::ExecutableMemory);
        // Every normal mapped entry clears the complete MV buffer before
        // admission (outer, nested, and recursive). OSR deliberately preserves
        // pending values; the analysis treats each OSR header as unknown.
        let mut prepared_ir = ir.clone();
        egcl_compiler::t2::opt_clear_mv::eliminate_redundant(&mut prepared_ir, true);
        let ir = &prepared_ir;
        let capture = exec(JitBuffer::new(&emit_capture_stub(
            prepare,
            dispatch as *const u8,
        )))?;
        let deopt_capture = if has_metadata {
            Some(exec(JitBuffer::new(&emit_capture_stub(
                prepare_completed_deopt,
                dispatch as *const u8,
            )))?)
        } else {
            None
        };
        let mut deopt_veneer = match &deopt_capture {
            Some(capture) => Some(exec(JitBuffer::new(&emit_published_helper_veneer(
                resume_guard,
                capture.as_ptr(),
            )))?),
            None => None,
        };
        let veneer = exec(JitBuffer::new(&emit_published_helper_veneer(
            call_or_throw,
            capture.as_ptr(),
        )))?;
        let mut call_entries = Vec::new();
        for (sym, cell) in call_cells {
            if call_entries.iter().any(|(symbol, _, _)| symbol == sym) {
                continue;
            }
            let mapped = exec(JitBuffer::new(
                &egcl_compiler::t2::native_transfer::emit_published_call_veneer(
                    Arc::as_ptr(cell) as u64,
                    cell.entry_address(false) as u64,
                    cell.entry_address(true) as u64,
                    capture.as_ptr(),
                ),
            ))?;
            call_entries.push((*sym, Arc::clone(cell), mapped));
        }
        let named_veneers: Vec<_> = call_entries
            .iter()
            .map(|(symbol, _, adapter)| (*symbol, adapter.as_ptr() as u64))
            .collect();
        // Optimized baseline code still needs T1 heat and OSR handling. The
        // construction tier, not IR optimization or deopt metadata, selects this.
        let poll_fast_path = matches!(tier, CompilationTier::T2)
            .then_some(context.poll_fast_path)
            .flatten();
        let poll_helper = match tier {
            CompilationTier::T1 => poll_or_transfer,
            CompilationTier::T2 => poll_t2_or_transfer,
        };
        let poll = exec(JitBuffer::new(&emit_published_helper_veneer(
            poll_helper,
            capture.as_ptr(),
        )))?;
        let base_slots = body
            .n_locals
            .checked_add(body.max_stack)
            .ok_or(EmitDecline::FrameTooLarge)?;
        let completion = exec(JitBuffer::new(&emit_published_helper_veneer(
            complete_cleanup,
            capture.as_ptr(),
        )))?;
        let landing = exec(JitBuffer::new(&emit_native_landing_stub()))?;
        // Direct recursion first shares only immutable code/site metadata.
        // Dynamic handler and cleanup records still require their own mapped
        // activation ownership before these bodies can use recursive entries.
        // Sample before reading the definition: a concurrent replacement after
        // the identity check must leave a generation mismatch at entry.
        let recursive_generation = context.recursive_generation;
        let recursive_symbol = context.recursive_symbol;
        let mut recursive_prepare = match recursive_symbol {
            Some(_) => Some(exec(JitBuffer::new(&emit_published_helper_veneer(
                prepare_recursive,
                capture.as_ptr(),
            )))?),
            None => None,
        };
        let recursion = recursive_symbol.map(|symbol| RecursiveTransfer {
            symbol,
            arity: usize::from(body.arity),
            prepare: recursive_prepare.as_ref().unwrap().as_ptr() as u64,
            finish: finish_recursive as *const () as u64,
        });
        let native_frames = if matches!(tier, CompilationTier::T2)
            && body.arity <= 4
            && ir.osr_entries.is_empty()
            && has_metadata
        {
            recursive_symbol
                .zip(context.native_flags)
                .and_then(|(symbol, flags)| {
                    let cold = JitBuffer::new(&emit_published_helper_veneer(
                        native_frames::slow_call,
                        capture.as_ptr(),
                    ))?;
                    let guard = JitBuffer::new(&emit_published_helper_veneer(
                        deopt::resume_native_guard,
                        deopt_capture.as_ref()?.as_ptr(),
                    ))?;
                    let result =
                        egcl_compiler::t2::emit::emit_framed_native_handlers_with_native_recursion(
                            ir,
                            veneer.as_ptr() as u64,
                            base_slots,
                            save_cleanup as *const () as u64,
                            completion.as_ptr() as u64,
                            c2i_t2_mv as *const () as u64,
                            deliver_catch as *const () as u64,
                            deliver_handler as *const () as u64,
                            poll.as_ptr() as u64,
                            guard.as_ptr() as u64,
                            &named_veneers,
                            egcl_compiler::t2::emit::NativeRecursiveTransfer {
                                symbol,
                                arity: usize::from(body.arity),
                                initialize: native_frames::initialize as *const () as u64,
                                cold_veneer: cold.as_ptr() as u64,
                                flags,
                            },
                        )
                        .inspect_err(|error| {
                            t2_log_write(format_args!(
                                "{}: native recursive entry: {error:?}",
                                display_fn_name(&body.name)
                            ))
                        })
                        .ok()?;
                    recursive_prepare = Some(cold);
                    deopt_veneer = Some(guard);
                    Some(result)
                })
        } else {
            None
        };
        let mut ordinary_arity = None;
        let mut ordinary_cold = None;
        let mut ordinary_keywords = None;
        // Say which admission step keeps a body mapped-only (bliss-z4dlm);
        // these refusals used to be silent.
        let ordinary_declined = |reason: &dyn std::fmt::Display| {
            t2_log_write(format_args!(
                "{}: ordinary entry declined: {reason}",
                display_fn_name(&body.name)
            ))
        };
        let mut allocation_extent_counts = (0, 0, 0);
        let ordinary = ordinary_ir.filter(|ir| {
            let reason = if context.ordinary_suppressed {
                return false;
            } else if !context.ordinary_parameters {
                Some("parameter shape (ordinary_entry_parameters)".to_string())
            } else if !ordinary_native_body(body) {
                Some(format!("instruction {} (ordinary_native_body)",
                    first_refused_ordinary_op(body).as_deref().unwrap_or("parameter slots")))
            } else if !context.captured_entry_profitable {
                Some("captured environment helper cost (keep mapped entry)".to_string())
            } else if !ordinary_capture_protocol_present(body, ir) {
                Some("captured body without an explicit callable protocol".to_string())
            } else if !ordinary_primary_transport_eliminated(ir) {
                Some("a primary-value binder survived specialization (TakeValuesToLocals)".to_string())
            } else if !ordinary_single_frame_states(body, ir) {
                Some("a TAGBODY body with a multi-frame state (ordinary_single_frame_states)".to_string())
            } else {
                None
            };
            if let Some(reason) = &reason {
                ordinary_declined(reason);
            }
            reason.is_none()
        })
            .and_then(|ir| {
                use egcl_compiler::t2::emit::{OrdinaryLispEntryOptions, OrdinaryCallTarget, OrdinaryCallFlags};
                let mut ir = ir.clone();
                // Full calls start with inactive multiple values. OSR imports
                // retain separate descriptors and cannot constrain that proof.
                ir.osr_entries.clear();
                let mut call_cells = call_cells.to_vec();
                if let Err(error) = context.primitives.lower(body, &mut ir, &mut call_cells) {
                    ordinary_declined(&format!("primitive lowering (ordinary_primitives::lower): {error:?}"));
                    return None;
                }
                if std::env::var_os("EGCL_DISABLE_ARRAY_LAYOUT_FACTS").is_none() {
                    let layouts = egcl_compiler::t2::array_layout::hoist_layouts(&mut ir);
                    egcl_compiler::t2::opt_guard::reuse_layout_evidence(&mut ir);
                    if layouts != 0 {
                        t2_log_write(format_args!("{}: array layout witnesses: {layouts}", display_fn_name(&body.name)));
                    }
                }
                egcl_compiler::t2::opt_clear_mv::eliminate_redundant(&mut ir, true);
                use egcl_compiler::t2::emit::OrdinaryArgumentBinding;
                let keyword_plan = context.keyword_plan;
                let (entry_arity, binding) = if let Some(plan) = keyword_plan {
                    // The body is a function of its bound slots, entered
                    // with them as fixed arguments once the entry binds.
                    ir.set_variadic(false);
                    let decline = JitBuffer::new(&egcl_compiler::t2::native_transfer::emit_ordinary_entry_veneer(
                        ordinary_calls::keyword_decline, context.body_address))?;
                    let binding = OrdinaryArgumentBinding::Keywords(egcl_compiler::t2::emit::OrdinaryKeywordArguments {
                        bind: super::variadic_plan::native_keyword_bind as *const () as u64,
                        plan: plan.0,
                        decline: decline.as_ptr() as u64,
                    });
                    ordinary_keywords = Some(decline);
                    (plan.1, Some(binding))
                } else if body.min_args < body.arity {
                    (usize::from(body.arity), Some(OrdinaryArgumentBinding::Optional(
                        egcl_compiler::t2::emit::OrdinaryOptionalArguments {
                            min_arity: usize::from(body.min_args),
                            unsupplied: context.unsupplied,
                        })))
                } else {
                    (usize::from(body.arity), None)
                };
                // A typed body's entry checks its declarations and rejects
                // through a veneer that can name the failed one.
                let arity = JitBuffer::new(&if body.param_types.iter().any(|ty| !ty.is_any()) {
                    egcl_compiler::t2::native_transfer::emit_ordinary_arity_veneer(
                        ordinary_calls::wrong_arguments, context.body_address)
                } else {
                    egcl_compiler::t2::native_transfer::emit_ordinary_arity_veneer(
                        ordinary_calls::wrong_arity,
                        ordinary_calls::expected_arguments(body.min_args, body.arity))
                })?;
                let options = OrdinaryLispEntryOptions {
                    require_callable_home: context.private_noenv_callable_home && private_noenv_leaf_ir(&ir),
                    arity: entry_arity, arity_error_veneer: arity.as_ptr() as u64, binding,
                };
                let mut emitted = egcl_compiler::t2::emit::emit_framed_ordinary_leaf(&ir, options).ok()
                    .or_else(|| {
                        let flags = context.native_flags?;
                        let cold = JitBuffer::new(&egcl_compiler::t2::native_transfer::emit_ordinary_published_veneer(
                            ordinary_calls::cold_call))?;
                        let guard = has_metadata.then(||
                            JitBuffer::new(&egcl_compiler::t2::native_transfer::emit_ordinary_published_veneer(
                                ordinary_calls::guard))).flatten();
                        let emit_call = egcl_compiler::t2::emit::emit_framed_ordinary_tail_call;
                        #[cfg(test)]
                        let emit_call = if context.tail_emission {
                            emit_call
                        } else { egcl_compiler::t2::emit::emit_framed_ordinary_call };
                        let call_flags = OrdinaryCallFlags { gc_requested: flags.gc_requested,
                            sigusr1_pending: flags.sigusr1_pending, posted_signal: flags.posted_signal,
                            native_errors: flags.native_errors, signal_epoch: flags.signal_epoch };
                        let self_calls = if matches!(tier, CompilationTier::T2) && !body.variadic {
                            context.body_symbol
                                .map(|symbol| ordinary_t2::proven_self_calls(&ir, body, symbol))
                                .unwrap_or_default()
                        } else { Vec::new() };
                        let emit_general = || {
                            use egcl_compiler::t2::ir::Opcode;
                            let targets = ir.block_order().iter()
                                .flat_map(|&block| ir.block(block).insts.iter().copied())
                                .filter(|&inst| ir.inst(inst).opcode == Opcode::Call)
                                .map(|inst| {
                                    let symbol = ir.inst(inst).aux.named_call_target()?;
                                    let (_, cell) = call_cells.iter().find(|(name, _)| *name == symbol)?;
                                    Some((inst, OrdinaryCallTarget { symbol,
                                        descriptor_slot: cell.ordinary_entry_address() as u64,
                                        heat_slot: cell.ordinary_heat_address() as u64,
                                    revision_address: cell.revision_address() as u64 }))
                                }).collect::<Option<Vec<_>>>()?;
                            egcl_compiler::t2::emit::emit_framed_ordinary_function_with_self_call_facts(
                                &ir, options, &targets, call_flags, cold.as_ptr() as u64,
                                egcl_compiler::t2::emit::OrdinaryGuardOptions {
                                    veneer: guard.as_ref()?.as_ptr() as u64,
                                    multiple_values: c2i_t2_mv as *const () as u64,
                                }, poll.as_ptr() as u64, &self_calls,
                                egcl_compiler::t2::emit::OrdinarySelfCallFacts {
                                    // Root run scopes a matching control; named
                                    // ingress validates it. Runtime adapters
                                    // re-enter run, and callbacks restore the
                                    // enclosing control before native resume.
                                    trusted_control: true,
                                    // T2 is final: `record_loop_heat` has no
                                    // promotion left to request, so an idle
                                    // loop poll need not reach Rust. T1 keeps
                                    // sampling the heat that promotes it.
                                    top_tier: matches!(tier, CompilationTier::T2),
                                }).inspect_err(|error| t2_log_write(format_args!(
                                    "{}: ordinary entry declined: {error:?}", display_fn_name(&body.name)
                                ))).ok()
                        };
                        // Proven self calls must not be swallowed by an earlier
                        // specialized emitter that still performs dynamic lookup.
                        let emitted = if !self_calls.is_empty() {
                            emit_general()
                        } else {
                        call_cells.iter().find_map(|(symbol, cell)| {
                            let target = OrdinaryCallTarget { symbol: *symbol,
                                    descriptor_slot: cell.ordinary_entry_address() as u64,
                                    heat_slot: cell.ordinary_heat_address() as u64,
                                    revision_address: cell.revision_address() as u64 };
                            emit_call(&ir, options, target, call_flags, cold.as_ptr() as u64).ok()
                                .or_else(|| egcl_compiler::t2::emit::emit_framed_ordinary_guarded_call(
                                    &ir, options, target, call_flags, cold.as_ptr() as u64,
                                    egcl_compiler::t2::emit::OrdinaryGuardOptions {
                                        veneer: guard.as_ref()?.as_ptr() as u64,
                                        multiple_values: c2i_t2_mv as *const () as u64,
                                    }).ok())
                        }).or_else(|| {
                            use egcl_compiler::t2::ir::Opcode;
                            let calls: Vec<_> = ir.block_order().iter()
                                .flat_map(|&block| &ir.block(block).insts)
                                .filter(|&&inst| ir.inst(inst).opcode == Opcode::Call)
                                .collect();
                            if calls.len() != 2 { return None; }
                            let target = |inst| {
                                let symbol = ir.inst(inst).aux.named_call_target()?;
                                let (_, cell) = call_cells.iter().find(|(name, _)| *name == symbol)?;
                                Some(OrdinaryCallTarget { symbol,
                                    descriptor_slot: cell.ordinary_entry_address() as u64,
                                    heat_slot: cell.ordinary_heat_address() as u64,
                                    revision_address: cell.revision_address() as u64 })
                            };
                            egcl_compiler::t2::emit::emit_framed_ordinary_two_calls(
                                &ir, options, [target(*calls[0])?, target(*calls[1])?], call_flags,
                                cold.as_ptr() as u64, c2i_t2_mv as *const () as u64).ok()
                        }).or_else(emit_general)
                        }?;
                        if emitted.has_deopt { deopt_veneer = guard; }
                        ordinary_cold = Some(cold);
                        Some(emitted)
                    })?;
                let sites = SysvTransferTable::new(emitted.code.len(),
                    std::mem::take(&mut emitted.native_poll_transfer_sites)).ok()?;
                // Count only the IR whose machine emission and exact native
                // transfer-site construction both succeeded. The outer mapped
                // candidate predates these ordinary-only allocation rewrites.
                allocation_extent_counts = ir.block_order().iter()
                    .flat_map(|&block| ir.block(block).insts.iter())
                    .fold((0, 0, 0), |(values, proofs, dimensions), &inst| {
                        use egcl_compiler::t2::ir::{AuxData, Opcode};
                        let d = ir.inst(inst);
                        (values + usize::from(d.opcode == Opcode::PackedU8AllocationExtent),
                         proofs + usize::from(d.opcode == Opcode::TryStoreIntegerOctets1To8Extent
                             && matches!(d.aux, AuxData::AllocationExtentInBounds)),
                         dimensions + usize::from(d.opcode == Opcode::TryStoreIntegerOctets1To8Layout
                             && matches!(d.aux, AuxData::AllocationDimensionInBounds)))
                    });
                ordinary_arity = Some(arity);
                Some((emitted, sites))
            });
        // A shape not yet admitted by the native-frame emitter retains its
        // existing mapped execution and original recovery veneers.
        let has_raw = (0..ir.num_values()).any(|index| {
            ir.value(egcl_compiler::t2::ir::Value(index as u32)).repr
                != egcl_compiler::t2::ir::ValueRepresentation::Tagged
        });
        let storage = if has_raw {
            if !matches!(tier, CompilationTier::T2)
                || !has_metadata
                || (!ir.osr_entries.is_empty() && !raw_osr)
                || !raw_numeric_native_body(body, ir)
            {
                return Err(EmitDecline::RawValues);
            }
            egcl_compiler::t2::emit::NativeCallStorage::NativeStack
        } else {
            egcl_compiler::t2::emit::NativeCallStorage::ManagedActivation
        };
        // An ordinary call cannot import a live managed activation. Retain a
        // distinct mapped owner (including its maps and recovery veneers) for
        // each existing OSR header. Passing no ordinary IR prevents recursion.
        let osr_companion = if ordinary.is_some() && !ir.osr_entries.is_empty() {
            Some(Box::new(
                Self::try_emit_prepared_with_ordinary(
                    body,
                    ir,
                    None,
                    has_metadata,
                    call_cells,
                    context,
                    tier,
                )
                .map_err(|decline| EmitDecline::OsrCompanion(Box::new(decline)))?,
            ))
        } else {
            None
        };
        let (emitted, sites) = match ordinary.or(native_frames) {
            Some(emitted) => emitted,
            None => egcl_compiler::t2::emit::emit_framed_native_handlers_with_entries_storage_and_poll_fast_path(
            ir,
            veneer.as_ptr() as u64,
            base_slots,
            save_cleanup as *const () as u64,
            completion.as_ptr() as u64,
            c2i_t2_mv as *const () as u64,
            deliver_catch as *const () as u64,
            deliver_handler as *const () as u64,
            poll.as_ptr() as u64,
            recursion,
            deopt_veneer.as_ref().map(|code| code.as_ptr() as u64),
            &named_veneers,
            egcl_compiler::t2::emit::NativeRootPublication::PublishedHomes,
            storage,
            poll_fast_path,
        )
        .inspect_err(|error| t2_log_write(format_args!(
            "{}: mapped emitter: {error:?}{}",
            display_fn_name(&body.name), describe_emit_error(error)
        )))
        .map_err(EmitDecline::Emitter)?,
        };
        // Reconstruct local, non-escaping BLOCK/TAGBODY records and pending
        // UNWIND-PROTECT cleanups from exact scope maps. Other records require the
        // forthcoming native scope state; rejecting them is part of admission.
        for site in sites.sites() {
            if let Some(scope) = site.map().control_scopes.iter().find(|scope| {
                scope.ownership != Ownership::Local
                    || !matches!(
                        scope.kind,
                        ScopeKind::Block {
                            register: false,
                            ..
                        } | ScopeKind::Tagbody { .. }
                            | ScopeKind::Unwind { .. }
                            | ScopeKind::Cleanup { .. }
                            | ScopeKind::Catch { .. }
                            | ScopeKind::HandlerCase { .. }
                            | ScopeKind::HandlerBind { .. }
                            | ScopeKind::RestartCase { .. }
                            | ScopeKind::SpecialBinding { .. }
                            | ScopeKind::LexicalEnvironment
                    )
            }) {
                return Err(EmitDecline::TransferScope(
                    scope.kind.clone(),
                    scope.ownership,
                ));
            }
        }
        let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(body)
            .map_err(EmitDecline::Scopes)?;
        let guard_handler_capacity = (0..body.code.len())
            .filter_map(|bcp| scopes.before(bcp as u32))
            .map(|active| {
                active
                    .iter()
                    .filter(|scope| {
                        !matches!(
                            scope.kind,
                            ScopeKind::Cleanup { .. }
                                | ScopeKind::PendingCleanup { .. }
                                | ScopeKind::SpecialBinding { .. }
                                | ScopeKind::LexicalEnvironment
                        )
                    })
                    .count()
            })
            .max()
            .unwrap_or(0);
        let mut cleanup_depths = std::collections::HashMap::new();
        for instruction in &body.code {
            if let Instr::PushUnwind { cleanup_bcp, .. } = instruction {
                let depth = scopes
                    .before(*cleanup_bcp)
                    .ok_or(EmitDecline::CleanupScope(*cleanup_bcp))?
                    .iter()
                    .filter(|scope| {
                        matches!(
                            scope.kind,
                            ScopeKind::Block { .. }
                                | ScopeKind::Tagbody { .. }
                                | ScopeKind::Unwind { .. }
                                | ScopeKind::Catch { .. }
                                | ScopeKind::HandlerCase { .. }
                                | ScopeKind::HandlerBind { .. }
                                | ScopeKind::RestartCase { .. }
                        )
                    })
                    .count();
                cleanup_depths.insert(*cleanup_bcp, depth);
            }
        }
        let slots = base_slots
            .checked_add(emitted.shadow_root_slots)
            .ok_or(EmitDecline::FrameTooLarge)?;
        if emitted
            .frame_entry
            .is_some_and(|offset| offset >= emitted.code.len())
        {
            return Err(EmitDecline::FrameEntry);
        }
        let code = exec(JitBuffer::new(&emitted.code))?;
        let arguments = native_debug::arguments(
            body,
            emitted.native_recursive_entry.is_some() || emitted.ordinary_entry.is_some(),
        );
        let osr_polls = emitted
            .native_calls
            .as_ref()
            .ok_or(EmitDecline::NativeCallSites)?
            .iter()
            .filter_map(|site| {
                let egcl_compiler::t2::x64_calls::NativeCallOrigin::LoopPoll(block) = site.origin
                else {
                    return None;
                };
                ir.osr_entries
                    .iter()
                    .find(|entry| entry.block == block)
                    .filter(|entry| {
                        let state = ir.frame_states.get(entry.frame_state);
                        state.scopes.len() == 1 && state.scopes[0].stack.is_empty()
                    })
                    .map(|entry| (site.return_offset, entry.bcp))
            })
            .collect();
        // Fixed for this code's lifetime, and every activation sizes buffers by
        // them: count once here, not by scanning the bytecode on each call.
        let osr_entries: HashMap<u32, usize> = emitted.osr_entries.into_iter().collect();
        let osr_raw_doubles = ir
            .osr_entries
            .iter()
            .filter(|entry| osr_entries.contains_key(&entry.bcp))
            .filter_map(|entry| {
                let state = ir.frame_states.get(entry.frame_state);
                let slots: Vec<u16> = state
                    .scopes
                    .first()?
                    .locals
                    .iter()
                    .enumerate()
                    .filter(|(_, source)| {
                        matches!(source,
                        egcl_compiler::t2::frame_state::ValueSource::Value { repr, .. }
                            if *repr == egcl_compiler::t2::ir::ValueRepresentation::UnboxedF64)
                    })
                    .map(|(slot, _)| slot as u16)
                    .collect();
                (!slots.is_empty()).then_some((entry.bcp, slots))
            })
            .collect();
        let special_binds = body
            .code
            .iter()
            .filter(|op| matches!(op, Instr::BindSpecial(_)))
            .count();
        let catch_pushes = body
            .code
            .iter()
            .filter(|op| matches!(op, Instr::PushCatch { .. }))
            .count();
        Ok(Self {
            installed_symbol: None,
            is_t2: matches!(tier, CompilationTier::T2),
            code,
            arguments,
            code_len: emitted.code.len(),
            frame_entry: emitted.frame_entry,
            ordinary_entry: emitted.ordinary_entry,
            allocation_extent_counts: if emitted.ordinary_entry.is_some() {
                allocation_extent_counts
            } else {
                (0, 0, 0)
            },
            ordinary_stack_required: emitted.ordinary_stack_required,
            ordinary_bound_entry: emitted.ordinary_bound_entry,
            ordinary_checks_params: emitted.ordinary_checks_params,
            _ordinary_keywords: ordinary_keywords,
            _ordinary_arity: ordinary_arity,
            _ordinary_cold: ordinary_cold,
            _primitive_cells: context.primitives.owners(),
            _native_calls: Arc::new(emitted.native_calls.ok_or(EmitDecline::NativeCallSites)?),
            has_deopt: emitted.has_deopt,
            bcp_offsets: emitted.bcp_offsets,
            osr_entries,
            osr_raw_doubles,
            osr_companion,
            osr_polls,
            _veneer: veneer,
            _call_entries: call_entries,
            _capture: capture,
            _poll: poll,
            _completion: completion,
            _recursive_prepare: recursive_prepare,
            _deopt_capture: deopt_capture,
            _deopt_veneer: deopt_veneer,

            recursive_symbol,
            recursive_generation,
            native_entry_kind_slot: emitted.native_entry_kind_slot,
            native_control_slot: emitted.native_control_slot,
            native_argument_base: emitted.native_argument_base,
            native_callable_slot: emitted.native_callable_slot,
            native_argument_count: emitted.native_argument_count,
            landing,
            sites,
            slots,
            cleanup_depths,
            control_scopes: scopes,
            guard_handler_capacity,
            special_binds,
            catch_pushes,
        })
    }
}

/// Values that require a registered Lisp execution or access to moving heap
/// metadata. Capture them before handing detached compiler data to a worker.
#[derive(Clone)]
pub(in crate::cli::bytecode) struct EmissionContext {
    body_address: usize,
    body_symbol: Option<u32>,
    recursive_symbol: Option<u32>,
    recursive_generation: u64,
    poll_fast_path: Option<egcl_compiler::t2::emit::NativePollFastPath>,
    native_flags: Option<egcl_compiler::t2::emit::NativeRecursiveFlags>,
    keyword_plan: Option<(u64, usize)>,
    unsupplied: u64,
    ordinary_parameters: bool,
    ordinary_suppressed: bool,
    captured_entry_profitable: bool,
    private_noenv_callable_home: bool,
    primitives: ordinary_primitives::Bindings,
    #[cfg(test)]
    tail_emission: bool,
}

impl EmissionContext {
    pub(in crate::cli::bytecode) fn capture_for_owner(
        body: &Arc<BytecodeFunction>,
        allow_recursion: bool,
        owner: u32,
    ) -> Self {
        let mut context = Self::capture(body, allow_recursion);
        // The actual publication identity is authoritative. A lambda's display
        // name may resolve to an unrelated interned symbol and proves nothing.
        context.private_noenv_callable_home = egcl_rt::symbols::is_uninterned(owner)
            && registry_get(owner).is_some_and(|current| Arc::ptr_eq(&current, body))
            && CLOSURE_COMPILATION.with(|cache| {
                cache
                    .borrow()
                    .get(&(Arc::as_ptr(body) as usize))
                    .is_some_and(|state| state.owner == owner)
            })
            && private_noenv_callable_body(body);
        context
    }

    pub(in crate::cli::bytecode) fn captured_entry_profitable(&self) -> bool {
        self.captured_entry_profitable
    }

    pub(in crate::cli::bytecode) fn capture(
        body: &Arc<BytecodeFunction>,
        allow_recursion: bool,
    ) -> Self {
        // Sample the generation before checking the currently installed body.
        let recursive_generation = direct_call_gen();
        let body_symbol = crate::cli::resolve_sym(&body.name).map(EgclVal::as_symbol_index);
        let recursive_symbol = body_symbol
            .filter(|_| {
                allow_recursion
                    && fixed_tagged_parameters(body)
                    && scope_free_native_body(body, false)
            })
            .filter(|symbol| recursive_definition_matches(*symbol, Arc::as_ptr(body)))
            .filter(|symbol| {
                body.code
                    .iter()
                    .any(|op| matches!(op, Instr::CallNamed { sym, .. } if sym == symbol))
            });
        let keyword_plan = body
            .variadic
            .then(|| super::super::variadic_plan::VariadicPlan::native_keywords(body))
            .flatten()
            .map(|plan| {
                (
                    std::ptr::from_ref(plan) as u64,
                    super::super::variadic_plan::bound_values(plan),
                )
            });
        Self {
            body_address: Arc::as_ptr(body) as usize,
            body_symbol,
            recursive_symbol,
            recursive_generation,
            poll_fast_path: native_poll::binding(),
            native_flags: native_frames::flags(),
            keyword_plan,
            unsupplied: super::super::unsupplied_marker().0,
            ordinary_parameters: ordinary_entry_parameters(body),
            ordinary_suppressed: ordinary_admission_suppressed(),
            captured_entry_profitable: ordinary_capture_entry_profitable(body),
            private_noenv_callable_home: false,
            primitives: ordinary_primitives::Bindings::capture(),
            #[cfg(test)]
            tail_emission: ORDINARY_TAIL_EMISSION.with(Cell::get),
        }
    }
}

pub(in crate::cli::bytecode) struct PreparedT1 {
    snapshot: egcl_compiler::t2::constants::BytecodeSnapshot,
    // Never inspected by the worker: the root owns original constant slots and
    // keyword plans while the detached snapshot supplies all compiler reads.
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    context: EmissionContext,
    symbol: u32,
    profiles: HashMap<u32, TypeProfile>,
    call_cells: Vec<(u32, Arc<egcl_rt::call_table::CallCell>)>,
    allow_speculation: bool,
}

pub(in crate::cli::bytecode) struct CompiledT1 {
    emitted: EmittedTransfer,
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    speculations: Vec<(u32, SpecType)>,
    cold_guesses: Vec<u32>,
    has_metadata: bool,
}

pub(in crate::cli::bytecode) fn prepare_background_t1(
    symbol: u32,
    body: Arc<BytecodeFunction>,
    allow_speculation: bool,
) -> Option<PreparedT1> {
    if !native_transfer::is_supported()
        || !activation_bound_parameters(&body)
        || captured_environment_decline(&body).is_some()
        || body.code.iter().any(|op| {
            matches!(
                op,
                Instr::PushBlock { register: true, .. } | Instr::NamedTag { .. }
            )
        })
    {
        return None;
    }
    let roots = egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
        bodies: vec![Arc::clone(&body)],
    });
    let context = EmissionContext::capture(&body, true);
    let profiles = (0..body.code.len())
        .filter_map(|bcp| {
            type_profile_at(Arc::as_ptr(&body) as usize, bcp as u32)
                .map(|profile| (bcp as u32, profile))
        })
        .collect();
    let call_cells = named_call_cells(&body);
    let snapshot = egcl_compiler::t2::constants::BytecodeSnapshot::capture(&body);
    Some(PreparedT1 {
        snapshot,
        roots,
        context,
        symbol,
        profiles,
        call_cells,
        allow_speculation,
    })
}

pub(in crate::cli::bytecode) fn compile_background_t1(input: PreparedT1) -> Option<CompiledT1> {
    let build = |optimize| -> Result<_, EmitDecline> {
        let body = &input.snapshot.body;
        let result_recovery = scope_free_native_body(body, true)
            && body
                .code
                .iter()
                .any(|op| matches!(op, Instr::CheckResult(_)));
        let has_metadata = scope_free_native_body(body, false) || result_recovery;
        let mut cold_guesses = Vec::new();
        let (ir, ordinary_ir, speculations) = if has_metadata {
            let mut ir = egcl_compiler::t2::build::build_for_transfer_optimization_snapshot(
                &input.snapshot,
                input.symbol,
            )
            .map_err(EmitDecline::Build)?;
            if optimize && scope_free_native_body(body, false) {
                let cold = RefCell::new(Vec::new());
                egcl_compiler::t2::speculate::speculate_with_feedback(
                    &mut ir,
                    &|bcp| {
                        use egcl_compiler::t2::speculate::SpecType as CompilerType;
                        match input.profiles.get(&bcp) {
                            None => {
                                if !cold.borrow().contains(&bcp) {
                                    cold.borrow_mut().push(bcp);
                                }
                                Some(CompilerType::Fixnum)
                            }
                            Some(profile) => match profile.speculation() {
                                Some(SpecType::Fixnum) => Some(CompilerType::Fixnum),
                                Some(SpecType::SingleFloat) => Some(CompilerType::SingleFloat),
                                _ => None,
                            },
                        }
                    },
                    egcl_compiler::t2::speculate::SpeculateOptions::default(),
                    &|bcp, kind| {
                        use egcl_compiler::t2::speculate::SpecType as CompilerType;
                        let kind = match kind {
                            CompilerType::Fixnum => SpecType::Fixnum,
                            CompilerType::SingleFloat => SpecType::SingleFloat,
                            CompilerType::DoubleFloat => SpecType::DoubleFloat,
                        };
                        // Use the immutable profiles captured for this exact body,
                        // never live worker-thread feedback or declarations alone.
                        input
                            .profiles
                            .get(&bcp)
                            .is_none_or(|profile| profile.permits(kind))
                    },
                );
                cold_guesses = cold.into_inner();
            }
            let speculations = t2_numeric_speculations(&ir, input.symbol);
            let mut passes = egcl_compiler::t2::pass::PassManager::new();
            passes.add(Box::new(egcl_compiler::t2::opt_fold::ConstFold));
            passes.add(Box::new(egcl_compiler::t2::opt_gvn::Gvn));
            passes.add(Box::new(egcl_compiler::t2::opt_guard::GuardElim));
            passes.add(Box::new(egcl_compiler::t2::opt_clear_mv::ClearMvElim));
            passes.add(Box::new(egcl_compiler::t2::opt_dce::Dce));
            passes.run(&mut ir);
            let ordinary_ir = ir.clone();
            let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(body)
                .map_err(EmitDecline::Scopes)?;
            egcl_compiler::t2::build::legalize_transfer_calls(&mut ir, &scopes)
                .map_err(EmitDecline::Legalize)?;
            (ir, Some(ordinary_ir), speculations)
        } else {
            (
                egcl_compiler::t2::build::build_from_bytecode_for_native_cleanups_snapshot(
                    &input.snapshot,
                    input.symbol,
                )
                .map_err(EmitDecline::Build)?,
                None,
                Vec::new(),
            )
        };
        let emitted = EmittedTransfer::try_emit_prepared_with_ordinary(
            body,
            &ir,
            ordinary_ir.as_ref(),
            has_metadata,
            &input.call_cells,
            &input.context,
            CompilationTier::T1,
        )?;
        Ok((emitted, speculations, cold_guesses, has_metadata))
    };
    let (emitted, speculations, cold_guesses, has_metadata) = build(input.allow_speculation)
        .or_else(|_| build(false))
        .inspect_err(|decline| {
            // The owner reports only that the compilation was declined; name
            // the step that refused it here, where the reason is known.
            egcl_rt::blog!(
                "compile",
                egcl_rt::log::TRACE,
                "[T1] {}: background compilation refused: {decline}",
                sym_label(input.symbol)
            )
        })
        .ok()?;
    Some(CompiledT1 {
        emitted,
        roots: input.roots,
        speculations,
        cold_guesses,
        has_metadata,
    })
}

pub(in crate::cli::bytecode) fn install_background_t1(
    symbol: u32,
    body: Arc<BytecodeFunction>,
    artifact: CompiledT1,
) -> Option<Rc<NativeCode>> {
    let roots = retain_native_body(&body);
    let metadata = artifact.has_metadata.then(|| {
        Arc::new(T2InstalledMetadata {
            speculations: artifact.speculations,
            cold_guesses: artifact.cold_guesses,
            _roots: artifact.roots,
            deopt_bodies: [(symbol, Arc::clone(&body))].into_iter().collect(),
        })
    });
    let code = artifact.emitted.install(body, roots, metadata).ok()?;
    install_baseline_code(symbol, code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detached_t1_emits_without_a_lisp_execution() {
        fn assert_send<T: Send>() {}
        assert_send::<PreparedT1>();
        assert_send::<CompiledT1>();
        let _lock = crate::cli::heap_test_lock()
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        egcl_rt::rooted!(params = reader::read_from_string("(x)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string("(x)").unwrap().0);
        let body = Arc::new(
            compile_function("DETACHED-T1-IDENTITY", *params, *forms, &env, false, false).unwrap(),
        );
        let symbol = egcl_rt::symbols::intern("DETACHED-T1-IDENTITY");
        let prepared = prepare_background_t1(symbol, Arc::clone(&body), false).unwrap();
        let artifact = std::thread::spawn(move || {
            assert!(egcl_rt::thread::current_execution_context_for_native().is_none());
            compile_background_t1(prepared).expect("detached worker must emit T1")
        })
        .join()
        .unwrap();
        let native = install_background_t1(symbol, body, artifact).unwrap();
        let NativeCodeStorage::Mapped(code) = &native._storage else {
            panic!("expected mapped T1")
        };
        assert_eq!(
            code.run(&[EgclVal::from_fixnum(42)], &mut env).unwrap(),
            EgclVal::from_fixnum(42)
        );
    }
}
