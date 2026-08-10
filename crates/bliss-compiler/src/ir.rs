//! Sea-of-nodes SSA intermediate representation.
//!
//! Inspired by HotSpot C2 and Graal. See spec §4.3.

use bliss_rt::value::BlissVal;

/// Unique identifier for an IR node within a graph.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct NodeId(pub u32);

/// IR node kinds. D4.01.
#[derive(Clone, Debug)]
pub enum NodeKind {
    /// Entry point of the graph.
    Start,
    /// Function return.
    Return,
    /// Compile-time constant.
    Constant(BlissVal),
    /// Function parameter (by index).
    Parameter(u32),
    /// SSA merge of values at a control-flow join.
    Phi,
    /// Control-flow merge point.
    Region,
    /// Conditional branch.
    Branch,
    /// Function call.
    Call,
    /// Runtime type guard (with uncommon trap metadata).
    TypeCheck {
        expected_type: BlissVal,
    },
    /// Tag an unboxed scalar into a BlissVal.
    Box,
    /// Untag a BlissVal to a raw scalar.
    Unbox,
    /// Heap load.
    MemLoad {
        offset: i32,
    },
    /// Heap store.
    MemStore {
        offset: i32,
    },
    /// GC safepoint poll.
    Safepoint,
}

/// Edge kinds in the IR graph. D4.02.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EdgeKind {
    /// Value produced by one node, consumed by another.
    Data,
    /// Sequencing for side-effecting nodes.
    Control,
    /// Serialises memory operations.
    Memory,
}

/// An edge between two nodes.
#[derive(Clone, Debug)]
pub struct Edge {
    pub from: NodeId,
    pub to: NodeId,
    pub kind: EdgeKind,
    /// Index of this input at the destination node.
    pub input_index: u32,
}

/// Source location attached to an IR node.
#[derive(Clone, Debug)]
pub struct IrSourceInfo {
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
    pub form: Option<BlissVal>,
}

// ── IR graph ───────────────────────────────────────────────────────

/// A sea-of-nodes IR graph representing a single function.
pub struct IrGraph {
    _private: (),
}

impl IrGraph {
    /// Create a new empty IR graph.
    pub fn new() -> Self {
        unimplemented!("IrGraph::new")
    }

    /// Add a node to the graph, returning its ID.
    pub fn add_node(&mut self, kind: NodeKind) -> NodeId {
        unimplemented!("IrGraph::add_node")
    }

    /// Add an edge between two nodes.
    pub fn add_edge(&mut self, edge: Edge) {
        unimplemented!("IrGraph::add_edge")
    }

    /// Get the kind of a node.
    pub fn node_kind(&self, id: NodeId) -> &NodeKind {
        unimplemented!("IrGraph::node_kind")
    }

    /// Get all input edges to a node.
    pub fn inputs(&self, id: NodeId) -> &[Edge] {
        unimplemented!("IrGraph::inputs")
    }

    /// Get all output edges from a node.
    pub fn uses(&self, id: NodeId) -> &[Edge] {
        unimplemented!("IrGraph::uses")
    }

    /// Get the Start node ID.
    pub fn start(&self) -> NodeId {
        unimplemented!("IrGraph::start")
    }

    /// Attach source info to a node.
    pub fn set_source_info(&mut self, id: NodeId, info: IrSourceInfo) {
        unimplemented!("IrGraph::set_source_info")
    }

    /// Get source info for a node.
    pub fn source_info(&self, id: NodeId) -> Option<&IrSourceInfo> {
        unimplemented!("IrGraph::source_info")
    }

    /// Remove a node and its edges from the graph.
    pub fn remove_node(&mut self, id: NodeId) {
        unimplemented!("IrGraph::remove_node")
    }

    /// Replace all uses of `old` with `new`.
    pub fn replace_uses(&mut self, old: NodeId, new: NodeId) {
        unimplemented!("IrGraph::replace_uses")
    }

    /// Total number of nodes.
    pub fn node_count(&self) -> usize {
        unimplemented!("IrGraph::node_count")
    }
}

// ── IR builder ─────────────────────────────────────────────────────

/// Builds an IR graph from a CL AST (used by T2 compiler).
pub struct IrBuilder {
    _private: (),
}

impl IrBuilder {
    /// Create a new IR builder.
    pub fn new() -> Self {
        unimplemented!("IrBuilder::new")
    }

    /// Build an IR graph from a CL form.
    pub fn build(&mut self, form: BlissVal) -> Result<IrGraph, crate::error::CompilerError> {
        unimplemented!("IrBuilder::build")
    }
}

// ── IR verifier ────────────────────────────────────────────────────

/// Verify IR graph integrity: SSA dominance, type consistency, edge well-formedness.
pub fn verify(graph: &IrGraph) -> Result<(), Vec<String>> {
    unimplemented!("verify")
}
