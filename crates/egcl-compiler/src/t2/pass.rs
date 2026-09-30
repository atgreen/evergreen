// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! T2 optimisation-pass contract (spec §4.5).
//!
//! **Phase-0 foundation / frozen contract.** Every optimisation parcel (P3, P4a–f)
//! implements [`Pass`] in its own file; the [`PassManager`] (lead-owned) drives
//! them in a fixed order. [`Analyses`] caches the derived analyses passes share
//! (dominators, loops, liveness) with lazy recompute, so passes never rebuild
//! dominance by hand and invalidation is centralised.

use crate::t2::ir::{Block, DominatorTree, Function};

/// A single optimisation pass over one function. Bodies live in per-pass files
/// (e.g. `opt_gvn.rs`, `opt_licm.rs`) so parcels never share a file.
pub trait Pass {
    /// A short stable name, for the pass log and `EGCL_IR_DUMP` filtering.
    fn name(&self) -> &'static str;

    /// Transform `f` in place. Passes MUST maintain the deopt-preservation
    /// invariant (spec §4.10 R4.60): never orphan a FrameState value source.
    /// A pass that mutates the CFG or value definitions MUST call
    /// [`Analyses::invalidate`] for what it changed.
    fn run(&mut self, f: &mut Function, a: &mut Analyses);
}

/// Lazily-computed, cached analyses shared across passes. Recomputed on demand
/// after invalidation.
#[derive(Default)]
pub struct Analyses {
    dominators: Option<DominatorTree>,
    loops: Option<LoopForest>,
}

impl Analyses {
    pub fn new() -> Analyses {
        Analyses::default()
    }

    /// The dominator tree, computed on first use (spec §4.3.4).
    pub fn dominators(&mut self, f: &Function) -> &DominatorTree {
        if self.dominators.is_none() {
            self.dominators = Some(f.dominators());
        }
        self.dominators.as_ref().unwrap()
    }

    /// The natural-loop forest, computed on first use (spec §4.3.4; for LICM).
    pub fn loops(&mut self, f: &Function) -> &LoopForest {
        if self.loops.is_none() {
            self.loops = Some(LoopForest::compute(f));
        }
        self.loops.as_ref().unwrap()
    }

    /// Drop cached analyses invalidated by a pass. Call after any CFG or
    /// value-definition change. Coarse for now (drops everything); parcels may
    /// refine to selective invalidation later.
    pub fn invalidate(&mut self) {
        self.dominators = None;
        self.loops = None;
    }
}

/// Natural-loop forest over the CFG (spec §4.3.4). Populated by parcel work
/// (LICM, P4b); the shape is frozen here.
#[derive(Clone, Debug, Default)]
pub struct LoopForest {
    /// One entry per natural loop: (header, body blocks).
    pub loops: Vec<NaturalLoop>,
}

#[derive(Clone, Debug)]
pub struct NaturalLoop {
    pub header: Block,
    pub blocks: Vec<Block>,
    /// Index into `LoopForest::loops` of the enclosing loop, if any.
    pub parent: Option<usize>,
}

impl LoopForest {
    /// Identify natural loops via back-edge analysis. Foundation-completion /
    /// P4b item — the type and entry point are frozen so LICM can build against
    /// them now.
    pub fn compute(_f: &Function) -> LoopForest {
        // TODO(P4b/foundation): back-edge detection over the dominator tree.
        LoopForest::default()
    }
}

/// Drives passes in a fixed pipeline order (spec §4.5 A4.02). Lead-owned; parcels
/// register their pass here via the integration step, not by editing each other.
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
