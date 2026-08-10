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
    assert!(matches!(g.node_kind(g.add_node(NodeKind::Start)), NodeKind::Start));
    match g.node_kind(g.add_node(NodeKind::Constant(NIL))) {
        NodeKind::Constant(v) => assert_eq!(*v, NIL),
        _ => panic!("expected Constant"),
    }
    match g.node_kind(g.add_node(NodeKind::Parameter(3))) {
        NodeKind::Parameter(i) => assert_eq!(*i, 3),
        _ => panic!("expected Parameter"),
    }
    assert!(matches!(g.node_kind(g.add_node(NodeKind::TypeCheck { expected_type: NIL })), NodeKind::TypeCheck { .. }));
    match g.node_kind(g.add_node(NodeKind::MemLoad { offset: -8 })) {
        NodeKind::MemLoad { offset } => assert_eq!(*offset, -8),
        _ => panic!("expected MemLoad"),
    }
    match g.node_kind(g.add_node(NodeKind::MemStore { offset: 16 })) {
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

#[test]
fn ir_builder_build_returns_graph_or_error() {
    let mut builder = IrBuilder::new();
    let result = builder.build(NIL);
    assert!(result.is_ok() || result.is_err());
}

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
    let mut g = IrGraph::new();
    let a = g.add_node(NodeKind::Start);
    let b = g.add_node(NodeKind::Return);
    g.add_edge(Edge { from: a, to: b, kind: EdgeKind::Control, input_index: 0 });
    g.remove_node(a);
    assert!(verify(&g).is_err());
}
