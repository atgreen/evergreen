//! Optimisation passes for the T2 compiler.
//!
//! Passes run in a fixed, deterministic order. See spec §4.5 / A4.02.

use crate::ir::IrGraph;

/// An optimisation pass that transforms an IR graph.
pub trait Pass {
    /// Human-readable name of this pass.
    fn name(&self) -> &str;

    /// Run this pass on the IR graph, returning whether it made changes.
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError>;
}

/// The pass manager runs passes in a fixed order.
pub struct PassManager {
    _private: (),
}

/// Pass ordering (A4.02):
/// 1. Type propagation
/// 2. Constant folding
/// 3. Inlining
/// 4. Escape analysis
/// 5. Type propagation (re-run)
/// 6. LICM
/// 7. Strength reduction
/// 8. DCE
/// 9. Null-check elimination
impl PassManager {
    /// Create a new pass manager with the default pass ordering.
    pub fn new() -> Self {
        unimplemented!("PassManager::new")
    }

    /// Run all passes on the given IR graph.
    pub fn run_all(&mut self, graph: &mut IrGraph) -> Result<(), crate::error::CompilerError> {
        unimplemented!("PassManager::run_all")
    }
}

// ── Individual passes ──────────────────────────────────────────────

/// Type propagation — forward data-flow analysis using the CL type lattice.
pub struct TypePropagation;
impl Pass for TypePropagation {
    fn name(&self) -> &str { "type-propagation" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("TypePropagation::run")
    }
}

/// Constant folding — evaluate constant expressions at compile time.
pub struct ConstantFolding;
impl Pass for ConstantFolding {
    fn name(&self) -> &str { "constant-folding" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("ConstantFolding::run")
    }
}

/// Inlining configuration.
pub struct InliningConfig {
    /// Max IR nodes per inline expansion (default: 50).
    pub budget: u32,
    /// Max inlining depth (default: 5).
    pub max_depth: u32,
}

/// Inlining — profile-guided, budget-limited function inlining.
pub struct Inlining {
    pub config: InliningConfig,
}
impl Pass for Inlining {
    fn name(&self) -> &str { "inlining" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("Inlining::run")
    }
}

/// Escape analysis — identify allocations that can be stack-allocated.
pub struct EscapeAnalysis;
impl Pass for EscapeAnalysis {
    fn name(&self) -> &str { "escape-analysis" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("EscapeAnalysis::run")
    }
}

/// Loop-invariant code motion.
pub struct Licm;
impl Pass for Licm {
    fn name(&self) -> &str { "licm" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("Licm::run")
    }
}

/// Strength reduction.
pub struct StrengthReduction;
impl Pass for StrengthReduction {
    fn name(&self) -> &str { "strength-reduction" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("StrengthReduction::run")
    }
}

/// Dead code elimination.
pub struct DeadCodeElimination;
impl Pass for DeadCodeElimination {
    fn name(&self) -> &str { "dce" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("DeadCodeElimination::run")
    }
}

/// Null-check elimination.
pub struct NullCheckElimination;
impl Pass for NullCheckElimination {
    fn name(&self) -> &str { "null-check-elimination" }
    fn run(&mut self, _graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        unimplemented!("NullCheckElimination::run")
    }
}
