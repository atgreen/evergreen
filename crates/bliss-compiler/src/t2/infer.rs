//! P3 — Type / range / representation inference (spec §4.5 R4.31, §4.10).
//!
//! **Parcel P3. Owner: (sub-agent).** Implement a sparse-conditional
//! (SCCP-family) forward propagation over the block-based SSA that infers, for
//! each `Value`, a narrowed `IRType` (tag bits + integer `Range`) and, where an
//! unboxed representation is provable/profitable, a `ValueRepresentation`.
//! Flow-sensitive through `Brif` conditions and `TypeCheck`/`Guard` results.
//! Profiling facts (spec §4.9) enter as *speculative* narrowings discharged by a
//! guard (spec R4.63) — inference proposes, deopt is the safety net. Implement as
//! a `Pass`. Unit-test on hand-built IR (e.g. a counted loop proving the index is
//! `FIXNUM[0, n)` so its overflow guard can later be dropped).

use crate::t2::ir::Function;
use crate::t2::pass::{Analyses, Pass};

/// The type/range/representation inference pass.
#[derive(Default)]
pub struct TypeInference;

impl Pass for TypeInference {
    fn name(&self) -> &'static str {
        "type-inference"
    }

    fn run(&mut self, _f: &mut Function, _a: &mut Analyses) {
        todo!("P3: sparse conditional type+range+representation propagation")
    }
}
