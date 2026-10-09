// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Bytecode → block-based SSA builder.
//!
//! # Purpose
//!
//! [`build_from_bytecode`] abstractly interprets a `BytecodeFunction` and
//! produces an [`ir::Function`](crate::t2::ir::Function) directly in SSA form
//! using Braun et al.'s on-the-fly construction: no dominance frontiers and no
//! φ placement pass. φs are block parameters created on demand by `read_var`
//! and completed when the block is sealed, which happens once every
//! predecessor edge has been recorded. The builder also attaches the deopt
//! metadata nothing downstream can reconstruct, namely a `FrameState` at every
//! deoptimising site and an OSR entry state at every empty-stack loop header,
//! expands intrinsics, inlines saved callee bodies, and in transfer mode builds
//! the exceptional control-flow edges native non-local transfer needs.
//!
//! # Contract
//!
//! **Input:** a `BytecodeFunction`: stack-machine code, constants, local-slot
//! and parameter layout, and the control-scope tables (`control_scope.rs`
//! derives the ordered scope map from it).
//!
//! **Output:** a `Function` that verify.rs accepts, or a [`BuildError`]:
//! `Unsupported(&str)` for a shape the builder does not model,
//! `UnsupportedInstr(name)` naming the exact `Instr` discriminant so a silent
//! tier loss is diagnosable without a disassembly, and `InvalidScopes` from
//! control-scope analysis. The caller keeps such a function at T1. Coverage is
//! grown by adding cases; nothing is approximated.
//!
//! **Entry points.** [`build_from_bytecode`] uses the default inlining
//! options; [`build_from_bytecode_with_inline_options`] takes per-site
//! INLINE/NOTINLINE policy, saved callee bodies, and hotness;
//! [`build_from_bytecode_for_transfers`] additionally converts every remaining
//! `Call` into an `Invoke` whose exceptional edge lands on an `NlxTransfer`
//! cold block carrying the pre-call control scopes;
//! [`build_from_bytecode_for_native_cleanups`] also admits cleanup, catch, and
//! handler predecessors before sealing. The last two are not installable yet;
//! the emitters decline their IR until native landing support is complete.
//!
//! # Algorithm (`Builder::run`)
//!
//! 1. An empty body becomes a function returning NIL.
//! 2. `find_leaders`: bcp 0, every branch and `Go` target, the fall-through
//!    after every conditional branch, and in transfer modes the scope
//!    transition points, cleanup and catch resume points, and handler clause
//!    bodies.
//! 3. `compute_depths`: the operand-stack depth on entry to each leader; it
//!    must agree on every incoming edge.
//! 4. `create_blocks`, `compute_reachable`, `compute_total_preds`: one `Block`
//!    per leader, reachability from the entry, and the structural
//!    predecessor-edge count of each block, counting edges out of reachable
//!    blocks only. That count is what drives sealing.
//! 5. `seed_entry`: entry-block parameters for the positional parameters (for
//!    a variadic function, the pre-collected &optional/&rest/&key slots the
//!    shared binding path fills before the body runs, bliss-32l), with
//!    declared parameter types recorded as checked entry params. The
//!    function-entry `FrameState` (bcp 0, empty stack) is built by hand:
//!    non-parameter locals are `Unbound` because at bcp 0 they are
//!    uninitialised, and `read_var` would otherwise conjure definitions that
//!    appear later. If bcp 0 is itself a branch target, the entry gets a
//!    one-shot block jumping to the bytecode header so the header's φs can
//!    merge initial values with loop-carried ones.
//! 6. `process_blocks` / `interpret_block`: blocks in leader order. An
//!    unreachable block is sealed and given a `Trap` terminator. Otherwise the
//!    incoming operand stack is materialised through `read_var(Var::Stack(k))`
//!    and each instruction is interpreted: constants, locals via
//!    `read_var`/`write_var`, arithmetic and comparisons to typed or generic
//!    opcodes, calls to `Call` with a `FrameState` from `build_frame_state`
//!    (every local and stack entry as a `Tagged` value source at that bcp),
//!    BLOCK/RETURN-FROM as a branch to the recorded resume point after
//!    resetting the stack (bliss-8tlo), and intrinsics via `expand_intrinsic`
//!    (CAR/CDR become a `Guard(CONS)` plus a load, sharing one proof identity
//!    so guard elimination can reuse either for the other; an unsupported
//!    shape keeps the ordinary call). `finish_block` and `record_edges` set
//!    the terminator and count the edge on each successor; a successor whose
//!    last edge has arrived is sealed, filling every incomplete φ's operand
//!    on every predecessor edge.
//! 7. `capture_osr_entries`: every backward branch target with an empty
//!    operand stack gets an `OsrEntry` whose `FrameState` covers the locals.
//!    The empty-stack condition belongs to the target, not the jumping
//!    instruction, so plain `Br`/`BrIfFalse` back-edges (DO, DOTIMES, DOLIST)
//!    qualify, not only tagbody `Go` (bliss-izt.4).
//! 8. `simplify_trivial_phis`: Braun's trivial-φ removal iterated to a
//!    fixpoint, rewriting uses and frame states.
//!
//! `build_with_saved_bodies` then walks every `Call` with a saved body and
//! decides inlining per site using `inlining.rs`: policy, arity match, depth
//! limit, recursion (the active-symbol set), body cost against the remaining
//! node budget, and the small-threshold/hotness rule. An accepted callee is
//! built recursively and spliced in with `Function::inline_call`, with the
//! caller's scopes advanced past the call (bcp + 1, arguments popped) so a
//! deopt inside the callee reconstructs both activations.
//!
//! # Design notes
//!
//! * Both local slots and operand-stack positions are Braun variables
//!   (`Var::Local`, `Var::Stack`). Treating the stack this way is what lets a
//!   block's live incoming stack entries become its leading parameters with
//!   no separate stack-model pass.
//! * Every `FrameState` records `Tagged` sources. Representation changes are
//!   the optimiser's job; the builder records the interpreter's view.
//! * Loops rely on sealing: a header is read while unsealed, so its reads
//!   create incomplete φs that the back-edge later completes.

use crate::control_scope::{is_never_returning_call, ScopeError, ScopeKind, ScopeMap};
use std::collections::{BTreeSet, HashMap, HashSet};

use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::inlining::{
    body_cost, decide, metadata_for_symbol, InlineDecision, InlineOptions, InlinePolicy,
    IntrinsicId,
};
use crate::t2::ir::{
    AuxData, Block, Function, IRType, Inst, InstData, InstFlags, Opcode, TypeBits, Value,
    ValueRepresentation,
};
use egcl_rt::bytecode::{typep_class, BytecodeFunction, DeclaredType, Instr, VarLoc};

/// Why the builder could not produce IR for a function (e.g. an opcode not yet
/// modelled). The caller keeps such a function at T1.
#[derive(Clone, Debug)]
pub enum BuildError {
    InvalidScopes(ScopeError),
    Unsupported(&'static str),
    /// An instruction the builder does not model, named by its `Instr`
    /// discriminant. The bare `Unsupported("opcode not modelled by T2 builder")`
    /// gave no way to tell WHICH instruction stopped a function from reaching
    /// T2, which made a silent tier loss (e.g. CHAR= stuck at T1) take a
    /// disassembly to diagnose.
    UnsupportedInstr(String),
}

/// The `Instr` variant name, for diagnostics. `Debug` renders the whole
/// instruction including operands; the discriminant alone is what identifies
/// the missing builder case.
fn instr_kind(instr: &Instr) -> String {
    let full = format!("{instr:?}");
    match full.find(['(', ' ', '{']) {
        Some(i) => full[..i].to_string(),
        None => full,
    }
}

/// A Braun variable: either a lexical local slot or an operand-stack position.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
enum Var {
    Local(u16),
    Stack(u16),
}

/// Build a block-based SSA `Function` from `bf`.
pub fn build_from_bytecode(bf: &BytecodeFunction) -> Result<Function, BuildError> {
    build_from_bytecode_with_inline_options(bf, InlineOptions::default())
}

/// Construct the native-transfer call contract with an explicit cold fallback
/// for every remaining call. This entry point stays separate from installation
/// until complete helper, scope, polling and native landing coverage is ready.
/// Body inlining must preserve logical control scopes before it can use this
/// path; intrinsic expansion still happens in the ordinary SSA builder.
/// Protected regions retain exceptional cleanup metadata and explicit normal
/// cleanup continuations. Emission requires matching runtime support; direct
/// exits crossing cleanup remain refused until native unwind lowering exists.
pub fn build_from_bytecode_for_transfers(bf: &BytecodeFunction) -> Result<Function, BuildError> {
    build_transfer_cfg(bf, false)
}

/// Include cleanup predecessors before SSA sealing. Installation requires native
/// landing/cursor support; the existing emitter deliberately declines this IR.
pub fn build_from_bytecode_for_native_cleanups(
    bf: &BytecodeFunction,
) -> Result<Function, BuildError> {
    build_transfer_cfg(bf, true)
}

fn build_transfer_cfg(
    bf: &BytecodeFunction,
    native_cleanups: bool,
) -> Result<Function, BuildError> {
    let scopes = ScopeMap::analyze_function(bf).map_err(BuildError::InvalidScopes)?;
    let mut builder = Builder::new(bf, InlineOptions::default());
    builder.transfer_mode = true;
    builder.native_cleanups = native_cleanups;
    let mut f = builder.run()?;
    let calls: Vec<Inst> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| f.inst(i).opcode == Opcode::Call)
        .collect();
    for call in calls {
        let data = f.inst(call).clone();
        let state = data
            .frame_state
            .ok_or(BuildError::Unsupported("unmapped transfer call"))?;
        let frame = f
            .frame_states
            .get(state)
            .scopes
            .last()
            .ok_or(BuildError::Unsupported(
                "transfer call without logical frame",
            ))?;
        let origin_bcp = frame.bcp;
        let control_scopes = scopes
            .before(origin_bcp)
            .ok_or(BuildError::Unsupported("transfer call without scope state"))?
            .to_vec();
        let cold = f.make_block();
        f.set_terminator(
            cold,
            InstData {
                opcode: Opcode::NlxTransfer,
                args: vec![],
                results: vec![],
                aux: AuxData::TransferSite {
                    origin_bcp,
                    scopes: control_scopes,
                },
                flags: InstFlags {
                    effectful: true,
                    safepoint: true,
                    call: true,
                    terminator: true,
                    ..InstFlags::default()
                },
                targets: vec![],
                frame_state: Some(state),
                source_pos: data.source_pos,
            },
        );
        f.make_call_exceptional(
            call,
            crate::t2::ir::BlockCall {
                block: cold,
                args: vec![],
            },
        )
        .map_err(BuildError::Unsupported)?;
    }
    Ok(f)
}

/// Build T2 IR with explicit per-call-site inlining policy. The ordinary
/// tiering path uses [`InlineOptions::default`]; this entry point is also the
/// seam where lexical INLINE/NOTINLINE declarations are supplied.
pub fn build_from_bytecode_with_inline_options(
    bf: &BytecodeFunction,
    inline_options: InlineOptions,
) -> Result<Function, BuildError> {
    let mut remaining_budget = inline_options.config.node_budget;
    let mut active = HashSet::new();
    if let Some(root) = inline_options.root_symbol() {
        active.insert(root);
    }
    build_with_saved_bodies(
        bf,
        &inline_options,
        inline_options.root_symbol(),
        0,
        &mut remaining_budget,
        &mut active,
        true,
    )
}

fn build_with_saved_bodies(
    bf: &BytecodeFunction,
    options: &InlineOptions,
    current_symbol: Option<u32>,
    depth: u8,
    remaining_budget: &mut u32,
    active: &mut HashSet<u32>,
    root_policies: bool,
) -> Result<Function, BuildError> {
    let builder_options = current_symbol.map_or_else(
        || options.clone(),
        |symbol| options.clone().with_root_symbol(symbol),
    );
    let mut f = Builder::new(bf, builder_options).run()?;
    let calls: Vec<Inst> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| f.inst(i).opcode == Opcode::Call)
        .collect();

    for call in calls {
        let data = f.inst(call).clone();
        let AuxData::CallTarget(symbol) = data.aux else {
            continue;
        };
        let nargs = data.args.len() as u16;
        let Some(body) = options.body(symbol) else {
            continue;
        };
        let Some(fsid) = data.frame_state else {
            continue;
        };
        let call_state = f.frame_states.get(fsid).clone();
        let Some(current_scope) = call_state.scopes.last() else {
            continue;
        };
        let policy = if root_policies {
            options.policy_at(current_scope.bcp)
        } else {
            InlinePolicy::Unspecified
        };
        let hot = current_symbol
            .is_some_and(|caller| options.call_site_is_hot(caller, current_scope.bcp));

        if policy == InlinePolicy::NotInline
            || nargs != body.arity
            || depth >= options.config.max_depth
            || active.contains(&symbol)
        {
            continue;
        }
        let mut eligibility_stack = active.clone();
        let Ok(cost) = body_cost(symbol, options, &mut eligibility_stack) else {
            continue;
        };
        if cost > *remaining_budget
            || (cost > options.config.small_threshold && !hot && policy != InlinePolicy::Inline)
        {
            continue;
        }

        active.insert(symbol);
        let mut callee_budget = *remaining_budget;
        let callee = build_with_saved_bodies(
            &body,
            options,
            Some(symbol),
            depth + 1,
            &mut callee_budget,
            active,
            false,
        )?;
        active.remove(&symbol);

        // The outer activation is suspended immediately after the invoke: its
        // arguments have been consumed and the callee's eventual Return will
        // push the result. Inner scope(s) resume at their own guard bcp.
        let mut caller_scopes = call_state.scopes;
        let caller = caller_scopes.last_mut().expect("non-empty FrameState");
        caller.bcp = caller.bcp.saturating_add(1);
        caller
            .stack
            .truncate(caller.stack.len().saturating_sub(nargs as usize));
        f.inline_call(call, &callee, &caller_scopes)
            .map_err(BuildError::Unsupported)?;
        *remaining_budget = remaining_budget.saturating_sub(cost);
    }
    Ok(f)
}

struct Builder<'a> {
    bf: &'a BytecodeFunction,
    f: Function,
    /// Sorted bytecode indices that begin a basic block.
    leaders: Vec<usize>,
    /// Leader bytecode index → its `Block`.
    block_of: HashMap<usize, Block>,
    /// Operand-stack depth on entry to each leader's block.
    entry_depth: HashMap<usize, usize>,
    /// `block_id` -> (`resume_bcp`, `sp_restore`) for each lexical BLOCK, from
    /// its `PushBlock`. A `ReturnFrom` is a branch to that resume point after
    /// resetting the operand stack to `sp_restore` and pushing the value
    /// (bliss-8tlo).
    block_exits: HashMap<u32, (u32, u16)>,
    /// Local slots that may still be read before being written, on entry to
    /// each bytecode index: one bitset (`u64` words) per instruction. Computed
    /// by `compute_local_liveness`; `build_frame_state` names only live slots
    /// and marks the rest `Unbound` (bliss-dfilp, bliss-enisp).
    local_live_in: Vec<Vec<u64>>,
    control_scopes: Option<ScopeMap>,
    /// Predecessor edges of a block: `(pred_block, target_index_in_pred_terminator)`.
    pred_edges: HashMap<Block, Vec<(Block, usize)>>,
    /// Total structural predecessor-edge count of each block (drives sealing).
    total_preds: Vec<usize>,
    /// Which blocks are reachable from the entry, by leader position.
    /// `compute_total_preds` counts edges only out of these; see
    /// `compute_reachable`.
    reachable: Vec<bool>,
    /// Predecessor edges already materialised (their terminator is set).
    seen_preds: Vec<usize>,
    /// Whether a block has been interpreted.
    interpreted: Vec<bool>,
    /// The terminator instruction of each finished block.
    terminator_inst: HashMap<Block, Inst>,
    /// Braun `var_def`: current SSA definition of `(var, block)`.
    current_def: HashMap<(Var, Block), Value>,
    /// Whether all predecessors of a block are known (Braun sealing).
    sealed: Vec<bool>,
    /// Incomplete phis (header params) awaiting operand fill at seal time.
    incomplete_phis: HashMap<Block, Vec<(Var, Value)>>,
    inline_options: InlineOptions,
    remaining_inline_budget: u32,
    /// Body cloning will increment this while descending an inline call tree.
    /// Intrinsic expansions are leaves, so the first implementation stays at 0.
    inline_depth: u8,
    root_symbol: u32,
    /// Admit protected scopes and explicit normal cleanup handoffs.
    transfer_mode: bool,
    /// Build exceptional cleanup predecessors before SSA sealing. Kept behind
    /// its own entry while native landing maps and cursor dispatch are wired.
    native_cleanups: bool,
}

#[derive(Clone, Copy)]
struct HandlerDestination {
    push_bcp: u32,
    table_index: u32,
    clause_index: u32,
    body_bcp: u32,
    var_slot: Option<u16>,
    sp_restore: u16,
}

impl<'a> Builder<'a> {
    fn handler_transition(&self, bcp: usize) -> Option<(u32, bool)> {
        if !self.native_cleanups {
            return None;
        }
        match self.bf.code.get(bcp)? {
            Instr::PushHandlerCase { .. } => Some((bcp as u32, true)),
            Instr::PopHandlerCase => {
                let scope = self.control_scopes.as_ref()?.before(bcp as u32)?.last()?;
                matches!(scope.kind, ScopeKind::HandlerCase { .. })
                    .then_some((scope.push_bcp, false))
            }
            _ => None,
        }
    }

    fn handler_bind_transition(&self, bcp: usize) -> Option<(u32, bool)> {
        if !self.native_cleanups {
            return None;
        }
        match self.bf.code.get(bcp)? {
            Instr::PushHandlerBind { .. } => Some((bcp as u32, true)),
            Instr::PopHandlerBind => {
                let scope = self.control_scopes.as_ref()?.before(bcp as u32)?.last()?;
                matches!(scope.kind, ScopeKind::HandlerBind { .. })
                    .then_some((scope.push_bcp, false))
            }
            _ => None,
        }
    }

    fn restart_case_transition(&self, bcp: usize) -> Option<(u32, bool)> {
        if !self.native_cleanups {
            return None;
        }
        match self.bf.code.get(bcp)? {
            Instr::PushRestartCase { .. } => Some((bcp as u32, true)),
            Instr::PopRestartCase => {
                let scope = self.control_scopes.as_ref()?.before(bcp as u32)?.last()?;
                matches!(scope.kind, ScopeKind::RestartCase { .. })
                    .then_some((scope.push_bcp, false))
            }
            _ => None,
        }
    }
    fn exceptional_handlers(&self, bcp: u32) -> Vec<HandlerDestination> {
        self.control_scopes
            .as_ref()
            .and_then(|s| s.before(bcp))
            .into_iter()
            .flatten()
            .rev()
            .take_while(|s| !matches!(s.kind, ScopeKind::Unwind { .. }))
            .flat_map(|scope| {
                let clauses = match scope.kind {
                    ScopeKind::HandlerCase { table_index } => self
                        .bf
                        .handler_cases
                        .get(table_index as usize)
                        .map(|info| (table_index, &info.clauses)),
                    _ => None,
                };
                clauses.into_iter().flat_map(move |(table_index, clauses)| {
                    clauses
                        .iter()
                        .enumerate()
                        .map(move |(index, clause)| HandlerDestination {
                            push_bcp: scope.push_bcp,
                            table_index,
                            clause_index: index as u32,
                            body_bcp: clause.body_bcp,
                            var_slot: clause.var_slot,
                            sp_restore: scope.sp_restore,
                        })
                })
            })
            .collect()
    }

    fn catch_transition(&self, bcp: usize) -> Option<(u32, bool)> {
        if !self.native_cleanups {
            return None;
        }
        match self.bf.code.get(bcp)? {
            Instr::PushCatch { .. } => Some((bcp as u32, true)),
            Instr::PopHandler => {
                let scope = self.control_scopes.as_ref()?.before(bcp as u32)?.last()?;
                matches!(scope.kind, ScopeKind::Catch { .. }).then_some((scope.push_bcp, false))
            }
            _ => None,
        }
    }
    fn normal_cleanup_return(&self, bcp: u32) -> Result<(u32, u32), BuildError> {
        let scopes = self
            .control_scopes
            .as_ref()
            .expect("scope analysis before SSA");
        let cleanup = scopes
            .before(bcp)
            .and_then(|active| {
                active.iter().rev().find_map(|scope| {
                    if let ScopeKind::Cleanup { cleanup_bcp } = scope.kind {
                        Some(cleanup_bcp)
                    } else {
                        None
                    }
                })
            })
            .ok_or(BuildError::Unsupported(
                "cleanup return without active continuation",
            ))?;
        match scopes.cleanup_resumes(cleanup) {
            [resume] => Ok((cleanup, *resume)),
            [] if self.native_cleanups => Ok((cleanup, u32::MAX)),
            _ => Err(BuildError::Unsupported(
                "cleanup requires dynamic continuation selection",
            )),
        }
    }

    fn check_direct_exit(&self, bcp: u32) -> Result<(), BuildError> {
        let exit = self
            .control_scopes
            .as_ref()
            .and_then(|scopes| scopes.exit_at(bcp))
            .ok_or(BuildError::Unsupported("direct exit without active scope"))?;
        if exit.removed.iter().any(|scope| {
            !matches!(
                scope.kind,
                ScopeKind::Block { .. } | ScopeKind::Tagbody { .. }
            )
        }) {
            return Err(BuildError::Unsupported(
                "direct exit requires dynamic unwinding",
            ));
        }
        Ok(())
    }

    fn new(bf: &'a BytecodeFunction, inline_options: InlineOptions) -> Builder<'a> {
        let remaining_inline_budget = inline_options.config.node_budget;
        let root_symbol = inline_options
            .root_symbol()
            .unwrap_or_else(|| egcl_rt::symbols::intern(&bf.name));
        Builder {
            bf,
            f: Function::new(bf.name.clone()),
            leaders: Vec::new(),
            block_of: HashMap::new(),
            entry_depth: HashMap::new(),
            block_exits: HashMap::new(),
            local_live_in: Vec::new(),
            control_scopes: None,
            pred_edges: HashMap::new(),
            total_preds: Vec::new(),
            reachable: Vec::new(),
            seen_preds: Vec::new(),
            interpreted: Vec::new(),
            terminator_inst: HashMap::new(),
            current_def: HashMap::new(),
            sealed: Vec::new(),
            incomplete_phis: HashMap::new(),
            inline_options,
            remaining_inline_budget,
            inline_depth: 0,
            root_symbol,
            transfer_mode: false,
            native_cleanups: false,
        }
    }

    // ── Driver ──────────────────────────────────────────────────────

    fn run(mut self) -> Result<Function, BuildError> {
        let code = &self.bf.code;

        // Empty body → a function that returns NIL.
        if code.is_empty() {
            let entry = self.f.entry();
            let nil = self.emit_const_nil(entry);
            self.set_term(entry, ret(nil));
            return Ok(self.f);
        }

        self.control_scopes =
            Some(ScopeMap::analyze_function(self.bf).map_err(BuildError::InvalidScopes)?);
        if self.native_cleanups {
            self.f.handler_cases = self.bf.handler_cases.clone();
        }
        self.find_leaders()?;
        self.compute_depths()?;
        self.compute_local_liveness()?;
        self.create_blocks();
        self.compute_reachable()?;
        self.compute_total_preds()?;
        self.seed_entry();
        // Record the function-entry interpreter state (bcp 0, empty stack):
        // speculation anchors parameter pre-guards on it — a failed pre-guard
        // deopts to bcp 0 and re-runs the whole function in T0, which is
        // always correct because nothing has executed yet (bliss-x5y.25).
        // Built by hand rather than via build_frame_state: at bcp 0 the
        // non-parameter locals are UNINITIALISED, and read_var would conjure
        // their (later-emitted) init values — a state at instruction 0 must
        // not reference values defined after it (verify V8).
        let entry = self.f.entry();
        let params = self.f.block(entry).params.clone();
        let locals = (0..self.bf.n_locals as usize)
            .map(|i| match params.get(i) {
                Some(&p) => ValueSource::Value {
                    value: p,
                    repr: ValueRepresentation::Tagged,
                },
                None => ValueSource::Unbound,
            })
            .collect();
        let entry_fs = self.f.frame_states.add(FrameState {
            scopes: vec![FrameScope {
                function: self.root_symbol,
                bcp: 0,
                locals,
                stack: Vec::new(),
            }],
            remat: vec![],
        });
        self.f.entry_frame_state = Some(entry_fs);
        let first_body = self.block_of[&0];
        if first_body != entry {
            // Function parameters belong to a one-shot entry. The bytecode
            // header needs ordinary phis joining those initial values with
            // loop-carried values, including initially NIL non-parameters.
            self.total_preds[first_body.index()] += 1;
            self.sealed[entry.index()] = true;
            let term = self.f.set_terminator(entry, jump(first_body));
            self.record_edges(entry, term, vec![first_body]);
        }
        self.process_blocks()?;
        self.capture_osr_entries();
        self.simplify_trivial_phis();
        Ok(self.f)
    }

    /// Capture the live root-frame locals at every empty-stack BACKWARD BRANCH
    /// target. These FrameStates participate in the ordinary optimizer rewrite
    /// path, which keeps OSR entry state correct as phis and values are
    /// simplified.
    ///
    /// The safety condition is the operand stack being empty at the target
    /// (`entry_depth == 0`), which is what makes the state map trivial: locals
    /// already live in shared EgclStack frame slots, so only the position has
    /// to transfer. That condition is a property of the target, not of the
    /// instruction that jumps to it — so every backward branch qualifies, not
    /// just a tagbody `Go`.
    ///
    /// This used to look at `Go` alone, which meant DO/DOTIMES/DOLIST loops —
    /// which lower to ordinary `Br`/`BrIfFalse` back-edges, not to tagbody
    /// `Go` — got no OSR safepoint at all, and so could never be entered at T2
    /// once already running. Those are precisely the loops OSR exists for
    /// (bliss-izt.4).
    fn capture_osr_entries(&mut self) {
        let mut headers = BTreeSet::new();
        for (i, instr) in self.bf.code.iter().enumerate() {
            let target_bcp = match instr {
                Instr::Go { target_bcp, .. } => *target_bcp,
                // A conditional branch has already popped its test value by the
                // time control reaches the target, so the target's own
                // entry_depth is still the authority on stack emptiness.
                Instr::Br(t) | Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => *t,
                _ => continue,
            };
            let target = target_bcp as usize;
            if target < i && self.entry_depth.get(&target) == Some(&0) {
                headers.insert(target_bcp);
            }
        }
        for bcp in headers {
            let Some(&block) = self.block_of.get(&(bcp as usize)) else {
                continue;
            };
            let frame_state = self.build_frame_state(block, &[], bcp);
            self.f.osr_entries.push(crate::t2::ir::OsrEntry {
                bcp,
                block,
                frame_state,
                checks: Vec::new(),
            });
        }
    }

    // ── Pass 1: basic-block leaders ─────────────────────────────────

    fn find_leaders(&mut self) -> Result<(), BuildError> {
        let code = &self.bf.code;
        let mut set: BTreeSet<usize> = BTreeSet::new();
        set.insert(0);
        for (i, instr) in code.iter().enumerate() {
            if (self.catch_transition(i).is_some()
                || self.handler_transition(i).is_some()
                || self.handler_bind_transition(i).is_some()
                || self.restart_case_transition(i).is_some())
                && i + 1 < code.len()
            {
                set.insert(i + 1);
            }
            match instr {
                Instr::PushHandlerCase { hc, .. } if self.native_cleanups => {
                    for clause in &self.bf.handler_cases[*hc as usize].clauses {
                        set.insert(clause.body_bcp as usize);
                    }
                }
                Instr::PushCatch { resume_bcp, .. } if self.native_cleanups => {
                    set.insert(*resume_bcp as usize);
                }
                Instr::PushUnwind { cleanup_bcp, .. } if self.native_cleanups => {
                    set.insert(*cleanup_bcp as usize);
                }
                Instr::CallNamed { .. }
                | Instr::SetValues(_)
                | Instr::Throw
                | Instr::EvalHost(_)
                | Instr::LoadFunction(_)
                    if self.native_cleanups =>
                {
                    if i + 1 < code.len() {
                        set.insert(i + 1);
                    }
                }
                Instr::EnterCleanupNormal {
                    cleanup_bcp,
                    resume_bcp,
                } if self.transfer_mode => {
                    set.insert(*cleanup_bcp as usize);
                    set.insert(*resume_bcp as usize);
                }
                Instr::Br(t) => {
                    set.insert(*t as usize);
                }
                Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => {
                    set.insert(*t as usize);
                    set.insert(i + 1); // fall-through
                }
                Instr::Go { target_bcp, .. } => {
                    set.insert(*target_bcp as usize);
                }
                // A call that never returns normally ENDS its block, so what
                // follows begins a new one (bliss-wukf). That block is usually
                // unreachable, which is what `compute_reachable` exists to
                // account for.
                Instr::CallNamed { sym, .. } if is_never_returning_call(*sym) => {
                    if i + 1 < code.len() {
                        set.insert(i + 1);
                    }
                }
                Instr::PushBlock {
                    block_id,
                    resume_bcp,
                    sp_restore,
                    ..
                } => {
                    // The resume point is where normal completion and every
                    // `ReturnFrom` converge, so it begins a block.
                    self.block_exits
                        .insert(*block_id, (*resume_bcp, *sp_restore));
                    set.insert(*resume_bcp as usize);
                }
                _ => {}
            }
        }
        // Validate every leader lands inside the code.
        for &l in &set {
            if l >= code.len() {
                return Err(BuildError::Unsupported("branch target out of range"));
            }
        }
        self.leaders = set.into_iter().collect();
        Ok(())
    }

    // ── Pass 2: operand-stack depth at each reachable index ─────────

    fn compute_depths(&mut self) -> Result<(), BuildError> {
        let code = &self.bf.code;
        let mut depth_at: HashMap<usize, i32> = HashMap::new();
        let mut work: Vec<usize> = vec![0];
        depth_at.insert(0, 0);

        while let Some(i) = work.pop() {
            let d = depth_at[&i];
            // `push` propagates a depth to a successor index.
            let push =
                |idx: usize, nd: i32, depth_at: &mut HashMap<usize, i32>, work: &mut Vec<usize>| {
                    if idx >= code.len() {
                        return;
                    }
                    if let std::collections::hash_map::Entry::Vacant(e) = depth_at.entry(idx) {
                        e.insert(nd);
                        work.push(idx);
                    }
                };
            if self.native_cleanups
                && (self.catch_transition(i).is_some()
                    || self.handler_transition(i).is_some()
                    || self.handler_bind_transition(i).is_some()
                    || self.restart_case_transition(i).is_some()
                    || matches!(
                        code[i],
                        Instr::CallNamed { .. }
                            | Instr::EvalHost(_)
                            | Instr::LoadFunction(_)
                            | Instr::SetValues(_)
                            | Instr::CleanupReturn
                            | Instr::Throw
                    ))
            {
                if let Some((cleanup, depth)) = self.exceptional_cleanup(i as u32) {
                    push(cleanup as usize, i32::from(depth), &mut depth_at, &mut work);
                }
                for target in self.exceptional_handlers(i as u32) {
                    push(
                        target.body_bcp as usize,
                        i32::from(target.sp_restore),
                        &mut depth_at,
                        &mut work,
                    );
                }
                for (_, resume, depth) in self.exceptional_catches(i as u32) {
                    push(
                        resume as usize,
                        i32::from(depth) + 1,
                        &mut depth_at,
                        &mut work,
                    );
                }
            }
            match &code[i] {
                Instr::EvalHost(_) if self.native_cleanups => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::PushHandlerBind { hb } if self.native_cleanups => {
                    let count = self
                        .bf
                        .handler_binds
                        .get(*hb as usize)
                        .ok_or(BuildError::Unsupported("invalid handler-bind table"))?
                        .types
                        .len() as i32;
                    if d < count {
                        return Err(BuildError::Unsupported("handler-bind without handlers"));
                    }
                    push(i + 1, d - count, &mut depth_at, &mut work);
                }
                Instr::PushHandlerCase { .. }
                | Instr::PopHandlerCase
                | Instr::PopHandlerBind
                | Instr::PushRestartCase { .. }
                | Instr::PopRestartCase
                    if self.native_cleanups =>
                {
                    push(i + 1, d, &mut depth_at, &mut work);
                }
                Instr::PushCatch { .. } if self.native_cleanups => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::EnterCleanupNormal { cleanup_bcp, .. } if self.transfer_mode => {
                    if d < 1 {
                        return Err(BuildError::Unsupported("cleanup entry without primary"));
                    }
                    push(*cleanup_bcp as usize, d - 1, &mut depth_at, &mut work);
                }
                Instr::CleanupReturn if self.transfer_mode => {
                    let (_, resume) = self.normal_cleanup_return(i as u32)?;
                    push(resume as usize, d + 1, &mut depth_at, &mut work);
                }
                Instr::Const(_)
                | Instr::LoadLocal(_)
                | Instr::LoadGlobal(_)
                | Instr::LoadEnvVar(_)
                | Instr::LoadFunction(_)
                | Instr::Dup => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::TypeP(_) => {
                    push(i + 1, d, &mut depth_at, &mut work);
                }
                Instr::MemoryFence(_) => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::StoreLocal(_) | Instr::StoreGlobal(_) | Instr::StoreEnvVar(_) | Instr::Pop => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::Throw if self.native_cleanups => {}
                Instr::CallNamed { sym, .. }
                    if self.transfer_mode && is_never_returning_call(*sym) => {}
                Instr::CallNamed { nargs, .. } => {
                    push(i + 1, d - (*nargs as i32) + 1, &mut depth_at, &mut work);
                }
                Instr::SetValues(n) => {
                    push(i + 1, d - (*n as i32) + 1, &mut depth_at, &mut work);
                }
                Instr::TakeValuesToLocals { .. } => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::ClearMv => {
                    push(i + 1, d, &mut depth_at, &mut work); // no operand-stack effect
                }
                Instr::PushBlock { .. } | Instr::PushTag { .. } | Instr::PopHandler => {
                    push(i + 1, d, &mut depth_at, &mut work); // handler markers: no stack effect
                }
                Instr::PushUnwind { .. } if self.transfer_mode => {
                    // The exact handler is in every protected Invoke map. Its
                    // cold body is bytecode fallback, not a normal SSA successor.
                    push(i + 1, d, &mut depth_at, &mut work);
                }
                Instr::Br(t) => {
                    push(*t as usize, d, &mut depth_at, &mut work);
                }
                Instr::Go { target_bcp, .. } => {
                    push(*target_bcp as usize, d, &mut depth_at, &mut work);
                }
                Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => {
                    push(*t as usize, d - 1, &mut depth_at, &mut work);
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::ReturnFrom { block_id } => {
                    // Transfers to the block's resume point with the operand
                    // stack reset to `sp_restore` plus the returned value —
                    // the same depth normal completion arrives with.
                    let (resume_bcp, sp_restore) = *self
                        .block_exits
                        .get(block_id)
                        .ok_or(BuildError::Unsupported("ReturnFrom to an unknown block"))?;
                    push(
                        resume_bcp as usize,
                        sp_restore as i32 + 1,
                        &mut depth_at,
                        &mut work,
                    );
                }
                Instr::Return => {}
                _ => return Err(BuildError::UnsupportedInstr(instr_kind(&code[i]))),
            }
        }

        for (&idx, &d) in &depth_at {
            self.entry_depth.insert(idx, d.max(0) as usize);
        }
        Ok(())
    }

    // ── Pass 2b: local-slot liveness at each bytecode index ─────────

    /// Whether local `slot` may be read before it is next written, starting at
    /// bytecode index `bcp`. Indices the analysis did not cover (none, once
    /// `compute_local_liveness` has run) are reported live.
    fn local_is_live_at(&self, slot: u16, bcp: usize) -> bool {
        match self.local_live_in.get(bcp) {
            Some(words) => {
                let slot = slot as usize;
                words
                    .get(slot / 64)
                    .is_some_and(|w| w & (1u64 << (slot % 64)) != 0)
            }
            None => true,
        }
    }

    /// Backward "may be read before written" dataflow over the bytecode,
    /// one bit per local slot, to a fixpoint.
    ///
    /// Uses are `LoadLocal`; definitions are `StoreLocal` and the slots a
    /// `TakeValuesToLocals` fills. Control successors follow the same
    /// relation as `compute_depths`. Where the interpreter could resume at a
    /// position this analysis cannot see through — a handler, cleanup, or
    /// catch landing (`native_cleanups`), a cleanup body entered as bytecode
    /// fallback (`PushUnwind` in `transfer_mode`), or host evaluation — every
    /// slot is treated as live at that instruction, so the result is only
    /// ever conservative.
    ///
    /// Only this is sound for frame states: a deopt at `bcp` resumes the
    /// interpreter at `bcp` and the interpreter reads locals only through
    /// `LoadLocal`, so a slot dead here is a slot whose reconstructed value
    /// can never be observed.
    fn compute_local_liveness(&mut self) -> Result<(), BuildError> {
        let code = &self.bf.code;
        let n = code.len();
        let words = usize::from(self.bf.n_locals).div_ceil(64);
        let mut live_in: Vec<Vec<u64>> = vec![vec![0u64; words]; n];
        let all_ones = |w: usize| -> u64 {
            let used = usize::from(self.bf.n_locals) - w * 64;
            if used >= 64 {
                u64::MAX
            } else {
                (1u64 << used) - 1
            }
        };

        // Per-instruction transfer: (successors, everything-live, uses, defs).
        let mut succs: Vec<Vec<usize>> = vec![Vec::new(); n];
        let mut opaque: Vec<bool> = vec![false; n];
        let mut uses: Vec<Option<u16>> = vec![None; n];
        let mut defs: Vec<(u16, u16)> = vec![(0, 0); n]; // (base, count)
        for i in 0..n {
            let push = |t: usize, succs: &mut Vec<Vec<usize>>| {
                if t < n {
                    succs[i].push(t);
                }
            };
            if self.native_cleanups
                && (self.catch_transition(i).is_some()
                    || self.handler_transition(i).is_some()
                    || self.handler_bind_transition(i).is_some()
                    || self.restart_case_transition(i).is_some()
                    || matches!(
                        code[i],
                        Instr::CallNamed { .. }
                            | Instr::EvalHost(_)
                            | Instr::LoadFunction(_)
                            | Instr::SetValues(_)
                            | Instr::CleanupReturn
                            | Instr::Throw
                    ))
            {
                opaque[i] = true;
            }
            match &code[i] {
                Instr::LoadLocal(k) => {
                    uses[i] = Some(*k);
                    push(i + 1, &mut succs);
                }
                Instr::StoreLocal(k) => {
                    defs[i] = (*k, 1);
                    push(i + 1, &mut succs);
                }
                Instr::TakeValuesToLocals { nvars, slot_base } => {
                    defs[i] = (*slot_base, *nvars);
                    push(i + 1, &mut succs);
                }
                Instr::Br(t) => push(*t as usize, &mut succs),
                Instr::Go { target_bcp, .. } => push(*target_bcp as usize, &mut succs),
                Instr::BrIfFalse(t) | Instr::BrIfTrue(t) => {
                    push(*t as usize, &mut succs);
                    push(i + 1, &mut succs);
                }
                Instr::ReturnFrom { block_id } => {
                    let (resume_bcp, _) = *self
                        .block_exits
                        .get(block_id)
                        .ok_or(BuildError::Unsupported("ReturnFrom to an unknown block"))?;
                    push(resume_bcp as usize, &mut succs);
                }
                Instr::Return => {}
                Instr::Throw => opaque[i] = true,
                Instr::EvalHost(_) | Instr::PushUnwind { .. } => {
                    opaque[i] = true;
                    push(i + 1, &mut succs);
                }
                Instr::EnterCleanupNormal { cleanup_bcp, .. } => {
                    opaque[i] = true;
                    push(*cleanup_bcp as usize, &mut succs);
                }
                Instr::CleanupReturn => {
                    opaque[i] = true;
                    if let Ok((_, resume)) = self.normal_cleanup_return(i as u32) {
                        push(resume as usize, &mut succs);
                    }
                }
                Instr::CallNamed { sym, .. }
                    if self.transfer_mode && is_never_returning_call(*sym) => {}
                _ => push(i + 1, &mut succs),
            }
        }

        let mut changed = true;
        while changed {
            changed = false;
            for i in (0..n).rev() {
                let mut out = vec![0u64; words];
                if opaque[i] {
                    for (w, slot) in out.iter_mut().enumerate() {
                        *slot = all_ones(w);
                    }
                } else {
                    for &s in &succs[i] {
                        for w in 0..words {
                            out[w] |= live_in[s][w];
                        }
                    }
                    let (base, count) = defs[i];
                    for k in base..base.saturating_add(count) {
                        let k = usize::from(k);
                        if k / 64 < words {
                            out[k / 64] &= !(1u64 << (k % 64));
                        }
                    }
                    if let Some(k) = uses[i] {
                        let k = usize::from(k);
                        if k / 64 < words {
                            out[k / 64] |= 1u64 << (k % 64);
                        }
                    }
                }
                if out != live_in[i] {
                    live_in[i] = out;
                    changed = true;
                }
            }
        }
        self.local_live_in = live_in;
        Ok(())
    }

    // ── Pass 3: block creation ──────────────────────────────────────

    fn create_blocks(&mut self) {
        let leaders = self.leaders.clone();
        let entry_is_loop_header = self.bf.code.iter().any(|instr| {
            matches!(
                instr,
                Instr::Br(0)
                    | Instr::BrIfFalse(0)
                    | Instr::BrIfTrue(0)
                    | Instr::Go { target_bcp: 0, .. }
            )
        });
        for &l in &leaders {
            let b = if l == 0 && !entry_is_loop_header {
                self.f.entry()
            } else {
                self.f.make_block()
            };
            self.block_of.insert(l, b);
        }
        let n = self.f.num_blocks();
        self.total_preds = vec![0; n];
        self.seen_preds = vec![0; n];
        self.interpreted = vec![false; n];
        self.sealed = vec![false; n];
    }

    /// Inclusive-exclusive bytecode range of the block that starts at leader
    /// index `p` in `self.leaders`.
    fn block_range(&self, p: usize) -> (usize, usize) {
        let start = self.leaders[p];
        let end = self
            .leaders
            .get(p + 1)
            .copied()
            .unwrap_or(self.bf.code.len());
        (start, end)
    }

    // ── Pass 4: structural predecessor counts ───────────────────────

    /// Mark the blocks reachable from the entry, following the same structural
    /// successor relation `compute_total_preds` uses.
    ///
    /// Without this, `compute_total_preds` counted edges out of blocks that are
    /// never entered, while `process_blocks` replaces such a block with a
    /// `trap()` and emits NO edges. A block reachable only from an unreachable
    /// one therefore had `total_preds` it could never see, was never sealed, and
    /// its incomplete phis tripped "index out of bounds: the len is 0 but the
    /// index is 0" in `add_phi_operands`.
    ///
    /// That was latent until a call which never returns normally began
    /// terminating its block (bliss-wukf): before that every non-leader
    /// instruction fell through, so unreachable blocks did not arise in
    /// practice.
    fn compute_reachable(&mut self) -> Result<(), BuildError> {
        let n = self.leaders.len();
        let mut pos_of: std::collections::HashMap<Block, usize> =
            std::collections::HashMap::with_capacity(n);
        for (p, l) in self.leaders.iter().enumerate() {
            pos_of.insert(self.block_of[l], p);
        }
        let mut reachable = vec![false; n];
        if n > 0 {
            reachable[0] = true; // the entry leader is bytecode index 0
            let mut work = vec![0usize];
            while let Some(p) = work.pop() {
                let (start, end) = self.block_range(p);
                for s in self.structural_succs(start, end)? {
                    let sp = *pos_of
                        .get(&s)
                        .ok_or(BuildError::Unsupported("successor is not a leader block"))?;
                    if !reachable[sp] {
                        reachable[sp] = true;
                        work.push(sp);
                    }
                }
            }
        }
        self.reachable = reachable;
        Ok(())
    }

    /// True when the block at leader position `p` is entered at all.
    fn block_is_reachable(&self, p: usize) -> bool {
        self.reachable.get(p).copied().unwrap_or(true)
    }

    fn compute_total_preds(&mut self) -> Result<(), BuildError> {
        for p in 0..self.leaders.len() {
            // An unreachable block emits no edges (process_blocks traps it), so
            // counting its successors would leave them permanently unsealed.
            if !self.block_is_reachable(p) {
                continue;
            }
            let (start, end) = self.block_range(p);
            for s in self.structural_succs(start, end)? {
                self.total_preds[s.index()] += 1;
            }
        }
        Ok(())
    }

    /// Successor blocks of the block spanning `[start, end)`. The block's
    /// terminator is the **first** control-flow instruction in the range — the
    /// same one `interpret_block` stops at — NOT `code[end-1]`: an unconditional
    /// `Br`/`Go`/`Return` is not followed by a leader, so unreachable trailing
    /// instructions (e.g. a `PopHandler` after a loop's `Go`) may sit after it in
    /// the same block. Reading the last instruction would then miscount edges and
    /// mis-seal loop headers (bliss-fe8). Only the successor multiset matters here
    /// (for predecessor counting); the real edge order comes from `finish_block`.
    fn structural_succs(&self, start: usize, end: usize) -> Result<Vec<Block>, BuildError> {
        let code = &self.bf.code;
        let blk = |t: usize| -> Result<Block, BuildError> {
            self.block_of
                .get(&t)
                .copied()
                .ok_or(BuildError::Unsupported(
                    "branch target is not a block leader",
                ))
        };
        for (offset, instr) in code[start..end].iter().enumerate() {
            if self.catch_transition(start + offset).is_some()
                || self.handler_transition(start + offset).is_some()
                || self.handler_bind_transition(start + offset).is_some()
                || self.restart_case_transition(start + offset).is_some()
            {
                let mut successors = vec![blk(end)?];
                for target in self.exceptional_handlers((start + offset) as u32) {
                    successors.push(blk(target.body_bcp as usize)?);
                }
                for (_, resume, _) in self.exceptional_catches((start + offset) as u32) {
                    successors.push(blk(resume as usize)?);
                }
                if let Some((cleanup, _)) = self.exceptional_cleanup((start + offset) as u32) {
                    successors.push(blk(cleanup as usize)?);
                }
                return Ok(successors);
            }
            match instr {
                Instr::CallNamed { .. }
                | Instr::SetValues(_)
                | Instr::Throw
                | Instr::EvalHost(_)
                | Instr::LoadFunction(_)
                    if self.native_cleanups =>
                {
                    let mut successors = Vec::new();
                    for target in self.exceptional_handlers((start + offset) as u32) {
                        successors.push(blk(target.body_bcp as usize)?);
                    }
                    for (_, resume, _) in self.exceptional_catches((start + offset) as u32) {
                        successors.push(blk(resume as usize)?);
                    }
                    if !matches!(instr, Instr::Throw)
                        && !matches!(instr, Instr::CallNamed { sym, .. } if is_never_returning_call(*sym))
                    {
                        successors.push(blk(end)?);
                    }
                    if let Some((cleanup, _)) = self.exceptional_cleanup((start + offset) as u32) {
                        successors.push(blk(cleanup as usize)?);
                    }
                    return Ok(successors);
                }
                Instr::EnterCleanupNormal { cleanup_bcp, .. } if self.transfer_mode => {
                    return Ok(vec![blk(*cleanup_bcp as usize)?]);
                }
                Instr::CleanupReturn if self.transfer_mode => {
                    let (_, resume) = self.normal_cleanup_return((start + offset) as u32)?;
                    let mut successors = Vec::new();
                    if resume != u32::MAX {
                        successors.push(blk(resume as usize)?);
                    }
                    if self.native_cleanups {
                        for target in self.exceptional_handlers((start + offset) as u32) {
                            successors.push(blk(target.body_bcp as usize)?);
                        }
                        for (_, resume, _) in self.exceptional_catches((start + offset) as u32) {
                            successors.push(blk(resume as usize)?);
                        }
                        if let Some((cleanup, _)) =
                            self.exceptional_cleanup((start + offset) as u32)
                        {
                            successors.push(blk(cleanup as usize)?);
                        }
                    }
                    return Ok(successors);
                }
                Instr::Br(t) => return Ok(vec![blk(*t as usize)?]),
                Instr::Go { target_bcp, .. } => return Ok(vec![blk(*target_bcp as usize)?]),
                Instr::ReturnFrom { block_id } => {
                    let (resume_bcp, _) = *self
                        .block_exits
                        .get(block_id)
                        .ok_or(BuildError::Unsupported("ReturnFrom to an unknown block"))?;
                    return Ok(vec![blk(resume_bcp as usize)?]);
                }
                Instr::BrIfTrue(t) => return Ok(vec![blk(*t as usize)?, blk(end)?]),
                Instr::BrIfFalse(t) => return Ok(vec![blk(end)?, blk(*t as usize)?]),
                Instr::Return => return Ok(vec![]),
                // A call that never returns normally has NO successors. Without
                // this the scan falls through to "no terminator in range" and
                // counts an edge to the next leader, so that block is sealed
                // expecting a value from a predecessor Term::Trap never
                // creates -- regalloc2 then panics with "trying to get a VReg
                // before observing its class" (bliss-wukf).
                Instr::CallNamed { sym, .. } if is_never_returning_call(*sym) => {
                    return Ok(vec![]);
                }
                _ => {}
            }
        }
        // No terminator in range: fall through to the next leader (if any).
        if end < code.len() {
            Ok(vec![blk(end)?])
        } else {
            Ok(vec![])
        }
    }

    // ── Entry seeding ───────────────────────────────────────────────

    /// Seed the entry block: locals `0..arity` are function parameters (entry
    /// block parameters); locals `arity..n_locals` start as NIL so a read before
    /// the first store never hits an undefined variable.
    fn seed_entry(&mut self) {
        // Record variadic-ness so the emitter can suppress the positional
        // register self-call entry (bliss-32l): a variadic function's entry
        // params are pre-collected slots, not positional arguments.
        self.f.set_variadic(self.bf.variadic);
        let entry = self.f.entry();
        // Number of parameters that live in frame slots (the contiguous slot
        // prefix `0..p`). For a fixed lambda list this is `arity`. For a
        // NON-capturing variadic one it also covers the &optional/&rest/&key
        // slots, which the shared `bind_variadic` path (run_native) fills BEFORE
        // the compiled body runs — so T2 must seed them from their slots, not
        // NIL them out (bliss-32l). Capturing variadic functions (boxed params)
        // are declined from T2 upstream, so here they never occur.
        let arity = if self.bf.variadic {
            self.bf
                .param_layout
                .iter()
                .filter(|(_, loc)| matches!(loc, VarLoc::Slot(_)))
                .count() as u16
        } else {
            self.bf.arity
        }
        .min(self.bf.n_locals);
        for i in 0..arity {
            let declared = self
                .bf
                .param_types
                .get(i as usize)
                .copied()
                .unwrap_or_default();
            let ty = match declared {
                DeclaredType::Any => IRType::TOP,
                DeclaredType::Fixnum => IRType::of(TypeBits::FIXNUM),
                DeclaredType::SingleFloat => IRType::of(TypeBits::SINGLE_FLOAT),
            };
            let p = self
                .f
                .add_block_param(entry, ty, ValueRepresentation::Tagged);
            if !declared.is_any() {
                self.f.mark_entry_param_checked(p);
            }
            self.write_var(Var::Local(i), entry, p);
        }
        for i in arity..self.bf.n_locals {
            let nil = self.emit_const_nil(entry);
            self.write_var(Var::Local(i), entry, nil);
        }
    }

    // ── Main processing loop ────────────────────────────────────────

    fn process_blocks(&mut self) -> Result<(), BuildError> {
        for p in 0..self.leaders.len() {
            let leader = self.leaders[p];
            let block = self.block_of[&leader];

            // Unreachable non-entry block: keep it well-formed with a Trap and
            // move on. Tested against the reachability marking as well as the
            // predecessor count -- a block whose only predecessors are
            // themselves unreachable now has a zero count too, but testing
            // reachability directly says what is meant.
            if block != self.f.entry()
                && (!self.block_is_reachable(p) || self.total_preds[block.index()] == 0)
            {
                self.sealed[block.index()] = true;
                self.set_term(block, trap());
                self.interpreted[block.index()] = true;
                continue;
            }

            // Seal now if every predecessor has already been interpreted.
            if self.seen_preds[block.index()] == self.total_preds[block.index()] {
                self.seal(block);
            }

            self.interpret_block(p, block)?;
        }
        Ok(())
    }

    fn interpret_block(&mut self, p: usize, block: Block) -> Result<(), BuildError> {
        let (start, end) = self.block_range(p);
        let depth = self.entry_depth.get(&start).copied().unwrap_or(0);

        // Materialise the incoming operand stack.
        let mut stack: Vec<Value> = Vec::with_capacity(depth);
        for k in 0..depth {
            stack.push(self.read_var(Var::Stack(k as u16), block));
        }

        if self.native_cleanups
            && self.bf.code.iter().any(|i| {
                matches!(i,
            Instr::PushUnwind { cleanup_bcp, .. } if *cleanup_bcp as usize == start)
            })
        {
            let resumes = self
                .control_scopes
                .as_ref()
                .unwrap()
                .cleanup_resumes(start as u32);
            let resume_bcp = match resumes {
                [resume] => *resume,
                [] => u32::MAX,
                _ => return Err(BuildError::Unsupported("ambiguous cleanup resume")),
            };
            let fs = self.build_frame_state(block, &stack, start as u32);
            self.f.push_inst(
                block,
                InstData {
                    opcode: Opcode::CleanupLanding,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::CleanupContinuation {
                        cleanup_bcp: start as u32,
                        resume_bcp,
                    },
                    flags: InstFlags {
                        effectful: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(fs),
                    source_pos: 0,
                },
                &[],
            );
        }

        // Interpret straight-line instructions until a terminator (or the range
        // ends and we fall through).
        let mut term: Option<Term> = None;
        let code = &self.bf.code;
        for (offset, instruction) in code[start..end].iter().enumerate() {
            let i = start + offset;
            if self.native_cleanups
                && matches!(instruction, Instr::EvalHost(_) | Instr::LoadFunction(_))
            {
                let aux = match instruction {
                    Instr::EvalHost(index) => {
                        if self.bf.constants.get(*index as usize).is_none() {
                            return Err(BuildError::Unsupported("invalid host form constant"));
                        }
                        AuxData::HostEval(*index)
                    }
                    Instr::LoadFunction(symbol) => AuxData::FunctionLookup(*symbol),
                    _ => unreachable!(),
                };
                let fs = self.build_frame_state(block, &stack, i as u32);
                let (inst, results) = self.f.push_inst(
                    block,
                    InstData {
                        opcode: Opcode::Call,
                        args: vec![],
                        results: vec![],
                        aux,
                        flags: runtime_call_flags(),
                        targets: vec![],
                        frame_state: Some(fs),
                        source_pos: 0,
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            if let Some((push_bcp, enter)) = self.handler_transition(i) {
                let fs = self.build_frame_state(block, &stack, i as u32);
                let (inst, results) = self.f.push_inst(
                    block,
                    InstData {
                        opcode: Opcode::Call,
                        args: vec![],
                        results: vec![],
                        aux: AuxData::HandlerScope { push_bcp, enter },
                        flags: runtime_call_flags(),
                        targets: vec![],
                        frame_state: Some(fs),
                        source_pos: 0,
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            if let Some((push_bcp, enter)) = self.handler_bind_transition(i) {
                let fs = self.build_frame_state(block, &stack, i as u32);
                let args = if enter {
                    let Instr::PushHandlerBind { hb } = instruction else {
                        unreachable!();
                    };
                    let count = self
                        .bf
                        .handler_binds
                        .get(*hb as usize)
                        .ok_or(BuildError::Unsupported("invalid handler-bind table"))?
                        .types
                        .len();
                    let first = stack
                        .len()
                        .checked_sub(count)
                        .ok_or(BuildError::Unsupported("handler-bind without handlers"))?;
                    stack.split_off(first)
                } else {
                    vec![]
                };
                let (inst, results) = self.f.push_inst(
                    block,
                    InstData {
                        opcode: Opcode::Call,
                        args,
                        results: vec![],
                        aux: AuxData::HandlerBindScope { push_bcp, enter },
                        flags: runtime_call_flags(),
                        targets: vec![],
                        frame_state: Some(fs),
                        source_pos: 0,
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            if let Some((push_bcp, enter)) = self.restart_case_transition(i) {
                let fs = self.build_frame_state(block, &stack, i as u32);
                let (inst, results) = self.f.push_inst(
                    block,
                    InstData {
                        opcode: Opcode::Call,
                        args: vec![],
                        results: vec![],
                        aux: AuxData::RestartCaseScope { push_bcp, enter },
                        flags: runtime_call_flags(),
                        targets: vec![],
                        frame_state: Some(fs),
                        source_pos: 0,
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            if let Some((push_bcp, enter)) = self.catch_transition(i) {
                let fs = self.build_frame_state(block, &stack, i as u32);
                let args = if enter {
                    vec![stack
                        .pop()
                        .ok_or(BuildError::Unsupported("CATCH without tag"))?]
                } else {
                    vec![]
                };
                let (inst, results) = self.f.push_inst(
                    block,
                    InstData {
                        opcode: Opcode::Call,
                        args,
                        results: vec![],
                        aux: AuxData::CatchScope { push_bcp, enter },
                        flags: runtime_call_flags(),
                        targets: vec![],
                        frame_state: Some(fs),
                        source_pos: 0,
                    },
                    &[(IRType::TOP, ValueRepresentation::Tagged)],
                );
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            match instruction {
                Instr::Const(idx) => {
                    let v = self.emit_const(block, *idx)?;
                    stack.push(v);
                }
                Instr::LoadLocal(s) => {
                    let v = self.read_var(Var::Local(*s), block);
                    stack.push(v);
                }
                Instr::StoreLocal(s) => {
                    let v = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (StoreLocal)"))?;
                    self.write_var(Var::Local(*s), block, v);
                }
                Instr::LoadEnvVar(index) | Instr::StoreEnvVar(index) => {
                    if self.bf.names.get(*index as usize).is_none() {
                        return Err(BuildError::Unsupported("invalid environment name index"));
                    }
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    if matches!(instruction, Instr::LoadEnvVar(_)) {
                        let value = self.emit(
                            block, Opcode::EnvironmentValue, vec![],
                            AuxData::EnvironmentName(*index), runtime_call_flags(),
                            Some(fs), IRType::TOP,
                        ).expect("environment read has a result");
                        stack.push(value);
                    } else {
                        let value = stack.pop().ok_or(BuildError::Unsupported(
                            "stack underflow (StoreEnvVar)",
                        ))?;
                        self.emit_effect(block, Opcode::SetEnvironmentValue, vec![value],
                            AuxData::EnvironmentName(*index), Some(fs));
                    }
                }
                Instr::LoadGlobal(sym) => {
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let v = self.emit(
                        block,
                        Opcode::SymbolValue,
                        vec![],
                        AuxData::SymbolRef(*sym),
                        runtime_call_flags(),
                        Some(fs),
                        IRType::TOP,
                    );
                    stack.push(v.expect("SymbolValue has a result"));
                }
                Instr::LoadFunction(sym) => {
                    // `#'f`: a runtime read of the symbol's function cell, with
                    // the same shape as LoadGlobal above (bliss-m285). Without
                    // this the whole function was rejected, which is why any
                    // code mentioning #'f — (mapcar #'f …) and friends — could
                    // never reach T2.
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let v = self.emit(
                        block,
                        Opcode::SymbolFunction,
                        vec![],
                        AuxData::SymbolRef(*sym),
                        runtime_call_flags(),
                        Some(fs),
                        IRType::TOP,
                    );
                    stack.push(v.expect("SymbolFunction has a result"));
                }
                Instr::StoreGlobal(sym) => {
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let v = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (StoreGlobal)"))?;
                    self.emit_effect(
                        block,
                        Opcode::SetSymbolValue,
                        vec![v],
                        AuxData::SymbolRef(*sym),
                        Some(fs),
                    );
                }
                Instr::Pop => {
                    stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (Pop)"))?;
                }
                Instr::ClearMv => {
                    // Reset multiple-values state; no operand effect (bliss-mzp).
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    self.emit_effect(block, Opcode::ClearMv, vec![], AuxData::None, Some(fs));
                }
                Instr::TakeValuesToLocals { nvars, slot_base } => {
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let primary = stack.pop().ok_or(BuildError::Unsupported(
                        "stack underflow (TakeValuesToLocals)",
                    ))?;
                    if slot_base.saturating_add(*nvars) > self.bf.n_locals {
                        return Err(BuildError::Unsupported(
                            "TakeValuesToLocals destination out of range",
                        ));
                    }
                    let result_types =
                        vec![(IRType::TOP, ValueRepresentation::Tagged); *nvars as usize];
                    let (_inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::TakeValuesToLocals,
                            args: vec![primary],
                            results: vec![],
                            aux: AuxData::ValuesLocals {
                                nvars: *nvars,
                                slot_base: *slot_base,
                            },
                            flags: runtime_call_flags(),
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &result_types,
                    );
                    for (offset, result) in results.into_iter().enumerate() {
                        self.write_var(Var::Local(*slot_base + offset as u16), block, result);
                    }
                }
                Instr::Throw if self.native_cleanups => {
                    if stack.len() < 2 {
                        return Err(BuildError::Unsupported("stack underflow (Throw)"));
                    }
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let args = stack.split_off(stack.len() - 2);
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args,
                            results: vec![],
                            aux: AuxData::TransferThrow,
                            flags: InstFlags {
                                effectful: true,
                                call: true,
                                safepoint: true,
                                ..InstFlags::default()
                            },
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    self.finish_native_invoke(block, inst, stack, results[0], i as u32, None)?;
                    return Ok(());
                }
                Instr::SetValues(n) => {
                    let n = *n as usize;
                    if stack.len() < n {
                        return Err(BuildError::Unsupported("stack underflow (SetValues)"));
                    }
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let split = stack.len() - n;
                    let args: Vec<Value> = stack.split_off(split);
                    let values_sym = egcl_rt::symbols::intern("VALUES");
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args,
                            results: vec![],
                            aux: AuxData::CallTarget(values_sym),
                            flags: InstFlags {
                                effectful: true,
                                call: true,
                                safepoint: true,
                                ..InstFlags::default()
                            },
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    if self.native_cleanups {
                        self.finish_native_invoke(
                            block,
                            inst,
                            stack,
                            results[0],
                            i as u32,
                            Some(end),
                        )?;
                        return Ok(());
                    }
                    stack.push(results[0]);
                }
                Instr::PushBlock { sp_restore, .. } | Instr::PushTag { sp_restore, .. } => {
                    if *sp_restore != 0 {
                        return Err(BuildError::Unsupported("handler with non-empty sp_restore"));
                    }
                }
                Instr::PopHandler => {}
                Instr::PushUnwind { .. } if self.transfer_mode => {}
                Instr::EnterCleanupNormal {
                    cleanup_bcp,
                    resume_bcp,
                } if self.transfer_mode => {
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let primary = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("cleanup entry without primary"))?;
                    self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::CleanupSave,
                            args: vec![primary],
                            results: vec![],
                            aux: AuxData::CleanupContinuation {
                                cleanup_bcp: *cleanup_bcp,
                                resume_bcp: *resume_bcp,
                            },
                            flags: runtime_call_flags(),
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[],
                    );
                    term = Some(Term::Jump(self.block_of[&(*cleanup_bcp as usize)]));
                    break;
                }
                Instr::CleanupReturn if self.transfer_mode => {
                    let (cleanup_bcp, resume_bcp) = self.normal_cleanup_return(i as u32)?;
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let (inst, values) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::CleanupRestore,
                            args: vec![],
                            results: vec![],
                            aux: AuxData::CleanupContinuation {
                                cleanup_bcp,
                                resume_bcp,
                            },
                            flags: runtime_call_flags(),
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    if self.native_cleanups {
                        self.finish_native_invoke(
                            block,
                            inst,
                            stack,
                            values[0],
                            i as u32,
                            (resume_bcp != u32::MAX).then_some(resume_bcp as usize),
                        )?;
                        return Ok(());
                    }
                    stack.push(values[0]);
                    term = Some(Term::Jump(self.block_of[&(resume_bcp as usize)]));
                    break;
                }
                Instr::Dup => {
                    let v = *stack
                        .last()
                        .ok_or(BuildError::Unsupported("stack underflow (Dup)"))?;
                    stack.push(v);
                }
                Instr::CallNamed { sym, nargs } => {
                    let n = *nargs as usize;
                    if stack.len() < n {
                        return Err(BuildError::Unsupported("stack underflow (CallNamed)"));
                    }
                    // Compiler-known functions reach T2 through shared metadata
                    // and call-site policy. An expansion hook can still decline
                    // for operand-shape reasons (for example dynamic TYPEP),
                    // leaving the normal Call and its FrameState intact.
                    if let Some(metadata) =
                        metadata_for_symbol(*sym).filter(|_| !self.native_cleanups)
                    {
                        let policy = self.inline_options.policy_at(i as u32);
                        let decision = decide(
                            metadata,
                            *nargs,
                            policy,
                            self.inline_options
                                .call_site_is_hot(self.root_symbol, i as u32),
                            self.inline_depth,
                            self.remaining_inline_budget,
                            self.inline_options.config,
                        );
                        if let InlineDecision::Expand(intrinsic) = decision {
                            if self.expand_intrinsic(intrinsic, block, &mut stack, i, start)? {
                                self.remaining_inline_budget -= metadata.cost;
                                continue;
                            }
                        }
                    }
                    // Snapshot the pre-call frame (args still live) for deopt.
                    let bcp = i as u32;
                    let fs = self.build_frame_state(block, &stack, bcp);
                    let split = stack.len() - n;
                    let args: Vec<Value> = stack.split_off(split); // arg0..arg{n-1}
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args,
                            results: vec![],
                            aux: AuxData::CallTarget(*sym),
                            flags: InstFlags {
                                effectful: true,
                                call: true,
                                safepoint: true,
                                ..InstFlags::default()
                            },
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
                    if self.native_cleanups {
                        self.finish_native_invoke(
                            block,
                            inst,
                            stack,
                            results[0],
                            i as u32,
                            (!is_never_returning_call(*sym)).then_some(end),
                        )?;
                        return Ok(());
                    }
                    // A call that NEVER RETURNS NORMALLY ends this path
                    // (bliss-wukf). Without it T2 carried on executing code
                    // that must not run: a store after the error landed, and a
                    // later error superseded the real one.
                    //
                    // CLHS says ERROR never returns normally, so everything
                    // after the call is unreachable under correct semantics.
                    // The result is deliberately NOT pushed -- nothing can
                    // observe it, and a value defined here that no block
                    // publishes gives regalloc a use with no def.
                    if is_never_returning_call(*sym) {
                        term = Some(Term::Trap);
                        break;
                    }
                    stack.push(results[0]);
                }
                Instr::TypeP(class) => {
                    if !matches!(
                        *class,
                        typep_class::STRING
                            | typep_class::SYMBOL
                            | typep_class::PACKAGE
                            | typep_class::LIST
                            | typep_class::CONS
                            | typep_class::NULL
                            | typep_class::BOOLEAN
                            | typep_class::HASH_TABLE
                    ) {
                        return Err(BuildError::Unsupported("unknown TypeP class"));
                    }
                    // The check is pure, but TYPEP's single-value return must
                    // survive folding or elimination of that check.
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    self.emit_effect(block, Opcode::ClearMv, vec![], AuxData::None, Some(fs));
                    let value = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (TypeP)"))?;
                    let result = self
                        .emit(
                            block,
                            Opcode::TypeCheck,
                            vec![value],
                            AuxData::TypepClass(*class),
                            InstFlags::default(),
                            None,
                            IRType::TOP,
                        )
                        .ok_or(BuildError::Unsupported("TypeP has a result"))?;
                    stack.push(result);
                }
                Instr::MemoryFence(kind) => {
                    let result = self
                        .emit(
                            block,
                            Opcode::MemoryFence,
                            vec![],
                            AuxData::MemoryFence(*kind),
                            InstFlags {
                                effectful: true,
                                ..InstFlags::default()
                            },
                            None,
                            IRType::of(TypeBits::NULL),
                        )
                        .ok_or(BuildError::Unsupported("MemoryFence has a result"))?;
                    stack.push(result);
                }
                Instr::Br(t) => {
                    let s = self.block_of[&(*t as usize)];
                    term = Some(Term::Jump(s));
                    break;
                }
                Instr::Go { target_bcp, .. } => {
                    self.check_direct_exit(i as u32)?;
                    let s = self.block_of[&(*target_bcp as usize)];
                    term = Some(Term::Jump(s));
                    break;
                }
                Instr::ReturnFrom { .. } => {
                    self.check_direct_exit(i as u32)?;
                    // The runtime pops the value, unwinds to the block, resets
                    // the operand stack to `sp_restore` and pushes the value
                    // back. Mirror that here so the exit stack matches what
                    // normal completion leaves, and the Braun merge at the
                    // resume block sees one consistent slot from every edge.
                    //
                    // The exit check refuses intervening cleanup/dynamic scopes.
                    // The legacy builder also declines PushUnwind entirely.
                    let v = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (ReturnFrom)"))?;
                    let exit = self
                        .control_scopes
                        .as_ref()
                        .expect("scope analysis before SSA")
                        .exit_at(i as u32)
                        .ok_or(BuildError::Unsupported("ReturnFrom without active scope"))?;
                    let (resume_bcp, sp_restore) = (exit.target_bcp, exit.sp_restore);
                    let restore = sp_restore as usize;
                    if stack.len() < restore {
                        return Err(BuildError::Unsupported("ReturnFrom below sp_restore"));
                    }
                    stack.truncate(restore);
                    stack.push(v);
                    let s = self.block_of[&(resume_bcp as usize)];
                    term = Some(Term::Jump(s));
                    break;
                }
                Instr::BrIfFalse(t) => {
                    let cond = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (BrIfFalse)"))?;
                    let false_blk = self.block_of[&(*t as usize)];
                    let true_blk = self.block_of[&end]; // fall-through
                    term = Some(Term::Brif(cond, true_blk, false_blk));
                    break;
                }
                Instr::BrIfTrue(t) => {
                    let cond = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (BrIfTrue)"))?;
                    let true_blk = self.block_of[&(*t as usize)];
                    let false_blk = self.block_of[&end]; // fall-through
                    term = Some(Term::Brif(cond, true_blk, false_blk));
                    break;
                }
                Instr::Return => {
                    let v = match stack.pop() {
                        Some(v) => v,
                        None => self.emit_const_nil(block),
                    };
                    term = Some(Term::Ret(v));
                    break;
                }
                _ => return Err(BuildError::UnsupportedInstr(instr_kind(&code[i]))),
            }
        }

        // Publish this block's exit definition of each operand-stack slot so
        // successors can read them through `read_var`, then finish the block.
        self.finish_block(block, term, stack, end);
        Ok(())
    }

    fn exceptional_cleanup(&self, bcp: u32) -> Option<(u32, u16)> {
        self.control_scopes
            .as_ref()?
            .before(bcp)?
            .iter()
            .rev()
            .find_map(|scope| {
                if let ScopeKind::Unwind { cleanup_bcp } = scope.kind {
                    Some((cleanup_bcp, scope.sp_restore))
                } else {
                    None
                }
            })
    }

    /// Only catches before the next cleanup can be entered immediately. The
    /// cleanup completion site exposes the next set after that cleanup runs.
    fn exceptional_catches(&self, bcp: u32) -> Vec<(u32, u32, u16)> {
        self.control_scopes
            .as_ref()
            .and_then(|s| s.before(bcp))
            .into_iter()
            .flatten()
            .rev()
            .take_while(|scope| !matches!(scope.kind, ScopeKind::Unwind { .. }))
            .filter_map(|scope| match scope.kind {
                ScopeKind::Catch { resume_bcp } => {
                    Some((scope.push_bcp, resume_bcp, scope.sp_restore))
                }
                _ => None,
            })
            .collect()
    }

    fn synthetic_block(&mut self) -> Block {
        let block = self.f.make_block();
        self.total_preds.push(1);
        self.seen_preds.push(0);
        self.interpreted.push(false);
        self.sealed.push(false);
        block
    }

    fn finish_native_invoke(
        &mut self,
        block: Block,
        call: Inst,
        mut stack: Vec<Value>,
        result: Value,
        bcp: u32,
        resume: Option<usize>,
    ) -> Result<(), BuildError> {
        use crate::t2::ir::BlockCall;
        let normal = self.synthetic_block();
        let cold = self.synthetic_block();
        let projected = self
            .f
            .add_block_param(normal, IRType::TOP, ValueRepresentation::Tagged);
        let fs = self.f.inst(call).frame_state;
        let invoke = self.f.inst_mut(call);
        invoke.opcode = Opcode::Invoke;
        invoke.flags.terminator = true;
        invoke.targets = vec![
            BlockCall {
                block: normal,
                args: vec![result],
            },
            BlockCall {
                block: cold,
                args: vec![],
            },
        ];
        for (slot, &value) in stack.iter().enumerate() {
            self.write_var(Var::Stack(slot as u16), block, value);
        }
        self.record_edges(block, call, vec![normal, cold]);
        self.seal(normal);
        self.seal(cold);
        let cold_stack = stack.clone();
        let mut targets = if let Some((cleanup, depth)) = self.exceptional_cleanup(bcp) {
            if usize::from(depth) > cold_stack.len() {
                return Err(BuildError::Unsupported("cleanup consumes enclosing stack"));
            }
            vec![BlockCall {
                block: self.block_of[&(cleanup as usize)],
                args: vec![],
            }]
        } else {
            vec![]
        };
        let mut catch_edges = Vec::new();
        for (push_bcp, resume_bcp, depth) in self.exceptional_catches(bcp) {
            if usize::from(depth) > cold_stack.len() {
                return Err(BuildError::Unsupported("catch consumes enclosing stack"));
            }
            let landing = self.synthetic_block();
            targets.push(BlockCall {
                block: landing,
                args: vec![],
            });
            catch_edges.push((landing, push_bcp, resume_bcp, depth));
        }
        let mut handler_edges = Vec::new();
        for target in self.exceptional_handlers(bcp) {
            if usize::from(target.sp_restore) > cold_stack.len() {
                return Err(BuildError::Unsupported("handler consumes enclosing stack"));
            }
            let landing = self.synthetic_block();
            targets.push(BlockCall {
                block: landing,
                args: vec![],
            });
            handler_edges.push((landing, target));
        }
        if !matches!(
            self.f.inst(call).aux,
            AuxData::CatchScope { .. }
                | AuxData::HandlerScope { .. }
                | AuxData::HandlerBindScope { .. }
                | AuxData::RestartCaseScope { .. }
        ) {
            stack.push(projected);
        }
        let normal_term = resume
            .map(|bcp| Term::Jump(self.block_of[&bcp]))
            .unwrap_or(Term::Trap);
        self.finish_block(normal, Some(normal_term), stack, self.bf.code.len());
        let scopes = self
            .control_scopes
            .as_ref()
            .unwrap()
            .before(bcp)
            .unwrap()
            .to_vec();
        self.finish_block(
            cold,
            Some(Term::Transfer(InstData {
                opcode: Opcode::NlxTransfer,
                args: vec![],
                results: vec![],
                aux: AuxData::TransferSite {
                    origin_bcp: bcp,
                    scopes,
                },
                flags: InstFlags {
                    effectful: true,
                    safepoint: true,
                    call: true,
                    terminator: true,
                    ..InstFlags::default()
                },
                targets,
                frame_state: fs,
                source_pos: 0,
            })),
            cold_stack.clone(),
            self.bf.code.len(),
        );
        for (landing, target) in handler_edges {
            self.seal(landing);
            let prefix = cold_stack[..usize::from(target.sp_restore)].to_vec();
            let state = self.build_frame_state(landing, &prefix, target.body_bcp);
            let (_, results) = self.f.push_inst(
                landing,
                InstData {
                    opcode: Opcode::HandlerLanding,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::HandlerDestination {
                        push_bcp: target.push_bcp,
                        table_index: target.table_index,
                        clause_index: target.clause_index,
                    },
                    flags: InstFlags {
                        effectful: true,
                        call: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(state),
                    source_pos: 0,
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            if let Some(slot) = target.var_slot {
                self.write_var(Var::Local(slot), landing, results[0]);
            }
            self.finish_block(
                landing,
                Some(Term::Jump(self.block_of[&(target.body_bcp as usize)])),
                prefix,
                self.bf.code.len(),
            );
        }
        for (landing, push_bcp, resume_bcp, depth) in catch_edges {
            self.seal(landing);
            let mut prefix = cold_stack[..usize::from(depth)].to_vec();
            let state = self.build_frame_state(landing, &prefix, resume_bcp);
            let (_, results) = self.f.push_inst(
                landing,
                InstData {
                    opcode: Opcode::CatchLanding,
                    args: vec![],
                    results: vec![],
                    aux: AuxData::CatchDestination {
                        push_bcp,
                        resume_bcp,
                    },
                    flags: InstFlags {
                        effectful: true,
                        call: true,
                        ..InstFlags::default()
                    },
                    targets: vec![],
                    frame_state: Some(state),
                    source_pos: 0,
                },
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
            prefix.push(results[0]);
            self.finish_block(
                landing,
                Some(Term::Jump(self.block_of[&(resume_bcp as usize)])),
                prefix,
                self.bf.code.len(),
            );
        }
        Ok(())
    }

    /// Write exit stack defs, set the terminator, and do predecessor bookkeeping
    /// (marking edges and sealing any already-interpreted successor whose last
    /// predecessor this block is).
    fn finish_block(&mut self, block: Block, term: Option<Term>, stack: Vec<Value>, end: usize) {
        // The exit operand stack that flows to successors.
        let exit_stack: &[Value] = match &term {
            Some(Term::Ret(_)) | Some(Term::Trap) => &[], // no successors
            _ => &stack,
        };
        for (k, &v) in exit_stack.iter().enumerate() {
            self.write_var(Var::Stack(k as u16), block, v);
        }

        // Build the terminator and record its outgoing edges as (successor, idx).
        let (data, edges): (InstData, Vec<Block>) = match term {
            Some(Term::Jump(s)) => (jump(s), vec![s]),
            Some(Term::Transfer(data)) => {
                let edges = data.targets.iter().map(|edge| edge.block).collect();
                (data, edges)
            }
            Some(Term::Brif(cond, t, f)) => (brif(cond, t, f), vec![t, f]),
            Some(Term::Ret(v)) => (ret(v), vec![]),
            Some(Term::Trap) => (trap(), vec![]),
            None => {
                // Fall through to the next leader, or return NIL at code end.
                if end < self.bf.code.len() {
                    let s = self.block_of[&end];
                    (jump(s), vec![s])
                } else {
                    let nil = self.emit_const_nil(block);
                    (ret(nil), vec![])
                }
            }
        };

        let inst = self.f.set_terminator(block, data);
        self.record_edges(block, inst, edges);
    }

    fn record_edges(&mut self, block: Block, inst: Inst, edges: Vec<Block>) {
        self.terminator_inst.insert(block, inst);
        self.interpreted[block.index()] = true;

        for (idx, s) in edges.into_iter().enumerate() {
            self.pred_edges.entry(s).or_default().push((block, idx));
            self.seen_preds[s.index()] += 1;
            // A successor already interpreted (a loop header) becomes sealable
            // once its final predecessor — this block — is in.
            if self.interpreted[s.index()]
                && !self.sealed[s.index()]
                && self.seen_preds[s.index()] == self.total_preds[s.index()]
            {
                self.seal(s);
            }
        }
    }

    // ── Braun SSA construction primitives ───────────

    fn write_var(&mut self, var: Var, block: Block, v: Value) {
        self.current_def.insert((var, block), v);
    }

    fn read_var(&mut self, var: Var, block: Block) -> Value {
        if let Some(&v) = self.current_def.get(&(var, block)) {
            return v;
        }
        if !self.sealed[block.index()] {
            // May still gain predecessors: record an incomplete phi.
            let phi = self
                .f
                .add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
            self.incomplete_phis
                .entry(block)
                .or_default()
                .push((var, phi));
            self.current_def.insert((var, block), phi);
            return phi;
        }
        let preds = self.pred_edges.get(&block).cloned().unwrap_or_default();

        if preds.len() == 1 {
            let val = self.read_var(var, preds[0].0);
            self.current_def.insert((var, block), val);
            val
        } else {
            // Zero or many predecessors: introduce a phi (block parameter).
            let phi = self
                .f
                .add_block_param(block, IRType::TOP, ValueRepresentation::Tagged);
            self.current_def.insert((var, block), phi); // break cycles first
            self.add_phi_operands(var, block, phi);
            phi
        }
    }

    fn add_phi_operands(&mut self, var: Var, block: Block, _phi: Value) {
        let preds = self.pred_edges.get(&block).cloned().unwrap_or_default();
        for (pred, idx) in preds {
            let val = self.read_var(var, pred);
            let term = self.terminator_inst[&pred];
            self.f.inst_mut(term).targets[idx].args.push(val);
        }
    }

    fn seal(&mut self, block: Block) {
        if self.sealed[block.index()] {
            return;
        }
        if let Some(phis) = self.incomplete_phis.remove(&block) {
            for (var, phi) in phis {
                self.add_phi_operands(var, block, phi);
            }
        }
        self.sealed[block.index()] = true;
    }

    // ── Trivial-phi elimination (Braun `tryRemoveTrivialPhi`) ──

    /// Collapse trivial block parameters as a fixpoint post-pass. A parameter
    /// whose incoming arguments (across every predecessor edge) are all either
    /// itself or one single other value `u` is a trivial phi ≡ `u`. The base
    /// Braun construction leaves these behind for loop-invariant values — e.g. a
    /// `dotimes` bound read inside the loop gets a header phi even though it never
    /// changes — and each one needlessly pins a register, which is what pushes a
    /// global-accumulator loop past the framed emitter's register budget
    /// (bliss-fe8). Removing a parameter drops it from the block and drops the
    /// matching argument from every predecessor edge, keeping the two positionally
    /// consistent (the emitter correlates params↔args by position, not by id).
    fn simplify_trivial_phis(&mut self) {
        loop {
            let mut found: Option<(Block, usize, Value, Value)> = None;
            'outer: for &b in self.block_of.values() {
                let preds = match self.pred_edges.get(&b) {
                    Some(p) if !p.is_empty() => p.clone(),
                    _ => continue, // entry / unreachable: no incoming phi arguments
                };
                let nparams = self.f.block(b).params.len();
                for pos in 0..nparams {
                    let p = self.f.block(b).params[pos];
                    let mut other: Option<Value> = None;
                    let mut trivial = true;
                    for &(pred, idx) in &preds {
                        let term = self.terminator_inst[&pred];
                        let arg = self.f.inst(term).targets[idx].args[pos];
                        if arg == p {
                            continue; // self-reference: does not disqualify
                        }
                        match other {
                            None => other = Some(arg),
                            Some(u) if u == arg => {}
                            Some(_) => {
                                trivial = false;
                                break;
                            }
                        }
                    }
                    // `other == None` means every edge feeds the phi itself — an
                    // undefined/dead cycle; leave it (a real definition never is).
                    if trivial {
                        if let Some(u) = other {
                            found = Some((b, pos, p, u));
                            break 'outer;
                        }
                    }
                }
            }
            let Some((b, pos, p, u)) = found else { break };
            self.replace_value(p, u);
            self.f.block_mut(b).params.remove(pos);
            for &(pred, idx) in &self.pred_edges[&b].clone() {
                let term = self.terminator_inst[&pred];
                self.f.inst_mut(term).targets[idx].args.remove(pos);
            }
        }
    }

    /// Replace every use of value `p` with `u` — in instruction operands, in edge
    /// (block-call) arguments, and in deopt frame states — so a removed trivial
    /// phi leaves no dangling reference.
    fn replace_value(&mut self, p: Value, u: Value) {
        // Synthetic normal/cold call bridges also carry phi uses. Rewriting
        // only bytecode leaders leaves their edge arguments dangling.
        let blocks = self.f.block_order().to_vec();
        for b in blocks {
            let insts = self.f.block(b).insts.clone();
            for inst in insts {
                let d = self.f.inst_mut(inst);
                for a in d.args.iter_mut() {
                    if *a == p {
                        *a = u;
                    }
                }
                for tc in d.targets.iter_mut() {
                    for a in tc.args.iter_mut() {
                        if *a == p {
                            *a = u;
                        }
                    }
                }
            }
        }
        for i in 0..self.f.frame_states.len() {
            let fs = self
                .f
                .frame_states
                .get_mut(crate::t2::frame_state::FrameStateId(i as u32));
            for scope in fs.scopes.iter_mut() {
                for src in scope.locals.iter_mut().chain(scope.stack.iter_mut()) {
                    if let ValueSource::Value { value, .. } = src {
                        if *value == p {
                            *value = u;
                        }
                    }
                }
            }
        }
    }

    // ── Frame state (deopt anchor) ──────────────────────────────────

    fn build_frame_state(
        &mut self,
        block: Block,
        stack: &[Value],
        bcp: u32,
    ) -> crate::t2::frame_state::FrameStateId {
        // A slot the interpreter can never read again from `bcp` is not
        // reconstructed: naming it would keep its value deopt-live (and so
        // spilled, and carried through every loop header) for nothing. The
        // interpreter frame receives UNBOUND-MARKER there, as it already does
        // for not-yet-initialised locals in the entry state.
        let mut locals = Vec::with_capacity(self.bf.n_locals as usize);
        for i in 0..self.bf.n_locals {
            if !self.local_is_live_at(i, bcp as usize) {
                locals.push(ValueSource::Unbound);
                continue;
            }
            let v = self.read_var(Var::Local(i), block);
            locals.push(ValueSource::Value {
                value: v,
                repr: ValueRepresentation::Tagged,
            });
        }
        let stack_srcs = stack
            .iter()
            .map(|&v| ValueSource::Value {
                value: v,
                repr: ValueRepresentation::Tagged,
            })
            .collect();
        let scope = FrameScope {
            function: self.root_symbol,
            bcp,
            locals,
            stack: stack_srcs,
        };
        self.f.frame_states.add(FrameState {
            scopes: vec![scope],
            remat: vec![],
        })
    }

    /// Materialise one metadata-selected leaf expansion. `Ok(false)` means the
    /// hook does not support this operand shape, so the normal call is retained.
    /// The operand stack is changed only after support is proven.
    fn expand_intrinsic(
        &mut self,
        intrinsic: IntrinsicId,
        block: Block,
        stack: &mut Vec<Value>,
        bcp: usize,
        block_start: usize,
    ) -> Result<bool, BuildError> {
        if matches!(intrinsic, IntrinsicId::Car | IntrinsicId::Cdr) {
            // Keep the type proof as an ordinary SSA guard.  CAR and CDR use
            // the same proof identity, so the general dominator-based guard
            // pass can reuse a CAR proof for a later CDR (and vice versa).
            // NIL deliberately takes the deopt path: CL defines (CAR NIL) and
            // (CDR NIL) as NIL, which the generic semantic path preserves.
            let fs = self.build_frame_state(block, stack, bcp as u32);
            let cons = stack
                .pop()
                .ok_or(BuildError::Unsupported("stack underflow (CAR/CDR)"))?;
            let guard_flags = InstFlags {
                effectful: true,
                guard: true,
                ..InstFlags::default()
            };
            let checked_cons = self
                .emit(
                    block,
                    Opcode::Guard,
                    vec![cons],
                    AuxData::TypeTag(IRType::of(TypeBits::CONS)),
                    guard_flags,
                    Some(fs),
                    IRType::of(TypeBits::CONS),
                )
                .ok_or(BuildError::Unsupported("cons guard has a result"))?;
            let opcode = if intrinsic == IntrinsicId::Car {
                Opcode::Car
            } else {
                Opcode::Cdr
            };
            let result = self
                .emit(
                    block,
                    opcode,
                    vec![checked_cons],
                    AuxData::None,
                    InstFlags::default(),
                    None,
                    IRType::TOP,
                )
                .ok_or(BuildError::Unsupported("CAR/CDR has a result"))?;
            stack.push(result);
            return Ok(true);
        }

        if intrinsic == IntrinsicId::FirstChar {
            // Inline the metadata-owned body below the CL function layer.  Keep
            // layout validation as an explicit SSA guard so the post-inlining
            // dominator pass can merge an equivalent guard cloned from another
            // callee.  Both raw string operations consume the refined value and
            // never revalidate its layout themselves.
            let fs = self.build_frame_state(block, stack, bcp as u32);
            let string = stack
                .pop()
                .ok_or(BuildError::Unsupported("stack underflow (FIRST-CHAR)"))?;
            let guard_flags = InstFlags {
                effectful: true,
                guard: true,
                ..InstFlags::default()
            };
            let checked_string = self
                .emit(
                    block,
                    Opcode::Guard,
                    vec![string],
                    AuxData::StringLayout,
                    guard_flags,
                    Some(fs),
                    IRType::of(TypeBits::STRING),
                )
                .ok_or(BuildError::Unsupported("StringLayout guard has a result"))?;
            // This faithfully represents the source LENGTH operation.  Its
            // result is unused because CHAR-at-zero's bounds guard subsumes the
            // positive-length branch; ordinary deopt-aware DCE removes the pure
            // load in the production pipeline.
            let _byte_length = self
                .emit(
                    block,
                    Opcode::StringByteLength,
                    vec![checked_string],
                    AuxData::None,
                    InstFlags::default(),
                    None,
                    IRType::of(TypeBits::FIXNUM),
                )
                .ok_or(BuildError::Unsupported("StringByteLength has a result"))?;
            let zero = self
                .emit(
                    block,
                    Opcode::ConstFixnum,
                    vec![],
                    AuxData::FixnumImm(0),
                    InstFlags::default(),
                    None,
                    IRType::of(TypeBits::FIXNUM),
                )
                .expect("ConstFixnum has a result");
            let result = self
                .emit(
                    block,
                    Opcode::StringAsciiCharAt,
                    vec![checked_string, zero],
                    AuxData::None,
                    guard_flags,
                    Some(fs),
                    IRType::of(TypeBits::CHARACTER),
                )
                .ok_or(BuildError::Unsupported("StringAsciiCharAt has a result"))?;
            stack.push(result);
            return Ok(true);
        }

        let type_bits = match intrinsic {
            // NOT inlinable (bliss-74rl).  A TypeBits::CONS test is a raw tag
            // test, but egcl represents a closure as the cons
            // `(EGCL::CLOSURE . id)`: it carries the cons tag while its Common
            // Lisp type is FUNCTION.  The interpreter's CONSP answers NIL for
            // one; an inlined tag test answers T, so a T2-promoted CONSP
            // silently disagreed with every other tier.  Fall back to a real
            // call -- correctness outranks the saved call (the bliss-x5y.9
            // rule: a fast path must be bit-identical to the tree-walker).
            IntrinsicId::Consp => return Ok(false),
            IntrinsicId::Symbolp => Some(TypeBits::SYMBOL),
            IntrinsicId::Integerp => Some(TypeBits::FIXNUM.join(TypeBits::BIGNUM)),
            // NOT inlinable, for the same reason CONSP is not (bliss-c02n).
            // The inlined STRING test proves the heap tag and then accepts only
            // SIMPLE_BASE_STRING / SIMPLE_CHARACTER_STRING. A string with a
            // fill pointer, an adjustable one, or a DISPLACED one is a
            // COMPLEX_ARRAY, so the inline test answered NIL where the
            // interpreter -- which consults is_complex_vector + cvec_is_string
            // -- answers T.
            //
            // That is a silent wrong ANSWER, not a missed optimisation: a
            // promoted STRINGP flipped from T to NIL once the function got hot,
            // so boot.lisp's %COERCE-LIKE took its (t ...) branch and REMOVE /
            // REMOVE-IF / DELETE-IF / SUBSTITUTE / REMOVE-DUPLICATES started
            // returning a general vector instead of a string. It only showed
            // under the ansi harness because nothing else called them enough
            // times to promote.
            //
            // Fall back to a real call. Correctness outranks the saved call
            // (the bliss-x5y.9 rule: a fast path must be bit-identical to the
            // tree-walker).
            IntrinsicId::Stringp => return Ok(false),
            IntrinsicId::TypepConstant => {
                if bcp <= block_start {
                    return Ok(false);
                }
                let Instr::Const(cidx) = self.bf.code[bcp - 1] else {
                    return Ok(false);
                };
                let Some(type_name) = self
                    .bf
                    .constants
                    .get(cidx as usize)
                    .copied()
                    .filter(|v| v.is_symbol())
                    .and_then(|v| crate::reader::symbol_name(v.as_symbol_index()))
                else {
                    return Ok(false);
                };
                match type_name.rsplit(':').next().unwrap_or(&type_name) {
                    "FIXNUM" => Some(TypeBits::FIXNUM),
                    // (TYPEP x 'CONS) is the same unsound tag test as CONSP
                    // above -- a closure would answer T.  See bliss-74rl.
                    "CONS" => return Ok(false),
                    "SYMBOL" => Some(TypeBits::SYMBOL),
                    "INTEGER" => Some(TypeBits::FIXNUM.join(TypeBits::BIGNUM)),
                    _ => return Ok(false),
                }
            }
            IntrinsicId::Eq | IntrinsicId::Null => None,
            IntrinsicId::Car | IntrinsicId::Cdr => {
                unreachable!("handled as guarded field loads above")
            }
            IntrinsicId::FirstChar => unreachable!("handled as an inline body above"),
        };

        if let Some(bits) = type_bits {
            if intrinsic == IntrinsicId::TypepConstant {
                // Its ConstSymbol is now dead and the ordinary DCE pass removes it.
                stack
                    .pop()
                    .ok_or(BuildError::Unsupported("stack underflow (TYPEP type)"))?;
            }
            let x = stack
                .pop()
                .ok_or(BuildError::Unsupported("stack underflow (type predicate)"))?;
            let result = self
                .emit(
                    block,
                    Opcode::TypeCheck,
                    vec![x],
                    AuxData::TypeTag(IRType::of(bits)),
                    InstFlags::default(),
                    None,
                    IRType::TOP,
                )
                .ok_or(BuildError::Unsupported("TypeCheck has a result"))?;
            stack.push(result);
            return Ok(true);
        }

        let args = match intrinsic {
            IntrinsicId::Null => {
                let x = stack
                    .pop()
                    .ok_or(BuildError::Unsupported("stack underflow (NULL)"))?;
                vec![x, self.emit_const_nil(block)]
            }
            IntrinsicId::Eq => {
                let b = stack
                    .pop()
                    .ok_or(BuildError::Unsupported("stack underflow (EQ rhs)"))?;
                let a = stack
                    .pop()
                    .ok_or(BuildError::Unsupported("stack underflow (EQ lhs)"))?;
                vec![a, b]
            }
            IntrinsicId::Consp
            | IntrinsicId::Symbolp
            | IntrinsicId::Integerp
            | IntrinsicId::TypepConstant
            | IntrinsicId::Stringp => {
                unreachable!("handled as TypeCheck above")
            }
            IntrinsicId::Car | IntrinsicId::Cdr => {
                unreachable!("handled as guarded field loads above")
            }
            IntrinsicId::FirstChar => unreachable!("handled as an inline body above"),
        };
        let result = self
            .emit(
                block,
                Opcode::GenericEq,
                args,
                AuxData::None,
                InstFlags::default(),
                None,
                IRType::TOP,
            )
            .ok_or(BuildError::Unsupported("GenericEq has a result"))?;
        stack.push(result);
        Ok(true)
    }

    // ── Instruction emit helpers ────────────────────────────────────

    fn set_term(&mut self, block: Block, data: InstData) {
        let inst = self.f.set_terminator(block, data);
        self.terminator_inst.insert(block, inst);
    }

    /// Emit a pure/effectful non-terminator with a single result, returning it.
    #[allow(clippy::too_many_arguments)]
    fn emit(
        &mut self,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        aux: AuxData,
        flags: InstFlags,
        frame_state: Option<crate::t2::frame_state::FrameStateId>,
        ty: IRType,
    ) -> Option<Value> {
        let (_i, results) = self.f.push_inst(
            block,
            InstData {
                opcode,
                args,
                results: vec![],
                aux,
                flags,
                targets: vec![],
                frame_state,
                source_pos: 0,
            },
            &[(ty, ValueRepresentation::Tagged)],
        );
        results.into_iter().next()
    }

    /// Emit a result-less effectful instruction.
    fn emit_effect(
        &mut self,
        block: Block,
        opcode: Opcode,
        args: Vec<Value>,
        aux: AuxData,
        frame_state: Option<crate::t2::frame_state::FrameStateId>,
    ) {
        self.f.push_inst(
            block,
            InstData {
                opcode,
                args,
                results: vec![],
                aux,
                flags: runtime_call_flags(),
                targets: vec![],
                frame_state,
                source_pos: 0,
            },
            &[],
        );
    }

    fn emit_const_nil(&mut self, block: Block) -> Value {
        self.emit(
            block,
            Opcode::ConstNil,
            vec![],
            AuxData::None,
            InstFlags::default(),
            None,
            IRType::of(TypeBits::NULL),
        )
        .expect("ConstNil has a result")
    }

    /// Classify a constant-pool `EgclVal` and emit the matching `Const*`.
    fn emit_const(&mut self, block: Block, idx: u16) -> Result<Value, BuildError> {
        let slot = self
            .bf
            .constants
            .get(idx as usize)
            .ok_or(BuildError::Unsupported("constant index out of range"))?;
        let val = *slot;

        let (opcode, aux, ty) = if val.is_nil() {
            (Opcode::ConstNil, AuxData::None, IRType::of(TypeBits::NULL))
        } else if val == egcl_rt::value::T {
            (Opcode::ConstT, AuxData::None, IRType::of(TypeBits::SYMBOL))
        } else if val.is_fixnum() {
            (
                Opcode::ConstFixnum,
                AuxData::FixnumImm(val.as_fixnum()),
                IRType::of(TypeBits::FIXNUM),
            )
        } else if val.is_character() {
            (
                Opcode::ConstChar,
                AuxData::CharImm(val.as_char()),
                IRType::of(TypeBits::CHARACTER),
            )
        } else if val.is_single_float() {
            (
                Opcode::ConstFloat,
                AuxData::FloatImm(val.as_single_float()),
                IRType::of(TypeBits::SINGLE_FLOAT),
            )
        } else if val.tag() == egcl_rt::value::TAG_SYMBOL {
            (
                Opcode::ConstSymbol,
                AuxData::SymbolRef(val.as_symbol_index()),
                IRType::of(TypeBits::SYMBOL),
            )
        } else {
            // Cons / heap object / function: retain its GC-rewritten pool slot.
            (
                Opcode::ConstHeapObj,
                AuxData::HeapLiteral {
                    slot: slot as *const _ as usize,
                },
                IRType::TOP,
            )
        };

        Ok(self
            .emit(block, opcode, vec![], aux, InstFlags::default(), None, ty)
            .expect("Const* has a result"))
    }
}

/// A block's control-flow conclusion, captured during interpretation.
enum Term {
    Jump(Block),
    Transfer(InstData),
    /// `Brif(cond, taken_when_true, taken_when_false)`.
    Brif(Value, Block, Block),
    Ret(Value),
    /// Unreachable continuation with no successors: emitted after a call that
    /// never returns normally (bliss-wukf).
    Trap,
}

// ── Free helpers for terminator InstData ────────────────────────────

fn runtime_call_flags() -> InstFlags {
    InstFlags {
        effectful: true,
        call: true,
        safepoint: true,
        ..InstFlags::default()
    }
}

fn jump(target: Block) -> InstData {
    InstData {
        opcode: Opcode::Jump,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![crate::t2::ir::BlockCall {
            block: target,
            args: vec![],
        }],
        frame_state: None,
        source_pos: 0,
    }
}

fn brif(cond: Value, t: Block, f: Block) -> InstData {
    InstData {
        opcode: Opcode::Brif,
        args: vec![cond],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![
            crate::t2::ir::BlockCall {
                block: t,
                args: vec![],
            },
            crate::t2::ir::BlockCall {
                block: f,
                args: vec![],
            },
        ],
        frame_state: None,
        source_pos: 0,
    }
}

fn ret(v: Value) -> InstData {
    InstData {
        opcode: Opcode::Return,
        args: vec![v],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

fn trap() -> InstData {
    InstData {
        opcode: Opcode::Trap,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

// ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::inlining::{InlineConfig, InlineOptions, InlinePolicy};
    use crate::t2::ir::Opcode;
    use egcl_rt::value::EgclVal;
    use std::sync::Arc;

    fn bf(
        name: &str,
        code: Vec<Instr>,
        constants: Vec<EgclVal>,
        n_locals: u16,
        arity: u16,
        max_stack: u16,
    ) -> BytecodeFunction {
        BytecodeFunction {
            code,
            constants,
            load_time_values: vec![],
            handler_cases: vec![],
            handler_binds: vec![],
            names: vec![],
            restart_cases: vec![],
            nested_functions: vec![],
            param_layout: vec![],
            param_types: vec![],
            has_env: false,
            n_locals,
            max_stack,
            arity,
            name: name.to_string(),
            params_form: egcl_rt::value::NIL,
            min_args: arity,
            max_args: Some(arity),
            variadic: false,
        }
    }

    fn term_opcode(f: &Function, b: Block) -> Opcode {
        let t = f.terminator(b).expect("block terminated");
        f.inst(t).opcode
    }

    fn has_opcode(f: &Function, b: Block, op: Opcode) -> bool {
        f.block(b).insts.iter().any(|&i| f.inst(i).opcode == op)
    }

    #[test]
    fn declared_parameter_type_seeds_entry_ssa() {
        let mut typed = bf(
            "typed",
            vec![Instr::LoadLocal(0), Instr::Return],
            vec![],
            1,
            1,
            1,
        );
        typed.param_types = vec![DeclaredType::Fixnum];
        let f = build_from_bytecode(&typed).expect("build declared function");
        let parameter = f.block(f.entry()).params[0];
        assert_eq!(f.value(parameter).ty, IRType::of(TypeBits::FIXNUM));
        assert!(f.is_entry_param_checked(parameter));
    }

    #[test]
    fn returns_constant() {
        // (lambda () 42)
        let f = build_from_bytecode(&bf(
            "k",
            vec![Instr::Const(0), Instr::Return],
            vec![EgclVal::from_fixnum(42)],
            0,
            0,
            1,
        ))
        .expect("builds");

        assert_eq!(f.num_blocks(), 1);
        let entry = f.entry();
        assert!(has_opcode(&f, entry, Opcode::ConstFixnum));
        assert_eq!(term_opcode(&f, entry), Opcode::Return);
        // The returned value is the const's result.
        let t = f.terminator(entry).unwrap();
        assert_eq!(f.inst(t).args.len(), 1);
    }

    #[test]
    fn if_merge_has_join_parameter() {
        // (lambda (c) (if c 10 20))
        //  0: LoadLocal 0     ; c
        //  1: BrIfFalse 4     ; false -> else
        //  2: Const 0 (=10)   ; then
        //  3: Br 5
        //  4: Const 1 (=20)   ; else
        //  5: Return          ; merge
        let f = build_from_bytecode(&bf(
            "iff",
            vec![
                Instr::LoadLocal(0),
                Instr::BrIfFalse(4),
                Instr::Const(0),
                Instr::Br(5),
                Instr::Const(1),
                Instr::Return,
            ],
            vec![EgclVal::from_fixnum(10), EgclVal::from_fixnum(20)],
            1,
            1,
            2,
        ))
        .expect("builds");

        // entry(0), then(2), else(4), merge(5).
        assert_eq!(f.num_blocks(), 4);
        let order = f.block_order().to_vec();
        let entry = order[0];
        let merge = order[3];

        // The entry ends in a conditional branch with two successors.
        assert_eq!(term_opcode(&f, entry), Opcode::Brif);
        assert_eq!(f.succs(entry).len(), 2);

        // The merge block joins the two arms via exactly one block parameter
        // (the merged result value), and returns it.
        assert_eq!(f.preds(merge).len(), 2);
        assert_eq!(
            f.block(merge).params.len(),
            1,
            "merge should have a single phi parameter for the joined value"
        );
        assert_eq!(term_opcode(&f, merge), Opcode::Return);

        // Both incoming edges to the merge carry exactly one argument.
        for pred in f.preds(merge) {
            let t = f.terminator(pred).unwrap();
            let call = f.inst(t).targets.iter().find(|c| c.block == merge).unwrap();
            assert_eq!(
                call.args.len(),
                1,
                "edge into merge passes the joined value"
            );
        }
    }

    #[test]
    fn invoke_conversion_preserves_bytecode_call_state_and_live_cleanup_input() {
        let mut f = build_from_bytecode(&bf(
            "invoke-conversion",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: 123456,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            2,
        ))
        .expect("build ordinary call");
        let entry = f.entry();
        let call = f
            .block(entry)
            .insts
            .iter()
            .copied()
            .find(|&i| f.inst(i).opcode == Opcode::Call)
            .unwrap();
        let before = f.inst(call).frame_state;
        let cleanup = f.make_block();
        let saved = f.add_block_param(cleanup, IRType::TOP, ValueRepresentation::Tagged);
        let live = f.inst(call).args[0];
        f.set_terminator(
            cleanup,
            InstData {
                opcode: Opcode::Return,
                args: vec![saved],
                results: vec![],
                aux: AuxData::None,
                flags: InstFlags::default(),
                targets: vec![],
                frame_state: None,
                source_pos: 0,
            },
        );
        let normal = f
            .make_call_exceptional(
                call,
                crate::t2::ir::BlockCall {
                    block: cleanup,
                    args: vec![live],
                },
            )
            .expect("split call and project result");
        assert_eq!(f.inst(call).opcode, Opcode::Invoke);
        assert_eq!(f.inst(call).frame_state, before);
        assert_eq!(f.succs(entry), vec![normal, cleanup]);
        assert_eq!(f.inst(call).targets[1].args, vec![live]);
        assert!(
            crate::t2::verify::verify(&f).is_ok(),
            "{:?}",
            crate::t2::verify::verify(&f)
        );
    }

    #[test]
    fn frame_states_name_only_locals_the_interpreter_can_still_read() {
        // Local 0 is the parameter; local 1 is a let-bound temporary that is
        // written at 3 and read once at 4. Every frame state after that read
        // must leave slot 1 Unbound, and the last one (nothing is read after
        // bcp 8) must leave slot 0 Unbound too (bliss-dfilp, bliss-enisp).
        //  0: LoadLocal 0
        //  1: Const 0            ; 1
        //  2: CallNamed + 2      ; state@2: l0 live (read at 7), l1 dead
        //  3: StoreLocal 1
        //  4: LoadLocal 1
        //  5: Const 0
        //  6: CallNamed + 2      ; state@6: l0 live, l1 dead
        //  7: LoadLocal 0
        //  8: CallNamed + 2      ; state@8: both dead
        //  9: Return
        let plus = egcl_rt::symbols::intern("+");
        let f = build_from_bytecode(&bf(
            "dead-locals",
            vec![
                Instr::LoadLocal(0),
                Instr::Const(0),
                Instr::CallNamed {
                    sym: plus,
                    nargs: 2,
                },
                Instr::StoreLocal(1),
                Instr::LoadLocal(1),
                Instr::Const(0),
                Instr::CallNamed {
                    sym: plus,
                    nargs: 2,
                },
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: plus,
                    nargs: 2,
                },
                Instr::Return,
            ],
            vec![EgclVal::from_fixnum(1)],
            2,
            1,
            3,
        ))
        .expect("builds");

        let live_slots = |bcp: u32| -> Vec<bool> {
            let (_, fs) = f
                .frame_states
                .iter()
                .find(|(_, fs)| fs.scopes.len() == 1 && fs.scopes[0].bcp == bcp)
                .unwrap_or_else(|| panic!("no frame state at bcp {bcp}"));
            fs.scopes[0]
                .locals
                .iter()
                .map(|src| matches!(src, ValueSource::Value { .. }))
                .collect()
        };
        assert_eq!(live_slots(2), vec![true, false], "state at the first call");
        assert_eq!(
            live_slots(6),
            vec![true, false],
            "state after l1's last read"
        );
        assert_eq!(live_slots(8), vec![false, false], "state at the last call");
        assert!(
            crate::t2::verify::verify(&f).is_ok(),
            "{:?}",
            crate::t2::verify::verify(&f)
        );
    }

    #[test]
    fn counted_loop_header_has_parameter() {
        // A loop whose header carries local 0 across the back edge:
        //  0: Const 0            ; A
        //  1: StoreLocal 0       ; l0 = A
        //  2: LoadLocal 0        ; header
        //  3: BrIfFalse 7        ; exit when false
        //  4: Const 1            ; body: B
        //  5: StoreLocal 0       ; l0 = B
        //  6: Br 2               ; back-edge
        //  7: LoadLocal 0        ; exit
        //  8: Return
        let f = build_from_bytecode(&bf(
            "loop",
            vec![
                Instr::Const(0),
                Instr::StoreLocal(0),
                Instr::LoadLocal(0),
                Instr::BrIfFalse(7),
                Instr::Const(1),
                Instr::StoreLocal(0),
                Instr::Br(2),
                Instr::LoadLocal(0),
                Instr::Return,
            ],
            vec![EgclVal::from_fixnum(3), EgclVal::from_fixnum(0)],
            1,
            0,
            1,
        ))
        .expect("builds");

        // entry(0), header(2), body(4), exit(7).
        assert_eq!(f.num_blocks(), 4);
        let order = f.block_order().to_vec();
        let header = order[1];
        let body = order[2];

        // The header is a real loop header: two predecessors (entry + back-edge).
        let preds = f.preds(header);
        assert_eq!(preds.len(), 2, "header has forward + back edge");
        assert!(preds.contains(&body), "body is the back-edge predecessor");

        // The loop-carried local became a header block parameter (an SSA phi).
        assert!(
            !f.block(header).params.is_empty(),
            "header must carry a loop-carried parameter"
        );
        assert_eq!(term_opcode(&f, header), Opcode::Brif);

        // The back-edge is a Jump to the header passing one argument per header
        // parameter.
        let bt = f.terminator(body).unwrap();
        assert_eq!(f.inst(bt).opcode, Opcode::Jump);
        let back = f
            .inst(bt)
            .targets
            .iter()
            .find(|c| c.block == header)
            .unwrap();
        assert_eq!(
            back.args.len(),
            f.block(header).params.len(),
            "back-edge args match header params"
        );
    }

    #[test]
    fn call_carries_frame_state() {
        // (lambda (x) (foo x)) — one named call, which must anchor a FrameState.
        // Use a freshly-interned, test-private callee so the inline pass can
        // never resolve it to an inlinable body and delete the Call. A hardcoded
        // symbol index is not a stable "unknown function": whether it names an
        // inlinable/defined function depends on the process-global symbol
        // registry + function cells other parallel tests mutate, which made this
        // test flaky (bliss-dck).
        let foo = egcl_rt::symbols::intern("T2-BUILD-FRAME-STATE-UNDEFINED-CALLEE");
        let f = build_from_bytecode(&bf(
            "call",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: foo, nargs: 1 },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            2,
        ))
        .expect("builds");

        let entry = f.entry();
        let call = f
            .block(entry)
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("has a call");
        assert!(call.frame_state.is_some(), "call must carry a FrameState");
        assert_eq!(call.args.len(), 1, "one argument popped for the call");
        assert!(!f.frame_states.is_empty(), "frame-state table populated");
    }

    #[test]
    fn notinline_keeps_known_function_as_a_call() {
        let eq = egcl_rt::symbols::intern("EQ");
        let input = bf(
            "notinline-eq",
            vec![
                Instr::LoadLocal(0),
                Instr::LoadLocal(1),
                Instr::CallNamed { sym: eq, nargs: 2 },
                Instr::Return,
            ],
            vec![],
            2,
            2,
            2,
        );
        let options = InlineOptions::default().with_policy(2, InlinePolicy::NotInline);
        let f = build_from_bytecode_with_inline_options(&input, options).expect("builds");
        let call = f
            .block(f.entry())
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("NOTINLINE must retain the call");
        assert!(
            call.frame_state.is_some(),
            "retained call keeps precise deopt state"
        );
        assert!(!has_opcode(&f, f.entry(), Opcode::GenericEq));
    }

    #[test]
    fn budget_and_depth_limits_keep_known_function_as_a_call() {
        let null = egcl_rt::symbols::intern("NULL");
        let input = bf(
            "limited-null",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: null,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );

        let mut no_budget = InlineOptions::default();
        no_budget.config.node_budget = 0;
        let f = build_from_bytecode_with_inline_options(&input, no_budget).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));

        let mut no_depth = InlineOptions::default();
        no_depth.config = InlineConfig {
            max_depth: 0,
            ..InlineConfig::default()
        };
        let f = build_from_bytecode_with_inline_options(&input, no_depth).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));
    }

    #[test]
    fn inline_policy_overrides_profitability_threshold() {
        let null = egcl_rt::symbols::intern("NULL");
        let input = bf(
            "explicit-inline-null",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: null,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );

        let mut default_policy = InlineOptions::default();
        default_policy.config.small_threshold = 0;
        let f = build_from_bytecode_with_inline_options(&input, default_policy).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::Call));

        let mut explicit_inline = InlineOptions::default().with_policy(1, InlinePolicy::Inline);
        explicit_inline.config.small_threshold = 0;
        let f = build_from_bytecode_with_inline_options(&input, explicit_inline).expect("builds");
        assert!(has_opcode(&f, f.entry(), Opcode::GenericEq));
        assert!(!has_opcode(&f, f.entry(), Opcode::Call));
    }

    #[test]
    fn unsupported_typep_shape_keeps_call_and_frame_state() {
        let typep = egcl_rt::symbols::intern("TYPEP");
        let input = bf(
            "dynamic-typep",
            vec![
                Instr::LoadLocal(0),
                Instr::LoadLocal(1),
                Instr::CallNamed {
                    sym: typep,
                    nargs: 2,
                },
                Instr::Return,
            ],
            vec![],
            2,
            2,
            2,
        );
        let f = build_from_bytecode(&input).expect("builds");
        let call = f
            .block(f.entry())
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::Call)
            .expect("dynamic TYPEP must stay a call");
        assert!(call.frame_state.is_some());
    }

    #[test]
    fn bytecode_typep_boolean_keeps_its_single_value_effect() {
        let input = bf(
            "boolean-typep-opcode",
            vec![
                Instr::LoadLocal(0),
                Instr::TypeP(typep_class::BOOLEAN),
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );

        let f = build_from_bytecode(&input).expect("TypeP bytecode must reach T2");
        let check = f
            .block(f.entry())
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::TypeCheck)
            .expect("TypeP must become a TypeCheck");
        assert!(!check.flags.effectful);
        assert!(!check.flags.guard);
        assert!(check.frame_state.is_none());
        let clear = f
            .block(f.entry())
            .insts
            .iter()
            .map(|&i| f.inst(i))
            .find(|d| d.opcode == Opcode::ClearMv)
            .expect("TypeP must clear stale multiple values even if its check is optimized away");
        assert!(clear.flags.effectful);
        assert!(clear.frame_state.is_some());
    }

    #[test]
    fn unsupported_opcode_is_reported_and_names_the_instruction() {
        // `Throw` is a deferred opcode → the builder must decline, not panic.
        let r = build_from_bytecode(&bf(
            "nlx",
            vec![Instr::Throw, Instr::Return],
            vec![],
            0,
            0,
            2,
        ));
        // Naming the instruction is the point: a bare "not modelled" message
        // gives no way to tell WHICH instruction kept a function at T1, and a
        // tier loss is silent — correct output, just slower.
        match r {
            Err(BuildError::UnsupportedInstr(kind)) => assert_eq!(kind, "Throw"),
            other => panic!("expected UnsupportedInstr(\"Throw\"), got {other:?}"),
        }
    }

    #[test]
    fn saved_body_clones_cfg_binds_arguments_and_merges_returns() {
        let helper = egcl_rt::symbols::intern("BODY-INLINE-HELPER");
        let caller = egcl_rt::symbols::intern("BODY-INLINE-CALLER");
        let body = Arc::new(bf(
            "BODY-INLINE-HELPER",
            vec![
                Instr::LoadLocal(0),
                Instr::BrIfFalse(4),
                Instr::Const(0),
                Instr::Return,
                Instr::Const(1),
                Instr::Return,
            ],
            vec![egcl_rt::value::T, egcl_rt::value::NIL],
            1,
            1,
            1,
        ));
        let input = bf(
            "BODY-INLINE-CALLER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let options = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(helper, body);
        let f = build_from_bytecode_with_inline_options(&input, options).expect("builds");
        assert!(f.block_order().iter().all(|&b| {
            f.block(b)
                .insts
                .iter()
                .all(|&i| f.inst(i).opcode != Opcode::Call)
        }));
        assert!(
            f.num_blocks() >= 5,
            "callee diamond and continuation were cloned"
        );
        crate::t2::verify::verify(&f).expect("cloned CFG verifies");
    }

    #[test]
    fn cloned_guard_has_nested_scopes_and_chained_source_position() {
        let helper = egcl_rt::symbols::intern("BODY-INLINE-FIRST-CHAR");
        let caller = egcl_rt::symbols::intern("BODY-INLINE-FIRST-CHAR-CALLER");
        let first_char = egcl_rt::symbols::intern("UIOP/UTILITY:FIRST-CHAR");
        let body = Arc::new(bf(
            "BODY-INLINE-FIRST-CHAR",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: first_char,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        ));
        let input = bf(
            "BODY-INLINE-FIRST-CHAR-CALLER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let options = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(helper, body);
        let f = build_from_bytecode_with_inline_options(&input, options).expect("builds");
        let guard = f
            .block_order()
            .iter()
            .flat_map(|&b| f.block(b).insts.iter().copied())
            .find(|&i| f.inst(i).flags.guard)
            .expect("FIRST-CHAR guard cloned");
        let data = f.inst(guard);
        let scopes = &f.frame_states.get(data.frame_state.unwrap()).scopes;
        assert_eq!(scopes.len(), 2);
        assert_eq!(scopes[0].function, caller);
        assert_eq!(scopes[1].function, helper);
        assert!(f.source_positions[data.source_pos as usize]
            .inlined_at
            .is_some());
        crate::t2::verify::verify(&f).expect("nested metadata verifies");
    }

    #[test]
    fn hot_profile_inlines_body_above_small_threshold_but_hard_limits_win() {
        let helper = egcl_rt::symbols::intern("PROFILED-LARGE-INLINE-HELPER");
        let caller = egcl_rt::symbols::intern("PROFILED-LARGE-INLINE-CALLER");
        let mut leaf_code = Vec::new();
        for _ in 0..16 {
            leaf_code.push(Instr::LoadLocal(0));
            leaf_code.push(Instr::Pop);
        }
        leaf_code.extend([Instr::LoadLocal(0), Instr::Return]);
        let leaf = Arc::new(bf(
            "PROFILED-LARGE-INLINE-HELPER",
            leaf_code,
            vec![],
            1,
            1,
            1,
        ));
        let input = bf(
            "PROFILED-LARGE-INLINE-CALLER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let base = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(helper, leaf);

        let has_call = |f: &Function| {
            f.block_order()
                .iter()
                .any(|&block| has_opcode(f, block, Opcode::Call))
        };

        let cold = build_from_bytecode_with_inline_options(&input, base.clone()).unwrap();
        assert!(has_call(&cold), "an unprofiled large body stays a call");

        let below = base.clone().with_call_site_profile(caller, 1, 7, 10);
        let below = build_from_bytecode_with_inline_options(&input, below).unwrap();
        assert!(
            has_call(&below),
            "70% is below the default 80% hot threshold"
        );

        let hot = base.clone().with_call_site_profile(caller, 1, 8, 10);
        let hot = build_from_bytecode_with_inline_options(&input, hot).unwrap();
        assert!(
            !has_call(&hot),
            "an 80% site may inline a larger eligible body"
        );
        crate::t2::verify::verify(&hot).expect("profile-guided inline verifies");

        let mut no_budget = base.clone().with_call_site_profile(caller, 1, 10, 10);
        no_budget.config.node_budget = 33;
        let no_budget = build_from_bytecode_with_inline_options(&input, no_budget).unwrap();
        assert!(
            has_call(&no_budget),
            "hotness must not override the growth budget"
        );

        let notinline = base
            .with_call_site_profile(caller, 1, 10, 10)
            .with_policy(1, InlinePolicy::NotInline);
        let notinline = build_from_bytecode_with_inline_options(&input, notinline).unwrap();
        assert!(has_call(&notinline), "NOTINLINE must override hotness");
    }

    #[test]
    fn body_limits_recursion_and_notinline_retain_calls() {
        let helper = egcl_rt::symbols::intern("BODY-INLINE-LIMITED");
        let caller = egcl_rt::symbols::intern("BODY-INLINE-LIMITED-CALLER");
        let leaf = Arc::new(bf(
            "BODY-INLINE-LIMITED",
            vec![Instr::LoadLocal(0), Instr::Return],
            vec![],
            1,
            1,
            1,
        ));
        let input = bf(
            "BODY-INLINE-LIMITED-CALLER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let base = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(helper, Arc::clone(&leaf));

        let mut budget = base.clone();
        budget.config.node_budget = 0;
        let f = build_from_bytecode_with_inline_options(&input, budget).unwrap();
        assert!(f
            .block_order()
            .iter()
            .any(|&b| has_opcode(&f, b, Opcode::Call)));

        let notinline = base.clone().with_policy(1, InlinePolicy::NotInline);
        let f = build_from_bytecode_with_inline_options(&input, notinline).unwrap();
        assert!(f
            .block_order()
            .iter()
            .any(|&b| has_opcode(&f, b, Opcode::Call)));

        let recursive = Arc::new(bf(
            "BODY-INLINE-LIMITED",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: helper,
                    nargs: 1,
                },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        ));
        let recursive_options = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(helper, recursive);
        let f = build_from_bytecode_with_inline_options(&input, recursive_options).unwrap();
        assert!(f
            .block_order()
            .iter()
            .any(|&b| has_opcode(&f, b, Opcode::Call)));
    }
}
