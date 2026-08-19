//! T2 call-site inlining policy and compiler-known function metadata.
//!
//! This is deliberately separate from the SSA builder.  The builder asks this
//! module what a function *is* and whether a particular call site may expand;
//! it does not grow a second list of magic CL names.  `IntrinsicId` is the
//! initial expansion representation.  A later body inliner can add a saved SSA
//! body without changing the policy or metadata contracts.

use std::collections::HashMap;

/// Stable compiler identity for a function with an inline expansion.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum KnownFunction {
    Eq,
    Null,
    Consp,
    Symbolp,
    Integerp,
    Typep,
}

/// Expansion hook understood by the T2 builder.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum IntrinsicId {
    Eq,
    Null,
    Consp,
    Symbolp,
    Integerp,
    TypepConstant,
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
        function: KnownFunction::Consp,
        name: "CONSP",
        fixed_arity: 1,
        effects: EffectSummary::PURE_TOTAL,
        cost: 1,
        expansion: IntrinsicId::Consp,
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
];

/// Resolve an interned symbol to compiler metadata.  Unqualified names are the
/// canonical representation used for inherited COMMON-LISP symbols; explicit
/// CL/COMMON-LISP qualification is accepted.  A same-spelling symbol in some
/// other package is not the CL function and must remain a normal call.
pub fn metadata_for_symbol(sym: u32) -> Option<&'static InlineMetadata> {
    let name = crate::reader::symbol_name(sym)?;
    let canonical = match name.rsplit_once(':') {
        None => name.as_str(),
        Some((package, bare))
            if matches!(package.trim_end_matches(':'), "CL" | "COMMON-LISP") => bare,
        Some(_) => return None,
    };
    KNOWN.iter().find(|m| m.name == canonical)
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
}

impl Default for InlineConfig {
    fn default() -> Self {
        Self {
            small_threshold: 30,
            max_depth: 6,
            node_budget: 500,
        }
    }
}

/// Per-compilation options, including lexical declaration decisions keyed by
/// the bytecode call-site PC.  Source lowering can populate this map later;
/// tests and compiler clients can already exercise the exact policy contract.
#[derive(Clone, Debug, Default)]
pub struct InlineOptions {
    pub config: InlineConfig,
    policies: HashMap<u32, InlinePolicy>,
}

impl InlineOptions {
    pub fn with_policy(mut self, bcp: u32, policy: InlinePolicy) -> Self {
        self.policies.insert(bcp, policy);
        self
    }

    pub fn policy_at(&self, bcp: u32) -> InlinePolicy {
        self.policies.get(&bcp).copied().unwrap_or_default()
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
    if metadata.cost <= config.small_threshold || policy == InlinePolicy::Inline {
        InlineDecision::Expand(metadata.expansion)
    } else {
        InlineDecision::Decline(DeclineReason::NotProfitable)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
            decide(eq(), 2, InlinePolicy::NotInline, 0, 500, config),
            InlineDecision::Decline(DeclineReason::NotInline)
        );
        assert_eq!(
            decide(eq(), 1, InlinePolicy::Inline, 0, 500, config),
            InlineDecision::Decline(DeclineReason::WrongArity)
        );
        assert_eq!(
            decide(eq(), 2, InlinePolicy::Inline, config.max_depth, 500, config),
            InlineDecision::Decline(DeclineReason::DepthLimit)
        );
        assert_eq!(
            decide(eq(), 2, InlinePolicy::Inline, 0, 0, config),
            InlineDecision::Decline(DeclineReason::Budget)
        );
    }

    #[test]
    fn inline_overrides_profitability_but_not_budget() {
        let mut large = *eq();
        large.cost = 40;
        let config = InlineConfig::default();
        assert_eq!(
            decide(&large, 2, InlinePolicy::Unspecified, 0, 500, config),
            InlineDecision::Decline(DeclineReason::NotProfitable)
        );
        assert_eq!(
            decide(&large, 2, InlinePolicy::Inline, 0, 500, config),
            InlineDecision::Expand(IntrinsicId::Eq)
        );
    }

    #[test]
    fn symbol_lookup_returns_stable_identity_and_rejects_other_packages() {
        let eq_symbol = bliss_rt::symbols::intern("EQ");
        assert_eq!(metadata_for_symbol(eq_symbol).map(|m| m.function), Some(KnownFunction::Eq));

        let shadow = bliss_rt::symbols::intern("SOME-OTHER-PACKAGE:EQ");
        assert_eq!(metadata_for_symbol(shadow), None);
    }
}
