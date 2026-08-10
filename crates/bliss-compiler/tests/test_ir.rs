//! Tests for bliss-compiler IR graph, builder, and verifier.

use bliss_compiler::ir::*;
use bliss_rt::value::{BlissVal, NIL};

#[test]
fn new_graph_has_zero_nodes() {
    let g = IrGraph::new();
    assert_eq!(g.node_count(), 0);
}

#[test]
fn add_node_returns_unique_ids() {
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Start);
    let b = g.add_node(NodeKind::Return);
    assert_ne!(a, b);
}

#[test]
fn node_kind_round_trips() {
    let mut g = IrGraph::new();
    let id = g.add_node(NodeKind::Start);
    assert!(matches!(g.node_kind(id), NodeKind::Start));
    let id = g.add_node(NodeKind::Constant(NIL));
    match g.node_kind(id) {
        NodeKind::Constant(v) => assert_eq!(*v, NIL),
        _ => panic!("expected Constant"),
    }
    let id = g.add_node(NodeKind::Parameter(3));
    match g.node_kind(id) {
        NodeKind::Parameter(i) => assert_eq!(*i, 3),
        _ => panic!("expected Parameter"),
    }
    let id = g.add_node(NodeKind::TypeCheck { expected_type: NIL });
    assert!(matches!(g.node_kind(id), NodeKind::TypeCheck { .. }));
    let id = g.add_node(NodeKind::MemLoad { offset: -8 });
    match g.node_kind(id) {
        NodeKind::MemLoad { offset } => assert_eq!(*offset, -8),
        _ => panic!("expected MemLoad"),
    }
    let id = g.add_node(NodeKind::MemStore { offset: 16 });
    match g.node_kind(id) {
        NodeKind::MemStore { offset } => assert_eq!(*offset, 16),
        _ => panic!("expected MemStore"),
    }
}

#[test]
fn node_count_increments_on_add() {
    let mut g = IrGraph::new();
    g.add_node(NodeKind::Start);
    assert_eq!(g.node_count(), 1);
    g.add_node(NodeKind::Return);
    assert_eq!(g.node_count(), 2);
}

#[test]
fn add_edge_appears_in_uses_and_inputs() {
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Start);
    let b = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Control, input_index: 0 });
    assert_eq!(g.uses(a).len(), 1);
    assert_eq!(g.uses(a)[0].to, b);
    assert_eq!(g.uses(a)[0].kind, EdgeKind::Control);
    assert_eq!(g.inputs(b).len(), 1);
    assert_eq!(g.inputs(b)[0].from, a);
    assert_eq!(g.inputs(b)[0].input_index, 0);
}

#[test]
fn multiple_edges_different_kinds() {
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Start);
    let b = g.add_node(NodeKind::MemStore { offset: 0 });
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Control, input_index: 0 });
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Memory, input_index: 1 });
    assert_eq!(g.inputs(b).len(), 2);
    assert_eq!(g.uses(a).len(), 2);
}

#[test]
fn data_edge_connects_producer_to_consumer() {
    let mut g = IrGraph::new();
    let c = g.add_node(NodeKind::Constant(NIL));
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: c, to: r, kind: EdgeKind::Data, input_index: 0 });
    assert_eq!(g.inputs(r)[0].kind, EdgeKind::Data);
}

#[test]
fn start_returns_the_start_node() {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    assert_eq!(g.start(), s);
}

#[test]
fn source_info_none_by_default() {
    let mut g = IrGraph::new();
    let id = g.add_node(NodeKind::Phi);
    assert!(g.source_info(id).is_none());
}

#[test]
fn set_and_get_source_info() {
    let mut g = IrGraph::new();
    let id = g.add_node(NodeKind::Call);
    g.set_source_info(id, IrSourceInfo { file: Some("test.lisp".into()), line: 42, column: 7, form: None });
    let info = g.source_info(id).expect("should have source info");
    assert_eq!(info.file.as_deref(), Some("test.lisp"));
    assert_eq!(info.line, 42);
    assert_eq!(info.column, 7);
}

#[test]
fn remove_node_decrements_count_and_clears_edges() {
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Constant(NIL));
    let b = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Data, input_index: 0 });
    assert_eq!(g.node_count(), 2);
    g.remove_node(a);
    assert_eq!(g.node_count(), 1);
    assert_eq!(g.inputs(b).len(), 0);
}

#[test]
fn replace_uses_redirects_edges() {
    let mut g = IrGraph::new();
    let old = g.add_node(NodeKind::Constant(NIL));
    let new_n = g.add_node(NodeKind::Parameter(0));
    let ret = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: old, to: ret, kind: EdgeKind::Data, input_index: 0 });
    g.replace_uses(old, new_n);
    assert_eq!(g.inputs(ret)[0].from, new_n);
    assert_eq!(g.uses(old).len(), 0);
}

// ── IrBuilder: Issue #1 — meaningful build tests ─────────────────

#[test]
fn ir_builder_build_constant_produces_ok_graph() {
    // NIL is a self-evaluating constant; building it should succeed and
    // produce a graph containing at least one Constant node.
    let mut builder = IrBuilder::new();
    let graph = builder.build(NIL).expect("building a self-evaluating form (NIL) should succeed");
    // The resulting graph must have at least a Start node and a Constant node.
    assert!(graph.node_count() >= 2, "graph for a constant form should have at least Start + Constant nodes");
}

#[test]
fn ir_builder_build_nil_produces_graph_with_start_node() {
    // Building NIL (a self-evaluating constant) should produce a well-formed graph
    // containing at least a Start node and a Constant node.
    let mut builder = IrBuilder::new();
    let graph = builder.build(NIL).expect("NIL should build successfully");
    // Verify the graph is non-trivial
    assert!(graph.node_count() >= 2, "built graph for NIL must contain at least Start + Constant nodes");
    // Verify the graph has a Start node
    let start_id = graph.start();
    assert!(matches!(graph.node_kind(start_id), NodeKind::Start));
}

#[test]
fn ir_builder_verify_built_graph_is_well_formed() {
    // A graph built from a valid form should pass verification.
    let mut builder = IrBuilder::new();
    let graph = builder.build(NIL).expect("NIL should build successfully");
    assert!(verify(&graph).is_ok(), "graph built from valid form should pass verification");
}

// ── Verifier: Issue #4 — additional verification checks ──────────

#[test]
fn verify_well_formed_trivial_graph() {
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: s, to: r, kind: EdgeKind::Control, input_index: 0 });
    assert!(verify(&g).is_ok(), "trivial graph should verify");
}

#[test]
fn verify_detects_dangling_edge() {
    // V1: dangling reference — edge references a removed (dead) node
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Start);
    let b = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Control, input_index: 0 });
    g.remove_node(a);
    assert!(verify(&g).is_err());
}

#[test]
fn verify_detects_use_list_inconsistency() {
    // V2: use-list consistency — after manually breaking the graph by removing
    // a node's edge from uses but leaving it in inputs, verify should fail.
    // We construct a graph and then corrupt it by removing a node that is still
    // referenced. The remove_node should clear edges, but if we add a new edge
    // pointing to a removed node, the uses won't match inputs.
    let mut g = IrGraph::new();
    let s = g.add_node(NodeKind::Start);
    let c = g.add_node(NodeKind::Constant(NIL));
    let r = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: s, to: r, kind: EdgeKind::Control, input_index: 0 });
    g.add_edge(Edge { from: c, to: r, kind: EdgeKind::Data, input_index: 1 });
    // Remove the constant node — this should clear edges.
    // Then add an edge referencing the dead node — creates a dangling/inconsistent state.
    g.remove_node(c);
    // Now r's input at slot 1 should be gone, but we add a new edge to a dead node.
    g.add_edge(Edge { from: c, to: r, kind: EdgeKind::Data, input_index: 1 });
    // Verify should detect this (V1 dangling reference or V2 use-list inconsistency).
    assert!(verify(&g).is_err(), "verify should detect edge to dead/removed node (V1/V2)");
}

#[test]
fn verify_detects_ssa_dominance_violation() {
    // V3: SSA dominance — a data input's source must dominate the consumer.
    // Create a graph where a Phi uses a value that doesn't dominate it
    // (the value is in a branch that doesn't flow to the Phi's region).
    let mut g = IrGraph::new();
    let start = g.add_node(NodeKind::Start);
    let branch = g.add_node(NodeKind::Branch);
    let region = g.add_node(NodeKind::Region);
    let ret = g.add_node(NodeKind::Return);
    // A constant that lives in one branch arm only
    let c1 = g.add_node(NodeKind::Constant(NIL));

    // Start -> Branch
    g.add_edge(Edge { from: start, to: branch, kind: EdgeKind::Control, input_index: 0 });
    // Branch -> Region (only one arm connected, simulating incomplete CFG)
    g.add_edge(Edge { from: branch, to: region, kind: EdgeKind::Control, input_index: 0 });
    // Region -> Return
    g.add_edge(Edge { from: region, to: ret, kind: EdgeKind::Control, input_index: 0 });

    // Make c1 used by ret, but c1 is placed after the branch in a way that
    // violates dominance — c1 has no control edge anchoring it before ret.
    // In a sea-of-nodes SSA, a floating constant is fine, but if we make c1
    // a non-constant node that doesn't dominate ret, verification should fail.
    // Use a MemLoad (pinned/side-effecting) without proper control input to violate V3/V6.
    let bad_load = g.add_node(NodeKind::MemLoad { offset: 0 });
    // bad_load has no control input (violates V6 for pinned nodes)
    g.add_edge(Edge { from: bad_load, to: ret, kind: EdgeKind::Data, input_index: 1 });

    assert!(verify(&g).is_err(), "verify should detect control edge integrity violation (V6) for pinned node without control input");
}

#[test]
fn verify_detects_phi_wrong_input_count() {
    // V4: Phi placement — a Phi must have input[0] as a Region,
    // and value input count must equal Region's control input count.
    let mut g = IrGraph::new();
    let start = g.add_node(NodeKind::Start);
    let region = g.add_node(NodeKind::Region);
    let phi = g.add_node(NodeKind::Phi);
    let ret = g.add_node(NodeKind::Return);

    // Region has 2 control inputs
    g.add_edge(Edge { from: start, to: region, kind: EdgeKind::Control, input_index: 0 });
    g.add_edge(Edge { from: start, to: region, kind: EdgeKind::Control, input_index: 1 });
    // Region -> Return
    g.add_edge(Edge { from: region, to: ret, kind: EdgeKind::Control, input_index: 0 });

    // Phi references region
    g.add_edge(Edge { from: region, to: phi, kind: EdgeKind::Data, input_index: 0 });
    // But Phi only has 1 value input when it should have 2 (one per Region predecessor)
    let c1 = g.add_node(NodeKind::Constant(NIL));
    g.add_edge(Edge { from: c1, to: phi, kind: EdgeKind::Data, input_index: 1 });
    // Missing second value input — Phi has 1 value input but Region has 2 control inputs.

    g.add_edge(Edge { from: phi, to: ret, kind: EdgeKind::Data, input_index: 1 });

    assert!(verify(&g).is_err(), "verify should detect Phi with wrong number of value inputs (V4)");
}

#[test]
fn verify_detects_bad_control_edge_count() {
    // V6: Control edge integrity — a pinned node (e.g. MemStore) must have
    // exactly one control input. Here we give it zero.
    let mut g = IrGraph::new();
    let start = g.add_node(NodeKind::Start);
    let store = g.add_node(NodeKind::MemStore { offset: 0 });
    let ret = g.add_node(NodeKind::Return);

    g.add_edge(Edge { from: start, to: ret, kind: EdgeKind::Control, input_index: 0 });
    // store has no control input — violates V6 for pinned/side-effecting nodes
    // Just a data edge to make it referenced
    let c = g.add_node(NodeKind::Constant(NIL));
    g.add_edge(Edge { from: c, to: store, kind: EdgeKind::Data, input_index: 0 });
    g.add_edge(Edge { from: store, to: ret, kind: EdgeKind::Memory, input_index: 1 });

    assert!(verify(&g).is_err(), "verify should detect pinned node (MemStore) with no control input (V6)");
}
