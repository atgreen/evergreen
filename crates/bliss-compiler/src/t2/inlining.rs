//! T2 call-site inlining policy and compiler-known function metadata.
//!
//! This is deliberately separate from the SSA builder.  The builder asks this
//! module what a function *is* and whether a particular call site may expand;
//! it does not grow a second list of magic CL names.  `IntrinsicId` is the
//! leaf expansion representation; saved bytecode bodies are built to SSA and
//! cloned as CFGs through the same legality, depth, and growth policy.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bliss_rt::bytecode::{BytecodeFunction, Instr};

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
    FirstChar,
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
    /// Metadata-owned inline template for UIOP/UTILITY:FIRST-CHAR.  It expands
    /// to the low-level string layout IR; the emitter has no FIRST-CHAR case.
    FirstChar,
}

/// Namespace owning the function identity.  Unqualified names denote inherited
/// COMMON-LISP symbols, while non-CL helpers must match their package exactly.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum FunctionNamespace {
    CommonLisp,
    Package(&'static str),
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
    pub namespace: FunctionNamespace,
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
        namespace: FunctionNamespace::CommonLisp,
        name: "EQ",
        fixed_arity: 2,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Eq,
    },
    InlineMetadata {
        function: KnownFunction::Null,
        namespace: FunctionNamespace::CommonLisp,
        name: "NULL",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Null,
    },
    InlineMetadata {
        function: KnownFunction::Car,
        namespace: FunctionNamespace::CommonLisp,
        name: "CAR",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Car,
    },
    InlineMetadata {
        function: KnownFunction::Cdr,
        namespace: FunctionNamespace::CommonLisp,
        name: "CDR",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 2,
        expansion: IntrinsicId::Cdr,
    },
    InlineMetadata {
        function: KnownFunction::Consp,
        namespace: FunctionNamespace::CommonLisp,
        name: "CONSP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Consp,
    },
    InlineMetadata {
        function: KnownFunction::Symbolp,
        namespace: FunctionNamespace::CommonLisp,
        name: "SYMBOLP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Symbolp,
    },
    InlineMetadata {
        function: KnownFunction::Integerp,
        namespace: FunctionNamespace::CommonLisp,
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
        namespace: FunctionNamespace::CommonLisp,
        name: "TYPEP",
        fixed_arity: 2,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::TypepConstant,
    },
    InlineMetadata {
        function: KnownFunction::Stringp,
        namespace: FunctionNamespace::CommonLisp,
        name: "STRINGP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Stringp,
    },
    InlineMetadata {
        function: KnownFunction::FirstChar,
        namespace: FunctionNamespace::Package("UIOP/UTILITY"),
        name: "FIRST-CHAR",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 3,
        expansion: IntrinsicId::FirstChar,
    },
];

/// Resolve an interned symbol to compiler metadata.  Unqualified names are the
/// canonical representation used for inherited COMMON-LISP symbols; explicit
/// CL/COMMON-LISP qualification is accepted.  A same-spelling symbol in some
/// other package is not the CL function and must remain a normal call.
pub fn metadata_for_symbol(sym: u32) -> Option<&'static InlineMetadata> {
    let name = crate::reader::symbol_name(sym)?;
    let (package, bare) = match bliss_rt::symbols::split_registry_key(&name) {
        None => (None, name.as_str()),
        Some((package, bare)) => (Some(package), bare),
    };
    KNOWN.iter().find(|m| {
        if m.name != bare {
            return false;
        }
        match m.namespace {
            FunctionNamespace::CommonLisp => {
                package.is_none() || matches!(package, Some("CL" | "COMMON-LISP"))
            }
            FunctionNamespace::Package(owner) => package == Some(owner),
        }
    })
}

/// Lexical declaration in force at an individual call site.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub enum InlinePolicy {
    #[default]
    Unspecified,
    Inline,
    NotInline,
}

/// Profitability limits.  Depth follows spec 04-05.  The 500-node budget is
/// the current T2 post-inline cap from spec 04-04; the larger 04-05 value needs
/// a separate spec reconciliation before arbitrary body cloning lands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct InlineConfig {
    pub small_threshold: u32,
    pub max_depth: u8,
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
    call_sites: HashMap<(u32, u32), CallSiteProfile>,
    root_symbol: Option<u32>,
}

impl InlineOptions {
    pub fn with_policy(mut self, bcp: u32, policy: InlinePolicy) -> Self {
        self.policies.insert(bcp, policy);
        self
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
    /// registered is not enough: [`body_cost`] still applies the conservative
    /// fixed-arity/purity/effect filter before the body may be cloned.
    pub fn with_body(mut self, symbol: u32, body: Arc<BytecodeFunction>) -> Self {
        self.bodies.insert(symbol, body);
        self
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
    use bliss_rt::value::NIL;

    fn saved_body(code: Vec<Instr>) -> Arc<BytecodeFunction> {
        Arc::new(BytecodeFunction {
            code,
            constants: vec![],
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
        })
    }

    fn eq() -> &'static InlineMetadata {
        KNOWN
            .iter()
            .find(|m| m.function == KnownFunction::Eq)
            .unwrap()
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
        let caller = bliss_rt::symbols::intern("PROFILED-INLINE-CALLER");
        let options = InlineOptions::default()
            .with_call_site_profile(caller, 7, 7, 10)
            .with_call_site_profile(caller, 8, 8, 10);
        assert!(!options.call_site_is_hot(caller, 7));
        assert!(options.call_site_is_hot(caller, 8));
    }

    #[test]
    fn symbol_lookup_returns_stable_identity_and_rejects_other_packages() {
        let eq_symbol = bliss_rt::symbols::intern("EQ");
        assert_eq!(
            metadata_for_symbol(eq_symbol).map(|m| m.function),
            Some(KnownFunction::Eq)
        );

        let shadow = bliss_rt::symbols::intern("SOME-OTHER-PACKAGE:EQ");
        assert_eq!(metadata_for_symbol(shadow), None);

        for (name, function) in [("CAR", KnownFunction::Car), ("CDR", KnownFunction::Cdr)] {
            assert_eq!(
                metadata_for_symbol(bliss_rt::symbols::intern(name)).map(|m| m.function),
                Some(function)
            );
            assert_eq!(
                metadata_for_symbol(bliss_rt::symbols::intern(&format!("OTHER:{name}"))),
                None
            );
        }

        let first_char = bliss_rt::symbols::intern("UIOP/UTILITY:FIRST-CHAR");
        assert_eq!(
            metadata_for_symbol(first_char).map(|m| m.function),
            Some(KnownFunction::FirstChar)
        );
        assert_eq!(
            metadata_for_symbol(bliss_rt::symbols::intern("FIRST-CHAR")),
            None
        );
        assert_eq!(
            metadata_for_symbol(bliss_rt::symbols::intern("OTHER:FIRST-CHAR")),
            None
        );
    }

    #[test]
    fn saved_body_filter_rejects_effects_closures_nlx_and_variadic_lambdas() {
        let symbols = [
            bliss_rt::symbols::intern("INLINE-EFFECT"),
            bliss_rt::symbols::intern("INLINE-CLOSURE"),
            bliss_rt::symbols::intern("INLINE-NLX"),
            bliss_rt::symbols::intern("INLINE-VARIADIC"),
            bliss_rt::symbols::intern("INLINE-DECLARED"),
        ];
        let mut variadic = saved_body(vec![Instr::LoadLocal(0), Instr::Return]);
        Arc::get_mut(&mut variadic).unwrap().variadic = true;
        Arc::get_mut(&mut variadic).unwrap().max_args = None;
        let mut declared = saved_body(vec![Instr::LoadLocal(0), Instr::Return]);
        Arc::get_mut(&mut declared).unwrap().param_types =
            vec![bliss_rt::bytecode::DeclaredType::Fixnum];
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
