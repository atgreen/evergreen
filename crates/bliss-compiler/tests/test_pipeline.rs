//! Compiler pipeline integration tests.
//!
//! Tests the reader → macroexpand → IR → optimization → codegen pipeline
//! end-to-end, exercising cross-module integration within bliss-compiler.

use bliss_compiler::reader::read_from_string;
use bliss_compiler::macroexpand::{
    macroexpand_1, macroexpand, macroexpand_all, Environment, VariableInfo,
};
use bliss_compiler::ir::{IrBuilder, IrGraph, NodeKind, EdgeKind, verify};
use bliss_compiler::opt::PassManager;
use bliss_compiler::codegen::{
    CodegenBackend, X86_64Backend, Aarch64Backend, CodeBuffer, TargetArch,
    LinearScanAllocator,
};
use bliss_compiler::tiered::{
    Tier, TierConfig, Interpreter, BaselineCompiler, OptimisingCompiler,
    check_promotion,
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

// ═══════════════════════════════════════════════════════════════════
// 2. Reader → Macroexpand → IR (IrBuilder)
// ═══════════════════════════════════════════════════════════════════

#[test]
fn pipeline_forms_to_ir() {
    // Each form should produce a 3-node graph: Start, Constant, Return
    for src in &["42", "nil", "t", "-7", "3.14", "'foo", "(+ 1 2)",
                 "(if t 1 2)", "(lambda (x) x)", "#\\a", "\"hello\""] {
        let graph = read_and_build_ir(src);
        assert_eq!(graph.node_count(), 3, "'{}' should produce 3-node graph", src);
        verify(&graph).unwrap_or_else(|e| panic!("verify failed for '{}': {:?}", src, e));
    }
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
fn tiered_compilers_produce_code() {
    let func = BlissVal::from_fixnum(0);
    let bc = BaselineCompiler::new().compile(func).unwrap();
    assert_eq!(bc.tier(), Tier::Baseline);
    assert!(bc.code_size() > 0);

    let oc = OptimisingCompiler::new().compile(func).unwrap();
    assert_eq!(oc.tier(), Tier::Optimising);
    assert!(oc.code_size() > 0);
}

#[test]
fn tiered_tiers_ordered() {
    assert!(Tier::Interpreter < Tier::Baseline);
    assert!(Tier::Baseline < Tier::Optimising);
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
    }
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
