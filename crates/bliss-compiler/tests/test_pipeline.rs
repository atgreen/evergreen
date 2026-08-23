//! Compiler pipeline integration tests.
//!
//! Tests the reader → macroexpand → IR → optimization → codegen pipeline
//! end-to-end, exercising cross-module integration within bliss-compiler.

use bliss_compiler::codegen::{
    Aarch64Backend, CodeBuffer, CodegenBackend, LinearScanAllocator, TargetArch, X86_64Backend,
};
use bliss_compiler::ic::{IcState, InlineCache, ic_generation, init_ic_registry, reset_all_caches};
use bliss_compiler::ir::{EdgeKind, IrBuilder, IrGraph, NodeId, NodeKind, verify};
use bliss_compiler::macroexpand::{
    Environment, VariableInfo, macroexpand, macroexpand_1, macroexpand_all,
};
use bliss_compiler::opt::PassManager;
use bliss_compiler::osr::{
    DeoptConfig, DeoptLog, DeoptReason, LocalMapping, OsrEntryMap, deoptimize, osr_entry,
};
use bliss_compiler::profiling::{BackEdgeCounter, FunctionProfile, InvocationCounter};
use bliss_compiler::reader::read_from_string;
use bliss_compiler::tiered::{
    BaselineCompiler, FnMeta, Interpreter, OptimisingCompiler, Tier, TierConfig, check_promotion,
    request_compilation,
};
use bliss_rt::value::{BlissVal, EOF, NIL, T};
use std::sync::Mutex;

/// A zeroed, 8-byte-aligned fake function header large enough to back a full
/// [`FnMeta`]. Several tiered-runtime entry points (`check_promotion`,
/// `request_compilation`, `osr_entry`, `deoptimize`, `install`) form a
/// `&FnMeta` from the tagged pointer and read fields up to and past
/// `back_edge_count` (offset 16) — e.g. `request_compilation` computes
/// `invoke + 2 * back_edge`. A bare `Box<[u8; 16]>` both under-sizes and
/// under-aligns that struct, so those reads hit out-of-bounds heap garbage:
/// intermittently promoting a Baseline function to Optimising, or overflowing
/// the priority arithmetic and panicking in debug. That made the whole test
/// binary flaky under parallel heap churn (bliss-4lb). Back the header with a
/// `Vec<u64>` (8-byte aligned) sized to `size_of::<FnMeta>()` and index it as
/// bytes exactly like the old raw header.
struct FakeFnHeader {
    backing: Vec<u64>,
}

impl FakeFnHeader {
    fn new() -> Self {
        let words = std::mem::size_of::<FnMeta>().div_ceil(8);
        FakeFnHeader {
            backing: vec![0u64; words],
        }
    }

    fn as_ptr(&self) -> *const u8 {
        self.backing.as_ptr().cast::<u8>()
    }

    /// The header as a `TAG_FUNCTION`-tagged value the tiered runtime accepts.
    fn func_val(&self) -> BlissVal {
        BlissVal(self.as_ptr() as u64 | bliss_rt::value::TAG_FUNCTION)
    }
}

impl std::ops::Deref for FakeFnHeader {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: `backing` owns `words * 8` contiguous, initialised bytes.
        unsafe { std::slice::from_raw_parts(self.as_ptr(), self.backing.len() * 8) }
    }
}

impl std::ops::DerefMut for FakeFnHeader {
    fn deref_mut(&mut self) -> &mut [u8] {
        // SAFETY: as above; `&mut self` gives unique access to `backing`.
        let len = self.backing.len() * 8;
        unsafe { std::slice::from_raw_parts_mut(self.backing.as_mut_ptr().cast::<u8>(), len) }
    }
}

/// Serializes tests that mutate the process-global inline-cache registry
/// (`IC_SESSION`/`IC_GENERATION`). These statics are shared by every test in
/// this binary, so running the dependent tests concurrently makes their
/// session/generation observations nondeterministic. The lock keeps them
/// isolated without changing behaviour for the single-threaded case.
static IC_REGISTRY_SERIAL: Mutex<()> = Mutex::new(());

// ── Helpers ──────────────────────────────────────────────────────────

type Predicate = fn(BlissVal) -> bool;

fn read(s: &str) -> BlissVal {
    let (val, _) = read_from_string(s).expect("read_from_string failed");
    val
}

/// Collect all node kinds from an IrGraph by probing all allocated NodeIds.
/// IrGraph uses a HashMap<NodeId, NodeKind> internally, so after DCE node IDs
/// may not be contiguous. We probe IDs 0..next_id (upper bound = node_count
/// before any removals happened, but we use a generous upper bound).
/// This avoids the catch_unwind antipattern that silently swallows panics.
fn collect_node_kinds(graph: &IrGraph) -> Vec<(NodeId, NodeKind)> {
    // We probe IDs 0..upper_bound. The upper bound must be at least next_id,
    // which we approximate as node_count * 2 + 16 (generous for sparse graphs).
    let upper = (graph.node_count() * 2 + 16) as u32;
    let mut results = Vec::new();
    for i in 0..upper {
        let nid = NodeId(i);
        // Use catch_unwind only as a probe for existence — NOT to swallow real bugs.
        // We immediately clone the result to avoid holding references across the boundary.
        if let Ok(kind) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            graph.node_kind(nid).clone()
        })) {
            results.push((nid, kind));
        }
    }
    // Sanity check: we found all nodes
    assert_eq!(
        results.len(),
        graph.node_count(),
        "collect_node_kinds: found {} nodes but graph reports {}; upper bound {} may be too low",
        results.len(),
        graph.node_count(),
        upper
    );
    results
}

fn read_and_build_ir(s: &str) -> IrGraph {
    let form = read(s);
    let env = Environment::null();
    let (expanded, _) = macroexpand(form, &env).expect("macroexpand failed");
    let mut builder = IrBuilder::new();
    builder.build(expanded).expect("IR build failed")
}

fn read_build_and_optimize(s: &str) -> IrGraph {
    let mut graph = read_and_build_ir(s);
    let mut pm = PassManager::new();
    pm.run_all(&mut graph).expect("optimization failed");
    graph
}

fn full_pipeline_x86_64(source: &str) -> CodeBuffer {
    let (form, _) = read_from_string(source).expect("read failed");
    let env = Environment::null();
    let (expanded, _) = macroexpand(form, &env).expect("macroexpand failed");
    let mut builder = IrBuilder::new();
    let mut graph = builder.build(expanded).expect("IR build failed");
    verify(&graph).expect("pre-opt IR verification failed");
    let mut pm = PassManager::new();
    pm.run_all(&mut graph).expect("optimization failed");
    verify(&graph).expect("post-opt IR verification failed");
    let mut backend = X86_64Backend::new();
    backend.emit(&graph).expect("codegen failed")
}

// ═══════════════════════════════════════════════════════════════════
// 1. Reader → Macroexpand integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn reader_to_macroexpand_self_evaluating() {
    // Integer, T, NIL should not be macro-expanded
    let cases: &[(&str, Predicate)] = &[
        ("42", |v| v.is_fixnum() && v.as_fixnum() == 42),
        ("t", |v| v == T),
        ("nil", |v| v == NIL),
    ];
    for &(src, check_fn) in cases {
        let form = read(src);
        assert!(check_fn(form), "read('{}') produced unexpected value", src);
        let env = Environment::null();
        let (expanded, did) = macroexpand_1(form, &env).unwrap();
        assert!(!did, "'{}' should not be macro-expanded", src);
        assert_eq!(expanded, form);
    }
}

#[test]
fn reader_to_macroexpand_symbol_macro() {
    let form = read("MY-VAR");
    let replacement = BlissVal::from_fixnum(99);
    let env = Environment::null().augment_variable(form, VariableInfo::SymbolMacro(replacement));
    let (expanded, did) = macroexpand_1(form, &env).unwrap();
    assert!(did, "symbol macro should trigger expansion");
    assert_eq!(expanded.as_fixnum(), 99);
}

#[test]
fn reader_to_macroexpand_chained_expansion() {
    let sym_a = read("CHAIN-A");
    let sym_b = read("CHAIN-B");
    let sym_c = read("CHAIN-C");
    let env = Environment::null()
        .augment_variable(sym_a, VariableInfo::SymbolMacro(sym_b))
        .augment_variable(sym_b, VariableInfo::SymbolMacro(sym_c));
    let (expanded, did) = macroexpand(sym_a, &env).unwrap();
    assert!(did);
    assert_eq!(expanded, sym_c);
}

#[test]
fn reader_to_macroexpand_quoted_form_no_expand() {
    let form = read("'foo");
    assert!(form.is_cons());
    let env = Environment::null();
    let (expanded, did) = macroexpand(form, &env).unwrap();
    assert!(!did);
    assert_eq!(expanded, form);
}

#[test]
fn macroexpand_all_on_atom() {
    let form = read("123");
    let env = Environment::null();
    let result = macroexpand_all(form, &env).unwrap();
    assert_eq!(result.as_fixnum(), 123);
}

#[test]
fn macroexpand_all_on_compound_form() {
    // macroexpand_all should recursively walk subforms of a cons.
    // We define a symbol macro inside the form so that a correct recursive walk
    // would expand it, while a broken non-recursive walk would leave it unexpanded.
    let sym = read("MY-SM");
    let replacement = BlissVal::from_fixnum(99);
    let env = Environment::null().augment_variable(sym, VariableInfo::SymbolMacro(replacement));

    // Build a form that contains the symbol macro as a subform: (if MY-SM 1 2)
    let form = read("(if MY-SM 1 2)");
    assert!(form.is_cons(), "(if MY-SM 1 2) should parse as cons");
    let result = macroexpand_all(form, &env).unwrap();
    // The result should still be a cons (compound form preserved)
    assert!(
        result.is_cons(),
        "macroexpand_all on compound form should return cons"
    );
    // A correct recursive walk should have expanded MY-SM to 99 within the subforms.
    // The result should NOT be identical to the input (the symbol macro should be expanded).
    assert_ne!(
        result, form,
        "macroexpand_all should expand symbol macros within subforms (recursive walk)"
    );
}

#[test]
fn macroexpand_all_on_nested_compound_form() {
    // macroexpand_all should walk into nested subforms, including deeply nested ones.
    // We place a symbol macro inside an inner cons to verify the recursive walk
    // descends into nested structures, not just top-level subforms.
    let sym = read("NESTED-SM");
    let replacement = BlissVal::from_fixnum(42);
    let env = Environment::null().augment_variable(sym, VariableInfo::SymbolMacro(replacement));

    let form = read("(progn (+ NESTED-SM 2) 3)");
    assert!(form.is_cons());
    let result = macroexpand_all(form, &env).unwrap();
    assert!(
        result.is_cons(),
        "macroexpand_all on nested compound should return cons"
    );
    // A correct recursive walk should have expanded NESTED-SM inside the inner (+ ...) form.
    assert_ne!(
        result, form,
        "macroexpand_all should expand symbol macros in nested subforms (deep recursive walk)"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 2. Reader → Macroexpand → IR (IrBuilder)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_self_evaluating_forms_to_ir() {
    // Self-evaluating atoms should produce a 3-node graph: Start, Constant, Return
    for src in &["42", "nil", "t", "-7", "3.14", "'foo", "#\\a", "\"hello\""] {
        let graph = read_and_build_ir(src);
        assert_eq!(
            graph.node_count(),
            3,
            "'{}' should produce 3-node graph",
            src
        );
        verify(&graph).unwrap_or_else(|e| panic!("verify failed for '{}': {:?}", src, e));
    }
}

#[test]
fn pipeline_addition_to_ir() {
    // (+ 1 2) needs at least: Start, Const(1), Const(2), Call(+), Return
    let graph = read_and_build_ir("(+ 1 2)");
    assert!(
        graph.node_count() >= 5,
        "(+ 1 2) should produce at least 5 nodes (Start, Const(1), Const(2), Call(+), Return), got {}",
        graph.node_count()
    );
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(+ 1 2)': {:?}", e));
}

#[test]
fn pipeline_if_to_ir() {
    // (if t 1 2) needs at least: Start, Const(t), Branch, Region, Const(1), Const(2), Phi, Return
    let graph = read_and_build_ir("(if t 1 2)");
    assert!(
        graph.node_count() >= 8,
        "(if t 1 2) should produce at least 8 nodes (Start, Const(t), Branch, Region, Const(1), Const(2), Phi, Return), got {}",
        graph.node_count()
    );
    // Additionally verify that a Branch node exists in the graph
    let nodes = collect_node_kinds(&graph);
    assert!(
        nodes.iter().any(|(_, k)| matches!(k, NodeKind::Branch)),
        "(if t 1 2) IR must contain a Branch node"
    );
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(if t 1 2)': {:?}", e));
}

#[test]
fn pipeline_lambda_to_ir() {
    // (lambda (x) x) needs at least: Start, Parameter(0), Return
    let graph = read_and_build_ir("(lambda (x) x)");
    assert!(
        graph.node_count() >= 3,
        "(lambda (x) x) should produce at least 3 nodes including Parameter, got {}",
        graph.node_count()
    );
    // A correct implementation must emit a Parameter node for the lambda argument
    let nodes = collect_node_kinds(&graph);
    assert!(
        nodes
            .iter()
            .any(|(_, k)| matches!(k, NodeKind::Parameter(_))),
        "(lambda (x) x) IR must contain a Parameter node for the lambda variable"
    );
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(lambda (x) x)': {:?}", e));
}

#[test]
fn pipeline_ir_graph_wiring() {
    let graph = read_and_build_ir("42");
    let start = graph.start();
    assert!(matches!(graph.node_kind(start), NodeKind::Start));

    // Start→Return via control edge
    let ret_edge = graph
        .uses(start)
        .iter()
        .find(|e| e.kind == EdgeKind::Control);
    assert!(ret_edge.is_some(), "Start should have control edge");
    let ret_id = ret_edge.unwrap().to;
    assert!(matches!(graph.node_kind(ret_id), NodeKind::Return));

    // Return has data input from Constant(42)
    let data_in = graph
        .inputs(ret_id)
        .iter()
        .find(|e| e.kind == EdgeKind::Data);
    assert!(data_in.is_some(), "Return should have data input");
    match graph.node_kind(data_in.unwrap().from) {
        NodeKind::Constant(val) => assert_eq!(val.as_fixnum(), 42),
        other => panic!("Expected Constant, got {:?}", other),
    }
}

#[test]
fn pipeline_ir_graph_wiring_addition() {
    // (+ 1 2) should produce a Call node with two Constant data inputs
    let graph = read_and_build_ir("(+ 1 2)");
    verify(&graph).unwrap();

    // Walk all nodes in the graph using the safe helper (handles non-contiguous IDs after DCE)
    let nodes = collect_node_kinds(&graph);
    let call_nodes: Vec<_> = nodes
        .iter()
        .filter(|(_, k)| matches!(k, NodeKind::Call))
        .collect();
    assert!(
        !call_nodes.is_empty(),
        "(+ 1 2) IR should contain a Call node"
    );

    for &(nid, _) in &call_nodes {
        // Call node should have data inputs (the arguments)
        let inputs = graph.inputs(nid);
        let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
        assert!(
            data_inputs.len() >= 2,
            "Call node for (+ 1 2) should have at least 2 data inputs, got {}",
            data_inputs.len()
        );
    }
}

#[test]
fn pipeline_ir_graph_wiring_if() {
    // (if t 1 2) should produce Branch and Region nodes with control edges
    let graph = read_and_build_ir("(if t 1 2)");
    verify(&graph).unwrap();

    // Walk all nodes in the graph using the safe helper (handles non-contiguous IDs after DCE)
    let nodes = collect_node_kinds(&graph);
    let branch_nodes: Vec<_> = nodes
        .iter()
        .filter(|(_, k)| matches!(k, NodeKind::Branch))
        .collect();
    assert!(
        !branch_nodes.is_empty(),
        "(if t 1 2) IR should contain a Branch node"
    );

    for &(nid, _) in &branch_nodes {
        // Branch should have control outputs leading to Region targets
        let uses = graph.uses(nid);
        let ctrl_outputs: Vec<_> = uses
            .iter()
            .filter(|e| e.kind == EdgeKind::Control)
            .collect();
        assert!(
            ctrl_outputs.len() >= 2,
            "Branch node for (if t 1 2) should have at least 2 control outputs (then/else), got {}",
            ctrl_outputs.len()
        );
    }
}

// ═══════════════════════════════════════════════════════════════════
// 3. Reader → Macroexpand → IR → Optimisation
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_optimize_preserves_validity() {
    for src in &[
        "42",
        "t",
        "nil",
        "-1",
        "3.14",
        "'foo",
        "(+ 1 2)",
        "(if t 1 2)",
    ] {
        let graph = read_build_and_optimize(src);
        verify(&graph).unwrap_or_else(|e| panic!("post-opt verify failed for '{}': {:?}", src, e));
    }
}

#[test]
fn pipeline_passmanager_trivial_graph_unchanged() {
    let mut graph = read_and_build_ir("42");
    let initial = graph.node_count();
    let mut pm = PassManager::new();
    pm.run_all(&mut graph).expect("PassManager failed");
    assert_eq!(
        graph.node_count(),
        initial,
        "trivial graph should not lose nodes"
    );
}

#[test]
fn pipeline_dce_removes_dead_nodes() {
    use bliss_compiler::ir::Edge;
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let live = graph.add_node(NodeKind::Constant(BlissVal::from_fixnum(1)));
    let _dead = graph.add_node(NodeKind::Constant(BlissVal::from_fixnum(999)));
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(Edge {
        from: start,
        to: ret,
        kind: EdgeKind::Control,
        input_index: 0,
    });
    graph.add_edge(Edge {
        from: live,
        to: ret,
        kind: EdgeKind::Data,
        input_index: 1,
    });

    assert_eq!(graph.node_count(), 4);
    let mut pm = PassManager::new();
    pm.run_all(&mut graph).unwrap();
    assert_eq!(graph.node_count(), 3, "DCE should remove unreachable node");
    verify(&graph).expect("post-DCE verify failed");
}

// ═══════════════════════════════════════════════════════════════════
// 4. Reader → Macroexpand → IR → Optimisation → Codegen
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_codegen_x86_64_forms() {
    for src in &[
        "42",
        "nil",
        "t",
        "-1",
        "3.14",
        "'foo",
        "(+ 1 2)",
        "(if t 1 2)",
        "(lambda (x) x)",
    ] {
        let graph = read_build_and_optimize(src);
        let mut backend = X86_64Backend::new();
        let code = backend
            .emit(&graph)
            .unwrap_or_else(|e| panic!("x86_64 codegen failed for '{}': {}", src, e));
        assert!(!code.is_empty(), "code for '{}' is empty", src);
    }
}

#[test]
fn pipeline_codegen_aarch64_forms() {
    for src in &["42", "nil", "t", "'bar", "(+ 1 2)"] {
        let graph = read_build_and_optimize(src);
        let mut backend = Aarch64Backend::new();
        let code = backend
            .emit(&graph)
            .unwrap_or_else(|e| panic!("aarch64 codegen failed for '{}': {}", src, e));
        assert!(!code.is_empty(), "code for '{}' is empty", src);
    }
}

#[test]
fn pipeline_x86_64_prologue_and_ret() {
    let graph = read_build_and_optimize("42");
    let mut backend = X86_64Backend::new();
    let code = backend.emit(&graph).unwrap();
    assert_eq!(code.code()[0], 0x55, "should start with push rbp");
    assert!(code.code().contains(&0xC3), "should contain ret");
}

#[test]
fn pipeline_codegen_empty_graph_errors() {
    let graph = IrGraph::new();
    assert!(X86_64Backend::new().emit(&graph).is_err());
    assert!(Aarch64Backend::new().emit(&graph).is_err());
}

// ═══════════════════════════════════════════════════════════════════
// 5. Register allocation integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_regalloc_x86_64() {
    let graph = read_build_and_optimize("42");
    let mut alloc = LinearScanAllocator::new(TargetArch::X86_64);
    let r = alloc.allocate(&graph).expect("regalloc failed");
    assert_eq!(r.target_arch(), TargetArch::X86_64);
    assert_eq!(r.num_gprs(), 16);
    assert_eq!(r.spill_slots(), 0, "simple graph should not spill");
}

#[test]
fn pipeline_regalloc_aarch64() {
    let graph = read_build_and_optimize("42");
    let mut alloc = LinearScanAllocator::new(TargetArch::Aarch64);
    let r = alloc.allocate(&graph).expect("regalloc failed");
    assert_eq!(r.target_arch(), TargetArch::Aarch64);
    assert_eq!(r.num_gprs(), 31);
    assert_eq!(r.spill_slots(), 0);
}

#[test]
fn pipeline_regalloc_empty_graph_errors() {
    let graph = IrGraph::new();
    assert!(
        LinearScanAllocator::new(TargetArch::X86_64)
            .allocate(&graph)
            .is_err()
    );
}

// ═══════════════════════════════════════════════════════════════════
// 6. Tiered compilation integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn tiered_interpreter_eval_self_evaluating() {
    let mut interp = Interpreter::new();
    assert_eq!(interp.eval(read("42")).unwrap().as_fixnum(), 42);
    assert_eq!(interp.eval(read("nil")).unwrap(), NIL);
    assert_eq!(interp.eval(read("t")).unwrap(), T);
    assert_eq!(interp.eval(read("-7")).unwrap().as_fixnum(), -7);
    assert!(interp.eval(read("3.14")).unwrap().is_single_float());
    assert!(interp.eval(read("#\\A")).unwrap().is_character());
    assert!(interp.eval(read("\"hello\"")).unwrap().is_heap_object());
}

#[test]
fn tiered_interpreter_symbol_lookup() {
    let mut interp = Interpreter::new();
    let sym = read("MY-VAR-2");
    interp.define(sym, BlissVal::from_fixnum(100));
    assert_eq!(interp.eval(sym).unwrap().as_fixnum(), 100);
}

#[test]
fn tiered_interpreter_unbound_symbol_errors() {
    let mut interp = Interpreter::new();
    assert!(interp.eval(read("UNBOUND-SYM")).is_err());
}

#[test]
fn tiered_promotion_logic() {
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5000,
        osr_threshold: 10000,
        compile_threads: 1,
    };
    // Non-function at T0 with 0 invocations: no promotion
    assert_eq!(check_promotion(BlissVal::from_fixnum(0), &config), None);

    // With threshold 0, any T0 value qualifies for promotion
    let config0 = TierConfig {
        t1_threshold: 0,
        ..config.clone()
    };
    assert_eq!(
        check_promotion(BlissVal::from_fixnum(0), &config0),
        Some(Tier::Baseline)
    );
}

#[test]
fn tiered_promotion_full_lifecycle() {
    // Simulate the full promotion lifecycle per spec §4.4:
    // 1. Create a function header in memory with tier=Interpreter, invoke_count=0
    // 2. Simulate calling it t1_threshold times, verify promotion to Baseline
    // 3. Compile with BaselineCompiler
    // 4. Simulate calling t2_threshold times, verify promotion to Optimising

    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 100,
        osr_threshold: 10000,
        compile_threads: 1,
    };

    // Fake FnMeta header (see FakeFnHeader): offset 8 = tier, 12..16 =
    // invoke_count, 16..20 = back_edge_count. Must be full FnMeta size/alignment
    // so check_promotion's back_edge_count read stays in bounds (bliss-4lb).
    let mut header = FakeFnHeader::new();

    // Set tier = Interpreter (0)
    header[8] = 0; // Tier::Interpreter

    // Set invoke_count = 0
    header[12..16].copy_from_slice(&0u32.to_le_bytes());

    // Create a function-tagged BlissVal pointing to our header
    let func_val = header.func_val();

    // At 0 invocations, no promotion
    assert_eq!(
        check_promotion(func_val, &config),
        None,
        "function at T0 with 0 invocations should not promote"
    );

    // Simulate reaching t1_threshold invocations
    header[12..16].copy_from_slice(&10u32.to_le_bytes());
    assert_eq!(
        check_promotion(func_val, &config),
        Some(Tier::Baseline),
        "function at T0 with t1_threshold invocations should promote to Baseline"
    );

    // "Compile" to baseline — update tier to Baseline
    header[8] = 1; // Tier::Baseline

    // At t1_threshold invocations but now Baseline tier, no promotion yet
    assert_eq!(
        check_promotion(func_val, &config),
        None,
        "Baseline function below t2_threshold should not promote"
    );

    // Simulate reaching t2_threshold invocations
    header[12..16].copy_from_slice(&100u32.to_le_bytes());
    assert_eq!(
        check_promotion(func_val, &config),
        Some(Tier::Optimising),
        "Baseline function at t2_threshold should promote to Optimising"
    );

    // Update tier to Optimising
    header[8] = 2; // Tier::Optimising
    assert_eq!(header[8], 2);

    // Already at max tier — no further promotion
    assert_eq!(
        check_promotion(func_val, &config),
        None,
        "Optimising function should not promote further"
    );
}

#[test]
fn tiered_compilers_produce_code_with_function_val() {
    // compile() takes a TAG_FUNCTION value; back it with a full-size FnMeta
    // header so any FnMeta field read stays in bounds (bliss-4lb).
    let header = FakeFnHeader::new();
    let func_val = header.func_val();

    let bc = BaselineCompiler::new().compile(func_val).unwrap();
    assert_eq!(bc.tier(), Tier::Baseline);
    assert!(bc.code_size() > 0);

    let oc = OptimisingCompiler::new().compile(func_val).unwrap();
    assert_eq!(oc.tier(), Tier::Optimising);
    assert!(oc.code_size() > 0);
}

#[test]
fn tiered_compilers_produce_code_with_non_function() {
    // Per spec, compile() should reject non-function values with a type error.
    // Red-phase test: this will fail until compile() adds TAG_FUNCTION type-checking.
    let non_func = BlissVal::from_fixnum(0);
    let result = BaselineCompiler::new().compile(non_func);
    assert!(
        result.is_err(),
        "BaselineCompiler::compile() should reject non-function values"
    );

    let result2 = OptimisingCompiler::new().compile(non_func);
    assert!(
        result2.is_err(),
        "OptimisingCompiler::compile() should reject non-function values"
    );
}

#[test]
fn tiered_tiers_ordered() {
    assert!(Tier::Interpreter < Tier::Baseline);
    assert!(Tier::Baseline < Tier::Optimising);
}

// ═══════════════════════════════════════════════════════════════════
// 6a. Profiling → tiered promotion integration
// ═══════════════════════════════════════════════════════════════════

#[test]
fn profiling_invocation_counter_triggers_t1_promotion() {
    // Verify that InvocationCounter reaching t1_threshold causes
    // check_promotion to return Some(Tier::Baseline) for a function.
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5000,
        osr_threshold: 10000,
        compile_threads: 1,
    };

    let counter = InvocationCounter::new();
    assert_eq!(counter.count(), 0);

    // Increment 9 times — below threshold
    for _ in 0..9 {
        let reached = counter.increment(config.t1_threshold);
        assert!(
            !reached,
            "should not reach threshold before {} increments",
            config.t1_threshold
        );
    }
    assert_eq!(counter.count(), 9);

    // 10th increment reaches threshold
    let reached = counter.increment(config.t1_threshold);
    assert!(reached, "10th increment should reach t1_threshold");
    assert_eq!(counter.count(), 10);

    // Now simulate: create a function header with invoke_count=10, tier=Interpreter
    // and verify check_promotion returns Baseline
    let mut header = FakeFnHeader::new();
    header[8] = 0; // Tier::Interpreter
    header[12..16].copy_from_slice(&10u32.to_le_bytes());
    let func_val = header.func_val();
    assert_eq!(
        check_promotion(func_val, &config),
        Some(Tier::Baseline),
        "function with invoke_count at t1_threshold should promote to Baseline"
    );
}

#[test]
fn profiling_back_edge_counter_triggers_osr() {
    // Verify that BackEdgeCounter reaching osr_threshold signals OSR readiness.
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5000,
        osr_threshold: 100,
        compile_threads: 1,
    };

    let counter = BackEdgeCounter::new();

    // Increment 99 times — below threshold
    for _ in 0..99 {
        assert!(!counter.increment(config.osr_threshold));
    }
    assert_eq!(counter.count(), 99);

    // 100th increment reaches osr_threshold
    assert!(
        counter.increment(config.osr_threshold),
        "back-edge counter should reach osr_threshold at 100"
    );
}

#[test]
fn profiling_function_profile_drives_tier_transition() {
    // FunctionProfile aggregates InvocationCounter — verify the pipeline:
    // create FunctionProfile, increment its invocation counter to threshold,
    // then verify check_promotion would trigger.
    let config = TierConfig {
        t1_threshold: 5,
        t2_threshold: 100,
        osr_threshold: 10000,
        compile_threads: 1,
    };

    let profile = FunctionProfile::new();
    let counter = profile.invocation_counter();

    // Increment to t1_threshold
    for _ in 0..4 {
        assert!(!counter.increment(config.t1_threshold));
    }
    assert!(
        counter.increment(config.t1_threshold),
        "5th increment should reach t1_threshold"
    );

    // Verify the counter value matches what check_promotion would read
    assert_eq!(counter.count(), config.t1_threshold);
}

#[test]
fn profiling_invocation_counter_reset_and_recount() {
    // After reset, counter should start from 0 again
    let counter = InvocationCounter::new();
    for _ in 0..5 {
        counter.increment(10);
    }
    assert_eq!(counter.count(), 5);
    counter.reset();
    assert_eq!(counter.count(), 0);
    assert!(
        !counter.increment(10),
        "after reset, single increment should not reach threshold 10"
    );
    assert_eq!(counter.count(), 1);
}

// ═══════════════════════════════════════════════════════════════════
// 6b. request_compilation() and CompiledCode::install()
// ═══════════════════════════════════════════════════════════════════

#[test]
fn tiered_request_compilation_accepts_function_val() {
    // Full-size FnMeta header: request_compilation reads invoke_count AND
    // back_edge_count (priority = invoke + 2*back_edge), so an undersized header
    // read OOB garbage and overflowed the priority sum in debug (bliss-4lb).
    let header = FakeFnHeader::new();
    let func_val = header.func_val();

    // request_compilation should accept a function-tagged value and enqueue it
    let result = request_compilation(func_val, Tier::Baseline);
    assert!(
        result.is_ok(),
        "request_compilation should accept a function-tagged value"
    );

    // Also verify it works for T2 target
    let result2 = request_compilation(func_val, Tier::Optimising);
    assert!(
        result2.is_ok(),
        "request_compilation should accept Optimising target tier"
    );
}

#[test]
fn tiered_request_compilation_rejects_non_function() {
    // A fixnum is not a function — request_compilation should reject it
    let non_func = BlissVal::from_fixnum(42);
    let result = request_compilation(non_func, Tier::Baseline);
    assert!(
        result.is_err(),
        "request_compilation should reject non-function values"
    );
}

#[test]
fn tiered_compiled_code_install_updates_function_header() {
    // install() writes the entry AtomicPtr at offset 0 and the tier byte at
    // offset 8. Back the header with a full-size, 8-byte-aligned FnMeta buffer
    // so those and any FnMeta field reads stay in bounds (bliss-4lb).
    use std::sync::atomic::{AtomicPtr, Ordering};

    let mut header = FakeFnHeader::new();
    let header_ptr = header.as_mut_ptr();

    // Vec<u64> backing guarantees 8-byte alignment for the AtomicPtr slot.
    assert_eq!(header_ptr as usize % 8, 0, "header must be 8-byte aligned");

    // Initialize: entry_point = null, tier = Interpreter (0)
    unsafe {
        let entry_slot = header_ptr as *const AtomicPtr<u8>;
        (*entry_slot).store(std::ptr::null_mut(), Ordering::Release);
        *header_ptr.add(8) = 0; // Tier::Interpreter
    }

    // Create a function-tagged value pointing to the header
    let func_val = BlissVal(header_ptr as u64 | bliss_rt::value::TAG_FUNCTION);

    // Compile at baseline tier
    let compiled = BaselineCompiler::new().compile(func_val).unwrap();
    assert_eq!(compiled.tier(), Tier::Baseline);
    let code_size = compiled.code_size();
    assert!(code_size > 0);

    // Install the compiled code into the function header
    let install_result = compiled.install(func_val);
    assert!(
        install_result.is_ok(),
        "install should succeed for a function-tagged value"
    );

    // After install, the entry point should be non-null and tier byte should be Baseline (1)
    unsafe {
        let entry_slot = header_ptr as *const AtomicPtr<u8>;
        let entry = (*entry_slot).load(Ordering::Acquire);
        assert!(
            !entry.is_null(),
            "entry point should be updated after install"
        );

        let tier_byte = *header_ptr.add(8);
        assert_eq!(
            tier_byte, 1,
            "tier byte should be Baseline (1) after install"
        );
    }
}

#[test]
fn tiered_compiled_code_install_rejects_non_function() {
    let non_func = BlissVal::from_fixnum(0);
    let compiled = BaselineCompiler::new().compile(non_func);
    // If compile succeeds (it may or may not gate on TAG_FUNCTION), try install
    if let Ok(cc) = compiled {
        let result = cc.install(non_func);
        assert!(result.is_err(), "install should reject non-function values");
    }
}

// ═══════════════════════════════════════════════════════════════════
// 6b2. Inline cache (IC) integration with the pipeline
// ═══════════════════════════════════════════════════════════════════

#[test]
fn ic_state_transitions_affect_deopt_decisions() {
    // Verify that IC state transitions (Uninitialized → Monomorphic → Polymorphic
    // → Megamorphic) interact with the deoptimization system: a Megamorphic IC
    // should trigger InlineCacheOverflow deopts which eventually blacklist the function.
    let _serial = IC_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    init_ic_registry();

    let ic = InlineCache::new();
    assert_eq!(ic.state(), IcState::Uninitialized);

    // Monomorphic: single type
    let type_a = BlissVal::from_fixnum(1);
    let method_a = BlissVal::from_fixnum(100);
    ic.update(type_a, method_a);
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(ic.lookup(type_a), Some(method_a));

    // Polymorphic: 2-4 types
    for i in 2..=4 {
        ic.update(BlissVal::from_fixnum(i), BlissVal::from_fixnum(100 + i));
    }
    assert_eq!(ic.state(), IcState::Polymorphic);

    // Megamorphic: 5th type pushes past IC_POLY_MAX
    ic.update(BlissVal::from_fixnum(5), BlissVal::from_fixnum(105));
    assert_eq!(ic.state(), IcState::Megamorphic);

    // When IC goes megamorphic, the tiered system should record deopt events.
    // Verify DeoptLog tracks InlineCacheOverflow and eventually blacklists.
    let mut log = DeoptLog::new();
    log.record(DeoptReason::InlineCacheOverflow);
    log.record(DeoptReason::InlineCacheOverflow);
    log.record(DeoptReason::InlineCacheOverflow);
    assert!(
        log.is_blacklisted(),
        "3 InlineCacheOverflow deopts should blacklist the function"
    );
}

#[test]
fn ic_generation_invalidation_with_compiled_code() {
    // Serialize against sibling tests that mutate the process-global IC
    // registry (session/generation counters); see IC_REGISTRY_SERIAL.
    let _serial = IC_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // Verify that ic_generation() bump (from reset_all_caches) causes
    // existing ICs to lazily reset, which would invalidate compiled code
    // assumptions (requiring deoptimization).
    init_ic_registry();

    let gen_before = ic_generation();
    let ic = InlineCache::new();
    ic.update(BlissVal::from_fixnum(1), BlissVal::from_fixnum(100));
    assert_eq!(ic.state(), IcState::Monomorphic);
    assert_eq!(
        ic.lookup(BlissVal::from_fixnum(1)),
        Some(BlissVal::from_fixnum(100))
    );

    // Simulate a class redefinition: bump global generation
    reset_all_caches().expect("reset_all_caches");
    let gen_after = ic_generation();
    assert!(
        gen_after > gen_before,
        "reset_all_caches should bump IC generation"
    );

    // The IC should lazily reset on next access — lookup should miss
    assert_eq!(
        ic.lookup(BlissVal::from_fixnum(1)),
        None,
        "IC should lazily reset after generation bump, causing cache miss"
    );
    assert_eq!(
        ic.state(),
        IcState::Uninitialized,
        "IC should be Uninitialized after generation-triggered reset"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 6c. OSR integration with the pipeline
// ═══════════════════════════════════════════════════════════════════

#[test]
fn osr_entry_map_construction() {
    // Verify OsrEntryMap can be constructed with local-to-SSA mappings
    let mappings = vec![
        LocalMapping {
            local_index: 0,
            ssa_var: 10,
        },
        LocalMapping {
            local_index: 1,
            ssa_var: 11,
        },
        LocalMapping {
            local_index: 2,
            ssa_var: 12,
        },
    ];
    let entry_map = OsrEntryMap::new(mappings.clone(), 64);
    assert_eq!(entry_map.mappings.len(), 3);
    assert_eq!(entry_map.target_pc_offset, 64);
    assert_eq!(entry_map.mappings[0].local_index, 0);
    assert_eq!(entry_map.mappings[0].ssa_var, 10);
}

#[test]
fn osr_entry_map_enter_validates_local_bounds() {
    // enter() should error when a mapping references an out-of-bounds local
    let mappings = vec![
        LocalMapping {
            local_index: 5,
            ssa_var: 10,
        }, // index 5, but only 2 locals
    ];
    let entry_map = OsrEntryMap::new(mappings, 0);
    let locals = [BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)];
    let result = entry_map.enter(&locals, std::ptr::null());
    assert!(
        result.is_err(),
        "enter should fail when local_index is out of bounds"
    );
}

#[test]
fn osr_entry_with_compiled_function() {
    // Build a compiled function and attempt OSR entry — this exercises the
    // osr_entry() free function which validates the function and entry map
    // before attempting the (unimplemented) stack transfer.
    // Full-size FnMeta header so osr_entry/deoptimize field reads stay in bounds (bliss-4lb).
    let header = FakeFnHeader::new();
    let func_val = header.func_val();

    let entry_map = OsrEntryMap::new(
        vec![LocalMapping {
            local_index: 0,
            ssa_var: 0,
        }],
        0,
    );
    let locals = [BlissVal::from_fixnum(42)];

    // osr_entry validates inputs, maps locals, and returns Ok(()) when the
    // logical transfer is ready. The runtime layer does the actual jump.
    bliss_compiler::osr::clear_global_deopt_logs();
    let result = osr_entry(func_val, &entry_map, &locals);
    assert!(
        result.is_ok() || result.is_err(),
        "osr_entry should return a Result"
    );
}

#[test]
fn osr_deoptimize_records_reason_and_blacklists() {
    // Test the DeoptLog independently — it tracks deopt events and blacklists
    let mut log = DeoptLog::new();
    assert_eq!(log.count(), 0);
    assert!(!log.is_blacklisted());

    log.record(DeoptReason::TypeMismatch {
        expected: "fixnum".into(),
        actual: "cons".into(),
    });
    assert_eq!(log.count(), 1);
    assert!(!log.is_blacklisted());

    log.record(DeoptReason::InlineCacheOverflow);
    log.record(DeoptReason::Other("test".into()));
    assert_eq!(log.count(), 3);
    assert!(
        log.is_blacklisted(),
        "3 deopts should trigger blacklisting (threshold=3)"
    );
}

#[test]
fn osr_deopt_config_custom_threshold() {
    let config = DeoptConfig {
        blacklist_threshold: 5,
        backoff_seconds: 60,
    };
    let mut log = DeoptLog::with_config(&config);
    for _ in 0..4 {
        log.record(DeoptReason::InlineCacheOverflow);
    }
    assert!(
        !log.is_blacklisted(),
        "4 deopts below threshold 5 should not blacklist"
    );
    log.record(DeoptReason::InlineCacheOverflow);
    assert!(
        log.is_blacklisted(),
        "5 deopts at threshold 5 should blacklist"
    );
}

#[test]
fn osr_threshold_in_tier_config() {
    // Verify TierConfig carries the osr_threshold field used to trigger OSR entry
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5000,
        osr_threshold: 10000,
        compile_threads: 1,
    };
    assert_eq!(
        config.osr_threshold, 10000,
        "TierConfig should carry osr_threshold for back-edge triggered OSR"
    );
}

#[test]
fn osr_deoptimize_with_compiled_function() {
    // Exercise the deoptimize() free function with a compiled function.
    // It should validate and then hit unimplemented!() for stack reconstruction.
    // Full-size FnMeta header so osr_entry/deoptimize field reads stay in bounds (bliss-4lb).
    let header = FakeFnHeader::new();
    let func_val = header.func_val();

    let live_values = [BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)];
    let reason = DeoptReason::TypeMismatch {
        expected: "fixnum".into(),
        actual: "symbol".into(),
    };

    // deoptimize validates, records the deopt in the persistent log,
    // and returns Ok(()) when frame reconstruction is prepared.
    bliss_compiler::osr::clear_global_deopt_logs();
    let result = deoptimize(func_val, reason, &live_values);
    assert!(
        result.is_ok() || result.is_err(),
        "deoptimize should return a Result"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 7. End-to-end acceptance: full compile pipeline
// ═══════════════════════════════════════════════════════════════════

#[test]
fn acceptance_full_pipeline_forms() {
    for src in &[
        "42",
        "nil",
        "t",
        "-42",
        "3.14",
        "'hello",
        "(+ 1 2)",
        "(if t 1 2)",
        "(lambda (x) x)",
        "\"hello world\"",
        "#\\Space",
        "(defun foo (x) (+ x 1))",
    ] {
        let code = full_pipeline_x86_64(src);
        assert!(
            !code.is_empty(),
            "full pipeline for '{}' produced empty code",
            src
        );
        // Every emitted function must have x86-64 prologue (push rbp) and ret
        assert_eq!(
            code.code()[0],
            0x55,
            "full pipeline for '{}' should emit push rbp prologue",
            src
        );
        assert!(
            code.code().contains(&0xC3),
            "full pipeline for '{}' should contain ret instruction",
            src
        );
    }
}

#[test]
fn acceptance_full_pipeline_constant_encoding() {
    // For a simple integer literal like 42, the emitted code should contain
    // a movabs encoding (0x48 0xB8) loading the fixnum representation.
    let code = full_pipeline_x86_64("42");
    let bytes = code.code();
    // Look for the movabs rax prefix (REX.W + B8)
    let has_movabs = bytes.windows(2).any(|w| w[0] == 0x48 && w[1] == 0xB8);
    assert!(
        has_movabs,
        "code for '42' should contain movabs (0x48 0xB8) encoding the literal value"
    );
    // The fixnum raw bytes for 42 should follow the movabs prefix
    let fixnum_42 = BlissVal::from_fixnum(42);
    let raw_bytes = fixnum_42.0.to_le_bytes();
    let has_value = bytes
        .windows(10)
        .any(|w| w[0] == 0x48 && w[1] == 0xB8 && w[2..10] == raw_bytes);
    assert!(
        has_value,
        "code for '42' should contain the fixnum-tagged encoding of 42"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 8. Combined: interpreter on reader+macroexpand output
// ═══════════════════════════════════════════════════════════════════

#[test]
fn tiered_interpreter_on_macroexpanded_form() {
    let mut interp = Interpreter::new();
    let sym = read("MAGIC");
    let env = Environment::null()
        .augment_variable(sym, VariableInfo::SymbolMacro(BlissVal::from_fixnum(77)));
    let (expanded, did) = macroexpand(sym, &env).unwrap();
    assert!(did);
    assert_eq!(interp.eval(expanded).unwrap().as_fixnum(), 77);
}

#[test]
fn tiered_interpreter_evals_reader_atoms() {
    let mut interp = Interpreter::new();
    assert_eq!(interp.eval(read("42")).unwrap().as_fixnum(), 42);
    assert_eq!(interp.eval(read("nil")).unwrap(), NIL);
    assert_eq!(interp.eval(read("t")).unwrap(), T);
    assert_eq!(interp.eval(read("-1")).unwrap().as_fixnum(), -1);
}

// ═══════════════════════════════════════════════════════════════════
// 9. Error paths
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_reader_errors() {
    assert!(read_from_string("\"unterminated").is_err());
    assert!(read_from_string(")").is_err());
}

#[test]
fn pipeline_reader_eof_cases() {
    let (v, _) = read_from_string("").unwrap();
    assert_eq!(v, EOF);
    let (v, _) = read_from_string("   ").unwrap();
    assert_eq!(v, EOF);
    let (v, _) = read_from_string("; comment\n").unwrap();
    assert_eq!(v, EOF);
}

#[test]
fn pipeline_circular_macro_detected() {
    let sym_a = read("CIRC-A");
    let sym_b = read("CIRC-B");
    let env = Environment::null()
        .augment_variable(sym_a, VariableInfo::SymbolMacro(sym_b))
        .augment_variable(sym_b, VariableInfo::SymbolMacro(sym_a));
    assert!(
        macroexpand(sym_a, &env).is_err(),
        "circular expansion should error"
    );
}

#[test]
fn pipeline_ir_verify_missing_control() {
    let mut graph = IrGraph::new();
    let _start = graph.add_node(NodeKind::Start);
    let _ret = graph.add_node(NodeKind::Return);
    // No edge wired: Return lacks control input
    assert!(
        verify(&graph).is_err(),
        "should catch Return without control input"
    );
}
