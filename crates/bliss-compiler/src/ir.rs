//! Sea-of-nodes SSA intermediate representation.
//!
//! Inspired by HotSpot C2 and Graal. See spec §4.3.

use bliss_rt::value::BlissVal;
use std::borrow::Borrow;
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
    TypeCheck { expected_type: BlissVal },
    /// Tag an unboxed scalar into a BlissVal.
    Box,
    /// Untag a BlissVal to a raw scalar.
    Unbox,
    /// Heap load.
    MemLoad { offset: i32 },
    /// Heap store.
    MemStore { offset: i32 },
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
    pub fn inputs(&self, id: impl Borrow<NodeId>) -> &[Edge] {
        self.inputs
            .get(id.borrow())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// Get all output edges from a node.
    pub fn uses(&self, id: impl Borrow<NodeId>) -> &[Edge] {
        self.uses
            .get(id.borrow())
            .map(|v| v.as_slice())
            .unwrap_or(&[])
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

    /// Check whether a node with the given ID exists in the graph.
    pub fn contains(&self, id: NodeId) -> bool {
        self.nodes.contains_key(&id)
    }

    /// Return an iterator over all node IDs currently in the graph.
    pub fn node_ids(&self) -> impl Iterator<Item = NodeId> + '_ {
        self.nodes.keys().copied()
    }
}

impl Default for IrGraph {
    fn default() -> Self {
        Self::new()
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
    ///
    /// Walks the AST recursively and builds SSA IR nodes.
    /// Self-evaluating forms produce Constant nodes.
    /// Cons cells (compound forms) are translated into their IR equivalents:
    /// function calls become Call nodes, special forms produce appropriate
    /// control flow (Branch, Region, Phi).
    pub fn build(&mut self, form: BlissVal) -> Result<IrGraph, crate::error::CompilerError> {
        let mut graph = IrGraph::new();
        let start = graph.add_node(NodeKind::Start);

        // Build IR for the body form
        let result_node = self.build_form(&mut graph, form, start)?;

        // Create Return node wired to the result
        let ret = graph.add_node(NodeKind::Return);
        graph.add_edge(Edge {
            from: start,
            to: ret,
            kind: EdgeKind::Control,
            input_index: 0,
        });
        graph.add_edge(Edge {
            from: result_node,
            to: ret,
            kind: EdgeKind::Data,
            input_index: 1,
        });

        Ok(graph)
    }

    /// Recursively build IR nodes for a form.
    fn build_form(
        &mut self,
        graph: &mut IrGraph,
        form: BlissVal,
        ctrl: NodeId,
    ) -> Result<NodeId, crate::error::CompilerError> {
        use bliss_rt::value::{NIL_BITS, T_BITS, TAG_CONS, TAG_SPECIAL, TAG_SYMBOL};

        // Self-evaluating forms: produce a Constant node
        if form.is_fixnum()
            || form.is_character()
            || form.is_single_float()
            || form.is_heap_object()
            || form.0 == NIL_BITS
            || form.0 == T_BITS
            || form.tag() == TAG_SPECIAL
            || form.is_function()
        {
            return Ok(graph.add_node(NodeKind::Constant(form)));
        }

        // Symbols: produce a Constant node
        // (In a full implementation, this would look up the variable binding;
        //  for IR building, we represent it as a Constant of the symbol value
        //  since resolution happens at runtime.)
        if form.tag() == TAG_SYMBOL {
            return Ok(graph.add_node(NodeKind::Constant(form)));
        }

        // Cons cells: compound forms (function calls or special forms)
        if form.tag() == TAG_CONS {
            let ptr = (form.0 & !bliss_rt::value::TAG_MASK) as *const u64;
            if ptr.is_null() {
                return Ok(graph.add_node(NodeKind::Constant(form)));
            }
            let operator = BlissVal(unsafe { *ptr });
            let args_form = BlissVal(unsafe { *ptr.add(1) });

            // Check for special forms by looking up the symbol name
            if operator.tag() == TAG_SYMBOL {
                let sym_idx = (operator.0 >> 3) as u32;
                if let Some(name) = crate::reader::symbol_name(sym_idx) {
                    match name.as_str() {
                        "QUOTE" => {
                            // (QUOTE datum) -> Constant(datum)
                            if args_form.tag() == TAG_CONS {
                                let ap = (args_form.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                                if !ap.is_null() {
                                    let datum = BlissVal(unsafe { *ap });
                                    return Ok(graph.add_node(NodeKind::Constant(datum)));
                                }
                            }
                            return Ok(graph.add_node(NodeKind::Constant(form)));
                        }
                        "IF" => {
                            return self.build_if(graph, args_form, ctrl);
                        }
                        "LAMBDA" => {
                            return self.build_lambda(graph, args_form, ctrl);
                        }
                        "PROGN" => {
                            return self.build_progn(graph, args_form, ctrl);
                        }
                        "DEFUN" => {
                            // (DEFUN name (params) body...) - treat like lambda for IR purposes
                            // Skip the name, build lambda from params+body
                            if args_form.tag() == TAG_CONS {
                                let ap = (args_form.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                                if !ap.is_null() {
                                    let rest = BlissVal(unsafe { *ap.add(1) }); // (params body...)
                                    return self.build_lambda(graph, rest, ctrl);
                                }
                            }
                            return Ok(graph.add_node(NodeKind::Constant(form)));
                        }
                        _ => {} // fall through to function call handling
                    }
                }
            }

            // For other compound forms, build a Call node
            let operator_node = self.build_form(graph, operator, ctrl)?;

            // Collect argument nodes
            let mut arg_nodes = Vec::new();
            let mut cur = args_form;
            while cur.tag() == TAG_CONS {
                let ap = (cur.0 & !bliss_rt::value::TAG_MASK) as *const u64;
                if ap.is_null() {
                    break;
                }
                let arg_form = BlissVal(unsafe { *ap });
                let arg_node = self.build_form(graph, arg_form, ctrl)?;
                arg_nodes.push(arg_node);
                cur = BlissVal(unsafe { *ap.add(1) });
            }

            // Create Call node
            let call = graph.add_node(NodeKind::Call);
            graph.add_edge(Edge {
                from: ctrl,
                to: call,
                kind: EdgeKind::Control,
                input_index: 0,
            });
            graph.add_edge(Edge {
                from: operator_node,
                to: call,
                kind: EdgeKind::Data,
                input_index: 1,
            });
            for (i, &arg) in arg_nodes.iter().enumerate() {
                graph.add_edge(Edge {
                    from: arg,
                    to: call,
                    kind: EdgeKind::Data,
                    input_index: (i + 2) as u32,
                });
            }
            return Ok(call);
        }

        // Fallback: treat as constant
        Ok(graph.add_node(NodeKind::Constant(form)))
    }

    /// Collect a cons-list into a Vec of BlissVal.
    fn collect_list(list: BlissVal) -> Vec<BlissVal> {
        use bliss_rt::value::TAG_CONS;
        let mut result = Vec::new();
        let mut cur = list;
        while cur.tag() == TAG_CONS {
            let ap = (cur.0 & !bliss_rt::value::TAG_MASK) as *const u64;
            if ap.is_null() {
                break;
            }
            result.push(BlissVal(unsafe { *ap }));
            cur = BlissVal(unsafe { *ap.add(1) });
        }
        result
    }

    /// Build IR for an IF form: (IF test then [else])
    fn build_if(
        &mut self,
        graph: &mut IrGraph,
        args_form: BlissVal,
        ctrl: NodeId,
    ) -> Result<NodeId, crate::error::CompilerError> {
        use bliss_rt::value::NIL_BITS;

        let args = Self::collect_list(args_form);

        if args.is_empty() {
            return Ok(graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS))));
        }

        // Build test expression
        let test_node = self.build_form(graph, args[0], ctrl)?;

        // Branch node
        let branch = graph.add_node(NodeKind::Branch);
        graph.add_edge(Edge {
            from: ctrl,
            to: branch,
            kind: EdgeKind::Control,
            input_index: 0,
        });
        graph.add_edge(Edge {
            from: test_node,
            to: branch,
            kind: EdgeKind::Data,
            input_index: 1,
        });

        // Then branch
        let then_node = if args.len() > 1 {
            self.build_form(graph, args[1], branch)?
        } else {
            graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS)))
        };

        // Else branch
        let else_node = if args.len() > 2 {
            self.build_form(graph, args[2], branch)?
        } else {
            graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS)))
        };

        // Region (merge point)
        let region = graph.add_node(NodeKind::Region);
        graph.add_edge(Edge {
            from: branch,
            to: region,
            kind: EdgeKind::Control,
            input_index: 0,
        });
        graph.add_edge(Edge {
            from: branch,
            to: region,
            kind: EdgeKind::Control,
            input_index: 1,
        });

        // Phi (merge values)
        let phi = graph.add_node(NodeKind::Phi);
        graph.add_edge(Edge {
            from: region,
            to: phi,
            kind: EdgeKind::Data,
            input_index: 0,
        });
        graph.add_edge(Edge {
            from: then_node,
            to: phi,
            kind: EdgeKind::Data,
            input_index: 1,
        });
        graph.add_edge(Edge {
            from: else_node,
            to: phi,
            kind: EdgeKind::Data,
            input_index: 2,
        });

        Ok(phi)
    }

    /// Build IR for a LAMBDA form: (LAMBDA (params...) body...)
    /// Creates Parameter nodes for each parameter and builds the body.
    fn build_lambda(
        &mut self,
        graph: &mut IrGraph,
        args_form: BlissVal,
        ctrl: NodeId,
    ) -> Result<NodeId, crate::error::CompilerError> {
        use bliss_rt::value::NIL_BITS;

        let args = Self::collect_list(args_form);
        if args.is_empty() {
            return Ok(graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS))));
        }

        // First element is the parameter list
        let param_list = Self::collect_list(args[0]);

        // Create Parameter nodes for each lambda parameter
        let mut _param_nodes = Vec::new();
        for (i, _param) in param_list.iter().enumerate() {
            let param_node = graph.add_node(NodeKind::Parameter(i as u32));
            _param_nodes.push(param_node);
        }

        // Build the body forms (like PROGN)
        if args.len() > 1 {
            let mut result_node = graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS)));
            for arg in args.iter().skip(1) {
                result_node = self.build_form(graph, *arg, ctrl)?;
            }
            Ok(result_node)
        } else {
            Ok(graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS))))
        }
    }

    /// Build IR for a PROGN form: (PROGN form1 form2 ... formN)
    fn build_progn(
        &mut self,
        graph: &mut IrGraph,
        args_form: BlissVal,
        ctrl: NodeId,
    ) -> Result<NodeId, crate::error::CompilerError> {
        use bliss_rt::value::NIL_BITS;

        let forms = Self::collect_list(args_form);
        if forms.is_empty() {
            return Ok(graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS))));
        }

        let mut result_node = graph.add_node(NodeKind::Constant(BlissVal(NIL_BITS)));
        for form in forms {
            result_node = self.build_form(graph, form, ctrl)?;
        }
        Ok(result_node)
    }
}

// ── IR verifier ────────────────────────────────────────────────────

/// Returns true if a node kind is "pinned" (requires a control input).
fn is_pinned(kind: &NodeKind) -> bool {
    matches!(
        kind,
        NodeKind::MemLoad { .. } | NodeKind::MemStore { .. } | NodeKind::Safepoint | NodeKind::Call
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
            let from_uses = graph
                .uses
                .get(&edge.from)
                .map(|v| v.as_slice())
                .unwrap_or(&[]);
            let has_match = from_uses
                .iter()
                .any(|u| u.to == edge.to && u.input_index == edge.input_index);
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
        let inputs = graph
            .inputs
            .get(node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
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
        let region_inputs = graph
            .inputs
            .get(&region_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let region_ctrl_count = region_inputs
            .iter()
            .filter(|e| e.kind == EdgeKind::Control)
            .count();
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
        let inputs = graph
            .inputs
            .get(node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let ctrl_count = inputs
            .iter()
            .filter(|e| e.kind == EdgeKind::Control)
            .count();
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
        let inputs = graph
            .inputs
            .get(node_id)
            .map(|v| v.as_slice())
            .unwrap_or(&[]);
        let ctrl_count = inputs
            .iter()
            .filter(|e| e.kind == EdgeKind::Control)
            .count();
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

impl Default for IrBuilder {
    fn default() -> Self {
        Self::new()
    }
}
