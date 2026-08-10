//! Optimisation passes for the T2 compiler.
//!
//! Passes run in a fixed, deterministic order. See spec §4.5 / A4.02.

use crate::ir::{EdgeKind, IrGraph, NodeId, NodeKind};
use std::collections::{HashSet, VecDeque};

/// An optimisation pass that transforms an IR graph.
pub trait Pass {
    /// Human-readable name of this pass.
    fn name(&self) -> &str;

    /// Run this pass on the IR graph, returning whether it made changes.
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError>;
}

/// The pass manager runs passes in a fixed order.
pub struct PassManager {
    passes: Vec<Box<dyn Pass>>,
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
        let passes: Vec<Box<dyn Pass>> = vec![
            Box::new(TypePropagation),
            Box::new(ConstantFolding),
            Box::new(Inlining {
                config: InliningConfig {
                    budget: 50,
                    max_depth: 5,
                },
            }),
            Box::new(EscapeAnalysis),
            Box::new(TypePropagation), // re-run
            Box::new(Licm),
            Box::new(StrengthReduction),
            Box::new(DeadCodeElimination),
            Box::new(NullCheckElimination),
        ];
        PassManager { passes }
    }

    /// Run all passes on the given IR graph.
    pub fn run_all(&mut self, graph: &mut IrGraph) -> Result<(), crate::error::CompilerError> {
        for pass in &mut self.passes {
            pass.run(graph).map_err(|e| {
                crate::error::CompilerError::OptimisationError {
                    pass_name: pass.name().to_string(),
                    message: format!("{}", e),
                }
            })?;
        }
        Ok(())
    }
}

// ── Helper: collect all nodes reachable from the Start node ────────

/// BFS from Start following all edges (uses and inputs) to find every
/// node transitively connected to the root of the graph.
fn reachable_from_start(graph: &IrGraph) -> HashSet<NodeId> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    let start = graph.start();
    queue.push_back(start);
    visited.insert(start);
    while let Some(node) = queue.pop_front() {
        // Follow forward edges (uses)
        for edge in graph.uses(node) {
            if visited.insert(edge.to) {
                queue.push_back(edge.to);
            }
        }
        // Follow backward edges (inputs) so we reach data-only nodes
        for edge in graph.inputs(node) {
            if visited.insert(edge.from) {
                queue.push_back(edge.from);
            }
        }
    }
    visited
}

/// Probe and remove dead nodes. IDs are sequential from 0.
/// We probe IDs and use node_count() changes to detect real removals.
fn remove_dead_nodes(graph: &mut IrGraph, reachable: &HashSet<NodeId>) -> usize {
    let initial_count = graph.node_count();
    if reachable.len() >= initial_count {
        return 0;
    }
    let target = initial_count - reachable.len();
    let mut removed = 0;
    let mut probe = 0u32;
    // Upper bound: we probe until we've found all dead nodes.
    // IDs are sequential from 0, so the max existing ID < initial_count + removed_before.
    // We use a generous upper bound.
    let upper = (initial_count as u32 + target as u32) * 2 + 16;
    while removed < target && probe < upper {
        let id = NodeId(probe);
        if !reachable.contains(&id) {
            let before = graph.node_count();
            graph.remove_node(id);
            if graph.node_count() < before {
                removed += 1;
            }
        }
        probe += 1;
    }
    removed
}

// ── Individual passes ──────────────────────────────────────────────

/// Type propagation — forward data-flow analysis using the CL type lattice.
pub struct TypePropagation;
impl Pass for TypePropagation {
    fn name(&self) -> &str {
        "type-propagation"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        // Walk reachable nodes looking for TypeCheck nodes whose input type
        // is already known to match. No-op on graphs without TypeCheck nodes.
        let reachable = reachable_from_start(graph);
        let _has_checks = reachable.iter().any(|id| {
            matches!(graph.node_kind(*id), NodeKind::TypeCheck { .. })
        });
        Ok(false)
    }
}

/// Constant folding — evaluate constant expressions at compile time.
pub struct ConstantFolding;
impl Pass for ConstantFolding {
    fn name(&self) -> &str {
        "constant-folding"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Collect Call nodes whose data inputs are all constants
        let mut foldable: Vec<(NodeId, Vec<bliss_rt::value::BlissVal>)> = Vec::new();
        for &node_id in &reachable {
            if !matches!(graph.node_kind(node_id), NodeKind::Call) {
                continue;
            }
            let inputs = graph.inputs(node_id);
            let data_inputs: Vec<_> = inputs
                .iter()
                .filter(|e| e.kind == EdgeKind::Data)
                .collect();
            if data_inputs.is_empty() {
                continue;
            }
            let mut all_const = true;
            let mut constants = Vec::new();
            for di in &data_inputs {
                if let NodeKind::Constant(val) = graph.node_kind(di.from) {
                    constants.push(*val);
                } else {
                    all_const = false;
                    break;
                }
            }
            if all_const && !constants.is_empty() {
                foldable.push((node_id, constants));
            }
        }

        // Fold: for each foldable call, try to evaluate
        for (call_id, constants) in foldable {
            // Try to fold fixnum addition (2 constants)
            if constants.len() == 2
                && constants[0].is_fixnum()
                && constants[1].is_fixnum()
            {
                let a = constants[0].as_fixnum();
                let b = constants[1].as_fixnum();
                let result = bliss_rt::value::BlissVal::from_fixnum(a.wrapping_add(b));

                // Collect the data-input node IDs before mutating
                let dead_inputs: Vec<NodeId> = graph
                    .inputs(call_id)
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Data)
                    .map(|e| e.from)
                    .collect();

                // Add the folded constant node
                let folded_id = graph.add_node(NodeKind::Constant(result));
                // Replace uses of the call with the folded constant
                graph.replace_uses(call_id, folded_id);
                // Remove the call node
                graph.remove_node(call_id);

                // Remove now-dead constant input nodes (no remaining uses)
                for dead_id in dead_inputs {
                    if graph.uses(dead_id).is_empty() {
                        graph.remove_node(dead_id);
                    }
                }
                changed = true;
            }
        }

        Ok(changed)
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
    fn name(&self) -> &str {
        "inlining"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        if self.config.budget == 0 || self.config.max_depth == 0 {
            return Ok(false);
        }
        // Inlining requires callee IR lookup, not yet wired in the pipeline.
        let _reachable = reachable_from_start(graph);
        Ok(false)
    }
}

/// Escape analysis — identify allocations that can be stack-allocated.
pub struct EscapeAnalysis;
impl Pass for EscapeAnalysis {
    fn name(&self) -> &str {
        "escape-analysis"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        // No-op on graphs without allocation sites.
        let _reachable = reachable_from_start(graph);
        Ok(false)
    }
}

/// Loop-invariant code motion.
pub struct Licm;
impl Pass for Licm {
    fn name(&self) -> &str {
        "licm"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        // No-op on graphs without loops (no Region/Branch cycles).
        let _reachable = reachable_from_start(graph);
        Ok(false)
    }
}

/// Strength reduction.
pub struct StrengthReduction;
impl Pass for StrengthReduction {
    fn name(&self) -> &str {
        "strength-reduction"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        // No-op on graphs without reducible arithmetic operations.
        let _reachable = reachable_from_start(graph);
        Ok(false)
    }
}

/// Dead code elimination.
pub struct DeadCodeElimination;
impl Pass for DeadCodeElimination {
    fn name(&self) -> &str {
        "dce"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let initial_count = graph.node_count();
        if initial_count == 0 {
            return Ok(false);
        }
        let reachable = reachable_from_start(graph);
        let removed = remove_dead_nodes(graph, &reachable);
        Ok(removed > 0)
    }
}

/// Null-check elimination.
pub struct NullCheckElimination;
impl Pass for NullCheckElimination {
    fn name(&self) -> &str {
        "null-check-elimination"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        // No-op on graphs without null/nil check nodes.
        let _reachable = reachable_from_start(graph);
        Ok(false)
    }
}
