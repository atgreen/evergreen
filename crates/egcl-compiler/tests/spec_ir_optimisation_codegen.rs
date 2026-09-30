// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU64, Ordering};
use std::thread;
use egcl_compiler::codegen::{
    Aarch64Backend, CodeBuffer, CodegenBackend, LinearScanAllocator, RelocKind, Relocation,
    StackMap, TargetArch, X86_64Backend, patch_code,
};
use egcl_compiler::ir::{Edge, EdgeKind, IrBuilder, IrGraph, NodeId, NodeKind, verify};
use egcl_compiler::opt::{
    ConstantFolding, DeadCodeElimination, EscapeAnalysis, FunctionRegistry, Inlining,
    InliningConfig, Licm, NullCheckElimination, Pass, PassManager, StrengthReduction,
    TypePropagation,
};
use egcl_compiler::osr::{
    ConversionKind, DeoptReason, LocalMapping, Location, OsrEntryMap, OsrSlotDesc, TypeGuard,
    clear_global_deopt_logs, deoptimize,
};
use egcl_compiler::read_from_string;
use egcl_compiler::tiered::{CompiledCode, Tier};
use egcl_rt::value::{T, TAG_FIXNUM, TAG_FUNCTION, EgclVal};

#[allow(dead_code)]
struct CodeBufferView {
    bytes: Vec<u8>,
    arch: TargetArch,
    relocations: Vec<Relocation>,
    stack_maps: Vec<StackMap>,
}

#[allow(dead_code)]
struct CompiledCodeView {
    code: Vec<u8>,
    tier: Tier,
}

#[repr(C)]
struct InstallHeader {
    entry: AtomicPtr<u8>,
    tier: u8,
    _pad: [u8; 3],
    invoke_count: u32,
}

fn edge(from: NodeId, to: NodeId, kind: EdgeKind, input_index: u32) -> Edge {
    Edge {
        from,
        to,
        kind,
        input_index,
    }
}

fn minimal_graph() -> IrGraph {
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph
}

fn count_nodes(graph: &IrGraph, pred: impl Fn(&NodeKind) -> bool) -> usize {
    graph
        .node_ids()
        .filter(|&id| pred(graph.node_kind(id)))
        .count()
}

fn single_return_data_input(graph: &IrGraph) -> NodeId {
    let ret = graph
        .node_ids()
        .find(|&id| matches!(graph.node_kind(id), NodeKind::Return))
        .expect("graph should contain a Return node");
    graph
        .inputs(ret)
        .iter()
        .find(|e| e.kind == EdgeKind::Data)
        .map(|e| e.from)
        .expect("Return should have a data input")
}

fn parse_form(source: &str) -> EgclVal {
    read_from_string(source)
        .unwrap_or_else(|err| panic!("reader failed for {source:?}: {err}"))
        .0
}

fn simple_callee_graph(result: EgclVal) -> IrGraph {
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let constant = graph.add_node(NodeKind::Constant(result));
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(constant, ret, EdgeKind::Data, 1));
    graph
}

fn code_buffer_view(buffer: &CodeBuffer) -> &CodeBufferView {
    // Bootstrap-layout assumption for red-phase backend tests: CodeBuffer keeps
    // its declared field order so tests can assert emitted metadata.
    unsafe { &*(buffer as *const CodeBuffer as *const CodeBufferView) }
}

fn compiled_code_from_parts(code: Vec<u8>, tier: Tier) -> CompiledCode {
    // Bootstrap-layout assumption for red-phase install-path tests.
    unsafe {
        std::mem::transmute::<CompiledCodeView, CompiledCode>(CompiledCodeView { code, tier })
    }
}

fn function_value_for_install(header: &InstallHeader) -> EgclVal {
    EgclVal((header as *const InstallHeader as u64) | TAG_FUNCTION)
}

#[test]
fn builder_parses_if_into_ssa_control_flow_and_pipeline_emits_code() {
    // Per R4.17, R4.18, and R4.19, the builder must construct SSA IR with
    // distinct data/control edges for macro-expanded CL control flow.
    // Per R4.31-R4.37 and R4.42, the full optimiser/backend pipeline must
    // accept the resulting graph end-to-end.
    let form = parse_form("(if nil 1 2)");
    let mut builder = IrBuilder::new();
    let mut graph = builder.build(form).expect("IR build should succeed");

    assert!(count_nodes(&graph, |k| matches!(k, NodeKind::Branch)) >= 1);
    assert!(count_nodes(&graph, |k| matches!(k, NodeKind::Region)) >= 1);
    assert!(count_nodes(&graph, |k| matches!(k, NodeKind::Phi)) >= 1);
    assert!(verify(&graph).is_ok(), "builder output should verify");

    let mut passes = PassManager::new();
    passes
        .run_all(&mut graph)
        .expect("optimisation pipeline should accept builder output");

    let mut backend = X86_64Backend::new();
    let buffer = backend.emit(&graph).expect("codegen should succeed");
    assert!(
        !buffer.code().is_empty(),
        "pipeline should emit machine code"
    );
}

#[test]
fn speculative_guards_carry_deopt_metadata_and_fail_via_deopt_path() {
    // Per R4.21, speculative guards must carry uncommon-trap/deoptimisation
    // metadata so failed speculation deoptimises instead of hard-failing.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let value = graph.add_node(NodeKind::Parameter(0));
    let guard = graph.add_node(NodeKind::TypeCheck {
        expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
    });
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, guard, EdgeKind::Control, 0));
    graph.add_edge(edge(value, guard, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(guard, ret, EdgeKind::Data, 1));

    let mut backend = X86_64Backend::new();
    let buffer = backend
        .emit(&graph)
        .expect("guarded codegen should succeed");
    let view = code_buffer_view(&buffer);
    assert!(
        view.relocations
            .iter()
            .any(|reloc| reloc.kind == RelocKind::PcRel32),
        "speculative guard should record a deopt/uncommon-trap relocation"
    );

    let mut osr_map = OsrEntryMap::new(
        vec![LocalMapping {
            local_index: 0,
            ssa_var: 0,
        }],
        17,
    );
    osr_map.slots.push(OsrSlotDesc {
        source_offset: 0,
        dest: Location::Register(0),
        conversion: ConversionKind::None,
    });
    osr_map.type_guards.push(TypeGuard {
        slot_index: 0,
        expected_tag: TAG_FIXNUM,
    });
    assert!(
        osr_map
            .enter(&[EgclVal::from_single_float(3.25)], std::ptr::null())
            .is_err(),
        "failed speculative guard should abandon the OSR entry instead of hard-erroring"
    );

    clear_global_deopt_logs();
    let header = InstallHeader {
        entry: AtomicPtr::new(std::ptr::null_mut()),
        tier: Tier::Optimising as u8,
        _pad: [0; 3],
        invoke_count: 0,
    };
    let function = function_value_for_install(&header);
    let result = deoptimize(
        function,
        DeoptReason::TypeMismatch {
            expected: "fixnum".into(),
            actual: "single-float".into(),
        },
        &[EgclVal::from_single_float(3.25)],
    );
    assert!(
        result.is_ok(),
        "failed speculation should route through deoptimisation metadata"
    );
}

#[test]
fn graph_retains_distinct_data_control_and_memory_edges() {
    // Per R4.18, the IR must encode data, control, and memory dependencies
    // as separate typed edges rather than conflating them.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let base = graph.add_node(NodeKind::Parameter(0));
    let value = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(7)));
    let store = graph.add_node(NodeKind::MemStore { offset: 8 });
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, store, EdgeKind::Control, 0));
    graph.add_edge(edge(start, store, EdgeKind::Memory, 1));
    graph.add_edge(edge(base, store, EdgeKind::Data, 2));
    graph.add_edge(edge(value, store, EdgeKind::Data, 3));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));

    let kinds: Vec<_> = graph.inputs(store).iter().map(|e| e.kind).collect();
    assert!(kinds.contains(&EdgeKind::Control));
    assert!(kinds.contains(&EdgeKind::Memory));
    assert_eq!(kinds.iter().filter(|&&k| k == EdgeKind::Data).count(), 2);
}

#[test]
fn verifier_rejects_phi_arity_and_pinned_control_violations() {
    // Per R4.20, verification must detect malformed SSA/edge invariants,
    // including bad Phi shape and invalid control wiring for pinned nodes.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let region = graph.add_node(NodeKind::Region);
    let phi = graph.add_node(NodeKind::Phi);
    let load = graph.add_node(NodeKind::MemLoad { offset: 0 });
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, region, EdgeKind::Control, 0));
    graph.add_edge(edge(start, region, EdgeKind::Control, 1));
    graph.add_edge(edge(region, phi, EdgeKind::Data, 0));
    graph.add_edge(edge(start, phi, EdgeKind::Data, 1));
    graph.add_edge(edge(start, load, EdgeKind::Control, 0));
    graph.add_edge(edge(region, load, EdgeKind::Control, 1));
    graph.add_edge(edge(load, ret, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));

    let errors = verify(&graph).expect_err("verifier should reject malformed IR");
    assert!(errors.iter().any(|e| e.contains("V4")));
    assert!(errors.iter().any(|e| e.contains("V6")));
}

#[test]
fn builder_is_expected_to_attach_source_locations_to_every_node() {
    // Per R4.22, every IR node must carry source-position information
    // sufficient for debugger/condition-system reporting.
    let form = parse_form("(progn 1 2 3)");
    let mut builder = IrBuilder::new();
    let graph = builder.build(form).expect("IR build should succeed");

    for id in graph.node_ids() {
        assert!(
            graph.source_info(id).is_some(),
            "node {id:?} should carry source information"
        );
    }
}

#[test]
fn type_propagation_eliminates_redundant_constant_type_check() {
    // Per R4.31 and R4.37, propagated type facts should remove redundant
    // runtime checks on an SSA value whose type is already known.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let constant = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(42)));
    let check = graph.add_node(NodeKind::TypeCheck {
        expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
    });
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, check, EdgeKind::Control, 0));
    graph.add_edge(edge(constant, check, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(check, ret, EdgeKind::Data, 1));

    let changed = TypePropagation
        .run(&mut graph)
        .expect("pass should succeed");
    assert!(changed, "redundant TypeCheck should be removed");
    assert!(!graph.contains(check));
    assert_eq!(single_return_data_input(&graph), constant);
}

#[test]
fn constant_folding_applies_numeric_contagion_to_mixed_constants() {
    // Per R4.36, constant folding must evaluate constant expressions at
    // compile time using CL numeric contagion rules.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let a = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(2)));
    let b = graph.add_node(NodeKind::Constant(EgclVal::from_single_float(1.5)));
    let call = graph.add_node(NodeKind::Call);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, call, EdgeKind::Control, 0));
    graph.add_edge(edge(a, call, EdgeKind::Data, 1));
    graph.add_edge(edge(b, call, EdgeKind::Data, 2));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(call, ret, EdgeKind::Data, 1));

    let changed = ConstantFolding
        .run(&mut graph)
        .expect("pass should succeed");
    assert!(changed, "mixed numeric constants should fold");

    let folded = single_return_data_input(&graph);
    match graph.node_kind(folded) {
        NodeKind::Constant(value) => assert_eq!(value.as_single_float(), 3.5),
        other => panic!("expected folded constant, got {other:?}"),
    }
}

#[test]
fn inlining_respects_notinline_registry_entries() {
    // Per R4.32, the inliner must honour NOTINLINE even for otherwise
    // eligible callees.
    let callee = EgclVal::from_fixnum(99);
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let callee_node = graph.add_node(NodeKind::Constant(callee));
    let call = graph.add_node(NodeKind::Call);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, call, EdgeKind::Control, 0));
    graph.add_edge(edge(callee_node, call, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(call, ret, EdgeKind::Data, 1));

    let mut registry = FunctionRegistry::new();
    registry.register(
        callee,
        simple_callee_graph(EgclVal::from_fixnum(5)),
        true,
        true,
        true,
    );
    let mut pass = Inlining {
        config: InliningConfig {
            budget: 100,
            max_depth: 4,
            small_threshold: 30,
        },
        registry,
    };

    let changed = pass.run(&mut graph).expect("pass should succeed");
    assert!(!changed, "NOTINLINE callee should not be expanded");
    assert!(graph.contains(call));
}

#[test]
fn inlining_expands_small_registered_callee_into_caller() {
    // Per R4.32, the inliner must inline known small callees subject to
    // budget/depth policy from A4.09.
    let callee = EgclVal::from_fixnum(1234);
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let callee_node = graph.add_node(NodeKind::Constant(callee));
    let call = graph.add_node(NodeKind::Call);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, call, EdgeKind::Control, 0));
    graph.add_edge(edge(callee_node, call, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(call, ret, EdgeKind::Data, 1));

    let mut registry = FunctionRegistry::new();
    registry.register(
        callee,
        simple_callee_graph(EgclVal::from_fixnum(77)),
        false,
        false,
        false,
    );
    let mut pass = Inlining {
        config: InliningConfig {
            budget: 100,
            max_depth: 4,
            small_threshold: 30,
        },
        registry,
    };

    let changed = pass.run(&mut graph).expect("pass should succeed");
    assert!(changed, "small callee should be inlined");
    assert!(
        !graph.contains(call),
        "Call should be removed after inlining"
    );
    match graph.node_kind(single_return_data_input(&graph)) {
        NodeKind::Constant(value) => assert_eq!(value.as_fixnum(), 77),
        other => panic!("expected inlined constant result, got {other:?}"),
    }
}

#[test]
fn escape_analysis_scalar_replaces_non_escaping_box_unbox_pair() {
    // Per R4.33, escape analysis must identify non-escaping allocations and
    // enable scalar replacement / stack-like elimination.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let raw = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(9)));
    let boxed = graph.add_node(NodeKind::Box);
    let unboxed = graph.add_node(NodeKind::Unbox);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(raw, boxed, EdgeKind::Data, 0));
    graph.add_edge(edge(boxed, unboxed, EdgeKind::Data, 0));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(unboxed, ret, EdgeKind::Data, 1));

    let changed = EscapeAnalysis.run(&mut graph).expect("pass should succeed");
    assert!(
        changed,
        "non-escaping Box/Unbox pair should be scalar-replaced"
    );
    assert!(!graph.contains(unboxed));
    assert_eq!(single_return_data_input(&graph), raw);
}

fn loop_graph_with_guarded_value(kind: NodeKind) -> (IrGraph, NodeId) {
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let arg = graph.add_node(NodeKind::Parameter(0));
    let cond = graph.add_node(NodeKind::Constant(T));
    let header = graph.add_node(NodeKind::Region);
    let guarded = graph.add_node(kind);
    let branch = graph.add_node(NodeKind::Branch);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, header, EdgeKind::Control, 0));
    graph.add_edge(edge(branch, header, EdgeKind::Control, 1));
    graph.add_edge(edge(header, guarded, EdgeKind::Control, 0));
    graph.add_edge(edge(arg, guarded, EdgeKind::Data, 1));
    graph.add_edge(edge(guarded, branch, EdgeKind::Control, 0));
    graph.add_edge(edge(cond, branch, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(guarded, ret, EdgeKind::Data, 1));

    (graph, guarded)
}

#[test]
fn licm_hoists_invariant_non_side_effecting_loop_node() {
    // Per R4.34, LICM must detect natural loops and hoist invariant
    // expressions that do not have observable side effects.
    let (mut graph, original_guard) = loop_graph_with_guarded_value(NodeKind::TypeCheck {
        expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
    });

    let changed = Licm.run(&mut graph).expect("pass should succeed");
    assert!(changed, "invariant TypeCheck should be hoisted");
    assert!(!graph.contains(original_guard));
    assert_eq!(
        count_nodes(&graph, |k| matches!(k, NodeKind::TypeCheck { .. })),
        1
    );
}

#[test]
fn licm_does_not_hoist_side_effecting_memory_node() {
    // Per R4.34, LICM must not hoist expressions with observable memory effects.
    let (mut graph, original_load) = loop_graph_with_guarded_value(NodeKind::MemLoad { offset: 8 });

    let changed = Licm.run(&mut graph).expect("pass should succeed");
    assert!(
        !changed,
        "side-effecting memory node should remain in the loop"
    );
    assert!(graph.contains(original_load));
}

#[test]
fn strength_reduction_removes_identity_operation_shape() {
    // Per R4.36, strength reduction should replace expensive arithmetic
    // patterns with cheaper equivalents where the result is identical.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let value = graph.add_node(NodeKind::Parameter(0));
    let zero = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(0)));
    let call = graph.add_node(NodeKind::Call);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, call, EdgeKind::Control, 0));
    graph.add_edge(edge(value, call, EdgeKind::Data, 1));
    graph.add_edge(edge(zero, call, EdgeKind::Data, 2));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(call, ret, EdgeKind::Data, 1));

    let changed = StrengthReduction
        .run(&mut graph)
        .expect("pass should succeed");
    assert!(changed, "identity operation should be reduced");
    assert_eq!(single_return_data_input(&graph), value);
}

#[test]
fn dead_code_elimination_removes_unreachable_nodes() {
    // Per R4.35, dead code elimination must remove side-effect-free nodes
    // that have no live uses.
    let mut graph = minimal_graph();
    let dead = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(404)));

    let changed = DeadCodeElimination
        .run(&mut graph)
        .expect("pass should succeed");
    assert!(changed);
    assert!(!graph.contains(dead));
}

#[test]
fn null_check_elimination_removes_dominated_identical_check() {
    // Per R4.37, redundant nil/type guards dominated by an identical
    // earlier guard on the same SSA value should be removed.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let value = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(1)));
    let first = graph.add_node(NodeKind::TypeCheck {
        expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
    });
    let second = graph.add_node(NodeKind::TypeCheck {
        expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
    });
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, first, EdgeKind::Control, 0));
    graph.add_edge(edge(value, first, EdgeKind::Data, 1));
    graph.add_edge(edge(first, second, EdgeKind::Control, 0));
    graph.add_edge(edge(value, second, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(second, ret, EdgeKind::Data, 1));

    let changed = NullCheckElimination
        .run(&mut graph)
        .expect("pass should succeed");
    assert!(changed, "dominated check should be removed");
    assert!(!graph.contains(second));
    assert_eq!(single_return_data_input(&graph), value);
}

#[test]
fn x86_64_backend_uses_system_v_parameter_and_frame_convention() {
    // Per R4.43, x86-64 codegen must follow System V AMD64 calling rules.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let param = graph.add_node(NodeKind::Parameter(0));
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(param, ret, EdgeKind::Data, 1));

    let mut backend = X86_64Backend::new();
    let buffer = backend.emit(&graph).expect("emit should succeed");
    let code = buffer.code();

    assert!(code.starts_with(&[0x55, 0x48, 0x89, 0xE5]));
    assert!(code.windows(3).any(|w| w == [0x48, 0x89, 0xF8]));
    assert!(code.ends_with(&[0x5D, 0xC3]));
}

#[test]
fn aarch64_backend_uses_aapcs64_parameter_and_frame_convention() {
    // Per R4.44, AArch64 codegen must follow AAPCS64 and emit a standard frame.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let param = graph.add_node(NodeKind::Parameter(1));
    let ret = graph.add_node(NodeKind::Return);
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(param, ret, EdgeKind::Data, 1));

    let mut backend = Aarch64Backend::new();
    let buffer = backend.emit(&graph).expect("emit should succeed");
    let code = buffer.code();

    assert!(code.starts_with(&0xA9BF7BFDu32.to_le_bytes()));
    assert!(code.windows(4).any(|w| w == 0xAA0103E0u32.to_le_bytes()));
    assert!(code.ends_with(&0xD65F03C0u32.to_le_bytes()));
}

#[test]
fn linear_scan_allocator_spills_when_live_ranges_exceed_registers() {
    // Per R4.45, register allocation must operate over SSA live ranges and
    // spill when pressure exceeds the available physical registers.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let seed = graph.add_node(NodeKind::Constant(EgclVal::from_fixnum(1)));
    let mut ctrl = start;
    let mut live_values = Vec::new();

    for _ in 0..20 {
        let check = graph.add_node(NodeKind::TypeCheck {
            expected_type: EgclVal::from_fixnum(TAG_FIXNUM as i64),
        });
        graph.add_edge(edge(ctrl, check, EdgeKind::Control, 0));
        graph.add_edge(edge(seed, check, EdgeKind::Data, 1));
        ctrl = check;
        live_values.push(check);
    }

    let call = graph.add_node(NodeKind::Call);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(ctrl, call, EdgeKind::Control, 0));
    for (i, value) in live_values.into_iter().enumerate() {
        graph.add_edge(edge(value, call, EdgeKind::Data, (i + 1) as u32));
    }
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(call, ret, EdgeKind::Data, 1));

    let mut allocator = LinearScanAllocator::new(TargetArch::X86_64);
    let allocation = allocator
        .allocate(&graph)
        .expect("allocation should succeed");
    assert!(allocation.spill_slots() > 0, "pressure should force spills");
}

#[test]
fn safepoints_emit_stack_maps_and_install_rejects_missing_maps() {
    // Per R4.46, every safepoint must carry a GC stack map, and missing maps
    // must be rejected during code installation rather than at GC time.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let safepoint = graph.add_node(NodeKind::Safepoint);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, safepoint, EdgeKind::Control, 0));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));

    let mut backend = X86_64Backend::new();
    let buffer = backend
        .emit(&graph)
        .expect("safepoint lowering should succeed");
    let view = code_buffer_view(&buffer);
    assert_eq!(
        view.stack_maps.len(),
        count_nodes(&graph, |kind| matches!(kind, NodeKind::Safepoint)),
        "every safepoint should emit exactly one stack map"
    );
    assert!(
        view.relocations
            .iter()
            .any(|reloc| reloc.kind == RelocKind::Safepoint),
        "safepoint emission should record safepoint relocation metadata"
    );

    let compiled = compiled_code_from_parts(buffer.code().to_vec(), Tier::Optimising);
    let header = InstallHeader {
        entry: AtomicPtr::new(std::ptr::null_mut()),
        tier: Tier::Baseline as u8,
        _pad: [0; 3],
        invoke_count: 0,
    };
    let install = compiled.install(function_value_for_install(&header));
    assert!(
        install.is_err(),
        "installation should reject compiled code that reaches a safepoint without an install-time stack-map validation payload"
    );
}

#[test]
fn backend_rejects_unlowered_ssa_only_nodes_before_emission() {
    // Per R4.42, IR nodes must be lowered to machine nodes before emission,
    // and an unlowered SSA-only node reaching the backend must abort codegen.
    let mut graph = IrGraph::new();
    let start = graph.add_node(NodeKind::Start);
    let region = graph.add_node(NodeKind::Region);
    let phi = graph.add_node(NodeKind::Phi);
    let ret = graph.add_node(NodeKind::Return);

    graph.add_edge(edge(start, region, EdgeKind::Control, 0));
    graph.add_edge(edge(start, region, EdgeKind::Control, 1));
    graph.add_edge(edge(region, phi, EdgeKind::Data, 0));
    graph.add_edge(edge(start, phi, EdgeKind::Data, 1));
    graph.add_edge(edge(start, ret, EdgeKind::Control, 0));
    graph.add_edge(edge(phi, ret, EdgeKind::Data, 1));

    let mut backend = X86_64Backend::new();
    assert!(
        backend.emit(&graph).is_err(),
        "SSA-only nodes that survive lowering should abort code generation"
    );
}

#[test]
fn patch_code_is_atomic_for_concurrent_readers_and_rejects_null_sites() {
    // Per R4.47, runtime patching must support concurrent-safe patch sites;
    // the observable API contract includes rejecting invalid sites and
    // applying the requested target atomically from the caller's perspective.
    let err = unsafe { patch_code(std::ptr::null_mut(), std::ptr::null()) };
    assert!(err.is_err(), "null patch sites must be rejected");

    let slot = Arc::new(AtomicU64::new(0x1111_2222_3333_4444));
    let stop = Arc::new(AtomicBool::new(false));
    let saw_torn = Arc::new(AtomicBool::new(false));
    let old_target = slot.load(Ordering::Relaxed);
    let new_target = 0xAAAA_BBBB_CCCC_DDDD_u64;

    thread::scope(|scope| {
        for _ in 0..4 {
            let slot = Arc::clone(&slot);
            let stop = Arc::clone(&stop);
            let saw_torn = Arc::clone(&saw_torn);
            scope.spawn(move || {
                while !stop.load(Ordering::Acquire) {
                    let observed = slot.load(Ordering::Acquire);
                    if observed != old_target && observed != new_target {
                        saw_torn.store(true, Ordering::Release);
                        break;
                    }
                }
            });
        }

        let slot = Arc::clone(&slot);
        let stop = Arc::clone(&stop);
        scope.spawn(move || {
            for target in [new_target, old_target, new_target, old_target, new_target] {
                unsafe {
                    patch_code(
                        (&*slot as *const AtomicU64).cast_mut().cast::<u8>(),
                        target as usize as *const u8,
                    )
                    .expect("patch should succeed");
                }
                thread::yield_now();
            }
            stop.store(true, Ordering::Release);
        });
    });

    assert!(
        !saw_torn.load(Ordering::Acquire),
        "concurrent readers should only observe the old or new target, never a torn patch"
    );
    assert_eq!(slot.load(Ordering::Acquire), new_target);
}
