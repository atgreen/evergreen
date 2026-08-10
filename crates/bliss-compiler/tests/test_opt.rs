//! Tests for bliss-compiler optimisation passes.

use bliss_compiler::ir::{Edge, EdgeKind, IrGraph, NodeKind};
use bliss_compiler::opt::*;
use bliss_rt::value::NIL;

fn minimal_graph() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: s, to: r, kind: EdgeKind::Control, input_index: 0 });
    g
}

fn graph_with_constant() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let c = g.add_node(NodeKind::Constant(NIL));
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: s, to: r, kind: EdgeKind::Control, input_index: 0 });
    g.add_edge(Edge { from: c, to: r, kind: EdgeKind::Data, input_index: 1 });
    g
}

// ── Pass names ────────────────────────────────────────────────────

#[test]
fn pass_names() {
    assert_eq!(TypePropagation.name(), "type-propagation");
    assert_eq!(ConstantFolding.name(), "constant-folding");
    assert_eq!(Inlining { config: InliningConfig { budget: 50, max_depth: 5 } }.name(), "inlining");
    assert_eq!(EscapeAnalysis.name(), "escape-analysis");
    assert_eq!(Licm.name(), "licm");
    assert_eq!(StrengthReduction.name(), "strength-reduction");
    assert_eq!(DeadCodeElimination.name(), "dce");
    assert_eq!(NullCheckElimination.name(), "null-check-elimination");
}

// ── Pass::run — Issue #3: assert Ok and check changed boolean ────

#[test]
fn type_propagation_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no typed nodes, type propagation should succeed
    // and report no changes.
    let changed = TypePropagation.run(&mut g).expect("type propagation should succeed on minimal graph");
    assert!(!changed, "type propagation on minimal graph (no typed nodes) should report no changes");
}

#[test]
fn constant_folding_run_on_constant_graph() {
    let mut g = graph_with_constant();
    let result = ConstantFolding.run(&mut g);
    assert!(result.is_ok(), "constant folding should succeed on graph with constant");
}

#[test]
fn inlining_zero_budget_no_changes() {
    let mut pass = Inlining { config: InliningConfig { budget: 0, max_depth: 0 } };
    let mut g = minimal_graph();
    let changed = pass.run(&mut g).expect("inlining should succeed on minimal graph");
    assert!(!changed, "inlining with zero budget should make no changes");
}

#[test]
fn escape_analysis_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no allocations, escape analysis should succeed
    // and report no changes.
    let changed = EscapeAnalysis.run(&mut g).expect("escape analysis should succeed on minimal graph");
    assert!(!changed, "escape analysis on minimal graph (no allocations) should report no changes");
}

#[test]
fn licm_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no loops, LICM should succeed and report no changes.
    let changed = Licm.run(&mut g).expect("LICM should succeed on minimal graph");
    assert!(!changed, "LICM on minimal graph (no loops) should report no changes");
}

#[test]
fn strength_reduction_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no multiplications, strength reduction should succeed
    // and report no changes.
    let changed = StrengthReduction.run(&mut g).expect("strength reduction should succeed on minimal graph");
    assert!(!changed, "strength reduction on minimal graph (no multiplications) should report no changes");
}

#[test]
fn dce_on_minimal_no_changes() {
    let mut g = minimal_graph();
    let changed = DeadCodeElimination.run(&mut g).expect("DCE should succeed on minimal graph");
    assert!(!changed, "DCE on minimal graph should make no changes");
}

#[test]
fn null_check_elimination_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no null checks, the pass should succeed
    // and report no changes.
    let changed = NullCheckElimination.run(&mut g).expect("null-check elimination should succeed on minimal graph");
    assert!(!changed, "null-check elimination on minimal graph (no null checks) should report no changes");
}

// ── PassManager ───────────────────────────────────────────────────

#[test]
fn pass_manager_run_all_on_minimal_graph() {
    let mut pm = PassManager::new();
    let mut g = minimal_graph();
    assert!(pm.run_all(&mut g).is_ok());
}

#[test]
fn pass_manager_preserves_count_on_no_call_graph() {
    // Issue #7: The old test asserted node_count() <= before, which is wrong
    // because inlining can increase node count. Scope this test to a graph
    // with no Call nodes — only DCE/folding behavior applies, so count
    // should be preserved or shrink.
    let mut pm = PassManager::new();
    let mut g = minimal_graph(); // no Call nodes, so inlining won't expand
    let before = g.node_count();
    pm.run_all(&mut g).expect("pass manager should succeed on minimal graph");
    assert!(
        g.node_count() <= before,
        "on a graph with no Call nodes, passes should preserve or shrink node count (was {}, now {})",
        before, g.node_count()
    );
}

#[test]
fn pass_manager_idempotent() {
    let mut pm = PassManager::new();
    let mut g = minimal_graph();
    pm.run_all(&mut g).expect("first run should succeed");
    let after_first = g.node_count();
    pm.run_all(&mut g).expect("second run should succeed");
    assert_eq!(g.node_count(), after_first);
}

#[test]
fn pass_manager_on_graph_with_constant() {
    let mut pm = PassManager::new();
    let mut g = graph_with_constant();
    assert!(pm.run_all(&mut g).is_ok(), "pass manager should succeed on graph with constant");
}

// ── InliningConfig ────────────────────────────────────────────────

#[test]
fn inlining_config_values() {
    let c = InliningConfig { budget: 100, max_depth: 10 };
    assert_eq!(c.budget, 100);
    assert_eq!(c.max_depth, 10);
}
