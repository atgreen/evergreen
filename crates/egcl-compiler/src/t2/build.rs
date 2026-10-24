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

use crate::control_scope::{ScopeError, ScopeKind, ScopeMap};
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
    legalize_transfer_calls(&mut f, &scopes)?;
    Ok(f)
}

/// Build scope-aware SSA while retaining ordinary calls for numeric speculation.
/// The caller must finish optimization before `legalize_transfer_calls` and
/// must not install this intermediate form as a native-transfer entry.
pub fn build_for_transfer_optimization(
    bf: &BytecodeFunction,
    symbol: u32,
) -> Result<Function, BuildError> {
    let mut builder = Builder::new(bf, InlineOptions::default().with_root_symbol(symbol));
    builder.transfer_mode = true;
    builder.run()
}

/// Build native registered-scope edges before SSA sealing while preserving
/// the root definition identity used by optimization and deoptimization.
pub fn build_for_native_scope_optimization(
    bf: &BytecodeFunction,
    symbol: u32,
) -> Result<Function, BuildError> {
    let mut builder = Builder::new(bf, InlineOptions::default().with_root_symbol(symbol));
    builder.transfer_mode = true;
    builder.native_cleanups = true;
    builder.run()
}

/// Give residual calls and runtime symbol access explicit exceptional successors
/// after optimization. The original ordinary candidate can retain its semantic
/// operations; this mapped clone preserves their MV-transparent helper protocol.
/// The supplied scope map must belong to the retained bytecode definition.
pub fn legalize_transfer_calls(f: &mut Function, scopes: &ScopeMap) -> Result<(), BuildError> {
    let calls: Vec<Inst> = f
        .block_order()
        .iter()
        .flat_map(|&b| f.block(b).insts.iter().copied())
        .filter(|&i| matches!(f.inst(i).opcode,
            Opcode::Call | Opcode::SymbolValue | Opcode::SetSymbolValue | Opcode::SymbolFunction))
        .collect();
    for call in calls {
        let operation = f.inst(call).opcode;
        if operation != Opcode::Call {
            let AuxData::SymbolRef(symbol) = f.inst(call).aux else {
                return Err(BuildError::Unsupported("symbol operation without symbol identity"));
            };
            let data = f.inst_mut(call);
            data.opcode = Opcode::Call;
            data.aux = match operation {
                Opcode::SymbolValue => AuxData::GlobalValue { symbol, store: false },
                Opcode::SetSymbolValue => AuxData::GlobalValue { symbol, store: true },
                Opcode::SymbolFunction => AuxData::FunctionLookup(symbol),
                _ => unreachable!(),
            };
        }
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
    Ok(())
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
        let self_expansion = cfg!(all(target_arch = "x86_64", target_os = "linux"))
            && options.config.self_inline_depth != 0
            && Some(symbol) == options.root_symbol()
            && Some(symbol) == current_symbol
            && std::ptr::eq(bf, body.as_ref())
            && bf.self_call_sites.binary_search(&current_scope.bcp).is_ok();
        let policy = if root_policies || self_expansion {
            options.policy_at(current_scope.bcp)
        } else {
            InlinePolicy::Unspecified
        };
        let hot = current_symbol
            .is_some_and(|caller| options.call_site_is_hot(caller, current_scope.bcp));

        if policy == InlinePolicy::NotInline
            || nargs != body.arity
            || depth >= options.config.max_depth
            || (active.contains(&symbol) && !self_expansion)
            || (self_expansion && depth >= options.config.self_inline_depth)
        {
            continue;
        }
        let cost = if self_expansion {
            self_inline_cost(&body)
        } else {
            let mut eligibility_stack = active.clone();
            body_cost(symbol, options, &mut eligibility_stack).ok()
        };
        let Some(cost) = cost else {
            continue;
        };
        if cost > *remaining_budget
            || (cost > options.config.small_threshold && !hot && policy != InlinePolicy::Inline)
        {
            continue;
        }

        let callee = if self_expansion {
            // Charge this copy before recursively expanding it. All sibling
            // self sites consume the same budget, rather than each receiving
            // an independent copy of the available growth allowance.
            *remaining_budget -= cost;
            let callee = build_with_saved_bodies(
                &body, options, Some(symbol), depth + 1, remaining_budget, active, false,
            )?;
            check_self_inline_arguments(&mut f, call, &body)?;
            callee
        } else {
            active.insert(symbol);
            let mut callee_budget = *remaining_budget;
            let callee = build_with_saved_bodies(
                &body, options, Some(symbol), depth + 1, &mut callee_budget, active, false,
            )?;
            active.remove(&symbol);
            // Every cloned FrameState is prefixed with the suspended caller,
            // so a callee deopt point would need two logical frames. Keep the
            // call when the runtime cannot rebuild them (bliss-6988n).
            if !options.config.cross_function_recovery && has_deopt_point(&callee) {
                continue;
            }
            *remaining_budget = remaining_budget.saturating_sub(cost);
            callee
        };

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
    }
    Ok(f)
}

/// Whether any reachable instruction can deoptimize, i.e. carries a FrameState
/// that `Function::inline_call` would clone with the caller's scopes prefixed.
fn has_deopt_point(f: &Function) -> bool {
    f.reverse_postorder().iter().any(|&block| {
        f.block(block).insts.iter().any(|&inst| f.inst(inst).frame_state.is_some())
    })
}

/// Local, fixed-argument bodies need no captured or dynamic scope reconstruction.
/// Named calls remain ordinary calls unless their own existing inline policy
/// admits them; cloning preserves their order and pre-call recovery states.
fn self_inline_cost(body: &BytecodeFunction) -> Option<u32> {
    if body.has_env || body.variadic
        || body.min_args != body.arity || body.max_args != Some(body.arity)
        || body.param_layout.len() != usize::from(body.arity)
        || body.param_layout.iter().enumerate().any(|(index, (_, location))|
            !matches!(location, VarLoc::Slot(slot) if usize::from(*slot) == index))
        || body.param_types.len() > usize::from(body.arity)
        || body.param_types.iter().any(|ty| matches!(ty, DeclaredType::SingleFloat))
        || body.code.iter().any(|op| !matches!(op,
            Instr::Const(_) | Instr::LoadLocal(_) | Instr::StoreLocal(_)
            | Instr::Pop | Instr::Dup | Instr::Br(_) | Instr::BrIfFalse(_)
            | Instr::BrIfTrue(_) | Instr::CallNamed { .. } | Instr::Return
            // A tested call's value-clearing marker (#419). It only drops
            // values and T2 removes it after a single-valued test, so it is not
            // charged. `body_cost` still refuses it: admitting it there lets
            // cross-function bodies inline into callers whose multi-frame
            // recovery the runtime does not yet admit (bliss-5ydhx).
            | Instr::ClearMv))
    {
        return None;
    }
    let charged = body.code.iter().filter(|op| !matches!(op, Instr::ClearMv)).count();
    u32::try_from(charged).ok()?.checked_add(
        u32::try_from(body.param_types.iter().filter(|ty| !ty.is_any()).count()).ok()?,
    )
}

/// An erased call must still establish the declared callee-entry facts. Failure
/// resumes the original call with its arguments still present. Before enabling
/// this producer, the embedding runtime must preserve retained self-call identity
/// on that recovery path and validate the arguments before any callee effect.
fn check_self_inline_arguments(
    f: &mut Function,
    call: Inst,
    body: &BytecodeFunction,
) -> Result<(), BuildError> {
    let data = f.inst(call).clone();
    let (block, mut position) = f.block_order().iter().find_map(|&block| {
        f.block(block).insts.iter().position(|&inst| inst == call).map(|at| (block, at))
    }).ok_or(BuildError::Unsupported("self-inline call is absent from layout"))?;
    for (index, declaration) in body.param_types.iter().enumerate() {
        let checked_type = match declaration {
            DeclaredType::Any => continue,
            DeclaredType::Fixnum => IRType::of(TypeBits::FIXNUM),
            DeclaredType::FixnumRange { lower, upper } => IRType {
                bits: TypeBits::FIXNUM,
                range: Some(crate::t2::ir::Range { lo: *lower, hi: *upper }),
                class_id: None,
            },
            DeclaredType::SingleFloat => return Err(BuildError::Unsupported("self-inline float argument")),
        };
        let (guard, values) = f.push_inst(block, InstData {
            opcode: Opcode::Guard,
            args: vec![data.args[index]],
            results: vec![],
            aux: AuxData::TypeTag(checked_type),
            flags: InstFlags { effectful: true, guard: true, ..Default::default() },
            targets: vec![], frame_state: data.frame_state, source_pos: data.source_pos,
        }, &[(checked_type, ValueRepresentation::Tagged)]);
        let layout = &mut f.block_mut(block).insts;
        let appended = layout.pop();
        debug_assert_eq!(appended, Some(guard));
        layout.insert(position, guard);
        position += 1;
        f.inst_mut(call).args[index] = values[0];
    }
    Ok(())
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
    /// `CHAR`/`SCHAR`/`ELT` calls that receive an inline simple-string path
    /// once the CFG is complete (`string_char::admit_string_char_calls`).
    string_char_calls: HashSet<Inst>,
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

    fn special_or_global_access(&self, bcp: usize) -> Option<AuxData> {
        if !self.native_cleanups { return None; }
        match self.bf.code.get(bcp)? {
            Instr::BindSpecial(_) => Some(AuxData::SpecialBinding { bcp: bcp as u32, enter: true }),
            Instr::UnbindSpecial(_) => Some(AuxData::SpecialBinding { bcp: bcp as u32, enter: false }),
            Instr::LoadGlobal(symbol) => Some(AuxData::GlobalValue { symbol: *symbol, store: false }),
            Instr::StoreGlobal(symbol) => Some(AuxData::GlobalValue { symbol: *symbol, store: true }),
            // Captured variables live in the activation's heap EnvFrame, which
            // only the runtime owns; it decodes the instruction at this PC.
            Instr::LoadEnvVar(index) | Instr::StoreEnvVar(index) | Instr::DefineEnvVar(index) => {
                self.bf.names.get(*index as usize)?;
                let value = !matches!(self.bf.code[bcp], Instr::LoadEnvVar(_));
                Some(AuxData::LexicalEnvironment { bcp: bcp as u32, value, result: !value })
            }
            Instr::PushEnvChild | Instr::PopEnvChild => {
                Some(AuxData::LexicalEnvironment { bcp: bcp as u32, value: false, result: false })
            }
            Instr::MakeClosure { func, capture_env: true } => {
                self.bf.nested_functions.get(*func as usize)?;
                Some(AuxData::LexicalEnvironment { bcp: bcp as u32, value: false, result: true })
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
            string_char_calls: HashSet::new(),
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
        admit_nil_list_loads(&mut self.f);
        super::string_char::admit_string_char_calls(&mut self.f, &self.string_char_calls);
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
            let Some(control_scopes) = self.control_scopes.as_ref()
                .and_then(|scopes| scopes.before(bcp)).map(<[_]>::to_vec)
            else {
                continue; // no entry may advertise an unknown control state
            };
            let frame_state = self.build_frame_state(block, &[], bcp);
            self.f.osr_entries.push(crate::t2::ir::OsrEntry {
                control_scopes,
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
                || self.special_or_global_access(i).is_some()
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
                | Instr::ValuesToList
                | Instr::AllocCons
                | Instr::Throw
                | Instr::EvalHost(_)
                | Instr::LoadFunction(_)
                | Instr::MakeClosure {
                    capture_env: false, ..
                } if self.native_cleanups => {
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
                    || self.special_or_global_access(i).is_some()
                    || self.restart_case_transition(i).is_some()
                    || matches!(
                        code[i],
                        Instr::CallNamed { .. }
                            | Instr::EvalHost(_)
                            | Instr::LoadFunction(_)
                            | Instr::MakeClosure {
                                capture_env: false,
                                ..
                            }
                            | Instr::SetValues(_)
                            | Instr::ValuesToList
                            | Instr::AllocCons
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
                Instr::EvalHost(_)
                | Instr::MakeClosure {
                    capture_env: false, ..
                } if self.native_cleanups => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                // Same stack effect without the mapped protocol: a checked-ABI
                // target lowers this to a plain adapter call (see the
                // `AuxData::NestedClosure` arm below), so the depth walk must
                // accept it rather than refuse the whole function.
                Instr::MakeClosure {
                    capture_env: false, ..
                } => {
                    push(i + 1, d + 1, &mut depth_at, &mut work); // -> closure
                }
                Instr::BindSpecial(_) if self.native_cleanups => {
                    if d < 1 { return Err(BuildError::Unsupported("special binding without value")); }
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::DefineEnvVar(_) if self.native_cleanups => {
                    if d < 1 { return Err(BuildError::Unsupported("environment binding without value")); }
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::PushEnvChild | Instr::PopEnvChild if self.native_cleanups => {
                    push(i + 1, d, &mut depth_at, &mut work);
                }
                Instr::MakeClosure { capture_env: true, .. } if self.native_cleanups => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::UnbindSpecial(_) if self.native_cleanups => {
                    push(i + 1, d, &mut depth_at, &mut work);
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
                Instr::TypeP(_) | Instr::CheckResult(_) => {
                    push(i + 1, d, &mut depth_at, &mut work);
                }
                Instr::MemoryFence(_) => {
                    push(i + 1, d + 1, &mut depth_at, &mut work);
                }
                Instr::StoreLocal(_) | Instr::StoreGlobal(_) | Instr::StoreEnvVar(_) | Instr::Pop => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::Throw if self.native_cleanups => {}
                Instr::CallNamed { nargs, .. } => {
                    push(i + 1, d - (*nargs as i32) + 1, &mut depth_at, &mut work);
                }
                Instr::SetValues(n) => {
                    push(i + 1, d - (*n as i32) + 1, &mut depth_at, &mut work);
                }
                Instr::TakeValuesToLocals { .. } => {
                    push(i + 1, d - 1, &mut depth_at, &mut work);
                }
                Instr::ValuesToList => {
                    push(i + 1, d, &mut depth_at, &mut work); // primary -> list
                }
                Instr::AllocCons => {
                    push(i + 1, d - 1, &mut depth_at, &mut work); // car, cdr -> cons
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
                    || self.special_or_global_access(i).is_some()
                    || self.restart_case_transition(i).is_some()
                    || matches!(
                        code[i],
                        Instr::CallNamed { .. }
                            | Instr::EvalHost(_)
                            | Instr::LoadFunction(_)
                            | Instr::MakeClosure {
                                capture_env: false,
                                ..
                            }
                            | Instr::SetValues(_)
                            | Instr::ValuesToList
                            | Instr::AllocCons
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
                || self.special_or_global_access(start + offset).is_some()
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
                | Instr::ValuesToList
                | Instr::AllocCons
                | Instr::Throw
                | Instr::EvalHost(_)
                | Instr::LoadFunction(_)
                | Instr::MakeClosure {
                    capture_env: false, ..
                } if self.native_cleanups => {
                    let mut successors = Vec::new();
                    for target in self.exceptional_handlers((start + offset) as u32) {
                        successors.push(blk(target.body_bcp as usize)?);
                    }
                    for (_, resume, _) in self.exceptional_catches((start + offset) as u32) {
                        successors.push(blk(resume as usize)?);
                    }
                    if !matches!(instr, Instr::Throw) {
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
                DeclaredType::FixnumRange { lower, upper } => IRType {
                    range: Some(crate::t2::ir::Range { lo: lower, hi: upper }),
                    ..IRType::of(TypeBits::FIXNUM)
                },
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
            if let Some(aux) = self.special_or_global_access(i) {
                // The cold state is BEFORE consuming a binding/store operand.
                // The runtime commits only after all fallible preparation.
                let fs = self.build_frame_state(block, &stack, i as u32);
                let args = if matches!(aux, AuxData::SpecialBinding { enter: true, .. }
                    | AuxData::GlobalValue { store: true, .. }
                    | AuxData::LexicalEnvironment { value: true, .. }) {
                    vec![stack.pop().ok_or(BuildError::Unsupported("special operation without value"))?]
                } else { vec![] };
                let (inst, results) = self.f.push_inst(block, InstData {
                    opcode: Opcode::Call, args, results: vec![], aux,
                    flags: runtime_call_flags(), targets: vec![], frame_state: Some(fs), source_pos: 0,
                }, &[(IRType::TOP, ValueRepresentation::Tagged)]);
                self.finish_native_invoke(block, inst, stack, results[0], i as u32, Some(end))?;
                return Ok(());
            }
            if self.native_cleanups
                && matches!(
                    instruction,
                    Instr::EvalHost(_)
                        | Instr::LoadFunction(_)
                        | Instr::MakeClosure {
                            capture_env: false,
                            ..
                        }
                )
            {
                let aux = match instruction {
                    Instr::EvalHost(index) => {
                        if self.bf.constants.get(*index as usize).is_none() {
                            return Err(BuildError::Unsupported("invalid host form constant"));
                        }
                        AuxData::HostEval(*index)
                    }
                    Instr::LoadFunction(symbol) => AuxData::FunctionLookup(*symbol),
                    Instr::MakeClosure { func, .. } => {
                        if self.bf.nested_functions.get(*func as usize).is_none() {
                            return Err(BuildError::Unsupported("invalid nested function"));
                        }
                        AuxData::NestedClosure(*func)
                    }
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
                    let fs = (*nvars != 1).then(|| self.build_frame_state(block, &stack, i as u32));
                    let primary = stack.pop().ok_or(BuildError::Unsupported(
                        "stack underflow (TakeValuesToLocals)",
                    ))?;
                    if slot_base.saturating_add(*nvars) > self.bf.n_locals {
                        return Err(BuildError::Unsupported(
                            "TakeValuesToLocals destination out of range",
                        ));
                    }
                    // A one-variable binder (SETF's store value, as PUSH and
                    // DECF expand) stores the primary and leaves the MV state
                    // alone, exactly as T0 runs it: a plain local write, which
                    // keeps such bodies eligible for the ordinary entry
                    // (bliss-z4dlm).
                    if *nvars == 1 {
                        self.write_var(Var::Local(*slot_base), block, primary);
                        continue;
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
                            frame_state: fs,
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
                Instr::AllocCons => {
                    // Backquote templates build their conses with AllocCons;
                    // plain (cons a b) is already a CallNamed. Lower it as
                    // that same call, like SetValues -> VALUES, so the cons
                    // allocates through a rooted, safepointed runtime call
                    // and the cold state keeps car and cdr on the stack
                    // (bliss-elwvl).
                    if stack.len() < 2 {
                        return Err(BuildError::Unsupported("stack underflow (AllocCons)"));
                    }
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let split = stack.len() - 2;
                    let args: Vec<Value> = stack.split_off(split);
                    let cons_sym = egcl_rt::symbols::intern("CONS");
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args,
                            results: vec![],
                            aux: AuxData::CallTarget(cons_sym),
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
                            results[0],
                            i as u32,
                            Some(end),
                        )?;
                        return Ok(());
                    }
                    stack.push(results[0]);
                }
                Instr::ValuesToList => {
                    // `(multiple-value-list form)`: the helper reads the
                    // runtime tuple left by FORM, so it is a mapped helper
                    // Invoke like LoadFunction. The cold state is before the
                    // primary is consumed (bliss-6z1hh).
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let primary = stack
                        .pop()
                        .ok_or(BuildError::Unsupported("stack underflow (ValuesToList)"))?;
                    let (inst, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args: vec![primary],
                            results: vec![],
                            aux: AuxData::ValuesToList,
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
                            results[0],
                            i as u32,
                            Some(end),
                        )?;
                        return Ok(());
                    }
                    stack.push(results[0]);
                }
                // Non-capturing closure creation for a CHECKED-ABI target.
                // Under `native_cleanups` the mapped path above has already
                // turned this into an `Invoke` and returned, so this arm runs
                // only for the emitters that call a plain C adapter; the guard
                // keeps x86-64's mapped IR byte-identical.
                Instr::MakeClosure {
                    func,
                    capture_env: false,
                } if !self.native_cleanups => {
                    if self.bf.nested_functions.get(*func as usize).is_none() {
                        return Err(BuildError::Unsupported("invalid nested function"));
                    }
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let (_, results) = self.f.push_inst(
                        block,
                        InstData {
                            opcode: Opcode::Call,
                            args: vec![],
                            results: vec![],
                            aux: AuxData::NestedClosure(*func),
                            flags: runtime_call_flags(),
                            targets: vec![],
                            frame_state: Some(fs),
                            source_pos: 0,
                        },
                        &[(IRType::TOP, ValueRepresentation::Tagged)],
                    );
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
                            if let Some(binding) = super::builtin_binding::snapshot(*sym) {
                                let state = self.build_frame_state(block, &stack, i as u32);
                                let position = self.f.block(block).insts.len();
                                if self.expand_intrinsic(intrinsic, block, &mut stack, i, start)? {
                                    super::builtin_binding::insert(
                                        &mut self.f, block, position, state, binding,
                                    );
                                    // Every supported intrinsic returns one value.
                                    // Keep that effect after its binding/type guards,
                                    // even if its pure result is folded or discarded.
                                    self.emit_effect(
                                        block, Opcode::ClearMv, vec![], AuxData::None, Some(state),
                                    );
                                    self.remaining_inline_budget -= metadata.cost;
                                    continue;
                                }
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
                    if !self.native_cleanups
                        && n == 2
                        && self.inline_options.policy_at(bcp) != InlinePolicy::NotInline
                        && super::string_char::is_string_char_accessor(*sym)
                    {
                        self.string_char_calls.insert(inst);
                    }
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
                    // The selected binding can change, even for ERROR. Keep
                    // its normal continuation; an actual error takes the call's
                    // existing exceptional path before this value is consumed.
                    stack.push(results[0]);
                }
                Instr::CheckResult(declaration) => {
                    let checked_type = match declaration {
                        DeclaredType::Any => continue,
                        DeclaredType::Fixnum => IRType::of(TypeBits::FIXNUM),
                        DeclaredType::FixnumRange { lower, upper } => IRType {
                            bits: TypeBits::FIXNUM,
                            range: Some(crate::t2::ir::Range { lo: *lower, hi: *upper }),
                            class_id: None,
                        },
                        DeclaredType::SingleFloat => {
                            return Err(BuildError::Unsupported("single-float result guard"));
                        }
                    };
                    // The producer has already completed. Recover at this
                    // assertion with its primary still on the stack, never at
                    // the producer call. Unlike TYPEP, this preserves MV state.
                    let fs = self.build_frame_state(block, &stack, i as u32);
                    let value = stack.pop()
                        .ok_or(BuildError::Unsupported("stack underflow (CheckResult)"))?;
                    let checked = self.emit(
                        block,
                        Opcode::Guard,
                        vec![value],
                        AuxData::TypeTag(checked_type),
                        InstFlags { effectful: true, guard: true, ..Default::default() },
                        Some(fs),
                        checked_type,
                    ).ok_or(BuildError::Unsupported("result guard has a result"))?;
                    stack.push(checked);
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
                | AuxData::SpecialBinding { .. }
                | AuxData::GlobalValue { store: true, .. }
                | AuxData::LexicalEnvironment { result: false, .. }
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
    ///
    /// Each sweep reads arguments through the substitutions found so far, so a
    /// phi made trivial by an earlier removal is found without rewriting the
    /// function first. Uses and parameters are then rewritten once. Rewriting
    /// the whole function per removed phi made this quadratic in function size,
    /// a large share of every T1 compile (bliss-t9tzf).
    fn simplify_trivial_phis(&mut self) {
        fn resolve(subst: &HashMap<Value, Value>, mut value: Value) -> Value {
            while let Some(&next) = subst.get(&value) {
                value = next;
            }
            value
        }
        // Every value in `subst` maps to one that is not, so chains end.
        let mut subst: HashMap<Value, Value> = HashMap::new();
        loop {
            let mut changed = false;
            for &b in self.block_of.values() {
                let preds = match self.pred_edges.get(&b) {
                    Some(p) if !p.is_empty() => p,
                    _ => continue, // entry / unreachable: no incoming phi arguments
                };
                for (pos, &p) in self.f.block(b).params.iter().enumerate() {
                    if subst.contains_key(&p) {
                        continue;
                    }
                    let mut other: Option<Value> = None;
                    let mut trivial = true;
                    for &(pred, idx) in preds {
                        let term = self.terminator_inst[&pred];
                        let arg = resolve(&subst, self.f.inst(term).targets[idx].args[pos]);
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
                            subst.insert(p, u);
                            changed = true;
                        }
                    }
                }
            }
            if !changed {
                break;
            }
        }
        if subst.is_empty() {
            return;
        }
        let resolved: HashMap<Value, Value> =
            subst.keys().map(|&p| (p, resolve(&subst, p))).collect();
        self.replace_values(&resolved);
        let blocks: HashSet<Block> = self.block_of.values().copied().collect();
        for b in blocks {
            let removed: Vec<usize> = (self.f.block(b).params.iter().enumerate())
                .filter(|(_, p)| resolved.contains_key(p))
                .map(|(pos, _)| pos)
                .collect();
            if removed.is_empty() {
                continue;
            }
            for &pos in removed.iter().rev() {
                self.f.block_mut(b).params.remove(pos);
            }
            for &(pred, idx) in &self.pred_edges[&b].clone() {
                let term = self.terminator_inst[&pred];
                let args = &mut self.f.inst_mut(term).targets[idx].args;
                for &pos in removed.iter().rev() {
                    args.remove(pos);
                }
            }
        }
    }

    /// Replace every use of each key of `subst` with its value — in instruction
    /// operands, in edge (block-call) arguments, and in deopt frame states — so
    /// a removed trivial phi leaves no dangling reference.
    fn replace_values(&mut self, subst: &HashMap<Value, Value>) {
        let rewrite = |a: &mut Value| {
            if let Some(&u) = subst.get(a) {
                *a = u;
            }
        };
        // Synthetic normal/cold call bridges also carry phi uses. Rewriting
        // only bytecode leaders leaves their edge arguments dangling.
        let blocks = self.f.block_order().to_vec();
        for b in blocks {
            let insts = self.f.block(b).insts.clone();
            for inst in insts {
                let d = self.f.inst_mut(inst);
                d.args.iter_mut().for_each(rewrite);
                for tc in d.targets.iter_mut() {
                    tc.args.iter_mut().for_each(rewrite);
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
                        rewrite(value);
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
            // pass can reuse one proof where it dominates another.
            // CL defines (CAR NIL) and (CDR NIL) as NIL; `admit_nil_list_loads`
            // later routes NIL around this pair once the CFG is complete, so
            // only a non-list operand reaches the deopt path. That puts each
            // proof inside its own cons arm, where it rarely dominates another.
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
            // A double literal keeps its tagged box but is typed, so double
            // speculation can read it as a constant (bliss-thcbb).
            (
                Opcode::ConstHeapObj,
                AuxData::HeapLiteral {
                    slot: slot as *const _ as usize,
                },
                if val.is_double_float() {
                    IRType::of(TypeBits::DOUBLE_FLOAT)
                } else {
                    IRType::TOP
                },
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
    /// Unreachable continuation with no successors.
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

/// Give every expanded CAR/CDR an explicit NIL arm (bliss-jmwqr).
///
/// The intrinsic expansion is `Guard(CONS)` immediately followed by its field
/// load. NIL is an ordinary list operand -- `(CDDR '(1))` takes `(CDR NIL)` --
/// but it fails that tag guard, and each deopt counts toward the per-function
/// blacklist that returns the function to T0 for good. So split the block at
/// every such pair:
///
/// ```text
///   B:    ...; nil = ConstNil; t = GenericEq(x, nil); Brif t -> NilArm, ConsArm
///   NilArm:  Jump Join(nil)
///   ConsArm: g = Guard(x, CONS); r = Car/Cdr(g); Jump Join(r)
///   Join(r'): <rest of B, uses of r renamed to r'>
/// ```
///
/// The guard keeps its FrameState, so any other non-list operand still
/// deopts to the generic CL call, which signals the type error. Runs on the
/// finished CFG, where the guard has exactly one use and no later pass has
/// shared its proof. An operand whose type excludes NULL, or which a
/// dominating branch has already found non-NIL, keeps the plain guard (see
/// `branch_proves_non_nil`), so loops that test `(NULL x)` or `(ENDP x)`
/// first still share one cons proof among their accesses, and adjacent
/// accesses to one operand share a single test and proof.
/// New blocks are laid out after the split block, preserving the builder's
/// defs-before-uses layout order.
fn admit_nil_list_loads(f: &mut Function) {
    let mut uses: HashMap<Value, usize> = HashMap::new();
    let mut count = |value: Value| *uses.entry(value).or_insert(0) += 1;
    for &block in f.block_order() {
        for &inst in &f.block(block).insts {
            let data = f.inst(inst);
            data.args.iter().copied().for_each(&mut count);
            for target in &data.targets {
                target.args.iter().copied().for_each(&mut count);
            }
        }
    }
    for (_, state) in f.frame_states.iter() {
        let sources = state
            .scopes
            .iter()
            .flat_map(|scope| scope.locals.iter().chain(&scope.stack))
            .chain(state.remat.iter().flat_map(|recipe| &recipe.inputs));
        for source in sources {
            if let ValueSource::Value { value, .. } = source {
                count(*value);
            }
        }
    }

    let pair_at = |f: &Function, pair: &[Inst]| {
        let (guard, load) = (f.inst(pair[0]), f.inst(pair[1]));
        guard.opcode == Opcode::Guard
            && guard.flags.guard
            && matches!(guard.aux, AuxData::TypeTag(t) if t.bits == TypeBits::CONS)
            && matches!(load.opcode, Opcode::Car | Opcode::Cdr)
            && guard.results.len() == 1
            && load.args == guard.results
            && uses.get(&guard.results[0]) == Some(&1)
            && f.value(guard.args[0]).ty.bits.contains(TypeBits::NULL)
    };
    // Decide every proof on the unsplit CFG. Splitting adds no paths, so a
    // proof found here still holds wherever the guard ends up.
    let mut proven = HashSet::new();
    {
        let dom = f.dominators();
        let mut preds: HashMap<Block, Vec<Block>> = HashMap::new();
        for &block in f.block_order() {
            for succ in f.succs(block) {
                preds.entry(succ).or_default().push(block);
            }
        }
        for &block in f.block_order() {
            for pair in f.block(block).insts.windows(2) {
                if pair_at(f, pair) {
                    let operand = f.inst(pair[0]).args[0];
                    if branch_proves_non_nil(f, &dom, &preds, block, operand) {
                        proven.insert(pair[0]);
                    }
                }
            }
        }
    }
    let site = |f: &Function, block: Block| {
        f.block(block)
            .insts
            .windows(2)
            .position(|pair| pair_at(f, pair) && !proven.contains(&pair[0]))
    };
    let mut renamed = HashMap::new();
    let mut cons_exits = Vec::new();
    let mut work: Vec<Block> = f.block_order().to_vec();
    while let Some(block) = work.pop() {
        let Some(pos) = site(f, block) else {
            continue;
        };
        let insts = &f.block(block).insts;
        let (guard, load) = (insts[pos], insts[pos + 1]);
        let operand = f.inst(guard).args[0];
        let source_pos = f.inst(load).source_pos;
        // Later accesses to the same operand that follow with nothing in
        // between but MV resets and constants share this NIL test and cons
        // proof, as they shared the proof before the split: their loads move
        // into the cons arm and read this guard's result, and their own
        // guards are dropped, since a guard on the same value cannot fail once
        // this one has passed. `(+ (car x) (cdr x))` pays one test and one
        // guard. Only those instructions are skipped, so no load moves across
        // a call or store that could mutate the cell.
        let mut grouped = vec![pos];
        let mut next = pos + 2;
        while next < insts.len() {
            let data = f.inst(insts[next]);
            // ClearMv carries a FrameState, but it neither reads the heap
            // nor names a load that has not run yet.
            if matches!(
                data.opcode,
                Opcode::ClearMv
                    | Opcode::ConstNil
                    | Opcode::ConstT
                    | Opcode::ConstFixnum
                    | Opcode::ConstChar
                    | Opcode::ConstSymbol
            ) {
                next += 1;
            } else if next + 1 < insts.len()
                && pair_at(f, &insts[next..next + 2])
                && f.inst(insts[next]).args[0] == operand
            {
                grouped.push(next);
                next += 2;
            } else {
                break;
            }
        }
        let loads: Vec<Value> = grouped
            .iter()
            .map(|&at| f.inst(insts[at + 1]).results[0])
            .collect();
        // Inserted in reverse, so the layout reads B, NilArm, ConsArm, Join.
        let join = f.make_block_after(block);
        let cons_arm = f.make_block_after(block);
        let nil_arm = f.make_block_after(block);
        let proof = f.inst(guard).results[0];
        let tail = f.block_mut(block).insts.split_off(pos);
        let (mut moved, mut rest) = (Vec::new(), Vec::new());
        for (index, inst) in tail.into_iter().enumerate() {
            let at = pos + index;
            if at == pos || at == pos + 1 {
                moved.push(inst);
            } else if grouped.contains(&(at - 1)) {
                f.inst_mut(inst).args[0] = proof;
                moved.push(inst);
            } else if !grouped.contains(&at) {
                rest.push(inst);
            }
        }
        f.block_mut(join).insts = rest;
        f.block_mut(cons_arm).insts = moved;
        for &loaded in &loads {
            let loaded_data = f.value(loaded).clone();
            let merged = f.add_block_param(join, loaded_data.ty, loaded_data.repr);
            renamed.insert(loaded, merged);
        }

        let tagged = |ty| (ty, ValueRepresentation::Tagged);
        let at = |opcode, args| InstData {
            opcode,
            args,
            results: vec![],
            aux: AuxData::None,
            flags: InstFlags::default(),
            targets: vec![],
            frame_state: None,
            source_pos,
        };
        let (_, nil) = f.push_inst(
            block,
            at(Opcode::ConstNil, vec![]),
            &[tagged(IRType::of(TypeBits::NULL))],
        );
        // Pinned so GVN keeps each test private to its own branch. A shared
        // test would stay live from an earlier access, across a loop header
        // whose FrameState does not name it (so that OSR entry is declined),
        // and could no longer fuse into its Brif as a single cmp/jcc.
        let pinned = InstFlags {
            effectful: true,
            ..InstFlags::default()
        };
        let (_, is_nil) = f.push_inst(
            block,
            InstData {
                flags: pinned,
                ..at(Opcode::GenericEq, vec![operand, nil[0]])
            },
            &[tagged(IRType::TOP)],
        );
        f.set_terminator(
            block,
            InstData {
                source_pos,
                ..brif(is_nil[0], nil_arm, cons_arm)
            },
        );
        let to_join = |args| InstData {
            source_pos,
            targets: vec![crate::t2::ir::BlockCall { block: join, args }],
            ..jump(join)
        };
        f.set_terminator(nil_arm, to_join(vec![nil[0]; loads.len()]));
        // The cons edge must carry the loads' own results, so it is added only
        // after the uses are renamed below.
        cons_exits.push((cons_arm, to_join(loads)));
        work.push(join);
    }
    f.replace_values(&renamed);
    for (cons_arm, exit) in cons_exits {
        f.set_terminator(cons_arm, exit);
    }
}

/// Whether every path to `block` crossed a branch edge that only a non-NIL
/// `operand` takes: the true edge of a branch on the value itself or on
/// `(CONSP x)`, or the false edge of one on `(NULL x)`/`(NOT x)` (an `EQ` with
/// NIL) or `(ENDP x)`. Walks the dominator chain, accepting an edge only into
/// a block whose single predecessor is the branch. A wrong answer costs only
/// speed: the cons guard stays, and NIL then deopts as it used to.
fn branch_proves_non_nil(
    f: &Function,
    dom: &crate::t2::ir::DominatorTree,
    preds: &HashMap<Block, Vec<Block>>,
    block: Block,
    operand: Value,
) -> bool {
    let is_nil = |value: Value| {
        matches!(f.value(value).def, crate::t2::ir::ValueDef::Result { inst, .. }
            if f.inst(inst).opcode == Opcode::ConstNil)
    };
    let tests = |cond: Value, taken: bool| {
        if cond == operand {
            return taken;
        }
        let crate::t2::ir::ValueDef::Result { inst, .. } = f.value(cond).def else {
            return false;
        };
        let data = f.inst(inst);
        match (data.opcode, &data.aux, data.args.as_slice()) {
            (Opcode::GenericEq, _, &[a, b]) => {
                !taken && ((a == operand && is_nil(b)) || (b == operand && is_nil(a)))
            }
            (Opcode::Call, AuxData::CallTarget(symbol), &[arg]) if arg == operand => {
                match cl_function_name(*symbol).as_deref() {
                    Some("ENDP") => !taken,
                    Some("CONSP") => taken,
                    _ => false,
                }
            }
            _ => false,
        }
    };
    let mut current = block;
    loop {
        if let Some([branch]) = preds.get(&current).map(Vec::as_slice) {
            if let Some(term) = f.terminator(*branch) {
                let data = f.inst(term);
                if data.opcode == Opcode::Brif && data.targets.len() == 2 {
                    let (yes, no) = (data.targets[0].block, data.targets[1].block);
                    if yes != no
                        && (yes == current || no == current)
                        && tests(data.args[0], yes == current)
                    {
                        return true;
                    }
                }
            }
        }
        match dom.idom(current) {
            Some(up) if up != current => current = up,
            _ => return false,
        }
    }
}

/// The bare name of a COMMON-LISP function symbol, as `metadata_for_symbol`
/// recognises one; `None` for a same-spelling symbol in another package.
fn cl_function_name(sym: u32) -> Option<String> {
    let name = crate::reader::symbol_name(sym)?;
    match egcl_rt::symbols::split_registry_key(&name) {
        None => Some(name),
        Some(("CL" | "COMMON-LISP", bare)) => Some(bare.to_string()),
        Some(_) => None,
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
            self_call_sites: Vec::new(),
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
            binder_cache: Default::default(),
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
    fn result_check_refines_only_its_result_and_recovers_at_the_assertion() {
        let body = bf("checked-result", vec![
            Instr::LoadLocal(0),
            Instr::CheckResult(DeclaredType::FixnumRange { lower: 0, upper: 10 }),
            Instr::Return,
        ], vec![], 1, 1, 1);
        let f = build_from_bytecode(&body).expect("build result assertion");
        let entry = f.entry();
        let input = f.block(entry).params[0];
        let guard = f.block(entry).insts.iter().copied()
            .find(|&inst| f.inst(inst).opcode == Opcode::Guard).expect("result guard");
        let data = f.inst(guard);
        assert_eq!(data.args, vec![input]);
        assert_eq!(f.value(input).ty, IRType::TOP);
        assert_eq!(f.value(data.results[0]).ty.range,
            Some(crate::t2::ir::Range { lo: 0, hi: 10 }));
        let state = f.frame_states.get(data.frame_state.expect("recovery state"));
        assert_eq!(state.scopes[0].bcp, 1);
        assert!(matches!(state.scopes[0].stack.as_slice(),
            [ValueSource::Value { value, .. }] if *value == input));
        assert!(!has_opcode(&f, entry, Opcode::ClearMv));
        let ret = f.inst(f.terminator(entry).unwrap());
        assert_eq!(ret.args, data.results);
    }

    #[test]
    fn declared_parameter_range_seeds_entry_ssa() {
        use crate::t2::ir::Range;
        let mut typed = bf("bounded", vec![Instr::LoadLocal(0), Instr::Return],
            vec![], 1, 1, 1);
        typed.param_types = vec![DeclaredType::FixnumRange { lower: 0, upper: 40 }];
        let f = build_from_bytecode(&typed).expect("build bounded function");
        let parameter = f.block(f.entry()).params[0];
        assert_eq!(f.value(parameter).ty.bits, TypeBits::FIXNUM);
        assert_eq!(f.value(parameter).ty.range, Some(Range { lo: 0, hi: 40 }));
        assert!(f.is_entry_param_checked(parameter));
        assert_eq!(crate::t2::infer::infer(&f).range(parameter),
            Some(Range { lo: 0, hi: 40 }));
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
    fn mutable_error_call_retains_its_normal_continuation() {
        let error = egcl_rt::symbols::intern("ERROR");
        let input = bf(
            "returning-error",
            vec![
                Instr::CallNamed { sym: error, nargs: 0 },
                Instr::Pop,
                Instr::Const(0),
                Instr::Return,
            ],
            vec![EgclVal::from_fixnum(42)],
            0,
            0,
            1,
        );
        let scopes = ScopeMap::analyze_function(&input).unwrap();
        assert!(scopes.before(1).is_some(), "a replacement ERROR may return");
        for result in [
            build_from_bytecode(&input),
            build_from_bytecode_for_transfers(&input),
            build_from_bytecode_for_native_cleanups(&input),
        ] {
            let function = result.unwrap();
            crate::t2::verify::verify(&function).unwrap();
            assert!(function.block_order().iter().any(|&block| {
                function.block(block).insts.iter().any(|&inst| {
                    function.inst(inst).opcode == Opcode::Return
                })
            }), "the caller must retain its return after the named call");
        }
    }

    #[test]
    fn values_to_list_is_a_mapped_helper_invoke() {
        let input = bf(
            "values-to-list",
            vec![
                Instr::ClearMv,
                Instr::LoadLocal(0),
                Instr::ValuesToList,
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let plain = build_from_bytecode(&input).unwrap();
        crate::t2::verify::verify(&plain).unwrap();
        let collects = |function: &Function, opcode: Opcode| {
            function.block_order().iter().any(|&block| {
                function.block(block).insts.iter().any(|&inst| {
                    let data = function.inst(inst);
                    data.opcode == opcode
                        && matches!(data.aux, AuxData::ValuesToList)
                        && data.args.len() == 1
                        && data.frame_state.is_some()
                })
            })
        };
        assert!(collects(&plain, Opcode::Call));
        for result in [
            build_from_bytecode_for_transfers(&input),
            build_from_bytecode_for_native_cleanups(&input),
        ] {
            let function = result.unwrap();
            crate::t2::verify::verify(&function).unwrap();
            assert!(
                collects(&function, Opcode::Invoke),
                "the helper must own an exceptional edge"
            );
        }
    }

    #[test]
    fn alloc_cons_is_a_cons_call() {
        let input = bf(
            "alloc-cons",
            vec![
                Instr::LoadLocal(0),
                Instr::LoadLocal(1),
                Instr::AllocCons,
                Instr::Return,
            ],
            vec![],
            2,
            2,
            2,
        );
        let cons = egcl_rt::symbols::intern("CONS");
        let calls_cons = |function: &Function, opcode: Opcode| {
            function.block_order().iter().any(|&block| {
                function.block(block).insts.iter().any(|&inst| {
                    let data = function.inst(inst);
                    data.opcode == opcode
                        && matches!(data.aux, AuxData::CallTarget(symbol) if symbol == cons)
                        && data.args.len() == 2
                        && data.frame_state.is_some()
                })
            })
        };
        let plain = build_from_bytecode(&input).unwrap();
        crate::t2::verify::verify(&plain).unwrap();
        assert!(calls_cons(&plain, Opcode::Call));
        for result in [
            build_from_bytecode_for_transfers(&input),
            build_from_bytecode_for_native_cleanups(&input),
        ] {
            let function = result.unwrap();
            crate::t2::verify::verify(&function).unwrap();
            assert!(
                calls_cons(&function, Opcode::Invoke),
                "the allocating call must own an exceptional edge"
            );
        }
    }

    #[test]
    fn folded_mutable_builtin_retains_binding_deopt_on_every_backend() {
        #[cfg(target_arch = "x86_64")]
        use crate::t2::emit;
        #[cfg(target_arch = "aarch64")]
        use crate::t2::emit_a64;
        use crate::t2::pass::PassManager;
        use crate::t2::{emit_ppc64le, emit_riscv64, emit_s390x};
        let plus = egcl_rt::symbols::intern("+");
        let input = bf(
            "binding-fold",
            vec![
                Instr::Const(0),
                Instr::Const(1),
                Instr::CallNamed {
                    sym: plus,
                    nargs: 2,
                },
                Instr::Return,
            ],
            vec![EgclVal::from_fixnum(10), EgclVal::from_fixnum(20)],
            0,
            0,
            2,
        );
        let mut function = build_from_bytecode(&input).unwrap();
        assert_eq!(crate::t2::speculate::speculate(&mut function, &|_| None), 1);
        // Standard CL arithmetic is fixed. Add an independent mutable binding
        // dependency explicitly: this fixture tests folding and backend deopt
        // preservation, not whether canonical arithmetic needs a binding guard.
        let mutable = egcl_rt::symbols::intern("FOLDING-MUTABLE-BINDING-FIXTURE");
        let binding = crate::t2::builtin_binding::snapshot(mutable).unwrap();
        assert!(matches!(
            binding,
            crate::t2::builtin_binding::Binding::Guarded { .. }
        ));
        let block = function.entry();
        let position = function
            .block(block)
            .insts
            .iter()
            .position(|&inst| function.inst(inst).opcode == Opcode::FixnumAdd)
            .unwrap();
        let arithmetic = function.block(block).insts[position];
        let state = function.inst(arithmetic).frame_state.unwrap();
        crate::t2::builtin_binding::insert(&mut function, block, position, state, binding);
        let mut passes = PassManager::new();
        passes
            .add(Box::new(crate::t2::opt_fold::ConstFold))
            .add(Box::new(crate::t2::opt_clear_mv::ClearMvElim))
            .add(Box::new(crate::t2::opt_dce::Dce));
        passes.run(&mut function);
        crate::t2::verify::verify(&function).unwrap();
        let guards: Vec<_> = function
            .block_order()
            .iter()
            .flat_map(|&block| {
                function
                    .block(block)
                    .insts
                    .iter()
                    .map(|&inst| function.inst(inst))
            })
            .filter(|data| matches!(data.aux, AuxData::BuiltinBinding { .. }))
            .collect();
        assert_eq!(
            guards.len(),
            1,
            "folding must not erase binding sensitivity"
        );
        let state = function.frame_states.get(guards[0].frame_state.unwrap());
        assert_eq!(state.scopes[0].bcp, 2);
        assert_eq!(
            state.scopes[0].stack.len(),
            2,
            "resume with the original arguments"
        );
        assert!(
            function.block_order().iter().all(|&block| function
                .block(block)
                .insts
                .iter()
                .all(|&inst| function.inst(inst).opcode != Opcode::FixnumAdd)),
            "the arithmetic result must actually fold"
        );
        // These artifacts are inspected, not executed. Supply the return-effect
        // adapter as well as the deopt address now that folding retains ClearMv.
        let runtime = emit_s390x::RuntimeCalls {
            multiple_values: 1,
            ..Default::default()
        };
        // emit.rs and emit_a64.rs both resolve branches through
        // `egcl_rt::asm::Asm`, whose label fixups are the HOST's: on an
        // AArch64 host the x86 emitter lays x86 bytes but resolves their
        // displacements with AArch64 rules and returns BadBranch. Each of
        // those two is therefore only meaningful on its own host. The
        // System Z, POWER and RISC-V emitters carry their own
        // host-independent assemblers and run everywhere (bliss-d2ter).
        #[cfg_attr(
            not(any(target_arch = "x86_64", target_arch = "aarch64")),
            allow(unused_mut)
        )]
        let mut backends = vec![
            (
                "s390x",
                emit_s390x::emit_framed_with_runtime(
                    &function,
                    1,
                    input.num_slots(),
                    runtime.clone(),
                ),
            ),
            (
                "ppc64le",
                emit_ppc64le::emit_framed_with_runtime(
                    &function,
                    1,
                    input.num_slots(),
                    runtime.clone(),
                ),
            ),
            (
                "riscv64",
                emit_riscv64::emit_framed_with_runtime(
                    &function,
                    1,
                    input.num_slots(),
                    emit_riscv64::RuntimeCalls {
                        multiple_values: 1,
                        ..Default::default()
                    },
                ),
            ),
        ];
        #[cfg(target_arch = "x86_64")]
        backends.push((
            "x86",
            emit::emit_framed(&function, 1, 1, 0, 0, 0, 0, 0, 1, None),
        ));
        #[cfg(target_arch = "aarch64")]
        backends.push((
            "aarch64",
            emit_a64::emit_framed_with_runtime(&function, 1, input.num_slots(), runtime.clone()),
        ));
        let _ = &runtime;
        for (name, code) in backends {
            assert!(
                code.unwrap_or_else(|error| panic!("{name}: {error:?}"))
                    .has_deopt,
                "{name} must preserve a real deopt exit"
            );
        }
    }

    /// The NIL arm admit_nil_list_loads adds in front of CAR/CDR is built from
    /// GenericEq, Brif and a join-block parameter. Every backend must emit it,
    /// or a function using CAR/CDR loses its native code there: AArch64 once
    /// refused GenericEq outright (bliss-jmwqr).
    #[test]
    fn nil_armed_list_access_emits_on_every_backend() {
        #[cfg(target_arch = "x86_64")]
        use crate::t2::emit;
        #[cfg(target_arch = "aarch64")]
        use crate::t2::emit_a64;
        use crate::t2::{emit_ppc64le, emit_riscv64, emit_s390x};
        let car = egcl_rt::symbols::intern("CAR");
        let cdr = egcl_rt::symbols::intern("CDR");
        for (name, code) in [
            (
                "car",
                vec![
                    Instr::LoadLocal(0),
                    Instr::CallNamed { sym: car, nargs: 1 },
                    Instr::Return,
                ],
            ),
            (
                "cddr",
                vec![
                    Instr::LoadLocal(0),
                    Instr::CallNamed { sym: cdr, nargs: 1 },
                    Instr::CallNamed { sym: cdr, nargs: 1 },
                    Instr::Return,
                ],
            ),
            // CAR then CDR of one value share a single NIL test, so the join
            // carries one parameter per load.
            (
                "car-cdr",
                vec![
                    Instr::LoadLocal(0),
                    Instr::CallNamed { sym: car, nargs: 1 },
                    Instr::StoreLocal(1),
                    Instr::LoadLocal(0),
                    Instr::CallNamed { sym: cdr, nargs: 1 },
                    Instr::Return,
                ],
            ),
        ] {
            let locals = if name == "car-cdr" { 2 } else { 1 };
            let input = bf(name, code, vec![], locals, 1, 1);
            let function = build_from_bytecode(&input).unwrap();
            crate::t2::verify::verify(&function).unwrap();
            assert!(
                function.block_order().iter().any(|&block| function
                    .block(block)
                    .insts
                    .iter()
                    .any(|&inst| function.inst(inst).opcode == Opcode::GenericEq)),
                "{name}: the fixture must carry the NIL test it is checking"
            );
            if name == "car-cdr" {
                assert!(
                    function
                        .block_order()
                        .iter()
                        .any(|&block| function.block(block).params.len() >= 2),
                    "car-cdr: the grouped loads must meet at a multi-parameter join"
                );
            }
            let runtime = emit_s390x::RuntimeCalls {
                multiple_values: 1,
                ..Default::default()
            };
            // emit.rs and emit_a64.rs both resolve branches through
            // `egcl_rt::asm::Asm`, whose label fixups are the HOST's: on an
            // AArch64 host the x86 emitter lays x86 bytes but resolves their
            // displacements with AArch64 rules and returns BadBranch. Each of
            // those two is therefore only meaningful on its own host. The
            // System Z, POWER and RISC-V emitters carry their own
            // host-independent assemblers and run everywhere (bliss-d2ter).
            #[cfg_attr(
            not(any(target_arch = "x86_64", target_arch = "aarch64")),
            allow(unused_mut)
        )]
        let mut backends = vec![
                (
                    "s390x",
                    emit_s390x::emit_framed_with_runtime(
                        &function,
                        1,
                        input.num_slots(),
                        runtime.clone(),
                    ),
                ),
                (
                    "ppc64le",
                    emit_ppc64le::emit_framed_with_runtime(
                        &function,
                        1,
                        input.num_slots(),
                        runtime.clone(),
                    ),
                ),
                (
                    "riscv64",
                    emit_riscv64::emit_framed_with_runtime(
                        &function,
                        1,
                        input.num_slots(),
                        emit_riscv64::RuntimeCalls {
                            multiple_values: 1,
                            ..Default::default()
                        },
                    ),
                ),
            ];
            #[cfg(target_arch = "x86_64")]
            backends.push((
                "x86",
                emit::emit_framed(&function, 1, 1, 0, 0, 0, 0, 0, 1, None),
            ));
            #[cfg(target_arch = "aarch64")]
            backends.push((
                "aarch64",
                emit_a64::emit_framed_with_runtime(
                    &function,
                    1,
                    input.num_slots(),
                    runtime.clone(),
                ),
            ));
            let _ = &runtime;
            for (backend, code) in backends {
                assert!(
                    code.unwrap_or_else(|error| panic!("{name} on {backend}: {error:?}"))
                        .has_deopt,
                    "{name} on {backend}: a non-list operand must keep its deopt exit"
                );
            }
        }
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
        let helper = egcl_rt::symbols::intern("BODY-INLINE-CAR");
        let caller = egcl_rt::symbols::intern("BODY-INLINE-CAR-CALLER");
        let car = egcl_rt::symbols::intern("CAR");
        let body = Arc::new(bf(
            "BODY-INLINE-CAR",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed {
                    sym: car,
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
            "BODY-INLINE-CAR-CALLER",
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
            .find(|&i| matches!(f.inst(i).aux, AuxData::TypeTag(_)))
            .expect("CAR type guard cloned");
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

    /// Without cross-function recovery, a saved body that can deoptimize stays a
    /// call (so every FrameState keeps one logical frame), while one with no
    /// deopt point is still cloned (bliss-6988n).
    #[test]
    fn single_frame_recovery_keeps_deoptimizing_body_as_a_call() {
        let car = egcl_rt::symbols::intern("CAR");
        let guarded = egcl_rt::symbols::intern("SINGLE-FRAME-CAR-HELPER");
        let plain = egcl_rt::symbols::intern("SINGLE-FRAME-IDENTITY-HELPER");
        let caller = egcl_rt::symbols::intern("SINGLE-FRAME-CALLER");
        let guarded_body = Arc::new(bf(
            "SINGLE-FRAME-CAR-HELPER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: car, nargs: 1 },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        ));
        let plain_body = Arc::new(bf(
            "SINGLE-FRAME-IDENTITY-HELPER",
            vec![Instr::LoadLocal(0), Instr::Return],
            vec![],
            1,
            1,
            1,
        ));
        let input = bf(
            "SINGLE-FRAME-CALLER",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: plain, nargs: 1 },
                Instr::CallNamed { sym: guarded, nargs: 1 },
                Instr::Return,
            ],
            vec![],
            1,
            1,
            1,
        );
        let base = InlineOptions::default()
            .with_root_symbol(caller)
            .with_body(guarded, guarded_body)
            .with_body(plain, plain_body);
        let calls = |f: &Function| -> Vec<u32> {
            f.block_order()
                .iter()
                .flat_map(|&b| f.block(b).insts.iter().copied())
                .filter_map(|i| match (f.inst(i).opcode, &f.inst(i).aux) {
                    (Opcode::Call, AuxData::CallTarget(symbol)) => Some(*symbol),
                    _ => None,
                })
                .collect()
        };
        let single_frame = |f: &Function| {
            f.frame_states
                .iter()
                .all(|(_, state)| state.scopes.len() == 1 && state.scopes[0].function == caller)
        };

        let nested = build_from_bytecode_with_inline_options(&input, base.clone()).unwrap();
        assert!(calls(&nested).is_empty(), "default recovery clones both bodies");
        assert!(!single_frame(&nested), "the cloned CAR guard needs two frames");

        let mut restricted = base;
        restricted.config.cross_function_recovery = false;
        let f = build_from_bytecode_with_inline_options(&input, restricted).unwrap();
        assert_eq!(calls(&f), vec![guarded], "only the deoptimizing body stays a call");
        assert!(single_frame(&f), "every FrameState reconstructs one logical frame");
        crate::t2::verify::verify(&f).expect("restricted inline verifies");
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
    fn self_inline_cost_admits_a_tested_calls_clear_mv_without_charging_it() {
        let less = egcl_rt::symbols::intern("<");
        let with_test = |clear: bool| {
            let mut code = vec![Instr::LoadLocal(0), Instr::Const(0),
                Instr::CallNamed { sym: less, nargs: 2 }];
            if clear {
                code.push(Instr::ClearMv);
            }
            code.extend([Instr::BrIfFalse(code.len() as u32 + 3), Instr::LoadLocal(0),
                Instr::Return, Instr::Const(0), Instr::Return]);
            let mut body = bf("SELF-INLINE-CLEAR-MV", code, vec![EgclVal::from_fixnum(1)], 1, 1, 2);
            body.param_layout = vec![("N".into(), VarLoc::Slot(0))];
            body
        };
        let plain = self_inline_cost(&with_test(false)).expect("plain test body is eligible");
        assert_eq!(self_inline_cost(&with_test(true)), Some(plain),
            "the IF test's ClearMv (#419) must not disqualify or enlarge the body");
    }

    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    #[test]
    fn bounded_self_expansion_checks_arguments_and_retains_a_leaf_call() {
        use crate::t2::ir::Range;
        let symbol = egcl_rt::symbols::intern("BOUNDED-SELF-INLINE");
        let less = egcl_rt::symbols::intern("<");
        let minus = egcl_rt::symbols::intern("-");
        let mut body = bf("BOUNDED-SELF-INLINE", vec![
            Instr::LoadLocal(0), Instr::Const(0),
            Instr::CallNamed { sym: less, nargs: 2 }, Instr::BrIfFalse(6),
            Instr::LoadLocal(0), Instr::Return,
            Instr::LoadLocal(0), Instr::Const(1),
            Instr::CallNamed { sym: minus, nargs: 2 },
            Instr::CallNamed { sym: symbol, nargs: 1 }, Instr::Return,
        ], vec![EgclVal::from_fixnum(2), EgclVal::from_fixnum(1)], 1, 1, 2);
        body.self_call_sites = vec![9];
        body.param_layout = vec![("N".into(), VarLoc::Slot(0))];
        body.param_types = vec![DeclaredType::FixnumRange { lower: 0, upper: 40 }];
        let body = Arc::new(body);
        let mut options = InlineOptions::default().with_root_symbol(symbol)
            .with_body(symbol, Arc::clone(&body));
        options.config.self_inline_depth = 2;
        let f = build_from_bytecode_with_inline_options(&body, options.clone()).unwrap();
        crate::t2::verify::verify(&f).expect("bounded self CFG verifies");
        let instructions: Vec<_> = f.block_order().iter()
            .flat_map(|&block| f.block(block).insts.iter().copied()).collect();
        let leaves: Vec<_> = instructions.iter().copied().filter(|&inst| {
            f.inst(inst).opcode == Opcode::Call
                && matches!(f.inst(inst).aux, AuxData::CallTarget(target) if target == symbol)
        }).collect();
        assert_eq!(leaves.len(), 1, "one residual recursive leaf remains");
        let leaf = f.frame_states.get(f.inst(leaves[0]).frame_state.unwrap());
        assert_eq!(leaf.scopes.len(), 3, "two bounded expansions must actually occur");
        assert!(leaf.scopes.iter().all(|scope| scope.function == symbol));
        assert_eq!(leaf.scopes.last().unwrap().bcp, 9);
        let guards: Vec<_> = instructions.iter().copied().filter(|&inst| {
            f.inst(inst).opcode == Opcode::Guard
                && matches!(f.inst(inst).aux, AuxData::TypeTag(ty)
                    if ty.range == Some(Range { lo: 0, hi: 40 }))
        }).collect();
        assert_eq!(guards.len(), 2, "each erased call retains its argument contract");
        let mut depths = Vec::new();
        for guard in guards {
            let state = f.frame_states.get(f.inst(guard).frame_state.unwrap());
            let current = state.scopes.last().unwrap();
            assert_eq!(current.bcp, 9, "failed argument resumes the original call");
            assert_eq!(current.stack.len(), 1, "original argument remains pending");
            for parent in &state.scopes[..state.scopes.len() - 1] {
                assert_eq!(parent.bcp, 10);
                assert!(parent.stack.is_empty(), "suspended parent awaits the result");
            }
            depths.push(state.scopes.len());
        }
        depths.sort_unstable();
        assert_eq!(depths, vec![1, 2]);
        let mut disabled = options.clone();
        disabled.config.self_inline_depth = 0;
        let mut notinline = options.clone().with_policy(9, InlinePolicy::NotInline);
        notinline.config.self_inline_depth = 2;
        let mut no_budget = options.clone();
        no_budget.config.node_budget = 0;
        let mut no_depth = options.clone();
        no_depth.config.max_depth = 0;
        let different_body = options.clone().with_body(symbol, Arc::new((*body).clone()));
        for options in [disabled, notinline, no_budget, no_depth, different_body] {
            let declined = build_from_bytecode_with_inline_options(&body, options).unwrap();
            assert!(declined.frame_states.iter().all(|(_, state)| state.scopes.len() == 1));
        }
        let mut unproven = (*body).clone();
        unproven.self_call_sites.clear();
        let unproven = Arc::new(unproven);
        let unproven_options = options.with_body(symbol, Arc::clone(&unproven));
        let declined = build_from_bytecode_with_inline_options(&unproven, unproven_options).unwrap();
        assert!(declined.frame_states.iter().all(|(_, state)| state.scopes.len() == 1));
    }

    #[test]
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    fn bounded_self_expansion_shares_budget_between_siblings() {
        let symbol = egcl_rt::symbols::intern("BOUNDED-SELF-SIBLINGS");
        let mut body = bf(
            "BOUNDED-SELF-SIBLINGS",
            vec![
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: symbol, nargs: 1 },
                Instr::Pop,
                Instr::LoadLocal(0),
                Instr::CallNamed { sym: symbol, nargs: 1 },
                Instr::Return,
            ],
            vec![], 1, 1, 1,
        );
        body.self_call_sites = vec![1, 4];
        body.param_layout = vec![("N".into(), VarLoc::Slot(0))];
        body.param_types = vec![DeclaredType::Fixnum];
        let body = Arc::new(body);
        let mut options = InlineOptions::default().with_root_symbol(symbol)
            .with_body(symbol, Arc::clone(&body));
        options.config.self_inline_depth = 3;
        options.config.node_budget = self_inline_cost(&body).unwrap();
        let f = build_from_bytecode_with_inline_options(&body, options).unwrap();
        crate::t2::verify::verify(&f).expect("sibling expansion verifies");
        let mut leaf_depths = Vec::new();
        let mut guards = 0;
        for &block in f.block_order() {
            for &inst in &f.block(block).insts {
                let data = f.inst(inst);
                if data.opcode == Opcode::Call
                    && matches!(data.aux, AuxData::CallTarget(target) if target == symbol)
                {
                    leaf_depths.push(f.frame_states.get(data.frame_state.unwrap()).scopes.len());
                }
                if data.opcode == Opcode::Guard {
                    guards += 1;
                }
            }
        }
        leaf_depths.sort_unstable();
        assert_eq!(leaf_depths, vec![1, 2, 2], "only one sibling may spend the one-copy budget");
        assert_eq!(guards, 1, "exactly one call boundary was expanded");

        let mut unsupported = (*body).clone();
        unsupported.param_types = vec![DeclaredType::SingleFloat];
        assert!(self_inline_cost(&unsupported).is_none());
        unsupported.param_types = body.param_types.clone();
        unsupported.has_env = true;
        assert!(self_inline_cost(&unsupported).is_none());
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
