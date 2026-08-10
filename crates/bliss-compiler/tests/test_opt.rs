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

// ── Pass::run ─────────────────────────────────────────────────────

#[test]
fn type_propagation_run() {
    let mut g = minimal_graph();
    let _ = TypePropagation.run(&mut g);
}

#[test]
fn constant_folding_run_on_constant_graph() {
    let mut g = graph_with_constant();
    let _ = ConstantFolding.run(&mut g);
}

#[test]
fn inlining_zero_budget_no_changes() {
    let mut pass = Inlining { config: InliningConfig { budget: 0, max_depth: 0 } };
    let mut g = minimal_graph();
    if let Ok(changed) = pass.run(&mut g) {
        assert!(!changed);
    }
}

#[test]
fn escape_analysis_run() {
    let mut g = minimal_graph();
    let _ = EscapeAnalysis.run(&mut g);
}

#[test]
fn licm_run() {
    let mut g = minimal_graph();
    let _ = Licm.run(&mut g);
}

#[test]
fn strength_reduction_run() {
    let mut g = minimal_graph();
    let _ = StrengthReduction.run(&mut g);
}

#[test]
fn dce_on_minimal_no_changes() {
    let mut g = minimal_graph();
    if let Ok(changed) = DeadCodeElimination.run(&mut g) {
        assert!(!changed);
    }
}

#[test]
fn null_check_elimination_run() {
    let mut g = minimal_graph();
    let _ = NullCheckElimination.run(&mut g);
}

// ── PassManager ───────────────────────────────────────────────────

#[test]
fn pass_manager_run_all_on_minimal_graph() {
    let mut pm = PassManager::new();
    let mut g = minimal_graph();
    assert!(pm.run_all(&mut g).is_ok());
}

#[test]
fn pass_manager_preserves_or_shrinks_graph() {
    let mut pm = PassManager::new();
    let mut g = minimal_graph();
    let before = g.node_count();
    let _ = pm.run_all(&mut g);
    assert!(g.node_count() <= before);
}

#[test]
fn pass_manager_idempotent() {
    let mut pm = PassManager::new();
    let mut g = minimal_graph();
    let _ = pm.run_all(&mut g);
    let after_first = g.node_count();
    let _ = pm.run_all(&mut g);
    assert_eq!(g.node_count(), after_first);
}

#[test]
fn pass_manager_on_graph_with_constant() {
    let mut pm = PassManager::new();
    let mut g = graph_with_constant();
    let _ = pm.run_all(&mut g); // must not panic
}

// ── InliningConfig ────────────────────────────────────────────────

#[test]
fn inlining_config_values() {
    let c = InliningConfig { budget: 100, max_depth: 10 };
    assert_eq!(c.budget, 100);
    assert_eq!(c.max_depth, 10);
}
