// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 optimisation-pass contract: the [`Pass`] trait, the [`Analyses`] cache,
//! and the [`PassManager`].
//!
//! # Purpose
//!
//! Mid-end passes (`opt_fold`, `opt_gvn`, `opt_guard`, `opt_dce`, `opt_licm`,
//! `opt_escape`, `infer`) implement [`Pass`]. A [`PassManager`] runs them in
//! registration order over one [`Function`], sharing one [`Analyses`] so the
//! dominator tree is computed once per invalidation epoch rather than once per
//! pass.
//!
//! # Contract
//!
//! * `Pass::run(f, a)` transforms `f` in place under two obligations. It must
//!   preserve well-formedness as verify.rs checks it, and in particular must
//!   never orphan a `FrameState` value source: a value a deoptimising
//!   instruction's state names stays defined and dominating, or is replaced by
//!   a rematerialisation recipe (`opt_dce` owns that rewrite). And after any
//!   CFG or value-definition change it must call [`Analyses::invalidate`].
//!   Invalidation is coarse: it drops every cached analysis.
//! * `Analyses::dominators` lazily calls `Function::dominators()`. It is the
//!   only cached analysis; `opt_licm` and `opt_guard` each do their own
//!   back-edge detection over the dominator tree. A shared loop forest would
//!   belong here if a third consumer appears.
//! * `PassManager::run` creates a fresh `Analyses` per run and does not
//!   re-verify. Callers (drive.rs and the promotion path in
//!   `crates/egcl/src/cli/bytecode.rs`) verify after the pipeline and decline
//!   T2 on failure.
//!
//! # Pass order
//!
//! The promotion path registers `ConstFold` → `Gvn` → `GuardElim` →
//! `ClearMvElim` → `Dce`
//! after speculation; drive.rs registers `ConstFold` → `Gvn` → `Dce`. Folding
//! first exposes equal values to GVN; guard elimination runs after GVN and
//! after the builder's inlining so proofs cloned from separate callees collapse
//! to one dominating guard; DCE runs last to remove what the others orphaned
//! and to rewrite deopt-only values into rematerialisation recipes.
//!
//! # Debugging
//!
//! `Pass::name` labels a pass in logs. `EGCL_IR_DUMP` is read by the inference
//! pass, which then prints a per-function summary of narrowed facts to stderr.

use crate::t2::ir::{DominatorTree, Function};

/// A single optimisation pass over one function. Bodies live in per-pass files
/// (e.g. `opt_gvn.rs`, `opt_licm.rs`).
pub trait Pass {
    /// A short stable name, for the pass log and `EGCL_IR_DUMP` filtering.
    fn name(&self) -> &'static str;

    /// Transform `f` in place. Passes MUST maintain the deopt-preservation
    /// invariant: never orphan a FrameState value source.
    /// A pass that mutates the CFG or value definitions MUST call
    /// [`Analyses::invalidate`] for what it changed.
    fn run(&mut self, f: &mut Function, a: &mut Analyses);
}

/// Lazily-computed, cached analyses shared across passes. Recomputed on demand
/// after invalidation.
#[derive(Default)]
pub struct Analyses {
    dominators: Option<DominatorTree>,
}

impl Analyses {
    pub fn new() -> Analyses {
        Analyses::default()
    }

    /// The dominator tree, computed on first use.
    pub fn dominators(&mut self, f: &Function) -> &DominatorTree {
        if self.dominators.is_none() {
            self.dominators = Some(f.dominators());
        }
        self.dominators.as_ref().unwrap()
    }

    /// Drop cached analyses invalidated by a pass. Call after any CFG or
    /// value-definition change. Coarse: drops everything; selective
    /// invalidation is a possible refinement.
    pub fn invalidate(&mut self) {
        self.dominators = None;
    }
}

/// Drives registered passes in order over one function.
pub struct PassManager {
    passes: Vec<Box<dyn Pass>>,
}

impl PassManager {
    pub fn new() -> PassManager {
        PassManager { passes: Vec::new() }
    }

    pub fn add(&mut self, p: Box<dyn Pass>) -> &mut Self {
        self.passes.push(p);
        self
    }

    /// Run every registered pass in order, recomputing analyses as invalidated.
    pub fn run(&mut self, f: &mut Function) {
        let mut a = Analyses::new();
        for p in &mut self.passes {
            p.run(f, &mut a);
        }
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
    }
}
