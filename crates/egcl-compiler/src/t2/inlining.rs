// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Call-site inlining policy and compiler-known function metadata.
//!
//! # Purpose
//!
//! This module decides, for one call site, whether the SSA builder may expand
//! the callee inline, and what it knows about the callee. It is deliberately
//! separate from the builder: the builder asks this module what a function
//! *is* and whether a site may expand, and does not keep its own list of
//! magic CL names. Two expansion kinds share one policy:
//!
//! * **Intrinsics.** A fixed table ([`InlineMetadata`] entries in `KNOWN`)
//!   of functions with a hand-written IR template, identified by
//!   [`IntrinsicId`]: `EQ`, `NULL`, `CAR`, `CDR`, `SYMBOLP`, `INTEGERP`,
//!   `TYPEP` with a constant type specifier, and `STRINGP`. The builder
//!   guards the builtin binding before using a template.
//! * **Saved bytecode bodies.** A callee whose bytecode was registered with
//!   [`InlineOptions::with_body`] can be built to SSA and cloned as a CFG at
//!   the call site. `body_cost` decides eligibility and estimates growth.
//!
//! # Contract
//!
//! * [`metadata_for_symbol`] resolves an interned symbol to its `KNOWN` entry.
//!   An unqualified name denotes an inherited `COMMON-LISP` symbol; explicit
//!   `CL:`/`COMMON-LISP:` qualification is accepted. Library functions use
//!   their actual saved bodies or normal calls, regardless of their names.
//! * [`InlineOptions`] carries per-compilation state: lexical
//!   `INLINE`/`NOTINLINE` declarations keyed by bytecode call-site PC
//!   ([`InlinePolicy`]), registered bodies, per-site [`CallSiteProfile`]
//!   counters, and the root symbol of the function being compiled (for
//!   recursion detection and the outermost deopt scope). The production
//!   driver populates bodies and profiles from the tiering runtime.
//! * [`InlineConfig`] holds the limits: `small_threshold` (30 estimated IR
//!   nodes, always profitable), `max_depth` (6), `node_budget` (500 nodes of
//!   post-inline growth per compilation), and `hot_frequency_percent` (80:
//!   calls per 100 caller invocations needed to count a site as hot).
//!   `cross_function_recovery` (default true) says whether deopt can rebuild
//!   an inlined callee's frame; when false only bodies whose built IR has no
//!   deopt point (no FrameState) are cloned.
//! * [`decide`] returns [`InlineDecision`]: `Expand(IntrinsicId)` or
//!   `Decline(DeclineReason)`.
//!
//! # Decision order
//!
//! Hard constraints are checked before preference, so an explicit `INLINE`
//! never overrides them: `NOTINLINE`; wrong arity; an impure, allocating, or
//! possibly-signalling expansion ([`EffectSummary`] describes the expansion,
//! not a generic call to the surface function); depth at the limit; cost
//! above the remaining budget. A site that survives expands when its cost is
//! at or below `small_threshold`, or the site is hot, or `INLINE` was
//! declared; otherwise it is declined as not profitable.
//!
//! # Body eligibility (`body_cost`)
//!
//! Body inlining starts with the safe subset. A body is cloned only if it has
//! fixed positional arity, no captured environment, and no declared parameter
//! types (cloning would bypass the callee-entry validator), and its bytecode
//! uses only constants, local loads/stores, stack ops, branches, returns, and
//! calls that resolve either to a pure/total/non-allocating intrinsic with
//! matching arity (`TYPEP` excluded) or to another eligible registered body.
//! Cost is the instruction count plus callee costs. An `active` set turns
//! direct and mutual recursion into `DeclineReason::Recursive`.
//!
//! # Limits
//!
//! * The 500-node budget is the current post-inline cap; a larger budget
//!   needs a separate decision before arbitrary body cloning lands.
//! * Lexical declaration policies are not yet populated from source lowering;
//!   the map exists so tests and clients can exercise the exact contract.
//! * No interprocedural effect analysis: a body that calls anything outside
//!   the allow-list is declined wholesale.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use egcl_rt::bytecode::{BytecodeFunction, Instr};

/// Stable compiler identity for a function with an inline expansion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KnownFunction {
    Eq,
    Null,
    Car,
    Cdr,
    Consp,
    Symbolp,
    Integerp,
    Typep,
    Stringp,
}

/// Expansion hook understood by the T2 builder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IntrinsicId {
    Eq,
    Null,
    /// Guard the argument as a cons and load its first field.
    Car,
    /// Guard the argument as a cons and load its second field.
    Cdr,
    Consp,
    Symbolp,
    Integerp,
    TypepConstant,
    Stringp,
}

/// Effects of the expansion itself, rather than of an arbitrary generic call
/// to the surface CL function.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct EffectSummary {
    pub pure: bool,
    pub allocates: bool,
    pub may_signal: bool,
}

impl EffectSummary {
    const PURE_TOTAL: Self = Self {
        pure: true,
        allocates: false,
        may_signal: false,
    };
}

/// Immutable metadata owned by the compiler for one known function.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InlineMetadata {
    pub function: KnownFunction,
    pub name: &'static str,
    pub fixed_arity: u16,
    pub effects: EffectSummary,
    /// Estimated added T2 IR nodes.
    pub cost: u32,
    pub expansion: IntrinsicId,
}

const KNOWN: &[InlineMetadata] = &[
    InlineMetadata {
        function: KnownFunction::Eq,
        name: "EQ",
        fixed_arity: 2,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Eq,
    },
    InlineMetadata {
        function: KnownFunction::Null,
        name: "NULL",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Null,
    },
    InlineMetadata {
        function: KnownFunction::Car,
        name: "CAR",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Car,
    },
    InlineMetadata {
        function: KnownFunction::Cdr,
        name: "CDR",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Cdr,
    },
    InlineMetadata {
        function: KnownFunction::Symbolp,
        name: "SYMBOLP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Symbolp,
    },
    InlineMetadata {
        function: KnownFunction::Integerp,
        name: "INTEGERP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Integerp,
    },
    // Only a recognised constant type specifier selects this expansion.  That
    // specialised operation is total even though general TYPEP may signal for
    // a malformed type specifier.
    InlineMetadata {
        function: KnownFunction::Typep,
        name: "TYPEP",
        fixed_arity: 2,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::TypepConstant,
    },
    InlineMetadata {
        function: KnownFunction::Stringp,
        name: "STRINGP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Stringp,
    },
];

/// Resolve an interned symbol to compiler metadata.  Unqualified names are the
/// canonical representation used for inherited COMMON-LISP symbols; explicit
/// CL/COMMON-LISP qualification is accepted.  A same-spelling symbol in some
/// other package is not the CL function and must remain a normal call.
pub fn metadata_for_symbol(sym: u32) -> Option<&'static InlineMetadata> {
    let name = crate::reader::symbol_name(sym)?;
    let (package, bare) = match egcl_rt::symbols::split_registry_key(&name) {
        None => (None, name.as_str()),
        Some((package, bare)) => (Some(package), bare),
    };
    if package.is_some() && !matches!(package, Some("CL" | "COMMON-LISP")) {
        return None;
    }
    KNOWN.iter().find(|m| m.name == bare)
}

/// Lexical declaration in force at an individual call site.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum InlinePolicy {
    #[default]
    Unspecified,
    Inline,
    NotInline,
}

/// Profitability limits. The growth budget charges estimated body costs, not
/// exact emitted IR nodes; intrinsic lowering can expand individual bytecodes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InlineConfig {
    pub small_threshold: u32,
    pub max_depth: u8,
    /// Opt-in bound for source-proven self expansion. Zero keeps it disabled
    /// until the embedding runtime admits nested recovery for this compilation.
    pub self_inline_depth: u8,
    /// Whether the embedding runtime can reconstruct a suspended caller frame
    /// plus a different function's inlined frame at a deopt point. When false,
    /// a saved body is cloned only if no reachable instruction of its built IR
    /// carries a FrameState, so every surviving FrameState keeps exactly one
    /// logical frame (the root). Mapped x86-64 transfer declines any other
    /// shape; inlining into it would only leave the caller at T1 (bliss-6988n).
    pub cross_function_recovery: bool,
    pub node_budget: u32,
    /// Minimum calls per 100 profiled caller invocations for a call site to
    /// receive the larger, budget-limited hot-site allowance.
    pub hot_frequency_percent: u8,
}

impl Default for InlineConfig {
    fn default() -> Self {
        Self {
            small_threshold: 30,
            max_depth: 6,
            self_inline_depth: 0,
            cross_function_recovery: true,
            node_budget: 500,
            hot_frequency_percent: 80,
        }
    }
}

/// Runtime frequency sample for one bytecode call site. Both counters cover
/// the same T0/T1 sampling window so their ratio remains meaningful while the
/// caller moves between those tiers.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct CallSiteProfile {
    pub calls: u32,
    pub caller_invocations: u32,
}

impl CallSiteProfile {
    fn is_hot(self, threshold_percent: u8) -> bool {
        self.calls > 0
            && self.caller_invocations > 0
            && u64::from(self.calls) * 100
                >= u64::from(self.caller_invocations) * u64::from(threshold_percent)
    }
}

/// Per-compilation options, including lexical declaration decisions keyed by
/// the bytecode call-site PC.  Source lowering can populate this map later;
/// tests and compiler clients can already exercise the exact policy contract.
#[derive(Clone, Debug, Default)]
pub struct InlineOptions {
    pub config: InlineConfig,
    policies: HashMap<u32, InlinePolicy>,
    bodies: HashMap<u32, Arc<BytecodeFunction>>,
    constant_snapshots: HashMap<usize, Arc<super::constants::ConstantPoolSnapshot>>,
    call_sites: HashMap<(u32, u32), CallSiteProfile>,
    root_symbol: Option<u32>,
    // Owner-captured established DEFCONSTANT fixnums. No worker heap reads.
    constant_globals: HashMap<u32, i64>,
    constant_global_owner: Option<u32>,
}

impl InlineOptions {
    pub fn with_policy(mut self, bcp: u32, policy: InlinePolicy) -> Self {
        self.policies.insert(bcp, policy);
        self
    }

    /// Attach immutable facts captured by the compilation owner: each symbol
    /// is an established, bound DEFCONSTANT whose value is this fixnum. The
    /// caller must exclude special/dynamically bound references. This map is
    /// scoped to the root body; it must not leak into an inlined callee.
    pub fn with_constant_globals(mut self, values: HashMap<u32, i64>) -> Self {
        self.constant_globals = values;
        self.constant_global_owner = self.root_symbol;
        self
    }

    /// Transfer builders historically ignore body-inlining options. Preserve
    /// that boundary while carrying immutable constant metadata to workers.
    pub(crate) fn constant_globals_only(self, root: u32) -> Self {
        Self {
            root_symbol: Some(root),
            constant_globals: self.constant_globals,
            constant_global_owner: self.constant_global_owner,
            constant_snapshots: self.constant_snapshots,
            ..Self::default()
        }
    }

    pub(crate) fn constant_global(&self, body_symbol: u32, symbol: u32) -> Option<i64> {
        (self.constant_global_owner == Some(body_symbol))
            .then(|| self.constant_globals.get(&symbol).copied())
            .flatten()
    }

    pub fn policy_at(&self, bcp: u32) -> InlinePolicy {
        self.policies.get(&bcp).copied().unwrap_or_default()
    }

    /// Attach production profile counters for call-site `bcp` in `caller`.
    pub fn with_call_site_profile(
        mut self,
        caller: u32,
        bcp: u32,
        calls: u32,
        caller_invocations: u32,
    ) -> Self {
        self.call_sites.insert(
            (caller, bcp),
            CallSiteProfile {
                calls,
                caller_invocations,
            },
        );
        self
    }

    pub(crate) fn call_site_is_hot(&self, caller: u32, bcp: u32) -> bool {
        self.call_sites
            .get(&(caller, bcp))
            .copied()
            .is_some_and(|p| p.is_hot(self.config.hot_frequency_percent))
    }

    /// Make a saved bytecode body available to the body inliner.  Merely being
    /// registered is not enough: `body_cost` still applies the conservative
    /// fixed-arity/purity/effect filter before the body may be cloned.
    pub fn with_body(mut self, symbol: u32, body: Arc<BytecodeFunction>) -> Self {
        self.bodies.insert(symbol, body);
        self
    }

    /// Register worker-owned metadata together with its original constant-slot
    /// descriptions. The caller retains and roots the original body separately.
    pub fn with_body_snapshot(mut self, symbol: u32, snapshot: &super::constants::BytecodeSnapshot) -> Self {
        self.bodies.insert(symbol, Arc::clone(&snapshot.body));
        self.with_constant_snapshot(snapshot)
    }

    pub(crate) fn with_constant_snapshot(mut self, snapshot: &super::constants::BytecodeSnapshot) -> Self {
        self.constant_snapshots.insert(Arc::as_ptr(&snapshot.body) as usize, Arc::clone(&snapshot.constants));
        self
    }

    pub(crate) fn constants(&self, body: &BytecodeFunction) -> Arc<super::constants::ConstantPoolSnapshot> {
        self.constant_snapshots.get(&(body as *const _ as usize)).cloned()
            .unwrap_or_else(|| Arc::new(super::constants::ConstantPoolSnapshot::capture(body)))
    }

    /// Identify the function being compiled, for direct/mutual-recursion
    /// detection and for the outermost deopt scope.
    pub fn with_root_symbol(mut self, symbol: u32) -> Self {
        self.root_symbol = Some(symbol);
        self
    }

    pub(crate) fn body(&self, symbol: u32) -> Option<Arc<BytecodeFunction>> {
        self.bodies.get(&symbol).cloned()
    }

    pub(crate) fn root_symbol(&self) -> Option<u32> {
        self.root_symbol
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DeclineReason {
    NotInline,
    WrongArity,
    Impure,
    Allocates,
    MaySignal,
    DepthLimit,
    Budget,
    NotProfitable,
    Recursive,
    UnsupportedBody,
}

/// Return the conservative IR-growth estimate for a saved bytecode body.
///
/// Body inlining intentionally starts with the safe subset: fixed positional
/// arguments, no captured environment, allocation, global state, multiple
/// values, or non-local control. Calls are accepted only when they name an
/// already-proven pure/total intrinsic or another eligible saved body. The
/// visited set turns direct and mutual recursion into a clean decline.
pub(crate) fn body_cost(
    symbol: u32,
    options: &InlineOptions,
    active: &mut HashSet<u32>,
) -> Result<u32, DeclineReason> {
    if !active.insert(symbol) {
        return Err(DeclineReason::Recursive);
    }
    let result = (|| {
        let body = options.body(symbol).ok_or(DeclineReason::UnsupportedBody)?;
        if body.variadic
            || body.has_env
            || body.max_args != Some(body.arity)
            || body.min_args != body.arity
            // Until the inline boundary grows an explicit checked assertion,
            // cloning a declared body would bypass the callee-entry validator.
            || body.param_types.iter().any(|ty| !ty.is_any())
        {
            return Err(DeclineReason::UnsupportedBody);
        }

        let mut cost = 0u32;
        for op in &body.code {
            cost = cost.saturating_add(1);
            match op {
                Instr::Const(_)
                | Instr::LoadLocal(_)
                | Instr::StoreLocal(_)
                | Instr::Pop
                | Instr::Dup
                | Instr::Br(_)
                | Instr::BrIfFalse(_)
                | Instr::BrIfTrue(_)
                | Instr::Return => {}
                Instr::CallNamed { sym, nargs } => {
                    if let Some(m) = metadata_for_symbol(*sym) {
                        if *nargs != m.fixed_arity
                            || m.function == KnownFunction::Typep
                            || !m.effects.pure
                            || m.effects.allocates
                            || m.effects.may_signal
                        {
                            return Err(DeclineReason::UnsupportedBody);
                        }
                        cost = cost.saturating_add(m.cost);
                    } else {
                        let nested = options.body(*sym).ok_or(DeclineReason::UnsupportedBody)?;
                        if *nargs != nested.arity {
                            return Err(DeclineReason::WrongArity);
                        }
                        cost = cost.saturating_add(body_cost(*sym, options, active)?);
                    }
                }
                _ => return Err(DeclineReason::UnsupportedBody),
            }
        }
        Ok(cost)
    })();
    active.remove(&symbol);
    result
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum InlineDecision {
    Expand(IntrinsicId),
    Decline(DeclineReason),
}

/// Decide legality and profitability for one call site.  Hard constraints are
/// checked before the explicit INLINE preference; INLINE never overrides
/// wrong arity, effects, depth, or the compilation growth budget.
pub fn decide(
    metadata: &InlineMetadata,
    nargs: u16,
    policy: InlinePolicy,
    hot: bool,
    depth: u8,
    remaining_budget: u32,
    config: InlineConfig,
) -> InlineDecision {
    if policy == InlinePolicy::NotInline {
        return InlineDecision::Decline(DeclineReason::NotInline);
    }
    if nargs != metadata.fixed_arity {
        return InlineDecision::Decline(DeclineReason::WrongArity);
    }
    if !metadata.effects.pure {
        return InlineDecision::Decline(DeclineReason::Impure);
    }
    if metadata.effects.allocates {
        return InlineDecision::Decline(DeclineReason::Allocates);
    }
    if metadata.effects.may_signal {
        return InlineDecision::Decline(DeclineReason::MaySignal);
    }
    if depth >= config.max_depth {
        return InlineDecision::Decline(DeclineReason::DepthLimit);
    }
    if metadata.cost > remaining_budget {
        return InlineDecision::Decline(DeclineReason::Budget);
    }
    if metadata.cost <= config.small_threshold || hot || policy == InlinePolicy::Inline {
        InlineDecision::Expand(metadata.expansion)
    } else {
        InlineDecision::Decline(DeclineReason::NotProfitable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egcl_rt::value::NIL;

    fn saved_body(code: Vec<Instr>) -> Arc<BytecodeFunction> {
        Arc::new(BytecodeFunction {
            self_call_sites: Vec::new(),
            code,
            constants: vec![],
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
            name: "INLINE-FILTER-FIXTURE".into(),
            params_form: NIL,
            min_args: 1,
            max_args: Some(1),
            variadic: false,
            binder_cache: Default::default(),
        })
    }

    fn eq() -> &'static InlineMetadata {
        KNOWN
            .iter()
            .find(|m| m.function == KnownFunction::Eq)
            .unwrap()
    }

    #[test]
    fn transfer_options_keep_only_constant_facts_and_safe_snapshots() {
        let root = 17;
        let source = saved_body(vec![Instr::LoadLocal(0), Instr::Return]);
        let snapshot = super::super::constants::BytecodeSnapshot::capture(&source);
        let mut options = InlineOptions::default().with_root_symbol(root)
            .with_constant_globals(HashMap::from([(42, 5)]))
            .with_body_snapshot(99, &snapshot)
            .with_policy(3, InlinePolicy::NotInline)
            .with_call_site_profile(root, 3, 100, 100);
        options.config.node_budget = 0;
        let filtered = options.constant_globals_only(root);
        assert_eq!(filtered.constant_global(root, 42), Some(5));
        assert_eq!(filtered.config, InlineConfig::default());
        assert!(filtered.bodies.is_empty());
        assert!(filtered.policies.is_empty());
        assert!(filtered.call_sites.is_empty());
        assert_eq!(filtered.constant_snapshots.len(), 1);
    }

    #[test]
    fn hard_limits_and_notinline_win() {
        let config = InlineConfig::default();
        assert_eq!(
            decide(eq(), 2, InlinePolicy::NotInline, true, 0, 500, config),
            InlineDecision::Decline(DeclineReason::NotInline)
        );
        assert_eq!(
            decide(eq(), 1, InlinePolicy::Inline, true, 0, 500, config),
            InlineDecision::Decline(DeclineReason::WrongArity)
        );
        assert_eq!(
            decide(
                eq(),
                2,
                InlinePolicy::Inline,
                true,
                config.max_depth,
                500,
                config
            ),
            InlineDecision::Decline(DeclineReason::DepthLimit)
        );
        assert_eq!(
            decide(eq(), 2, InlinePolicy::Inline, true, 0, 0, config),
            InlineDecision::Decline(DeclineReason::Budget)
        );
    }

    #[test]
    fn inline_overrides_profitability_but_not_budget() {
        let mut large = *eq();
        large.cost = 40;
        let config = InlineConfig::default();
        assert_eq!(
            decide(&large, 2, InlinePolicy::Unspecified, false, 0, 500, config),
            InlineDecision::Decline(DeclineReason::NotProfitable)
        );
        assert_eq!(
            decide(&large, 2, InlinePolicy::Inline, false, 0, 500, config),
            InlineDecision::Expand(IntrinsicId::Eq)
        );
        assert_eq!(
            decide(&large, 2, InlinePolicy::Unspecified, true, 0, 500, config),
            InlineDecision::Expand(IntrinsicId::Eq),
            "a hot site receives the larger budget-limited allowance"
        );
    }

    #[test]
    fn call_site_hotness_uses_runtime_frequency_ratio() {
        let caller = egcl_rt::symbols::intern("PROFILED-INLINE-CALLER");
        let options = InlineOptions::default()
            .with_call_site_profile(caller, 7, 7, 10)
            .with_call_site_profile(caller, 8, 8, 10);
        assert!(!options.call_site_is_hot(caller, 7));
        assert!(options.call_site_is_hot(caller, 8));
    }

    #[test]
    fn symbol_lookup_returns_stable_identity_and_rejects_other_packages() {
        let eq_symbol = egcl_rt::symbols::intern("EQ");
        assert_eq!(
            metadata_for_symbol(eq_symbol).map(|m| m.function),
            Some(KnownFunction::Eq)
        );

        let shadow = egcl_rt::symbols::intern("SOME-OTHER-PACKAGE:EQ");
        assert_eq!(metadata_for_symbol(shadow), None);

        for (name, function) in [("CAR", KnownFunction::Car), ("CDR", KnownFunction::Cdr)] {
            assert_eq!(
                metadata_for_symbol(egcl_rt::symbols::intern(name)).map(|m| m.function),
                Some(function)
            );
            assert_eq!(
                metadata_for_symbol(egcl_rt::symbols::intern(&format!("OTHER:{name}"))),
                None
            );
        }

        let first_char = egcl_rt::symbols::intern("UIOP/UTILITY:FIRST-CHAR");
        assert_eq!(metadata_for_symbol(first_char), None);
        assert_eq!(
            metadata_for_symbol(egcl_rt::symbols::intern("FIRST-CHAR")),
            None
        );
        assert_eq!(
            metadata_for_symbol(egcl_rt::symbols::intern("OTHER:FIRST-CHAR")),
            None
        );
    }

    #[test]
    fn saved_body_filter_rejects_effects_closures_nlx_and_variadic_lambdas() {
        let symbols = [
            egcl_rt::symbols::intern("INLINE-EFFECT"),
            egcl_rt::symbols::intern("INLINE-CLOSURE"),
            egcl_rt::symbols::intern("INLINE-NLX"),
            egcl_rt::symbols::intern("INLINE-VARIADIC"),
            egcl_rt::symbols::intern("INLINE-DECLARED"),
        ];
        let mut variadic = saved_body(vec![Instr::LoadLocal(0), Instr::Return]);
        Arc::get_mut(&mut variadic).unwrap().variadic = true;
        Arc::get_mut(&mut variadic).unwrap().max_args = None;
        let mut declared = saved_body(vec![Instr::LoadLocal(0), Instr::Return]);
        Arc::get_mut(&mut declared).unwrap().param_types =
            vec![egcl_rt::bytecode::DeclaredType::Fixnum];
        let options = InlineOptions::default()
            .with_body(
                symbols[0],
                saved_body(vec![Instr::StoreGlobal(7), Instr::Return]),
            )
            .with_body(
                symbols[1],
                saved_body(vec![Instr::MakeClosureEnv(0), Instr::Return]),
            )
            .with_body(symbols[2], saved_body(vec![Instr::Throw, Instr::Return]))
            .with_body(symbols[3], variadic)
            .with_body(symbols[4], declared);

        for symbol in symbols {
            assert_eq!(
                body_cost(symbol, &options, &mut HashSet::new()),
                Err(DeclineReason::UnsupportedBody)
            );
        }
    }
}
