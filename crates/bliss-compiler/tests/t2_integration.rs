//! Wave-1 cross-parcel integration: P1 (build) → P2 (verify) → P3 (infer).
//!
//! Each parcel was unit-tested in isolation against hand-built IR. This proves
//! they actually compose: bytecode built by P1 must satisfy P2's verifier and be
//! analysable by P3's inference, all against the frozen Phase-0 contracts.

use bliss_compiler::t2::build::build_from_bytecode;
use bliss_compiler::t2::infer::infer;
use bliss_compiler::t2::lower::lower;
use bliss_compiler::t2::opt_dce::Dce;
use bliss_compiler::t2::opt_escape::EscapeAnalysis;
use bliss_compiler::t2::opt_fold::ConstFold;
use bliss_compiler::t2::opt_guard::GuardElim;
use bliss_compiler::t2::opt_gvn::Gvn;
use bliss_compiler::t2::opt_licm::Licm;
use bliss_compiler::t2::pass::PassManager;
use bliss_compiler::t2::regalloc::allocate;
use bliss_compiler::t2::verify::verify;
use bliss_rt::bytecode::{BytecodeFunction, Instr};
use bliss_rt::value::BlissVal;

fn bytecode_fn(name: &str, code: Vec<Instr>, constants: Vec<BlissVal>, n_locals: u16, max_stack: u16, arity: u16) -> BytecodeFunction {
    BytecodeFunction {
        code,
        constants,
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        param_layout: vec![],
        has_env: false,
        n_locals,
        max_stack,
        arity,
        name: name.to_string(),
    }
}

/// `(lambda () 42)` — the smallest real function: push a constant, return it.
/// P1 builds it, P2 must accept it, P3 must see the fixnum constant.
#[test]
fn const_return_builds_verifies_and_infers() {
    let bf = bytecode_fn(
        "const42",
        vec![Instr::Const(0), Instr::Return],
        vec![BlissVal::from_fixnum(42)],
        0,
        1,
        0,
    );

    let f = build_from_bytecode(&bf).expect("P1 should build a const-return function");

    // P2: the IR P1 produced must be well-formed.
    verify(&f).expect("P2 must accept P1's IR");

    // P3: inference must run on it and find at least one known fact.
    let facts = infer(&f);
    assert!(facts.known_count() >= 1, "P3 should infer the constant's type");
}

/// A branch + merge: `(if <x> 1 2)`-shaped bytecode exercises block parameters on
/// the merge, which is where P1/P2/P3 most need to agree.
#[test]
fn branch_merge_builds_and_verifies() {
    // 0: Const c0 (the test value)     -> stack [c0]
    // 1: BrIfFalse 4                   -> pop; false→4, true→2
    // 2: Const c1                      -> stack [c1]
    // 3: Br 5
    // 4: Const c2                      -> stack [c2]
    // 5: Return                        -> pop and return the merged value
    let bf = bytecode_fn(
        "branch",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            BlissVal::from_fixnum(0),
            BlissVal::from_fixnum(1),
            BlissVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );

    let f = build_from_bytecode(&bf).expect("P1 should build a branch/merge function");
    verify(&f).expect("P2 must accept P1's branch/merge IR (block params on the merge)");
    let _ = infer(&f); // must not panic on merged block parameters
}

/// Wave-2 mid-end: the four optimisation passes (P4a–d) must compose on real
/// P1-built branching IR and preserve well-formedness (P2 still accepts it).
#[test]
fn optimisation_passes_compose_and_preserve_wellformedness() {
    let bf = bytecode_fn(
        "opt",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            BlissVal::from_fixnum(0),
            BlissVal::from_fixnum(1),
            BlissVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );
    let mut f = build_from_bytecode(&bf).expect("build");
    verify(&f).expect("pre-opt IR must verify");

    let mut pm = PassManager::new();
    pm.add(Box::new(ConstFold)); // P4f
    pm.add(Box::new(Gvn)); // P4a
    pm.add(Box::new(Licm)); // P4b
    pm.add(Box::new(EscapeAnalysis)); // P4e
    pm.add(Box::new(GuardElim)); // P4d
    pm.add(Box::new(Dce)); // P4c
    pm.run(&mut f);

    // The whole mid-end must leave the IR well-formed (spec §4.10 R4.60 etc.).
    verify(&f).expect("post-opt IR must still verify");
}

/// Speculative lowering on real P1 IR: `(* x 5)` with a fixnum-hot profile at the
/// call site becomes a single guarded `FixnumMul` (no float path), and the result
/// still verifies. This is the profile → single-type-speculation step end to end.
#[test]
fn fixnum_profile_speculates_the_call() {
    use bliss_compiler::t2::ir::Opcode;
    use bliss_compiler::t2::speculate::{speculate, SpecType};

    // (lambda (x) (* x 5)): LoadLocal 0, Const 5, CallNamed *, Return. The
    // CallNamed is at bcp 2.
    let star = bliss_rt::symbols::intern("*");
    let bf = BytecodeFunction {
        code: vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed { sym: star, nargs: 2 },
            Instr::Return,
        ],
        constants: vec![BlissVal::from_fixnum(5)],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        param_layout: vec![],
        has_env: false,
        n_locals: 1,
        max_stack: 2,
        arity: 1,
        name: "mul5".to_string(),
    };

    let mut f = build_from_bytecode(&bf).expect("build");
    verify(&f).expect("pre-speculation IR verifies");

    // The site at bcp 2 is fixnum-hot → speculate FIXNUM.
    let n = speculate(&mut f, &|bcp| if bcp == 2 { Some(SpecType::Fixnum) } else { None });
    assert_eq!(n, 1, "the one arithmetic call site must be speculated");

    // Exactly one FixnumMul, guard-flagged, and no FloatMul anywhere.
    let mut fixnum_muls = 0;
    let mut float_muls = 0;
    for b in f.block_order().to_vec() {
        for &inst in &f.block(b).insts {
            match f.inst(inst).opcode {
                Opcode::FixnumMul => {
                    fixnum_muls += 1;
                    assert!(f.inst(inst).flags.guard, "FixnumMul must be a guarded deopt point");
                }
                Opcode::FloatMul => float_muls += 1,
                _ => {}
            }
        }
    }
    assert_eq!(fixnum_muls, 1, "one guarded FixnumMul");
    assert_eq!(float_muls, 0, "no float path emitted (mutually exclusive)");

    verify(&f).expect("speculated IR still verifies");
}

/// Branching reaches T2: `(x) -> (if (< x 100) (* x 2) 999)` speculates BOTH the
/// comparison (fused cmp+jcc) and the multiply, emits a multi-block framed
/// function, and executes correctly on both arms.
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn branching_if_speculates_and_runs() {
    use bliss_compiler::t2::emit::emit_framed;
    use bliss_compiler::t2::speculate::{speculate, SpecType};
    let lt = bliss_rt::symbols::intern("<");
    let mul = bliss_rt::symbols::intern("*");
    // 0 LoadLocal 0 ; 1 Const 100 ; 2 (< x 100) ; 3 BrIfFalse->8(else)
    // 4 LoadLocal 0 ; 5 Const 2 ; 6 (* x 2) ; 7 Br->9 ; 8 Const 999 ; 9 Return
    let bf = bytecode_fn(
        "clamp",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed { sym: lt, nargs: 2 },
            Instr::BrIfFalse(8),
            Instr::LoadLocal(0),
            Instr::Const(1),
            Instr::CallNamed { sym: mul, nargs: 2 },
            Instr::Br(9),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            BlissVal::from_fixnum(100),
            BlissVal::from_fixnum(2),
            BlissVal::from_fixnum(999),
        ],
        1,
        3,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build branching");
    let n = speculate(&mut f, &|bcp| (bcp == 2 || bcp == 6).then_some(SpecType::Fixnum));
    assert_eq!(n, 2, "both the comparison and the multiply are speculated");
    verify(&f).expect("speculated branching IR verifies");

    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, None).expect("emit branching function");
    let buf = bliss_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };

    let mut frame = [BlissVal::from_fixnum(50).0, 0u64, 0u64, 0u64];
    assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 100, "50<100 → 50*2");
    let mut frame = [BlissVal::from_fixnum(99).0, 0u64, 0u64, 0u64];
    assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 198, "99<100 → 99*2");
    let mut frame = [BlissVal::from_fixnum(100).0, 0u64, 0u64, 0u64];
    assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 999, "100≮100 → 999");
    let mut frame = [BlissVal::from_fixnum(200).0, 0u64, 0u64, 0u64];
    assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 999, "200≥100 → 999");
}

/// Function calls reach T2: `(f n) = (* n (g n))` contains a call, so it emits via
/// the callee-saved path (values survive the c2i call) — the multiply is speculated
/// and the call is lowered. Emission must succeed (has_calls: compiled_entry = 0).
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn call_containing_function_emits() {
    use bliss_compiler::t2::emit::emit_framed;
    use bliss_compiler::t2::speculate::{speculate, SpecType};
    let g = bliss_rt::symbols::intern("g-callee");
    let mul = bliss_rt::symbols::intern("*");
    // 0 LoadLocal 0 (n) ; 1 LoadLocal 0 (n) ; 2 (g n) ; 3 (* n <g>) ; 4 Return
    let bf = bytecode_fn(
        "callf",
        vec![
            Instr::LoadLocal(0),
            Instr::LoadLocal(0),
            Instr::CallNamed { sym: g, nargs: 1 },
            Instr::CallNamed { sym: mul, nargs: 2 },
            Instr::Return,
        ],
        vec![],
        1,
        3,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build call-containing fn");
    // Speculate only the multiply (bcp 3); the call at bcp 2 stays a generic Call.
    let n = speculate(&mut f, &|bcp| (bcp == 3).then_some(SpecType::Fixnum));
    assert_eq!(n, 1, "the multiply is speculated; the call is not");
    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, None).expect("a call-containing function must emit");
    assert!(!framed.code.is_empty());
    // A call function with ≤4 params gets a register entry (for direct self-calls),
    // so its compiled entry sits past the interpreter (frame-loading) entry.
    assert!(framed.compiled_entry > 0, "call function should expose a register entry");
}

/// Bitwise ops reach T2: `(x) -> (logand x 255)` speculates LogAnd and emits a
/// single `and` on the tagged value (tagged(a) & tagged(b) = tagged(a & b)).
#[cfg(all(target_arch = "x86_64", unix))]
#[test]
fn bitwise_logand_speculates_and_runs() {
    use bliss_compiler::t2::emit::emit_framed;
    use bliss_compiler::t2::ir::Opcode;
    use bliss_compiler::t2::speculate::{speculate, SpecType};
    let logand = bliss_rt::symbols::intern("LOGAND");
    // 0 LoadLocal 0 (x) ; 1 Const 255 ; 2 (logand x 255) ; 3 Return
    let bf = bytecode_fn(
        "mask",
        vec![
            Instr::LoadLocal(0),
            Instr::Const(0),
            Instr::CallNamed { sym: logand, nargs: 2 },
            Instr::Return,
        ],
        vec![BlissVal::from_fixnum(255)],
        1,
        2,
        1,
    );
    let mut f = build_from_bytecode(&bf).expect("build");
    let n = speculate(&mut f, &|bcp| (bcp == 2).then_some(SpecType::Fixnum));
    assert_eq!(n, 1, "the logand call is speculated");
    assert!(
        f.block_order().iter().any(|&b| f.block(b).insts.iter().any(|&i| f.inst(i).opcode == Opcode::LogAnd)),
        "a LogAnd op must be present"
    );
    let framed = emit_framed(&f, 0, 0, 0, 0, 0, 0, None).expect("emit bitwise");
    let buf = bliss_rt::jit::JitBuffer::new(&framed.code).expect("mmap");
    let func: extern "C" fn(*mut u64) -> u64 = unsafe { std::mem::transmute(buf.as_ptr()) };
    let mut frame = [BlissVal::from_fixnum(0x3E7).0, 0u64, 0u64];
    assert_eq!(BlissVal(func(frame.as_mut_ptr())).as_fixnum(), 0x3E7 & 255, "999 & 255 = 231");
}

/// Wave-3 milestone: with P5b (block-CFG lowering) + P6b (multi-block regalloc),
/// the FULL backend now composes on BRANCHING IR — not just straight-line. This
/// is the pipeline stage that the single-block limitation previously blocked.
#[test]
fn backend_lowers_and_allocates_branching() {
    // (if <c> 1 2)-shaped bytecode → a diamond CFG (no critical edges).
    let bf = bytecode_fn(
        "br",
        vec![
            Instr::Const(0),
            Instr::BrIfFalse(4),
            Instr::Const(1),
            Instr::Br(5),
            Instr::Const(2),
            Instr::Return,
        ],
        vec![
            BlissVal::from_fixnum(0),
            BlissVal::from_fixnum(1),
            BlissVal::from_fixnum(2),
        ],
        0,
        1,
        0,
    );
    let f = build_from_bytecode(&bf).expect("build");

    let mut mf = lower(&f); // P5b — must produce a multi-block CFG
    assert!(mf.blocks.len() >= 3, "branch/merge must lower to a block CFG: {}", mf.blocks.len());

    allocate(&mut mf); // P6b — must allocate the branching CFG (was blocked at single-block)
    assert!(!mf.allocation.is_empty(), "P6b must allocate the branching function");
}

/// Wave-2 backend: P5 lower → P6 regalloc on real P1-built straight-line IR.
/// (Branching IR needs the deferred MachFunc block-CFG before P6's single-block
/// model handles it — see the wave-2 contract-gap notes.)
#[test]
fn backend_lowers_and_allocates_straight_line() {
    let bf = bytecode_fn(
        "be",
        vec![Instr::Const(0), Instr::Return],
        vec![BlissVal::from_fixnum(7)],
        0,
        1,
        0,
    );
    let f = build_from_bytecode(&bf).expect("build");

    let mut mf = lower(&f); // P5
    assert!(!mf.insts.is_empty(), "P5 must produce machine instructions");

    allocate(&mut mf); // P6
    assert!(!mf.allocation.is_empty(), "P6 must assign a location to each vreg");
}
