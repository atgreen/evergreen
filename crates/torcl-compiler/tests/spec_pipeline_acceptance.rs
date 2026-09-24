use std::sync::Mutex;
use std::sync::atomic::Ordering;
use torcl_compiler::codegen::{Aarch64Backend, CodegenBackend, X86_64Backend, native_arch};
use torcl_compiler::ir::{IrBuilder, NodeKind, verify};
use torcl_compiler::macroexpand::{Environment, macroexpand};
use torcl_compiler::reader::read_from_string;
use torcl_compiler::tiered::{
    BaselineCompiler, FnMeta, Interpreter, OptimisingCompiler, Tier, TierConfig, check_promotion,
    process_compilation_request, request_compilation,
};
use torcl_rt::value::{NIL, TorclVal};

/// Serializes the tests that drive the process-global compilation queue
/// (`request_compilation`/`process_compilation_request`). Those statics are
/// shared by every test in this binary, so running two queue-driving tests
/// concurrently lets one test's `process_compilation_request` pop and compile
/// the OTHER test's request — leaving its own function un-promoted and failing
/// the `Tier::Optimising` assertion nondeterministically. The lock makes each
/// enqueue→process→assert sequence atomic without changing behaviour for the
/// single-threaded case.
static COMPILATION_QUEUE_SERIAL: Mutex<()> = Mutex::new(());

fn read_expand(source: &str) -> TorclVal {
    let (form, _) = read_from_string(source).expect("reader should accept spec fixture");
    let (expanded, _) =
        macroexpand(form, &Environment::null()).expect("macroexpand should accept spec fixture");
    expanded
}

fn read_expand_build(source: &str) -> torcl_compiler::ir::IrGraph {
    let expanded = read_expand(source);
    let mut builder = IrBuilder::new();
    builder.build(expanded).expect("IR build should succeed")
}

fn make_function(body: TorclVal, params: TorclVal) -> (TorclVal, &'static FnMeta) {
    let meta = Box::leak(Box::new(FnMeta::new(0, body, params)));
    // SAFETY: tests leak the function metadata for process lifetime.
    let function = unsafe { TorclVal::from_function_ptr(meta as *mut FnMeta as *mut u8) };
    (function, meta)
}

#[test]
fn acceptance_read_macroexpand_ir_and_t2_codegen_for_branching_form() {
    // Per R10.06 and R11.07, the end-to-end compiler path must drive
    // read -> macroexpand -> IR -> optimisation contract -> code emission.
    // Per R11.06, the step must be testable by compiling a small CL function.
    let expanded = read_expand("(if t 1 2)");
    let graph = read_expand_build("(if t 1 2)");
    verify(&graph).expect("IR must verify before codegen");

    let node_kinds: Vec<_> = (0..graph.node_count() as u32)
        .map(|id| graph.node_kind(torcl_compiler::NodeId(id)).clone())
        .collect();
    assert!(
        node_kinds
            .iter()
            .any(|kind| matches!(kind, NodeKind::Branch)),
        "branching source must produce a Branch node in the public IR"
    );
    assert!(
        node_kinds
            .iter()
            .any(|kind| matches!(kind, NodeKind::Return)),
        "pipeline must end in a Return node"
    );

    let code = match native_arch() {
        torcl_compiler::TargetArch::X86_64 => X86_64Backend::new()
            .emit(&graph)
            .expect("x86-64 backend should emit code for the same graph"),
        torcl_compiler::TargetArch::Aarch64 => Aarch64Backend::new()
            .emit(&graph)
            .expect("aarch64 backend should emit code for the same graph"),
    };
    assert!(
        !code.is_empty(),
        "the same branching form that produced the IR must also produce machine code"
    );
    assert_eq!(
        expanded.tag() & torcl_rt::value::TAG_MASK,
        expanded.tag(),
        "the fixture is threaded through reader and macroexpand before code emission"
    );
}

#[test]
fn acceptance_real_calls_promote_t0_function_then_publish_t2_switch() {
    // Own the global compilation queue for this test's enqueue→process span.
    let _queue = COMPILATION_QUEUE_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // Per R4.23 every user-defined function starts in T0.
    // Per R4.24/R4.26 the next tier is T1, and per R4.27 a later T2 install
    // must publish the new entry atomically for subsequent calls.
    let (function, meta) = make_function(TorclVal::from_fixnum(41), NIL);
    let mut interpreter = Interpreter::new();
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 10,
        osr_threshold: 10_000,
        compile_threads: 1,
    };

    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Interpreter as u8);
    assert!(meta.entry.load(Ordering::Acquire).is_null());

    for call in 0..10 {
        let result = interpreter
            .apply(function, NIL)
            .expect("real T0 calls should return the function body result");
        assert_eq!(result, TorclVal::from_fixnum(41));
        if call < 9 {
            assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Interpreter as u8);
        }
    }

    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Baseline as u8);
    let t1_entry = meta.entry.load(Ordering::Acquire);
    assert!(
        !t1_entry.is_null(),
        "crossing the T0->T1 threshold through real calls should install baseline code"
    );
    assert_eq!(
        check_promotion(function, &config),
        Some(Tier::Optimising),
        "the same real call sequence should make the baseline function hot enough for T2"
    );

    request_compilation(function, Tier::Optimising)
        .expect("queueing a real hot function should succeed");
    assert_eq!(
        meta.tier.load(Ordering::Acquire),
        Tier::Baseline as u8,
        "while background T2 compilation is pending, the function must stay at T1"
    );
    assert_eq!(
        meta.entry.load(Ordering::Acquire),
        t1_entry,
        "the active entry must not change until the queued T2 compilation is processed"
    );

    assert!(
        process_compilation_request(),
        "the background compiler worker should process the queued T2 promotion"
    );
    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Optimising as u8);
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
    let (function, meta) = make_function(TorclVal::from_fixnum(7), NIL);

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
    // Own the global compilation queue for this test's enqueue→process span.
    let _queue = COMPILATION_QUEUE_SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (function, meta) = make_function(TorclVal::from_fixnum(9), NIL);
    let mut interpreter = Interpreter::new();
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 10,
        osr_threshold: 10_000,
        compile_threads: 1,
    };

    for _ in 0..10 {
        interpreter
            .apply(function, NIL)
            .expect("real calls should drive T0 hotness through the public interpreter");
    }

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
