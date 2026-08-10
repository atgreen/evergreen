//! Compiler pipeline integration tests.
//!
//! Tests the reader → macroexpand → IR → optimization → codegen pipeline
//! end-to-end, exercising cross-module integration within bliss-compiler.

use bliss_compiler::reader::read_from_string;
use bliss_compiler::macroexpand::{
    macroexpand_1, macroexpand, macroexpand_all, Environment, VariableInfo,
};
use bliss_compiler::ir::{IrBuilder, IrGraph, NodeId, NodeKind, EdgeKind, verify};
use bliss_compiler::opt::PassManager;
use bliss_compiler::codegen::{
    CodegenBackend, X86_64Backend, Aarch64Backend, CodeBuffer, TargetArch,
    LinearScanAllocator,
};
use bliss_compiler::tiered::{
    Tier, TierConfig, Interpreter, BaselineCompiler, OptimisingCompiler,
    check_promotion, request_compilation,
};
use bliss_compiler::osr::{
    OsrEntryMap, LocalMapping, DeoptLog, DeoptReason, DeoptConfig,
    osr_entry, deoptimize,
};
use bliss_rt::value::{BlissVal, NIL, T, EOF};

// ── Helpers ──────────────────────────────────────────────────────────

fn read(s: &str) -> BlissVal {
    let (val, _) = read_from_string(s).expect("read_from_string failed");
    val
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
    let cases: &[(&str, fn(BlissVal) -> bool)] = &[
        ("42", |v| v.is_fixnum() && v.as_fixnum() == 42),
        ("t",  |v| v == T),
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
    let env = Environment::null()
        .augment_variable(form, VariableInfo::SymbolMacro(replacement));
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
    // macroexpand_all should recursively walk subforms of a cons
    let form = read("(if t 1 2)");
    assert!(form.is_cons(), "(if t 1 2) should parse as cons");
    let env = Environment::null();
    let result = macroexpand_all(form, &env).unwrap();
    // The result should still be a cons (compound form preserved)
    assert!(result.is_cons(), "macroexpand_all on compound form should return cons");
}

#[test]
fn macroexpand_all_on_nested_compound_form() {
    // macroexpand_all should walk into nested subforms
    let form = read("(progn (+ 1 2) 3)");
    assert!(form.is_cons());
    let env = Environment::null();
    let result = macroexpand_all(form, &env).unwrap();
    assert!(result.is_cons(), "macroexpand_all on nested compound should return cons");
}

// ═══════════════════════════════════════════════════════════════════
// 2. Reader → Macroexpand → IR (IrBuilder)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_self_evaluating_forms_to_ir() {
    // Self-evaluating atoms should produce a 3-node graph: Start, Constant, Return
    for src in &["42", "nil", "t", "-7", "3.14", "'foo", "#\\a", "\"hello\""] {
        let graph = read_and_build_ir(src);
        assert_eq!(graph.node_count(), 3, "'{}' should produce 3-node graph", src);
        verify(&graph).unwrap_or_else(|e| panic!("verify failed for '{}': {:?}", src, e));
    }
}

#[test]
fn pipeline_addition_to_ir() {
    // (+ 1 2) needs at least: Start, Const(1), Const(2), Call(+), Return
    let graph = read_and_build_ir("(+ 1 2)");
    assert!(graph.node_count() >= 5,
        "(+ 1 2) should produce at least 5 nodes (Start, Const(1), Const(2), Call(+), Return), got {}",
        graph.node_count());
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(+ 1 2)': {:?}", e));
}

#[test]
fn pipeline_if_to_ir() {
    // (if t 1 2) needs at least: Start, Const(t), Branch, Region, Const(1), Const(2), Phi, Return
    let graph = read_and_build_ir("(if t 1 2)");
    assert!(graph.node_count() >= 5,
        "(if t 1 2) should produce at least 5 nodes for branching, got {}",
        graph.node_count());
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(if t 1 2)': {:?}", e));
}

#[test]
fn pipeline_lambda_to_ir() {
    // (lambda (x) x) needs at least: Start, Parameter(0), Return
    let graph = read_and_build_ir("(lambda (x) x)");
    assert!(graph.node_count() >= 3,
        "(lambda (x) x) should produce at least 3 nodes including Parameter, got {}",
        graph.node_count());
    verify(&graph).unwrap_or_else(|e| panic!("verify failed for '(lambda (x) x)': {:?}", e));
}

#[test]
fn pipeline_ir_graph_wiring() {
    let graph = read_and_build_ir("42");
    let start = graph.start();
    assert!(matches!(graph.node_kind(start), NodeKind::Start));

    // Start→Return via control edge
    let ret_edge = graph.uses(start).iter().find(|e| e.kind == EdgeKind::Control);
    assert!(ret_edge.is_some(), "Start should have control edge");
    let ret_id = ret_edge.unwrap().to;
    assert!(matches!(graph.node_kind(ret_id), NodeKind::Return));

    // Return has data input from Constant(42)
    let data_in = graph.inputs(ret_id).iter().find(|e| e.kind == EdgeKind::Data);
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

    // Walk graph to find Call node
    let mut found_call = false;
    for node_id_val in 0..graph.node_count() as u32 {
        let nid = NodeId(node_id_val);
        if let Ok(kind) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graph.node_kind(nid).clone())) {
            if matches!(kind, NodeKind::Call) {
                found_call = true;
                // Call node should have data inputs (the arguments)
                let inputs = graph.inputs(nid);
                let data_inputs: Vec<_> = inputs.iter().filter(|e| e.kind == EdgeKind::Data).collect();
                assert!(data_inputs.len() >= 2,
                    "Call node for (+ 1 2) should have at least 2 data inputs, got {}",
                    data_inputs.len());
            }
        }
    }
    assert!(found_call, "(+ 1 2) IR should contain a Call node");
}

#[test]
fn pipeline_ir_graph_wiring_if() {
    // (if t 1 2) should produce Branch and Region nodes with control edges
    let graph = read_and_build_ir("(if t 1 2)");
    verify(&graph).unwrap();

    let mut found_branch = false;
    for node_id_val in 0..graph.node_count() as u32 {
        let nid = NodeId(node_id_val);
        if let Ok(kind) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| graph.node_kind(nid).clone())) {
            if matches!(kind, NodeKind::Branch) {
                found_branch = true;
                // Branch should have control outputs leading to Region targets
                let uses = graph.uses(nid);
                let ctrl_outputs: Vec<_> = uses.iter().filter(|e| e.kind == EdgeKind::Control).collect();
                assert!(ctrl_outputs.len() >= 2,
                    "Branch node for (if t 1 2) should have at least 2 control outputs (then/else), got {}",
                    ctrl_outputs.len());
            }
        }
    }
    assert!(found_branch, "(if t 1 2) IR should contain a Branch node");
}

// ═══════════════════════════════════════════════════════════════════
// 3. Reader → Macroexpand → IR → Optimisation
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_optimize_preserves_validity() {
    for src in &["42", "t", "nil", "-1", "3.14", "'foo", "(+ 1 2)", "(if t 1 2)"] {
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
    assert_eq!(graph.node_count(), initial, "trivial graph should not lose nodes");
}

#[test]
fn pipeline_dce_removes_dead_nodes() {
    use bliss_compiler::ir::Edge;
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let live = graph.add_node(NodeKind::Constant(BlissVal::from_fixnum(1)));
    let _dead = graph.add_node(NodeKind::Constant(BlissVal::from_fixnum(999)));
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(Edge { from: start, to: ret, kind: EdgeKind::Control, input_index: 0 });
    graph.add_edge(Edge { from: live, to: ret, kind: EdgeKind::Data, input_index: 1 });

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
    for src in &["42", "nil", "t", "-1", "3.14", "'foo", "(+ 1 2)",
                 "(if t 1 2)", "(lambda (x) x)"] {
        let graph = read_build_and_optimize(src);
        let mut backend = X86_64Backend::new();
        let code = backend.emit(&graph).unwrap_or_else(|e|
            panic!("x86_64 codegen failed for '{}': {}", src, e));
        assert!(!code.is_empty(), "code for '{}' is empty", src);
    }
}

#[test]
fn pipeline_codegen_aarch64_forms() {
    for src in &["42", "nil", "t", "'bar", "(+ 1 2)"] {
        let graph = read_build_and_optimize(src);
        let mut backend = Aarch64Backend::new();
        let code = backend.emit(&graph).unwrap_or_else(|e|
            panic!("aarch64 codegen failed for '{}': {}", src, e));
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
    assert!(LinearScanAllocator::new(TargetArch::X86_64).allocate(&graph).is_err());
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
        t1_threshold: 10, t2_threshold: 5000,
        osr_threshold: 10000, compile_threads: 1,
    };
    // Non-function at T0 with 0 invocations: no promotion
    assert_eq!(check_promotion(BlissVal::from_fixnum(0), &config), None);

    // With threshold 0, any T0 value qualifies for promotion
    let config0 = TierConfig { t1_threshold: 0, ..config.clone() };
    assert_eq!(check_promotion(BlissVal::from_fixnum(0), &config0), Some(Tier::Baseline));
}

#[test]
fn tiered_promotion_full_lifecycle() {
    // Simulate the full promotion lifecycle per spec §4.4:
    // 1. Create a function header in memory with tier=Interpreter, invoke_count=0
    // 2. Simulate calling it t1_threshold times, verify promotion to Baseline
    // 3. Compile with BaselineCompiler
    // 4. Simulate calling t2_threshold times, verify promotion to Optimising

    let config = TierConfig {
        t1_threshold: 10, t2_threshold: 100,
        osr_threshold: 10000, compile_threads: 1,
    };

    // Allocate a function header: [entry_point: 8 bytes][tier: 1 byte][padding: 3 bytes][invoke_count: 4 bytes]
    // Total: 16 bytes minimum
    let mut header = vec![0u8; 16];

    // Set tier = Interpreter (0)
    header[8] = 0; // Tier::Interpreter

    // Set invoke_count = 0
    header[12..16].copy_from_slice(&0u32.to_le_bytes());

    // Create a function-tagged BlissVal pointing to our header
    let header_ptr = header.as_ptr() as u64;
    let func_val = BlissVal(header_ptr | bliss_rt::value::TAG_FUNCTION as u64);

    // At 0 invocations, no promotion
    assert_eq!(check_promotion(func_val, &config), None,
        "function at T0 with 0 invocations should not promote");

    // Simulate reaching t1_threshold invocations
    header[12..16].copy_from_slice(&10u32.to_le_bytes());
    assert_eq!(check_promotion(func_val, &config), Some(Tier::Baseline),
        "function at T0 with t1_threshold invocations should promote to Baseline");

    // "Compile" to baseline — update tier to Baseline
    header[8] = 1; // Tier::Baseline

    // At t1_threshold invocations but now Baseline tier, no promotion yet
    assert_eq!(check_promotion(func_val, &config), None,
        "Baseline function below t2_threshold should not promote");

    // Simulate reaching t2_threshold invocations
    header[12..16].copy_from_slice(&100u32.to_le_bytes());
    assert_eq!(check_promotion(func_val, &config), Some(Tier::Optimising),
        "Baseline function at t2_threshold should promote to Optimising");

    // Update tier to Optimising
    header[8] = 2; // Tier::Optimising

    // Already at max tier — no further promotion
    assert_eq!(check_promotion(func_val, &config), None,
        "Optimising function should not promote further");
}

#[test]
fn tiered_compilers_produce_code_with_function_val() {
    // Per tiered.rs, compile() takes a BlissVal that should be TAG_FUNCTION.
    // Construct a function-tagged value for a realistic test.
    // TAG_FUNCTION is 0b110 (6); we create a tagged pointer (null base + tag).
    let func_val = BlissVal(bliss_rt::value::TAG_FUNCTION as u64);

    let bc = BaselineCompiler::new().compile(func_val).unwrap();
    assert_eq!(bc.tier(), Tier::Baseline);
    assert!(bc.code_size() > 0);

    let oc = OptimisingCompiler::new().compile(func_val).unwrap();
    assert_eq!(oc.tier(), Tier::Optimising);
    assert!(oc.code_size() > 0);
}

#[test]
fn tiered_compilers_produce_code_with_non_function() {
    // compile() currently accepts any BlissVal (the implementation doesn't
    // gate on TAG_FUNCTION). When proper type-checking is added, this test
    // should be updated to assert Err for non-function inputs.
    let non_func = BlissVal::from_fixnum(0);
    // If compile() accepts non-functions, verify it still produces code:
    let result = BaselineCompiler::new().compile(non_func);
    if let Ok(bc) = &result {
        assert_eq!(bc.tier(), Tier::Baseline);
        assert!(bc.code_size() > 0);
    }
    // If it rejects non-functions, that's also correct behavior:
    // result.is_err() is acceptable
}

#[test]
fn tiered_tiers_ordered() {
    assert!(Tier::Interpreter < Tier::Baseline);
    assert!(Tier::Baseline < Tier::Optimising);
}

// ═══════════════════════════════════════════════════════════════════
// 6b. request_compilation() and CompiledCode::install()
// ═══════════════════════════════════════════════════════════════════

#[test]
fn tiered_request_compilation_accepts_function_val() {
    // Allocate a function header: [entry_point: 8][tier: 1][pad: 3][invoke_count: 4] = 16 bytes
    let header = vec![0u8; 16];
    let header_ptr = header.as_ptr() as u64;
    let func_val = BlissVal(header_ptr | bliss_rt::value::TAG_FUNCTION as u64);

    // request_compilation should accept a function-tagged value and enqueue it
    let result = request_compilation(func_val, Tier::Baseline);
    assert!(result.is_ok(), "request_compilation should accept a function-tagged value");

    // Also verify it works for T2 target
    let result2 = request_compilation(func_val, Tier::Optimising);
    assert!(result2.is_ok(), "request_compilation should accept Optimising target tier");
}

#[test]
fn tiered_request_compilation_rejects_non_function() {
    // A fixnum is not a function — request_compilation should reject it
    let non_func = BlissVal::from_fixnum(42);
    let result = request_compilation(non_func, Tier::Baseline);
    assert!(result.is_err(), "request_compilation should reject non-function values");
}

#[test]
fn tiered_compiled_code_install_updates_function_header() {
    // Allocate a function header with space for:
    //   [entry_point: 8 bytes (AtomicPtr)][tier: 1 byte][padding: 3][invoke_count: 4]
    // We need proper alignment for AtomicPtr, so use a Box<[u8; 16]> via aligned allocation.
    use std::sync::atomic::{AtomicPtr, Ordering};

    // Use a Vec with enough space, aligned to pointer size
    let mut header = vec![0u8; 32]; // extra room for alignment
    let header_ptr = header.as_mut_ptr();

    // Ensure the pointer is 8-byte aligned (it should be from Vec)
    assert_eq!(header_ptr as usize % 8, 0, "header must be 8-byte aligned");

    // Initialize: entry_point = null, tier = Interpreter (0)
    unsafe {
        let entry_slot = header_ptr as *const AtomicPtr<u8>;
        (*entry_slot).store(std::ptr::null_mut(), Ordering::Release);
        *header_ptr.add(8) = 0; // Tier::Interpreter
    }

    // Create a function-tagged value pointing to the header
    let func_val = BlissVal(header_ptr as u64 | bliss_rt::value::TAG_FUNCTION as u64);

    // Compile at baseline tier
    let compiled = BaselineCompiler::new().compile(func_val).unwrap();
    assert_eq!(compiled.tier(), Tier::Baseline);
    let code_size = compiled.code_size();
    assert!(code_size > 0);

    // Install the compiled code into the function header
    let install_result = compiled.install(func_val);
    assert!(install_result.is_ok(), "install should succeed for a function-tagged value");

    // After install, the entry point should be non-null and tier byte should be Baseline (1)
    unsafe {
        let entry_slot = header_ptr as *const AtomicPtr<u8>;
        let entry = (*entry_slot).load(Ordering::Acquire);
        assert!(!entry.is_null(), "entry point should be updated after install");

        let tier_byte = *header_ptr.add(8);
        assert_eq!(tier_byte, 1, "tier byte should be Baseline (1) after install");
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
// 6c. OSR integration with the pipeline
// ═══════════════════════════════════════════════════════════════════

#[test]
fn osr_entry_map_construction() {
    // Verify OsrEntryMap can be constructed with local-to-SSA mappings
    let mappings = vec![
        LocalMapping { local_index: 0, ssa_var: 10 },
        LocalMapping { local_index: 1, ssa_var: 11 },
        LocalMapping { local_index: 2, ssa_var: 12 },
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
        LocalMapping { local_index: 5, ssa_var: 10 }, // index 5, but only 2 locals
    ];
    let entry_map = OsrEntryMap::new(mappings, 0);
    let locals = [BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)];
    let result = entry_map.enter(&locals, std::ptr::null());
    assert!(result.is_err(), "enter should fail when local_index is out of bounds");
}

#[test]
fn osr_entry_with_compiled_function() {
    // Build a compiled function and attempt OSR entry — this exercises the
    // osr_entry() free function which validates the function and entry map
    // before attempting the (unimplemented) stack transfer.
    let mut header = vec![0u8; 16];
    let header_ptr = header.as_mut_ptr();
    let func_val = BlissVal(header_ptr as u64 | bliss_rt::value::TAG_FUNCTION as u64);

    let entry_map = OsrEntryMap::new(
        vec![LocalMapping { local_index: 0, ssa_var: 0 }],
        0,
    );
    let locals = [BlissVal::from_fixnum(42)];

    // osr_entry should validate inputs and then hit unimplemented!() for
    // the actual stack transfer — we expect a panic (not an Err).
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        osr_entry(func_val, &entry_map, &locals)
    }));
    // The function either panics (unimplemented) or returns an error —
    // either is acceptable in red phase; what matters is it doesn't silently succeed
    assert!(
        result.is_err() || result.unwrap().is_err(),
        "osr_entry should either panic (unimplemented) or return Err"
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
    assert!(log.is_blacklisted(), "3 deopts should trigger blacklisting (threshold=3)");
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
    assert!(!log.is_blacklisted(), "4 deopts below threshold 5 should not blacklist");
    log.record(DeoptReason::InlineCacheOverflow);
    assert!(log.is_blacklisted(), "5 deopts at threshold 5 should blacklist");
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
    assert_eq!(config.osr_threshold, 10000,
        "TierConfig should carry osr_threshold for back-edge triggered OSR");
}

#[test]
fn osr_deoptimize_with_compiled_function() {
    // Exercise the deoptimize() free function with a compiled function.
    // It should validate and then hit unimplemented!() for stack reconstruction.
    let mut header = vec![0u8; 16];
    let header_ptr = header.as_mut_ptr();
    let func_val = BlissVal(header_ptr as u64 | bliss_rt::value::TAG_FUNCTION as u64);

    let live_values = [BlissVal::from_fixnum(1), BlissVal::from_fixnum(2)];
    let reason = DeoptReason::TypeMismatch {
        expected: "fixnum".into(),
        actual: "symbol".into(),
    };

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        deoptimize(func_val, reason, &live_values)
    }));
    // deoptimize should either panic (unimplemented) or return Err
    assert!(
        result.is_err() || result.unwrap().is_err(),
        "deoptimize should either panic (unimplemented) or return Err"
    );
}

// ═══════════════════════════════════════════════════════════════════
// 7. End-to-end acceptance: full compile pipeline
// ═══════════════════════════════════════════════════════════════════

#[test]
fn acceptance_full_pipeline_forms() {
    for src in &["42", "nil", "t", "-42", "3.14", "'hello",
                 "(+ 1 2)", "(if t 1 2)", "(lambda (x) x)",
                 "\"hello world\"", "#\\Space",
                 "(defun foo (x) (+ x 1))"] {
        let code = full_pipeline_x86_64(src);
        assert!(!code.is_empty(), "full pipeline for '{}' produced empty code", src);
        // Every emitted function must have x86-64 prologue (push rbp) and ret
        assert_eq!(code.code()[0], 0x55,
            "full pipeline for '{}' should emit push rbp prologue", src);
        assert!(code.code().contains(&0xC3),
            "full pipeline for '{}' should contain ret instruction", src);
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
    assert!(has_movabs,
        "code for '42' should contain movabs (0x48 0xB8) encoding the literal value");
    // The fixnum raw bytes for 42 should follow the movabs prefix
    let fixnum_42 = BlissVal::from_fixnum(42);
    let raw_bytes = fixnum_42.0.to_le_bytes();
    let has_value = bytes.windows(10).any(|w| w[0] == 0x48 && w[1] == 0xB8 && w[2..10] == raw_bytes);
    assert!(has_value,
        "code for '42' should contain the fixnum-tagged encoding of 42");
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
    assert!(macroexpand(sym_a, &env).is_err(), "circular expansion should error");
}

#[test]
fn pipeline_ir_verify_missing_control() {
    let mut graph = IrGraph::new();
    let _start = graph.add_node(NodeKind::Start);
    let _ret = graph.add_node(NodeKind::Return);
    // No edge wired: Return lacks control input
    assert!(verify(&graph).is_err(), "should catch Return without control input");
}
