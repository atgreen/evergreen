use bliss_compiler::ir::{IrBuilder, NodeKind, verify};
use bliss_compiler::macroexpand::{Environment, macroexpand};
use bliss_compiler::reader::read_from_string;
use bliss_compiler::tiered::{
    BaselineCompiler, CompiledCode, FnMeta, OptimisingCompiler, Tier, TierConfig,
    check_promotion, process_compilation_request, request_compilation,
};
use bliss_rt::value::{BlissVal, NIL, T};
use std::sync::atomic::Ordering;

fn read_expand_build(source: &str) -> bliss_compiler::ir::IrGraph {
    let (form, _) = read_from_string(source).expect("reader should accept spec fixture");
    let (expanded, _) =
        macroexpand(form, &Environment::null()).expect("macroexpand should accept spec fixture");
    let mut builder = IrBuilder::new();
    builder.build(expanded).expect("IR build should succeed")
}

fn make_function(body: BlissVal, params: BlissVal) -> (BlissVal, &'static FnMeta) {
    let meta = Box::leak(Box::new(FnMeta::new(0, body, params)));
    // SAFETY: tests leak the function metadata for process lifetime.
    let function = unsafe { BlissVal::from_function_ptr(meta as *mut FnMeta as *mut u8) };
    (function, meta)
}

fn install_and_assert(code: CompiledCode, function: BlissVal, meta: &FnMeta, tier: Tier) {
    code.install(function)
        .expect("compiled code should install through public API");
    assert_eq!(meta.tier.load(Ordering::Acquire), tier as u8);
    assert!(
        !meta.entry.load(Ordering::Acquire).is_null(),
        "tier install must publish a non-null entry pointer"
    );
}

#[test]
fn acceptance_read_macroexpand_ir_and_t2_codegen_for_branching_form() {
    // Per R10.06 and R11.07, the end-to-end compiler path must drive
    // read -> macroexpand -> IR -> optimisation contract -> code emission.
    // Per R11.06, the step must be testable by compiling a small CL function.
    let graph = read_expand_build("(if t 1 2)");
    verify(&graph).expect("IR must verify before codegen");

    let node_kinds: Vec<_> = (0..graph.node_count() as u32 + 8)
        .filter_map(|id| {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                graph.node_kind(bliss_compiler::NodeId(id)).clone()
            }))
            .ok()
        })
        .collect();
    assert!(
        node_kinds.iter().any(|kind| matches!(kind, NodeKind::Branch)),
        "branching source must produce a Branch node in the public IR"
    );
    assert!(
        node_kinds.iter().any(|kind| matches!(kind, NodeKind::Return)),
        "pipeline must end in a Return node"
    );

    let mut compiler = OptimisingCompiler::new();
    let code = compiler
        .compile(T)
        .expect("public T2 compiler surface should produce code for a small fixture");
    assert_eq!(code.tier(), Tier::Optimising);
    assert!(code.code_size() > 0);
    assert!(!code.entry_point().is_null());
}

#[test]
fn acceptance_t0_function_promotes_and_installs_t1_then_t2() {
    // Per R4.23 every user-defined function starts in T0.
    // Per R4.24/R4.26 the next tier is T1, and per R4.27 a later T2 install
    // must publish the new entry atomically for subsequent calls.
    let (function, meta) = make_function(BlissVal::from_fixnum(41), NIL);
    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Interpreter as u8);
    assert!(meta.entry.load(Ordering::Acquire).is_null());

    let mut baseline = BaselineCompiler::new();
    let t1_code = baseline
        .compile(function)
        .expect("baseline compiler should accept a function object");
    install_and_assert(t1_code, function, meta, Tier::Baseline);
    let t1_entry = meta.entry.load(Ordering::Acquire);

    let mut opt = OptimisingCompiler::new();
    let t2_code = opt
        .compile(function)
        .expect("optimising compiler should accept the same function object");
    install_and_assert(t2_code, function, meta, Tier::Optimising);
    let t2_entry = meta.entry.load(Ordering::Acquire);

    assert_ne!(
        t1_entry, t2_entry,
        "installing T2 should replace the active entry point published by T1"
    );
}

#[test]
fn acceptance_public_pipeline_compiles_small_cl_fixture_independently_at_both_tiers() {
    // Per R11.03 and R11.06, small compiler steps must be independently testable
    // via public compilation surfaces.
    let (function, meta) = make_function(BlissVal::from_fixnum(7), NIL);

    let mut baseline = BaselineCompiler::new();
    let t1 = baseline
        .compile(function)
        .expect("T1 compilation should succeed for a small fixture");
    assert_eq!(t1.tier(), Tier::Baseline);
    assert!(t1.code_size() > 0);

    let mut opt = OptimisingCompiler::new();
    let t2 = opt
        .compile(function)
        .expect("T2 compilation should succeed for a small fixture");
    assert_eq!(t2.tier(), Tier::Optimising);
    assert!(t2.code_size() > 0);

    t2.install(function)
        .expect("compiled code should be installable onto the fixture function");
    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Optimising as u8);
}

#[test]
fn acceptance_background_compilation_request_processes_real_function_object() {
    // Per R4.25-R4.27, hot functions must transition through the tiered pipeline
    // using the public orchestration APIs rather than a fake seam.
    let (function, meta) = make_function(BlissVal::from_fixnum(9), NIL);
    meta.tier.store(Tier::Baseline as u8, Ordering::Release);
    meta.invoke_count.store(5_000, Ordering::Relaxed);

    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5_000,
        osr_threshold: 10_000,
        compile_threads: 1,
    };
    assert_eq!(check_promotion(function, &config), Some(Tier::Optimising));

    request_compilation(function, Tier::Optimising)
        .expect("queueing a real hot function should succeed");
    assert!(
        process_compilation_request(),
        "compiler worker API should process the queued request"
    );
    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Optimising as u8);
    assert!(
        !meta.entry.load(Ordering::Acquire).is_null(),
        "processing the real request must publish executable code"
    );
}
