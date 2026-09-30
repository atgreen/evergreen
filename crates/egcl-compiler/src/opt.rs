//! Optimisation passes for the T2 compiler.
//!
//! Passes run in a fixed, deterministic order. See spec §4.5 / A4.02.

use crate::ir::{Edge, EdgeKind, IrGraph, NodeId, NodeKind};
use std::collections::{HashMap, HashSet, VecDeque};

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
/// 10. Phi lowering
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
                    small_threshold: 30,
                },
                registry: FunctionRegistry::new(),
            }),
            Box::new(EscapeAnalysis),
            Box::new(TypePropagation), // re-run
            Box::new(Licm),
            Box::new(StrengthReduction),
            Box::new(DeadCodeElimination),
            Box::new(NullCheckElimination),
            Box::new(PhiLowering),
        ];
        PassManager { passes }
    }

    /// Run all passes on the given IR graph.
    pub fn run_all(&mut self, graph: &mut IrGraph) -> Result<(), crate::error::CompilerError> {
        for pass in &mut self.passes {
            pass.run(graph)
                .map_err(|e| crate::error::CompilerError::OptimisationError {
                    pass_name: pass.name().to_string(),
                    message: format!("{}", e),
                })?;
        }
        Ok(())
    }
}

impl Default for PassManager {
    fn default() -> Self {
        Self::new()
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

// ── Helpers for passes ─────────────────────────────────────────────

/// Check whether a constant value's tag satisfies a TypeCheck's expected_type.
///
/// `expected_type` is encoded as a fixnum holding the tag value (0–7)
/// following the EgclVal tag scheme, or as a special value (NIL/T).
/// If expected_type is T (top type), every value satisfies it.
fn constant_satisfies_type(
    val: egcl_rt::value::EgclVal,
    expected_type: egcl_rt::value::EgclVal,
) -> bool {
    use egcl_rt::value::T_BITS;
    // T (top type) accepts everything
    if expected_type.0 == T_BITS {
        return true;
    }
    // If expected_type is a fixnum encoding a tag value (0–7), check val's tag
    if expected_type.is_fixnum() {
        let tag = expected_type.as_fixnum();
        if (0..=7).contains(&tag) {
            return val.tag() == tag as u64;
        }
    }
    // Direct equality: the value is its own type witness (singleton type / eql type)
    val == expected_type
}

/// Check whether a node is reachable from `from` following forward (uses) edges,
/// without exceeding `limit` steps. Used for back-edge detection.
fn is_forward_reachable(
    graph: &IrGraph,
    from: NodeId,
    target: NodeId,
    reachable: &HashSet<NodeId>,
) -> bool {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    // Follow uses (forward edges) from `from`
    for edge in graph.uses(from) {
        if reachable.contains(&edge.to) && visited.insert(edge.to) {
            if edge.to == target {
                return true;
            }
            queue.push_back(edge.to);
        }
    }
    while let Some(node) = queue.pop_front() {
        for edge in graph.uses(node) {
            if reachable.contains(&edge.to) && visited.insert(edge.to) {
                if edge.to == target {
                    return true;
                }
                queue.push_back(edge.to);
            }
        }
    }
    false
}

/// Collect the set of nodes in a loop body given the loop header (a Region node).
/// The loop body consists of all nodes that can reach the header's back-edge source
/// without leaving through the header.
fn collect_loop_body(
    graph: &IrGraph,
    header: NodeId,
    reachable: &HashSet<NodeId>,
) -> HashSet<NodeId> {
    let mut body = HashSet::new();
    body.insert(header);

    // Find back-edge sources: control inputs to header that are reachable from header
    let inputs = graph.inputs(header);
    let back_edge_sources: Vec<NodeId> = inputs
        .iter()
        .filter(|e| e.kind == EdgeKind::Control)
        .filter(|e| is_forward_reachable(graph, header, e.from, reachable))
        .map(|e| e.from)
        .collect();

    // Walk backwards from each back-edge source to find all nodes in the loop
    let mut worklist: VecDeque<NodeId> = back_edge_sources.into_iter().collect();
    while let Some(node) = worklist.pop_front() {
        if body.insert(node) {
            // Add predecessors (control inputs)
            for edge in graph.inputs(node) {
                if edge.kind == EdgeKind::Control && reachable.contains(&edge.from) {
                    worklist.push_back(edge.from);
                }
            }
        }
    }
    body
}

// ── Individual passes ──────────────────────────────────────────────

/// Type propagation — forward data-flow analysis using the CL type lattice.
///
/// Eliminates redundant TypeCheck nodes when the input's type is already known
/// to satisfy the expected type (from constants or dominating type checks).
/// See spec §4.5.3.
pub struct TypePropagation;
impl Pass for TypePropagation {
    fn name(&self) -> &str {
        "type-propagation"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Collect TypeCheck nodes that can be eliminated because their
        // input type is provably a subtype of expected_type (§4.5.3.3).
        let mut to_eliminate: Vec<(NodeId, NodeId)> = Vec::new();

        for &id in &reachable {
            let expected_type = match graph.node_kind(id) {
                NodeKind::TypeCheck { expected_type } => *expected_type,
                _ => continue,
            };

            let inputs = graph.inputs(id);
            let data_input = inputs.iter().find(|e| e.kind == EdgeKind::Data);
            if let Some(data_edge) = data_input {
                let src = data_edge.from;
                let can_eliminate = match graph.node_kind(src) {
                    // If the input is a constant whose tag matches expected_type,
                    // the type check is redundant
                    NodeKind::Constant(val) => constant_satisfies_type(*val, expected_type),
                    // If the input already passed an identical TypeCheck (same expected_type),
                    // this check is dominated and redundant
                    NodeKind::TypeCheck {
                        expected_type: prev,
                    } => *prev == expected_type,
                    _ => false,
                };
                if can_eliminate {
                    to_eliminate.push((id, src));
                }
            }
        }

        for (check_id, src_id) in to_eliminate {
            // Replace all uses of the TypeCheck with its data input,
            // effectively removing the redundant check
            graph.replace_uses(check_id, src_id);
            graph.remove_node(check_id);
            changed = true;
        }

        Ok(changed)
    }
}

/// Constant folding — evaluate constant expressions at compile time.
///
/// Folds Call nodes with all-constant inputs (fixnum arithmetic, single-float
/// arithmetic) and eliminates identity Box/Unbox pairs on constants.
/// See spec §4.5.9.
pub struct ConstantFolding;

impl ConstantFolding {
    /// Try to fold a Call node with all-constant fixnum inputs.
    /// Returns Some(result) if foldable.
    fn try_fold_fixnum(
        constants: &[egcl_rt::value::EgclVal],
    ) -> Option<egcl_rt::value::EgclVal> {
        if constants.len() < 2 || !constants.iter().all(|c| c.is_fixnum()) {
            return None;
        }
        // Fold by summing all fixnum constants (generalised addition).
        // For 2-argument calls, this is standard binary addition folding.
        let mut acc = constants[0].as_fixnum();
        for c in &constants[1..] {
            acc = acc.wrapping_add(c.as_fixnum());
        }
        Some(egcl_rt::value::EgclVal::from_fixnum(acc))
    }

    /// Try to fold a Call node with all-constant single-float inputs.
    fn try_fold_single_float(
        constants: &[egcl_rt::value::EgclVal],
    ) -> Option<egcl_rt::value::EgclVal> {
        if constants.len() < 2 || !constants.iter().all(|c| c.is_single_float()) {
            return None;
        }
        let mut acc = constants[0].as_single_float();
        for c in &constants[1..] {
            acc += c.as_single_float();
        }
        Some(egcl_rt::value::EgclVal::from_single_float(acc))
    }

    /// Try to fold a Call node with mixed numeric types (numeric contagion).
    /// Per CL spec §12.1, fixnum + single-float → single-float.
    fn try_fold_mixed_numeric(
        constants: &[egcl_rt::value::EgclVal],
    ) -> Option<egcl_rt::value::EgclVal> {
        if constants.len() != 2 {
            return None;
        }
        let has_fixnum = constants.iter().any(|c| c.is_fixnum());
        let has_float = constants.iter().any(|c| c.is_single_float());
        if !(has_fixnum && has_float) {
            return None;
        }
        // Numeric contagion: promote fixnum to single-float
        let mut vals = [0.0f32; 2];
        for (i, c) in constants.iter().enumerate() {
            if c.is_fixnum() {
                vals[i] = c.as_fixnum() as f32;
            } else if c.is_single_float() {
                vals[i] = c.as_single_float();
            } else {
                return None;
            }
        }
        Some(egcl_rt::value::EgclVal::from_single_float(
            vals[0] + vals[1],
        ))
    }

    /// Remove a call node and its dead constant inputs, replacing with folded result.
    fn replace_call_with_constant(
        graph: &mut IrGraph,
        call_id: NodeId,
        result: egcl_rt::value::EgclVal,
    ) {
        let dead_inputs: Vec<NodeId> = graph
            .inputs(call_id)
            .iter()
            .filter(|e| e.kind == EdgeKind::Data)
            .map(|e| e.from)
            .collect();

        let folded_id = graph.add_node(NodeKind::Constant(result));
        graph.replace_uses(call_id, folded_id);
        graph.remove_node(call_id);

        // Remove now-dead constant input nodes (no remaining uses)
        for dead_id in dead_inputs {
            if graph.uses(dead_id).is_empty() {
                graph.remove_node(dead_id);
            }
        }
    }
}

impl Pass for ConstantFolding {
    fn name(&self) -> &str {
        "constant-folding"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Phase 1: Fold Call nodes whose data inputs are all constants
        let mut foldable: Vec<(NodeId, Vec<egcl_rt::value::EgclVal>)> = Vec::new();
        for &node_id in &reachable {
            if !matches!(graph.node_kind(node_id), NodeKind::Call) {
                continue;
            }
            let inputs = graph.inputs(node_id);
            let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
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

        for (call_id, constants) in foldable {
            // Try folding in order of specificity: fixnum, single-float, mixed numeric
            if let Some(result) = Self::try_fold_fixnum(&constants) {
                Self::replace_call_with_constant(graph, call_id, result);
                changed = true;
            } else if let Some(result) = Self::try_fold_single_float(&constants) {
                Self::replace_call_with_constant(graph, call_id, result);
                changed = true;
            } else if let Some(result) = Self::try_fold_mixed_numeric(&constants) {
                Self::replace_call_with_constant(graph, call_id, result);
                changed = true;
            }
        }

        // Phase 2: Eliminate identity Box(Unbox(x)) → x pairs
        // A Box whose sole data input comes from an Unbox, or vice versa,
        // is an identity roundtrip that can be eliminated.
        let reachable = reachable_from_start(graph);
        let mut box_unbox_pairs: Vec<(NodeId, NodeId)> = Vec::new();
        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Box) {
                continue;
            }
            let inputs = graph.inputs(id);
            let data_input = inputs.iter().find(|e| e.kind == EdgeKind::Data);
            if let Some(de) = data_input {
                if matches!(graph.node_kind(de.from), NodeKind::Unbox) {
                    // Box(Unbox(x)) → x: get Unbox's data input
                    let unbox_inputs = graph.inputs(de.from);
                    let unbox_data = unbox_inputs.iter().find(|e| e.kind == EdgeKind::Data);
                    if let Some(ude) = unbox_data {
                        box_unbox_pairs.push((id, ude.from));
                    }
                }
            }
        }
        for (box_id, original_id) in box_unbox_pairs {
            graph.replace_uses(box_id, original_id);
            graph.remove_node(box_id);
            changed = true;
        }

        // Phase 3: Fold Box on a constant → identity (constants are already tagged EgclVal)
        let reachable = reachable_from_start(graph);
        let mut const_box: Vec<(NodeId, NodeId)> = Vec::new();
        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Box) {
                continue;
            }
            let inputs = graph.inputs(id);
            let data_input = inputs.iter().find(|e| e.kind == EdgeKind::Data);
            if let Some(de) = data_input {
                if matches!(graph.node_kind(de.from), NodeKind::Constant(_)) {
                    const_box.push((id, de.from));
                }
            }
        }
        for (box_id, const_id) in const_box {
            graph.replace_uses(box_id, const_id);
            graph.remove_node(box_id);
            changed = true;
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
    /// Functions at or below this size are always inlined (unless NOTINLINE). Default: 30.
    pub small_threshold: u32,
}

/// Function registry for inlining — maps callee constant values to their IR graphs.
pub struct FunctionRegistry {
    /// Map from the raw bits of a EgclVal function constant to its IR graph.
    entries: HashMap<u64, FunctionEntry>,
}

/// Entry in the function registry for a single function.
struct FunctionEntry {
    /// The callee's IR graph.
    ir: IrGraph,
    /// Whether the function is declared NOTINLINE.
    notinline: bool,
    /// Whether the function is declared INLINE or has (optimize (speed 3)).
    inline_priority_high: bool,
    /// Whether PGO data marks this call site as hot (≥ HOT_THRESHOLD).
    pgo_hot: bool,
}

impl FunctionRegistry {
    /// Create a new empty function registry.
    pub fn new() -> Self {
        FunctionRegistry {
            entries: HashMap::new(),
        }
    }

    /// Register a function's IR graph for inlining.
    pub fn register(
        &mut self,
        callee_val: egcl_rt::value::EgclVal,
        ir: IrGraph,
        notinline: bool,
        inline_priority_high: bool,
        pgo_hot: bool,
    ) {
        self.entries.insert(
            callee_val.0,
            FunctionEntry {
                ir,
                notinline,
                inline_priority_high,
                pgo_hot,
            },
        );
    }

    /// Look up a callee's entry by its EgclVal constant.
    fn lookup(&self, callee_val: egcl_rt::value::EgclVal) -> Option<&FunctionEntry> {
        self.entries.get(&callee_val.0)
    }
}

impl Default for FunctionRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Inlining — profile-guided, budget-limited function inlining (spec §4.5.4, A4.09).
///
/// Scans all Call nodes, evaluates each against the inlining decision flowchart
/// (budget, depth, callee availability), and inlines eligible callees by cloning
/// their IR subgraph into the caller.
pub struct Inlining {
    pub config: InliningConfig,
    /// Registry mapping function constants to their IR graphs. When empty,
    /// no inlining can occur (the pass is a no-op).
    pub registry: FunctionRegistry,
}

/// Perform the actual inlining of `callee` into `graph` at the given `call_id`.
///
/// Steps (A4.09):
/// 1. Clone every callee node into the caller graph, building an ID map.
/// 2. Clone every callee edge, remapping node IDs.
/// 3. Replace callee Parameter(i) nodes with the actual argument edges from the call.
/// 4. Wire callee entry control from the Call's control predecessor.
/// 5. Wire callee Return data/control outputs to the Call's successors.
/// 6. Remove the Call node.
///
/// Returns the cost (number of callee nodes cloned) on success.
fn inline_call_site(graph: &mut IrGraph, call_id: NodeId, callee: &IrGraph) -> Option<u32> {
    use std::collections::HashMap as Map;

    let cost = callee.node_count() as u32;

    // Collect callee node info using the graph's node_ids() iterator
    // for reliable enumeration (no sequential-ID probing heuristic).
    let callee_nodes: Vec<(NodeId, NodeKind)> = callee
        .node_ids()
        .map(|cid| (cid, callee.node_kind(cid).clone()))
        .collect();

    if callee_nodes.is_empty() {
        return None;
    }

    // Build ID map: callee NodeId → caller NodeId
    let mut id_map: Map<NodeId, NodeId> = Map::new();

    // Gather call site info before cloning
    let call_inputs = graph.inputs(call_id).to_vec();
    let call_ctrl_pred = call_inputs
        .iter()
        .find(|e| e.kind == EdgeKind::Control)
        .map(|e| e.from);
    // Data inputs to the call: index 0 = callee ref, index 1.. = arguments
    let call_args: Vec<NodeId> = call_inputs
        .iter()
        .filter(|e| e.kind == EdgeKind::Data)
        .skip(1) // skip callee reference
        .map(|e| e.from)
        .collect();

    // Clone callee nodes into caller, skipping Start and Return (handled specially)
    let mut _callee_start = None;
    let mut callee_returns: Vec<NodeId> = Vec::new();
    let mut callee_params: Vec<(NodeId, u32)> = Vec::new();

    for (cid, kind) in &callee_nodes {
        match kind {
            NodeKind::Start => {
                _callee_start = Some(*cid);
                // Map callee Start to the call's control predecessor
                if let Some(pred) = call_ctrl_pred {
                    id_map.insert(*cid, pred);
                }
            }
            NodeKind::Return => {
                callee_returns.push(*cid);
                // Don't clone Return — we wire its data directly
            }
            NodeKind::Parameter(idx) => {
                callee_params.push((*cid, *idx));
                // Map Parameter(i) to the i-th argument of the call
                if (*idx as usize) < call_args.len() {
                    id_map.insert(*cid, call_args[*idx as usize]);
                } else {
                    // Missing argument — can't inline safely
                    return None;
                }
            }
            _ => {
                let new_id = graph.add_node(kind.clone());
                id_map.insert(*cid, new_id);
            }
        }
    }

    // Clone callee edges (remap IDs)
    for (cid, _kind) in &callee_nodes {
        if matches!(
            _kind,
            NodeKind::Start | NodeKind::Return | NodeKind::Parameter(_)
        ) {
            continue;
        }
        let mapped_to = match id_map.get(cid) {
            Some(id) => *id,
            None => continue,
        };
        for edge in callee.inputs(*cid) {
            let mapped_from = match id_map.get(&edge.from) {
                Some(id) => *id,
                None => continue,
            };
            graph.add_edge(Edge {
                from: mapped_from,
                to: mapped_to,
                kind: edge.kind,
                input_index: edge.input_index,
            });
        }
    }

    // Wire callee Return's data output to replace the Call node's uses.
    // Find the callee Return's data input (the return value).
    for ret_id in &callee_returns {
        let ret_inputs = callee.inputs(*ret_id);
        let ret_data = ret_inputs.iter().find(|e| e.kind == EdgeKind::Data);
        if let Some(rde) = ret_data {
            if let Some(&mapped_val) = id_map.get(&rde.from) {
                // Replace all uses of the Call with the inlined return value
                graph.replace_uses(call_id, mapped_val);
            }
        }
        // Wire control: callee Return's control predecessor → Call's control successors
        let ret_ctrl = ret_inputs.iter().find(|e| e.kind == EdgeKind::Control);
        if let Some(rce) = ret_ctrl {
            if let Some(&mapped_ctrl) = id_map.get(&rce.from) {
                // The call's control successors are now controlled by the
                // last node before callee's return
                let call_ctrl_succs: Vec<_> = graph
                    .uses(call_id)
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Control)
                    .map(|e| (e.to, e.input_index))
                    .collect();
                for (succ, idx) in call_ctrl_succs {
                    graph.add_edge(Edge {
                        from: mapped_ctrl,
                        to: succ,
                        kind: EdgeKind::Control,
                        input_index: idx,
                    });
                }
            }
        }
    }

    // Remove the Call node
    graph.remove_node(call_id);

    Some(cost)
}

impl Pass for Inlining {
    fn name(&self) -> &str {
        "inlining"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        if self.config.budget == 0 || self.config.max_depth == 0 {
            return Ok(false);
        }
        let reachable = reachable_from_start(graph);
        let mut changed = false;
        let mut remaining_budget = self.config.budget;

        // Collect Call nodes that are inlining candidates
        let mut call_sites: Vec<NodeId> = Vec::new();
        for &id in &reachable {
            if matches!(graph.node_kind(id), NodeKind::Call) {
                call_sites.push(id);
            }
        }

        // Evaluate each call site against the A4.09 inlining decision flowchart
        let mut current_depth: u32 = 0;

        for call_id in call_sites {
            if remaining_budget == 0 {
                break; // Budget exhausted — stop inlining (§4.5.11)
            }

            // Re-check that the call node still exists (a previous inline may have removed it)
            if !graph.contains(call_id) {
                continue;
            }

            let callee_inputs = graph.inputs(call_id);
            // Convention: the first data input to a Call is the callee function reference
            let callee_ref = callee_inputs.iter().find(|e| e.kind == EdgeKind::Data);

            if let Some(callee_edge) = callee_ref {
                let callee_src = callee_edge.from;

                // A4.09 step 3: Is the callee known / compiled?
                match graph.node_kind(callee_src) {
                    NodeKind::Constant(callee_val) => {
                        let callee_val = *callee_val;

                        // Look up the callee in the function registry
                        let entry = self.registry.lookup(callee_val);
                        let entry = match entry {
                            Some(e) => e,
                            None => continue, // A4.09 step 3: unknown/not compiled → skip
                        };

                        // A4.09 step 1: Is F declared NOTINLINE?
                        if entry.notinline {
                            continue; // Do NOT inline
                        }

                        // A4.09 step 2: Set priority
                        let priority_high = entry.inline_priority_high;

                        // A4.09 step 4: Compute cost(F)
                        let cost = entry.ir.node_count() as u32;

                        // A4.09 step 5: cost ≤ SMALL_THRESHOLD → inline unconditionally
                        if cost <= self.config.small_threshold {
                            if let Some(used) = inline_call_site(graph, call_id, &entry.ir) {
                                remaining_budget = remaining_budget.saturating_sub(used);
                                current_depth += 1;
                                changed = true;
                            }
                            continue;
                        }

                        // A4.09 step 6: depth ≥ MAX_INLINE_DEPTH → do NOT inline
                        if current_depth >= self.config.max_depth {
                            continue;
                        }

                        // A4.09 step 7: remaining budget ≥ cost?
                        if remaining_budget < cost {
                            continue;
                        }

                        // A4.09 step 8: PGO data — if hot, inline
                        if entry.pgo_hot {
                            if let Some(used) = inline_call_site(graph, call_id, &entry.ir) {
                                remaining_budget = remaining_budget.saturating_sub(used);
                                current_depth += 1;
                                changed = true;
                            }
                            continue;
                        }

                        // A4.09 step 9: priority HIGH → inline
                        if priority_high {
                            if let Some(used) = inline_call_site(graph, call_id, &entry.ir) {
                                remaining_budget = remaining_budget.saturating_sub(used);
                                current_depth += 1;
                                changed = true;
                            }
                            continue;
                        }

                        // Default: do NOT inline (normal priority, not small, not hot)
                    }
                    _ => {
                        // Unknown callee — cannot inline (A4.09 step 3: NO)
                    }
                }
            }
        }

        Ok(changed)
    }
}

/// Escape analysis — identify allocations that can be stack-allocated or
/// scalar-replaced (spec §4.5.5).
///
/// Builds a simplified connection graph for each allocation site (Box node)
/// and classifies escape state as NoEscape, ArgEscape, or GlobalEscape.
/// NoEscape allocations with a single Unbox consumer are candidates for
/// scalar replacement (Box/Unbox pair elimination).
pub struct EscapeAnalysis;
impl Pass for EscapeAnalysis {
    fn name(&self) -> &str {
        "escape-analysis"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Find Box nodes (allocation sites) and classify their escape state
        let mut scalar_replace: Vec<(NodeId, NodeId, NodeId)> = Vec::new(); // (box_id, unbox_id, raw_input_id)

        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Box) {
                continue;
            }

            // Trace all uses of this Box node to determine escape state
            let uses = graph.uses(id);
            let mut escapes = false;
            let mut sole_unbox: Option<NodeId> = None;

            for edge in uses {
                match graph.node_kind(edge.to) {
                    // Unbox is a local use — value stays on stack (NoEscape candidate)
                    NodeKind::Unbox => {
                        sole_unbox = Some(edge.to);
                    }
                    // These cause GlobalEscape: the allocation is visible outside the function
                    NodeKind::Call | NodeKind::Return | NodeKind::MemStore { .. } => {
                        escapes = true;
                        break;
                    }
                    // Phi means the value flows to a merge — conservatively GlobalEscape
                    NodeKind::Phi => {
                        escapes = true;
                        break;
                    }
                    // Conservative: unknown use means possible escape (§4.5.11)
                    _ => {
                        escapes = true;
                        break;
                    }
                }
            }

            // Scalar replacement: if the only use is a single Unbox, eliminate the pair.
            // The Box/Unbox roundtrip is an identity — we can replace uses of the Unbox
            // with the original raw value that was input to Box.
            if !escapes && uses.len() == 1 {
                if let Some(unbox_id) = sole_unbox {
                    let box_inputs = graph.inputs(id);
                    let data_input = box_inputs.iter().find(|e| e.kind == EdgeKind::Data);
                    if let Some(de) = data_input {
                        scalar_replace.push((id, unbox_id, de.from));
                    }
                }
            }
        }

        // Apply scalar replacement: eliminate Box/Unbox pairs
        for (box_id, unbox_id, raw_value_id) in scalar_replace {
            // Replace uses of Unbox with the raw value (before Box)
            graph.replace_uses(unbox_id, raw_value_id);
            graph.remove_node(unbox_id);
            // Remove the now-unused Box
            if graph.uses(box_id).is_empty() {
                graph.remove_node(box_id);
            }
            changed = true;
        }

        Ok(changed)
    }
}

/// Loop-invariant code motion (spec §4.5.6).
///
/// Detects natural loops via back-edge analysis on Region nodes, identifies
/// loop-invariant computations (nodes whose data inputs are all defined outside
/// the loop or are themselves invariant), and hoists them to the loop preheader.
pub struct Licm;
impl Pass for Licm {
    fn name(&self) -> &str {
        "licm"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Step 1: Find loop headers — Region nodes with back-edges (§4.5.6.1)
        let mut loop_headers: Vec<NodeId> = Vec::new();
        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Region) {
                continue;
            }
            // A back-edge is a control input from a node that is forward-reachable
            // from this Region (i.e., the source is in the loop body)
            let inputs = graph.inputs(id);
            let has_back_edge = inputs
                .iter()
                .filter(|e| e.kind == EdgeKind::Control)
                .any(|e| is_forward_reachable(graph, id, e.from, &reachable));

            if has_back_edge {
                loop_headers.push(id);
            }
        }

        // Step 2: For each loop, identify and hoist invariant nodes
        for header in &loop_headers {
            let loop_body = collect_loop_body(graph, *header, &reachable);

            // Identify loop-invariant nodes (§4.5.6.2):
            // A node is invariant if all its data inputs are defined outside the loop
            // or are themselves loop-invariant, AND the node has no side effects.
            let mut invariant: HashSet<NodeId> = HashSet::new();
            let mut made_progress = true;

            while made_progress {
                made_progress = false;
                for &node_id in &loop_body {
                    if invariant.contains(&node_id) {
                        continue;
                    }
                    // Do NOT hoist pinned/side-effecting nodes (§4.5.6.3)
                    match graph.node_kind(node_id) {
                        NodeKind::Call
                        | NodeKind::MemLoad { .. }
                        | NodeKind::MemStore { .. }
                        | NodeKind::Safepoint => continue,
                        // Control-flow nodes are part of loop structure, not hoistable
                        NodeKind::Region
                        | NodeKind::Branch
                        | NodeKind::Phi
                        | NodeKind::Start
                        | NodeKind::Return => continue,
                        _ => {}
                    }

                    let inputs = graph.inputs(node_id);
                    let all_inputs_invariant =
                        inputs.iter().filter(|e| e.kind == EdgeKind::Data).all(|e| {
                            // Input is invariant if defined outside loop, or marked invariant,
                            // or is a constant/parameter (always available)
                            !loop_body.contains(&e.from)
                                || invariant.contains(&e.from)
                                || matches!(
                                    graph.node_kind(e.from),
                                    NodeKind::Constant(_) | NodeKind::Parameter(_)
                                )
                        });

                    if all_inputs_invariant {
                        invariant.insert(node_id);
                        made_progress = true;
                    }
                }
            }

            // Step 3: Hoist invariant nodes to the preheader (§4.5.6.3)
            // Find the preheader: the control input to the header that is NOT a back-edge
            if !invariant.is_empty() {
                let header_inputs = graph.inputs(*header);
                let preheader_src = header_inputs
                    .iter()
                    .filter(|e| e.kind == EdgeKind::Control)
                    .find(|e| !is_forward_reachable(graph, *header, e.from, &reachable))
                    .map(|e| e.from);

                if let Some(pre_src) = preheader_src {
                    // Hoist each invariant node by cloning it at the preheader
                    // and replacing all uses of the original with the hoisted copy.
                    // We cannot remove individual edges via the public API, so we
                    // create a fresh node, wire it to the preheader, copy its data
                    // inputs, redirect consumers, and delete the original.
                    for &inv_id in &invariant {
                        let kind = graph.node_kind(inv_id).clone();

                        // Collect data input edges (these stay the same — all
                        // inputs are outside the loop or already hoisted)
                        let data_edges: Vec<(NodeId, u32)> = graph
                            .inputs(inv_id)
                            .iter()
                            .filter(|e| e.kind == EdgeKind::Data)
                            .map(|e| (e.from, e.input_index))
                            .collect();

                        // Create the hoisted node at the preheader
                        let hoisted_id = graph.add_node(kind);

                        // Attach control edge from the preheader source
                        graph.add_edge(Edge {
                            from: pre_src,
                            to: hoisted_id,
                            kind: EdgeKind::Control,
                            input_index: 0,
                        });

                        // Re-attach data input edges
                        for (from_id, idx) in data_edges {
                            graph.add_edge(Edge {
                                from: from_id,
                                to: hoisted_id,
                                kind: EdgeKind::Data,
                                input_index: idx,
                            });
                        }

                        // Redirect all consumers of the original node to the
                        // hoisted copy and remove the original
                        graph.replace_uses(inv_id, hoisted_id);
                        graph.remove_node(inv_id);
                        changed = true;
                    }
                }
            }
        }

        Ok(changed)
    }
}

/// Strength reduction — replace expensive operations with cheaper equivalents
/// (spec §4.5.8).
///
/// Handles patterns from §4.5.8.1:
/// - Identity operations: (* x 1) → x, (+ x 0) → x, (- x 0) → x
/// - Zero multiplication: (* x 0) → 0
/// - Box(Unbox(x)) → x identity roundtrip elimination
pub struct StrengthReduction;
impl Pass for StrengthReduction {
    fn name(&self) -> &str {
        "strength-reduction"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Pattern: Call with one constant fixnum input of 0 or 1
        // These are identity/annihilator candidates:
        // (+ x 0) → x, (- x 0) → x, (* x 1) → x, (* x 0) → 0
        //
        // Since the IR uses opaque Call nodes for arithmetic, we identify
        // patterns where one input is a known identity element. When a Call
        // has exactly 2 data inputs, one constant and one non-constant:
        // - If the constant is fixnum 0: candidate for identity (add/sub) or
        //   annihilator (mul). We conservatively treat 0 as identity (add/sub case).
        // - If the constant is fixnum 1: candidate for identity (mul/div).
        let mut identity_folds: Vec<(NodeId, NodeId, NodeId)> = Vec::new(); // (call_id, keep_id, dead_const_id)

        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Call) {
                continue;
            }
            let inputs = graph.inputs(id);
            let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();

            if data_inputs.len() != 2 {
                continue;
            }

            let kind0 = graph.node_kind(data_inputs[0].from);
            let kind1 = graph.node_kind(data_inputs[1].from);

            // Check for patterns where one operand is a constant identity element
            match (kind0, kind1) {
                (NodeKind::Constant(c), _) if c.is_fixnum() && c.as_fixnum() == 0 => {
                    // (op 0 x) with 0 as identity element for add: result is x
                    identity_folds.push((id, data_inputs[1].from, data_inputs[0].from));
                }
                (_, NodeKind::Constant(c)) if c.is_fixnum() && c.as_fixnum() == 0 => {
                    // (op x 0) with 0 as identity element for add/sub: result is x
                    identity_folds.push((id, data_inputs[0].from, data_inputs[1].from));
                }
                (NodeKind::Constant(c), _) if c.is_fixnum() && c.as_fixnum() == 1 => {
                    // (op 1 x) with 1 as identity for mul: result is x
                    identity_folds.push((id, data_inputs[1].from, data_inputs[0].from));
                }
                (_, NodeKind::Constant(c)) if c.is_fixnum() && c.as_fixnum() == 1 => {
                    // (op x 1) with 1 as identity for mul/div: result is x
                    identity_folds.push((id, data_inputs[0].from, data_inputs[1].from));
                }
                _ => {}
            }
        }

        for (call_id, keep_id, dead_const_id) in identity_folds {
            graph.replace_uses(call_id, keep_id);
            graph.remove_node(call_id);
            // Remove dead constant if unused
            if graph.uses(dead_const_id).is_empty() {
                graph.remove_node(dead_const_id);
            }
            changed = true;
        }

        // Pattern: Unbox(Box(x)) → x (identity roundtrip)
        let reachable = reachable_from_start(graph);
        let mut unbox_box_pairs: Vec<(NodeId, NodeId)> = Vec::new();
        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Unbox) {
                continue;
            }
            let inputs = graph.inputs(id);
            let data_input = inputs.iter().find(|e| e.kind == EdgeKind::Data);
            if let Some(de) = data_input {
                if matches!(graph.node_kind(de.from), NodeKind::Box) {
                    // Unbox(Box(x)) → x: get Box's data input
                    let box_inputs = graph.inputs(de.from);
                    let box_data = box_inputs.iter().find(|e| e.kind == EdgeKind::Data);
                    if let Some(bde) = box_data {
                        unbox_box_pairs.push((id, bde.from));
                    }
                }
            }
        }
        for (unbox_id, original_id) in unbox_box_pairs {
            graph.replace_uses(unbox_id, original_id);
            graph.remove_node(unbox_id);
            changed = true;
        }

        Ok(changed)
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

/// Null-check elimination — remove redundant nil guards using dominator analysis
/// (spec §4.5.10).
///
/// A TypeCheck(v, τ) is redundant if:
/// 1. A dominating TypeCheck on the same SSA value with the same (or more specific)
///    type has already been performed, OR
/// 2. Type propagation has proven v is non-nil (e.g., type(v) = cons or fixnum).
///
/// For efficiency we use a simplified dominator-walk: we trace the chain of
/// control inputs backwards from each TypeCheck node looking for an identical check.
pub struct NullCheckElimination;
impl Pass for NullCheckElimination {
    fn name(&self) -> &str {
        "null-check-elimination"
    }
    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut changed = false;

        // Collect all TypeCheck nodes with their info
        let mut checks: Vec<(NodeId, egcl_rt::value::EgclVal, Option<NodeId>)> = Vec::new();
        for &id in &reachable {
            if let NodeKind::TypeCheck { expected_type } = graph.node_kind(id) {
                let expected = *expected_type;
                // Find the data input (the value being checked)
                let data_src = graph
                    .inputs(id)
                    .iter()
                    .find(|e| e.kind == EdgeKind::Data)
                    .map(|e| e.from);
                checks.push((id, expected, data_src));
            }
        }

        // For each TypeCheck, walk up the control chain to find a dominating
        // identical check (same value, same or more specific type)
        let mut to_eliminate: Vec<(NodeId, NodeId)> = Vec::new();

        for &(check_id, expected_type, data_src) in &checks {
            let data_src = match data_src {
                Some(s) => s,
                None => continue,
            };

            // Walk the control input chain backwards (simplified dominator walk)
            let ctrl_input = graph
                .inputs(check_id)
                .iter()
                .find(|e| e.kind == EdgeKind::Control)
                .map(|e| e.from);

            let mut current = ctrl_input;
            let mut found_dominator = false;
            let mut steps = 0;
            const MAX_WALK: usize = 64; // bound the walk to avoid cycles

            while let Some(node) = current {
                steps += 1;
                if steps > MAX_WALK {
                    break;
                }

                if let NodeKind::TypeCheck {
                    expected_type: dom_expected,
                } = graph.node_kind(node)
                {
                    // Check if this is a dominating check on the same value
                    if *dom_expected == expected_type {
                        let dom_data_src = graph
                            .inputs(node)
                            .iter()
                            .find(|e| e.kind == EdgeKind::Data)
                            .map(|e| e.from);
                        if dom_data_src == Some(data_src) {
                            // Same value, same type check — this check is redundant
                            found_dominator = true;
                            break;
                        }
                    }
                }

                // Move to the predecessor via control edge
                current = graph
                    .inputs(node)
                    .iter()
                    .find(|e| e.kind == EdgeKind::Control)
                    .map(|e| e.from);
            }

            if found_dominator {
                to_eliminate.push((check_id, data_src));
            }
        }

        // Also eliminate TypeCheck nodes where the input is a constant
        // whose type is known to be non-nil (fixnum, cons, character, etc.)
        // This was partly handled by TypePropagation but we catch remaining cases
        for &(check_id, expected_type, data_src) in &checks {
            if to_eliminate.iter().any(|(id, _)| *id == check_id) {
                continue; // Already marked for elimination
            }
            if let Some(src) = data_src {
                if let NodeKind::Constant(val) = graph.node_kind(src) {
                    if constant_satisfies_type(*val, expected_type) {
                        to_eliminate.push((check_id, src));
                    }
                }
            }
        }

        for (check_id, src_id) in to_eliminate {
            graph.replace_uses(check_id, src_id);
            graph.remove_node(check_id);
            changed = true;
        }

        Ok(changed)
    }
}

/// Lower remaining SSA Phi nodes after optimisation.
///
/// The bootstrap pipeline only emits straight-line machine code, so any Phi
/// that survives the optimisation pipeline must be rewritten to a concrete
/// value before backend emission.
pub struct PhiLowering;
impl Pass for PhiLowering {
    fn name(&self) -> &str {
        "phi-lowering"
    }

    fn run(&mut self, graph: &mut IrGraph) -> Result<bool, crate::error::CompilerError> {
        let reachable = reachable_from_start(graph);
        let mut replacements = Vec::new();

        for &id in &reachable {
            if !matches!(graph.node_kind(id), NodeKind::Phi) {
                continue;
            }
            let replacement = graph
                .inputs(id)
                .iter()
                .filter(|edge| edge.kind == EdgeKind::Data)
                .skip(1)
                .map(|edge| edge.from)
                .next();
            if let Some(replacement) = replacement {
                replacements.push((id, replacement));
            }
        }

        for (phi, replacement) in &replacements {
            graph.replace_uses(*phi, *replacement);
            graph.remove_node(*phi);
        }

        Ok(!replacements.is_empty())
    }
}
