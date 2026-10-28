// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Mathematical integer result intervals, independent of profiles/representation.
//! A disjoint interval vetoes a doomed fixnum guess; an overlapping interval
//! never licenses removing a guard. Callers supply structurally proved integer
//! ranges at the call site, not sampled ranges or floating-point bounds.

use super::ir::Range;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Add,
    Sub,
    Mul,
    Neg,
    Increment,
    Decrement,
}

impl Operation {
    /// Exact fixed-standard identity only. Shadowed/opaque or variadic calls
    /// keep their existing policy and semantics, including unsupported arity.
    pub(crate) fn for_call(symbol: u32, arity: usize) -> Option<Self> {
        if super::builtin_binding::snapshot(symbol)
            != Some(super::builtin_binding::Binding::FixedStandard)
        {
            return None;
        }
        match (egcl_rt::symbols::registry_key(symbol)?.as_str(), arity) {
            ("+", 2) => Some(Self::Add),
            ("-", 2) => Some(Self::Sub),
            ("*", 2) => Some(Self::Mul),
            ("-", 1) => Some(Self::Neg),
            ("1+", 1) => Some(Self::Increment),
            ("1-", 1) => Some(Self::Decrement),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MathematicalRange {
    pub lo: i128,
    pub hi: i128,
}

/// Two i64 bounds multiply within signed i128, including MIN*MIN=2^126.
/// Widen BEFORE arithmetic: saturation to an i64 endpoint could falsely make
/// an overflowing result overlap the machine fixnum range.
pub(crate) fn result_range(operation: Operation, operands: &[Range]) -> Option<MathematicalRange> {
    if operands.iter().any(|range| range.lo > range.hi) {
        return None;
    }
    let range = match (operation, operands) {
        (Operation::Add, [a, b]) => MathematicalRange {
            lo: i128::from(a.lo) + i128::from(b.lo),
            hi: i128::from(a.hi) + i128::from(b.hi),
        },
        (Operation::Sub, [a, b]) => MathematicalRange {
            lo: i128::from(a.lo) - i128::from(b.hi),
            hi: i128::from(a.hi) - i128::from(b.lo),
        },
        (Operation::Mul, [a, b]) => {
            let corners = [
                i128::from(a.lo) * i128::from(b.lo),
                i128::from(a.lo) * i128::from(b.hi),
                i128::from(a.hi) * i128::from(b.lo),
                i128::from(a.hi) * i128::from(b.hi),
            ];
            MathematicalRange {
                lo: *corners.iter().min()?,
                hi: *corners.iter().max()?,
            }
        }
        (Operation::Neg, [a]) => MathematicalRange {
            lo: -i128::from(a.hi),
            hi: -i128::from(a.lo),
        },
        (Operation::Increment, [a]) => MathematicalRange {
            lo: i128::from(a.lo) + 1,
            hi: i128::from(a.hi) + 1,
        },
        (Operation::Decrement, [a]) => MathematicalRange {
            lo: i128::from(a.lo) - 1,
            hi: i128::from(a.hi) - 1,
        },
        _ => return None,
    };
    Some(range)
}

pub(crate) fn excludes_fixnum_result(operation: Operation, operands: &[Range]) -> bool {
    result_range(operation, operands).is_some_and(|range| {
        range.hi < i128::from(egcl_rt::bignum::FIXNUM_MIN)
            || range.lo > i128::from(egcl_rt::bignum::FIXNUM_MAX)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    const MIN: i64 = egcl_rt::bignum::FIXNUM_MIN;
    const MAX: i64 = egcl_rt::bignum::FIXNUM_MAX;
    fn exact(value: i64) -> Range {
        Range {
            lo: value,
            hi: value,
        }
    }

    #[test]
    fn wholly_outside_intervals_reject_doomed_fixnum_results() {
        for (op, args) in [
            (Operation::Add, vec![exact(MAX), Range { lo: 1, hi: 7 }]),
            (Operation::Sub, vec![exact(MIN), Range { lo: 1, hi: 7 }]),
            (
                Operation::Mul,
                vec![
                    Range {
                        lo: MAX / 2 + 1,
                        hi: MAX,
                    },
                    exact(2),
                ],
            ),
            (Operation::Mul, vec![exact(MIN), exact(-1)]),
            (Operation::Neg, vec![exact(MIN)]),
            (Operation::Increment, vec![exact(MAX)]),
            (Operation::Decrement, vec![exact(MIN)]),
        ] {
            assert!(excludes_fixnum_result(op, &args), "{op:?} {args:?}");
        }
    }

    #[test]
    fn overlap_endpoints_zero_and_negative_products_do_not_veto() {
        for (op, args) in [
            (
                Operation::Add,
                vec![
                    Range {
                        lo: MAX - 1,
                        hi: MAX,
                    },
                    exact(1),
                ],
            ),
            (
                Operation::Sub,
                vec![
                    Range {
                        lo: MIN,
                        hi: MIN + 1,
                    },
                    exact(1),
                ],
            ),
            (Operation::Add, vec![exact(MAX), exact(0)]),
            (Operation::Sub, vec![exact(MIN), exact(0)]),
            (Operation::Mul, vec![Range { lo: MIN, hi: MAX }, exact(0)]),
            (
                Operation::Mul,
                vec![Range { lo: -100, hi: 50 }, Range { lo: -3, hi: 2 }],
            ),
            (Operation::Neg, vec![exact(MAX)]),
        ] {
            assert!(!excludes_fixnum_result(op, &args), "{op:?} {args:?}");
        }
        assert_eq!(
            result_range(
                Operation::Mul,
                &[Range { lo: -100, hi: 50 }, Range { lo: -3, hi: 2 }]
            ),
            Some(MathematicalRange { lo: -200, hi: 300 })
        );
    }

    #[test]
    fn widened_arithmetic_and_invalid_shapes_remain_conservative() {
        assert_eq!(
            result_range(Operation::Mul, &[exact(i64::MIN), exact(i64::MIN)]),
            Some(MathematicalRange {
                lo: 1i128 << 126,
                hi: 1i128 << 126
            })
        );
        assert_eq!(
            result_range(Operation::Neg, &[exact(i64::MIN)]),
            Some(MathematicalRange {
                lo: 1i128 << 63,
                hi: 1i128 << 63
            })
        );
        for args in [
            vec![],
            vec![exact(1)],
            vec![exact(1), exact(2), exact(3)],
            vec![Range { lo: 9, hi: 2 }, exact(1)],
        ] {
            assert!(!excludes_fixnum_result(Operation::Add, &args));
        }
    }

    #[test]
    fn operation_selection_requires_canonical_identity_and_supported_arity() {
        for (name, arity, expected) in [
            ("+", 2, Some(Operation::Add)),
            ("-", 1, Some(Operation::Neg)),
            ("1+", 1, Some(Operation::Increment)),
            ("+", 3, None),
            ("RESULT-FACTS-USER::+", 2, None),
        ] {
            assert_eq!(
                Operation::for_call(egcl_rt::symbols::intern(name), arity),
                expected
            );
        }
    }
}
