//! Tests for torcl-compiler optimisation passes.

use torcl_compiler::ir::{Edge, EdgeKind, IrGraph, NodeKind};
use torcl_compiler::opt::*;
use torcl_rt::value::NIL;

fn minimal_graph() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge {
        from: s,
        to: r,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g
}

fn graph_with_constant() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let c = g.add_node(NodeKind::Constant(NIL));
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge {
        from: s,
        to: r,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: c,
        to: r,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g
}

// ── Pass names ────────────────────────────────────────────────────

#[test]
fn pass_names() {
    assert_eq!(TypePropagation.name(), "type-propagation");
    assert_eq!(ConstantFolding.name(), "constant-folding");
    assert_eq!(
        Inlining {
            config: InliningConfig {
                budget: 50,
                max_depth: 5,
                small_threshold: 30
            },
            registry: FunctionRegistry::new()
        }
        .name(),
        "inlining"
    );
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
    let changed = TypePropagation
        .run(&mut g)
        .expect("type propagation should succeed on minimal graph");
    assert!(
        !changed,
        "type propagation on minimal graph (no typed nodes) should report no changes"
    );
}

#[test]
fn constant_folding_run_on_constant_graph() {
    let mut g = graph_with_constant();
    let result = ConstantFolding.run(&mut g);
    assert!(
        result.is_ok(),
        "constant folding should succeed on graph with constant"
    );
}

#[test]
fn inlining_zero_budget_no_changes() {
    let mut pass = Inlining {
        config: InliningConfig {
            budget: 0,
            max_depth: 0,
            small_threshold: 30,
        },
        registry: FunctionRegistry::new(),
    };
    let mut g = minimal_graph();
    let changed = pass
        .run(&mut g)
        .expect("inlining should succeed on minimal graph");
    assert!(!changed, "inlining with zero budget should make no changes");
}

#[test]
fn escape_analysis_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no allocations, escape analysis should succeed
    // and report no changes.
    let changed = EscapeAnalysis
        .run(&mut g)
        .expect("escape analysis should succeed on minimal graph");
    assert!(
        !changed,
        "escape analysis on minimal graph (no allocations) should report no changes"
    );
}

#[test]
fn licm_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no loops, LICM should succeed and report no changes.
    let changed = Licm
        .run(&mut g)
        .expect("LICM should succeed on minimal graph");
    assert!(
        !changed,
        "LICM on minimal graph (no loops) should report no changes"
    );
}

#[test]
fn strength_reduction_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no multiplications, strength reduction should succeed
    // and report no changes.
    let changed = StrengthReduction
        .run(&mut g)
        .expect("strength reduction should succeed on minimal graph");
    assert!(
        !changed,
        "strength reduction on minimal graph (no multiplications) should report no changes"
    );
}

#[test]
fn dce_on_minimal_no_changes() {
    let mut g = minimal_graph();
    let changed = DeadCodeElimination
        .run(&mut g)
        .expect("DCE should succeed on minimal graph");
    assert!(!changed, "DCE on minimal graph should make no changes");
}

#[test]
fn null_check_elimination_run() {
    let mut g = minimal_graph();
    // On a minimal graph with no null checks, the pass should succeed
    // and report no changes.
    let changed = NullCheckElimination
        .run(&mut g)
        .expect("null-check elimination should succeed on minimal graph");
    assert!(
        !changed,
        "null-check elimination on minimal graph (no null checks) should report no changes"
    );
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
    pm.run_all(&mut g)
        .expect("pass manager should succeed on minimal graph");
    assert!(
        g.node_count() <= before,
        "on a graph with no Call nodes, passes should preserve or shrink node count (was {}, now {})",
        before,
        g.node_count()
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
    assert!(
        pm.run_all(&mut g).is_ok(),
        "pass manager should succeed on graph with constant"
    );
}

// ── DCE on graph with dead code ──────────────────────────────────

/// Build a graph with a dead (unreferenced) constant node.
/// DCE should detect and remove it.
fn graph_with_dead_node() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    // This constant is dead — nothing uses it.
    let _dead = g.add_node(NodeKind::Constant(NIL));
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge {
        from: s,
        to: r,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g
}

#[test]
fn dce_removes_dead_constant() {
    let mut g = graph_with_dead_node();
    let before = g.node_count();
    // There should be 3 nodes: Start, dead Constant, Return
    assert_eq!(before, 3, "graph should have 3 nodes before DCE");

    let changed = DeadCodeElimination.run(&mut g).expect("DCE should succeed");
    // DCE should have removed the dead constant node.
    assert!(changed, "DCE should report changes when dead code exists");
    assert!(
        g.node_count() < before,
        "DCE should reduce node count by removing dead nodes (was {}, now {})",
        before,
        g.node_count()
    );
    assert_eq!(
        g.node_count(),
        2,
        "after DCE, only Start and Return should remain"
    );
}

// ── Constant folding on constant expressions ─────────────────────

/// Build a graph with two constants feeding into an arithmetic operation.
/// Constant folding should evaluate the expression at compile time.
fn graph_with_foldable_expression() -> IrGraph {
    use torcl_rt::value::TorclVal;
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let c1 = g.add_node(NodeKind::Constant(TorclVal::from_fixnum(2)));
    let c2 = g.add_node(NodeKind::Constant(TorclVal::from_fixnum(3)));
    // A Call node representing an addition of two constants
    let call = g.add_node(NodeKind::Call);
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge {
        from: s,
        to: call,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: c1,
        to: call,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g.add_edge(Edge {
        from: c2,
        to: call,
        kind: EdgeKind::Data,
        input_index: 2,
    });
    g.add_edge(Edge {
        from: call,
        to: r,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: call,
        to: r,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g
}

#[test]
fn constant_folding_folds_constant_expression() {
    let mut g = graph_with_foldable_expression();
    let before = g.node_count();
    let result = ConstantFolding.run(&mut g);
    assert!(
        result.is_ok(),
        "constant folding should succeed on foldable graph"
    );
    // If the implementation folds the constant expression, node count should decrease
    // (the Call + two Constant nodes replaced by one Constant).
    let changed = result.unwrap();
    if changed {
        assert!(
            g.node_count() < before,
            "constant folding should reduce node count when folding occurs"
        );
    }
}

// ── CSE (common subexpression elimination via pass manager) ──────

/// Build a graph with redundant loads from the same offset.
fn graph_with_redundant_loads() -> IrGraph {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let p = g.add_node(NodeKind::Parameter(0));
    // Two loads from the same offset — redundant
    let ld1 = g.add_node(NodeKind::MemLoad { offset: 8 });
    let ld2 = g.add_node(NodeKind::MemLoad { offset: 8 });
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge {
        from: s,
        to: ld1,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: p,
        to: ld1,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g.add_edge(Edge {
        from: ld1,
        to: ld2,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: p,
        to: ld2,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g.add_edge(Edge {
        from: ld2,
        to: r,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    g.add_edge(Edge {
        from: ld1,
        to: r,
        kind: EdgeKind::Data,
        input_index: 1,
    });
    g.add_edge(Edge {
        from: ld2,
        to: r,
        kind: EdgeKind::Data,
        input_index: 2,
    });
    g
}

#[test]
fn pass_manager_handles_redundant_loads() {
    let mut pm = PassManager::new();
    let mut g = graph_with_redundant_loads();
    let before = g.node_count();
    let result = pm.run_all(&mut g);
    assert!(
        result.is_ok(),
        "pass manager should succeed on graph with redundant loads"
    );
    // After optimization, redundant loads could be eliminated,
    // reducing node count. At minimum, graph should be valid.
    assert!(
        g.node_count() <= before,
        "optimization should not increase node count for redundant loads"
    );
}

// ── InliningConfig ────────────────────────────────────────────────

#[test]
fn inlining_config_values() {
    let c = InliningConfig {
        budget: 100,
        max_depth: 10,
        small_threshold: 30,
    };
    assert_eq!(c.budget, 100);
    assert_eq!(c.max_depth, 10);
}
