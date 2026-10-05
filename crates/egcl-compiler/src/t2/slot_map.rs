// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! The single per-safepoint producer of `{which interpreter slots are live,
//! in what order, in what machine representation}` (bliss-ht4).
//!
//! # Purpose
//!
//! One [`FrameState`] at a safepoint is read in two directions:
//!
//! * **Export** (deopt.rs, and the native emitters' guard-deopt paths): lower
//!   the frame state to the descriptors that rebuild an interpreter frame on a
//!   failed guard. Each slot gets a rebox step derived from its representation.
//! * **Import** (the OSR entry path): transfer a running lower-tier frame into
//!   optimised code at a loop header. Each slot gets a conversion derived from
//!   the same representation.
//!
//! If the two sides derived liveness, order, or representation by separate
//! code, they would drift: one calling a slot `UnboxedFixnum` while the other
//! calls it `Tagged` is a miscompile that no non-OSR test observes, because the
//! export path runs constantly and the import path only on a loop that actually
//! OSRs. This module is the one place both sides obtain those facts, so they
//! can disagree only by bypassing it.
//!
//! # Contract
//!
//! * [`slot_specs`] enumerates one [`FrameScope`]'s slots as [`SlotSpec`]s:
//!   locals first in slot order, then operand-stack slots bottom to top. The
//!   order is part of the contract: `LoweredScope::slots` / `num_locals` on the
//!   export side and the OSR import list both index into it, so a change here
//!   moves both consumers together.
//! * [`source_repr`] is the only function that answers "what representation
//!   does this slot have". `Value` reports its own `repr`; `Const` and
//!   `Unbound` are `Tagged` by construction (an immediate and the unbound
//!   marker are never raw machine words); `Remat` reports the recipe's declared
//!   `result_repr`.
//! * The producer is total. A dangling `Remat` id is a malformed frame state
//!   that deopt lowering rejects, but `source_repr` still reports `Tagged` for
//!   it so both consumers see the same slot count and fail on the same slot.
//! * `SlotSpec.repr` is the machine representation only. It does not decide
//!   GC-rootness: a `Const` heap literal is `Tagged` yet rooted by the constant
//!   pool, and a `Remat` slot is recomputed cold rather than scanned. The
//!   live-ref rule used by the export side is
//!   `matches!(source, ValueSource::Value { .. }) && repr == Tagged`.
//!
//! The unit tests pin the type-level half of the invariant: for every
//! `ValueRepresentation`, the export rebox and the import conversion must be
//! inverses, so a representation that only one side learns about fails here
//! rather than in an OSR'd loop.

use crate::t2::frame_state::{FrameScope, FrameState, ValueSource};
use crate::t2::ir::ValueRepresentation;

/// Representation conversion applied to one slot when a lower-tier frame is
/// imported into T2 code at an OSR entry.
#[derive(Clone, Debug, PartialEq)]
pub enum ConversionKind {
    /// No conversion needed.
    None,
    /// Unbox a tagged fixnum to a raw i64.
    UnboxFixnum,
    /// Unbox a tagged float to a raw f64.
    UnboxFloat,
    /// Box a raw i64 into a tagged fixnum.
    BoxFixnum,
    /// Box a raw f64 into a tagged float.
    BoxFloat,
    /// Widen a 32-bit integer to 64-bit.
    WidenI32ToI64,
}

impl ConversionKind {
    /// The OSR *import* conversion implied by a T2 value's machine
    /// representation — the exact inverse of the deopt *export*
    /// [`crate::t2::deopt::Rebox::for_repr`] (bliss-ht4).
    ///
    /// Both directions are derived here and there rather than by separate
    /// ad-hoc matches, or a representation one side learns about and the other
    /// does not becomes a miscompile visible only on an OSR'd loop.
    ///
    /// Returns `None` when this conversion vocabulary cannot express the
    /// import. That is the case for `UnboxedF64`: export reboxes it by
    /// heap-allocating a double-float, but `ConversionKind` distinguishes only
    /// `UnboxFloat` (the single-float immediate), so there is no inverse. A
    /// caller that gets `None` must decline the OSR entry rather than transfer
    /// the slot unconverted.
    pub fn for_repr(repr: ValueRepresentation) -> Option<ConversionKind> {
        use ValueRepresentation as R;
        match repr {
            R::Tagged => Some(ConversionKind::None),
            R::UnboxedFixnum => Some(ConversionKind::UnboxFixnum),
            R::UnboxedF32 => Some(ConversionKind::UnboxFloat),
            R::UnboxedF64 => None,
        }
    }
}

/// One interpreter slot at a safepoint, with the representation both the deopt
/// export and the OSR import must agree on.
#[derive(Clone, Debug)]
pub struct SlotSpec {
    /// Position in the shared enumeration (locals first, then operand stack).
    pub index: u32,
    /// True for an interpreter local, false for an operand-stack slot.
    pub is_local: bool,
    /// Where the value comes from (the export side's view).
    pub source: ValueSource,
    /// The machine representation of that value — the single fact this module
    /// exists to keep consistent between import and export.
    ///
    /// Note this is the *machine* representation only. It does not by itself
    /// decide GC-rootness: a `Const` heap literal is `Tagged` yet is rooted
    /// immortally in the constant pool, and a `Remat` slot is recomputed on the
    /// cold path rather than scanned. The export-side live-ref rule is
    /// `matches!(source, ValueSource::Value { .. }) && repr == Tagged`.
    pub repr: ValueRepresentation,
}

/// The representation of one slot source. This is the *only* function that
/// answers "what representation is this slot?", for either direction.
///
/// A `Const` or an `Unbound` slot is by construction a tagged interpreter
/// datum: `Const` materialises a `EgclVal` immediate and `Unbound` writes the
/// unbound marker, so neither is ever a raw machine word. A `Remat` slot's
/// representation is the recipe's declared result representation.
pub fn source_repr(source: &ValueSource, fs: &FrameState) -> ValueRepresentation {
    match source {
        ValueSource::Value { repr, .. } => *repr,
        ValueSource::Const(_) | ValueSource::Unbound => ValueRepresentation::Tagged,
        ValueSource::Remat(id) => fs
            .remat
            .get(id.0 as usize)
            .map(|r| r.result_repr)
            // A dangling recipe id is a malformed FrameState; deopt lowering
            // rejects it with LowerError::BadRematId. Reporting Tagged here
            // keeps this producer total so both consumers see the same slot
            // count and can fail on the same slot rather than on different ones.
            .unwrap_or(ValueRepresentation::Tagged),
    }
}

/// Enumerate one logical frame's live slots in the shared order: locals in slot
/// order, then operand-stack slots bottom-to-top.
pub fn slot_specs(scope: &FrameScope, fs: &FrameState) -> Vec<SlotSpec> {
    scope
        .locals
        .iter()
        .map(|s| (s, true))
        .chain(scope.stack.iter().map(|s| (s, false)))
        .enumerate()
        .map(|(index, (source, is_local))| SlotSpec {
            index: index as u32,
            is_local,
            source: source.clone(),
            repr: source_repr(source, fs),
        })
        .collect()
}

/// How many of `slot_specs`' entries are interpreter locals.
pub fn num_locals(scope: &FrameScope) -> usize {
    scope.locals.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::t2::deopt::Rebox;
    use crate::t2::frame_state::{RematOp, RematRecipe, RematRecipeId};
    use crate::t2::ir::Value;
    use egcl_rt::value::EgclVal;

    const ALL_REPRS: [ValueRepresentation; 4] = [
        ValueRepresentation::Tagged,
        ValueRepresentation::UnboxedFixnum,
        ValueRepresentation::UnboxedF32,
        ValueRepresentation::UnboxedF64,
    ];

    /// The bliss-ht4 invariant at the type level: for every representation, the
    /// OSR import conversion and the deopt export rebox must be inverses. A new
    /// `ValueRepresentation` variant that only one side learns about fails here
    /// rather than silently miscompiling an OSR'd loop.
    #[test]
    fn import_and_export_conversions_are_inverses_for_every_representation() {
        for repr in ALL_REPRS {
            let export = Rebox::for_repr(repr);
            let import = ConversionKind::for_repr(repr);
            match (repr, export, import) {
                (ValueRepresentation::Tagged, Rebox::None, Some(ConversionKind::None)) => {}
                (
                    ValueRepresentation::UnboxedFixnum,
                    Rebox::ReboxFixnum,
                    Some(ConversionKind::UnboxFixnum),
                ) => {}
                (
                    ValueRepresentation::UnboxedF32,
                    Rebox::ReboxF32,
                    Some(ConversionKind::UnboxFloat),
                ) => {}
                // Export can rebox an f64 (it heap-allocates a double-float);
                // the `ConversionKind` vocabulary has no single/double
                // distinction, so there is no import conversion that can undo
                // it. `None` records that honestly and the OSR entry is
                // declined rather than emitted wrong.
                (ValueRepresentation::UnboxedF64, Rebox::ReboxF64, None) => {}
                other => panic!("import/export disagree for {repr:?}: {other:?}"),
            }
        }
    }

    #[test]
    fn slot_specs_enumerates_locals_then_stack_with_resolved_representations() {
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 7,
                locals: vec![
                    ValueSource::Value {
                        value: Value(0),
                        repr: ValueRepresentation::UnboxedFixnum,
                    },
                    ValueSource::Const(EgclVal::from_fixnum(3)),
                    ValueSource::Unbound,
                ],
                stack: vec![ValueSource::Remat(RematRecipeId(0))],
            }],
            remat: vec![RematRecipe {
                op: RematOp::UnboxFixnum,
                inputs: vec![],
                result_repr: ValueRepresentation::UnboxedFixnum,
            }],
        };
        let scope = &fs.scopes[0];
        let specs = slot_specs(scope, &fs);

        assert_eq!(specs.len(), 4);
        assert_eq!(num_locals(scope), 3);
        // Locals first, in order, then the operand stack.
        assert_eq!(
            specs.iter().map(|s| s.is_local).collect::<Vec<_>>(),
            [true, true, true, false]
        );
        assert_eq!(
            specs.iter().map(|s| s.index).collect::<Vec<_>>(),
            [0, 1, 2, 3]
        );
        // Representations are resolved per source kind, not assumed.
        assert_eq!(specs[0].repr, ValueRepresentation::UnboxedFixnum);
        assert_eq!(
            specs[1].repr,
            ValueRepresentation::Tagged,
            "Const is tagged"
        );
        assert_eq!(
            specs[2].repr,
            ValueRepresentation::Tagged,
            "Unbound writes the tagged unbound marker"
        );
        assert_eq!(
            specs[3].repr,
            ValueRepresentation::UnboxedFixnum,
            "Remat takes the recipe's result representation"
        );
    }

    /// A dangling remat id must not change the slot COUNT — both consumers have
    /// to see the same frame shape so they fail on the same slot.
    #[test]
    fn dangling_remat_id_still_yields_a_slot() {
        let fs = FrameState {
            scopes: vec![FrameScope {
                function: 0,
                bcp: 0,
                locals: vec![ValueSource::Remat(RematRecipeId(99))],
                stack: vec![],
            }],
            remat: vec![],
        };
        let specs = slot_specs(&fs.scopes[0], &fs);
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].repr, ValueRepresentation::Tagged);
    }
}
