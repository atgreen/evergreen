//! Wave-1 cross-parcel integration: P1 (build) → P2 (verify) → P3 (infer).
//!
//! Each parcel was unit-tested in isolation against hand-built IR. This proves
//! they actually compose: bytecode built by P1 must satisfy P2's verifier and be
//! analysable by P3's inference, all against the frozen Phase-0 contracts.

use bliss_compiler::t2::build::build_from_bytecode;
use bliss_compiler::t2::infer::infer;
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
