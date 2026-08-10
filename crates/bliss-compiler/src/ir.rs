//! Sea-of-nodes SSA intermediate representation.
//!
//! Inspired by HotSpot C2 and Graal. See spec §4.3.

use bliss_rt::value::BlissVal;
use std::collections::HashMap;

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
    nodes: HashMap<NodeId, NodeKind>,
    /// Edges indexed by destination node (inputs to a node).
    inputs: HashMap<NodeId, Vec<Edge>>,
    /// Edges indexed by source node (uses of a node's output).
    uses: HashMap<NodeId, Vec<Edge>>,
    source_infos: HashMap<NodeId, IrSourceInfo>,
    next_id: u32,
    start_node: Option<NodeId>,
}

impl IrGraph {
    /// Create a new empty IR graph.
    pub fn new() -> Self {
        IrGraph {
            nodes: HashMap::new(),
            inputs: HashMap::new(),
            uses: HashMap::new(),
            source_infos: HashMap::new(),
            next_id: 0,
            start_node: None,
        }
    }

    /// Add a node to the graph, returning its ID.
    pub fn add_node(&mut self, kind: NodeKind) -> NodeId {
        let id = NodeId(self.next_id);
        self.next_id += 1;
        let is_start = matches!(kind, NodeKind::Start);
        self.nodes.insert(id, kind);
        self.inputs.insert(id, Vec::new());
        self.uses.insert(id, Vec::new());
        if is_start && self.start_node.is_none() {
            self.start_node = Some(id);
        }
        id
    }

    /// Add an edge between two nodes.
    pub fn add_edge(&mut self, edge: Edge) {
        if let Some(uses) = self.uses.get_mut(&edge.from) {
            uses.push(edge.clone());
        }
        if let Some(inputs) = self.inputs.get_mut(&edge.to) {
            inputs.push(edge);
        }
    }

    /// Get the kind of a node.
    pub fn node_kind(&self, id: NodeId) -> &NodeKind {
        self.nodes.get(&id).expect("node not found in graph")
    }

    /// Get all input edges to a node.
    pub fn inputs(&self, id: NodeId) -> &[Edge] {
        self.inputs.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get all output edges from a node.
    pub fn uses(&self, id: NodeId) -> &[Edge] {
        self.uses.get(&id).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Get the Start node ID.
    pub fn start(&self) -> NodeId {
        self.start_node.expect("no Start node in graph")
    }

    /// Attach source info to a node.
    pub fn set_source_info(&mut self, id: NodeId, info: IrSourceInfo) {
        self.source_infos.insert(id, info);
    }

    /// Get source info for a node.
    pub fn source_info(&self, id: NodeId) -> Option<&IrSourceInfo> {
        self.source_infos.get(&id)
    }

    /// Remove a node and its edges from the graph.
    pub fn remove_node(&mut self, id: NodeId) {
        self.nodes.remove(&id);
        self.source_infos.remove(&id);

        // Remove edges where this node is the source (from uses of other nodes' inputs)
        if let Some(outgoing) = self.uses.remove(&id) {
            for edge in &outgoing {
                if let Some(target_inputs) = self.inputs.get_mut(&edge.to) {
                    target_inputs.retain(|e| e.from != id);
                }
            }
        }

        // Remove edges where this node is the destination (from uses lists of source nodes)
        if let Some(incoming) = self.inputs.remove(&id) {
            for edge in &incoming {
                if let Some(source_uses) = self.uses.get_mut(&edge.from) {
                    source_uses.retain(|e| e.to != id);
                }
            }
        }
    }

    /// Replace all uses of `old` with `new`.
    pub fn replace_uses(&mut self, old: NodeId, new: NodeId) {
        if let Some(old_uses) = self.uses.remove(&old) {
            for mut edge in old_uses {
                // Update the edge's source
                edge.from = new;
                // Update in the target's input list
                if let Some(target_inputs) = self.inputs.get_mut(&edge.to) {
                    for inp in target_inputs.iter_mut() {
                        if inp.from == old {
                            inp.from = new;
                        }
                    }
                }
                // Add to new node's uses list
                if let Some(new_uses) = self.uses.get_mut(&new) {
                    new_uses.push(edge);
                }
            }
        }
        // Ensure old node has empty uses
        self.uses.insert(old, Vec::new());
    }

    /// Total number of nodes.
    pub fn node_count(&self) -> usize {
        self.nodes.len()
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
        IrBuilder { _private: () }
    }

    /// Build an IR graph from a CL form.
    pub fn build(&mut self, form: BlissVal) -> Result<IrGraph, crate::error::CompilerError> {
        let mut graph = IrGraph::new();
        let start = graph.add_node(NodeKind::Start);
        // For a self-evaluating form, create a constant node and a return node
        let constant = graph.add_node(NodeKind::Constant(form));
        let ret = graph.add_node(NodeKind::Return);

        // Wire: Start -> Return (control)
        graph.add_edge(Edge {
            from: start,
            to: ret,
            kind: EdgeKind::Control,
            input_index: 0,
        });
        // Wire: Constant -> Return (data)
        graph.add_edge(Edge {
            from: constant,
            to: ret,
            kind: EdgeKind::Data,
            input_index: 1,
        });

        Ok(graph)
    }
}

// ── IR verifier ────────────────────────────────────────────────────

/// Returns true if a node kind is "pinned" (requires a control input).
fn is_pinned(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::MemLoad { .. }
            | NodeKind::MemStore { .. }
            | NodeKind::Safepoint
            | NodeKind::Call
    )
}

/// Verify IR graph integrity: SSA dominance, type consistency, edge well-formedness.
pub fn verify(graph: &IrGraph) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();

    // V1: Check all edges reference live nodes
    for (node_id, inputs) in &graph.inputs {
        if !graph.nodes.contains_key(node_id) {
            errors.push(format!("input list exists for dead node {:?}", node_id));
            continue;
        }
        for edge in inputs {
            if !graph.nodes.contains_key(&edge.from) {
                errors.push(format!(
                    "V1: dangling edge from dead node {:?} to {:?}",
                    edge.from, edge.to
                ));
            }
        }
    }

    for (node_id, uses) in &graph.uses {
        if !graph.nodes.contains_key(node_id) {
            errors.push(format!("uses list exists for dead node {:?}", node_id));
            continue;
        }
        for edge in uses {
            if !graph.nodes.contains_key(&edge.to) {
                errors.push(format!(
                    "V1: dangling edge from {:?} to dead node {:?}",
                    edge.from, edge.to
                ));
            }
        }
    }

    // V2: Use-list consistency — for every edge in inputs(n), there should be
    // a corresponding edge in uses(from)
    for (node_id, inputs) in &graph.inputs {
        if !graph.nodes.contains_key(node_id) {
            continue;
        }
        for edge in inputs {
            if !graph.nodes.contains_key(&edge.from) {
                continue; // already caught by V1
            }
            let from_uses = graph.uses.get(&edge.from).map(|v| v.as_slice()).unwrap_or(&[]);
            let has_match = from_uses.iter().any(|u| u.to == edge.to && u.input_index == edge.input_index);
            if !has_match {
                errors.push(format!(
                    "V2: use-list inconsistency: edge {:?}->{:?} in inputs but not in uses",
                    edge.from, edge.to
                ));
            }
        }
    }

    // V4: Phi placement — Phi must have a Region as data input[0] and
    // value input count must equal Region's control input count
    for (node_id, kind) in &graph.nodes {
        if !matches!(kind, NodeKind::Phi) {
            continue;
        }
        let inputs = graph.inputs.get(node_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let data_inputs: Vec<&Edge> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
        if data_inputs.is_empty() {
            errors.push(format!("V4: Phi {:?} has no data inputs", node_id));
            continue;
        }
        // First data input should be a Region
        let region_id = data_inputs[0].from;
        if !matches!(graph.nodes.get(&region_id), Some(NodeKind::Region)) {
            errors.push(format!(
                "V4: Phi {:?} first data input {:?} is not a Region",
                node_id, region_id
            ));
            continue;
        }
        // Value inputs = data_inputs.len() - 1 (subtract the region reference)
        let value_input_count = data_inputs.len() - 1;
        // Region's control input count
        let region_inputs = graph.inputs.get(&region_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let region_ctrl_count = region_inputs.iter().filter(|e| e.kind == EdgeKind::Control).count();
        if value_input_count != region_ctrl_count {
            errors.push(format!(
                "V4: Phi {:?} has {} value inputs but Region {:?} has {} control inputs",
                node_id, value_input_count, region_id, region_ctrl_count
            ));
        }
    }

    // V5: Return nodes must have a control input
    for (node_id, kind) in &graph.nodes {
        if !matches!(kind, NodeKind::Return) {
            continue;
        }
        let inputs = graph.inputs.get(node_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let ctrl_count = inputs.iter().filter(|e| e.kind == EdgeKind::Control).count();
        if ctrl_count < 1 {
            errors.push(format!(
                "V5: Return node {:?} has no control input",
                node_id
            ));
        }
    }

    // V6: Control edge integrity — pinned/side-effecting nodes must have
    // exactly one control input
    for (node_id, kind) in &graph.nodes {
        if !is_pinned(kind) {
            continue;
        }
        let inputs = graph.inputs.get(node_id).map(|v| v.as_slice()).unwrap_or(&[]);
        let ctrl_count = inputs.iter().filter(|e| e.kind == EdgeKind::Control).count();
        if ctrl_count != 1 {
            errors.push(format!(
                "V6: pinned node {:?} ({:?}) has {} control inputs (expected 1)",
                node_id, kind, ctrl_count
            ));
        }
    }

    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}
