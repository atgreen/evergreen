use super::*;
use crate::t2::frame_state::{FrameScope, FrameState, RematOp, RematRecipe};
use crate::t2::ir::{Block, BlockCall, IRType, InstData, InstFlags, TypeBits};

fn data(opcode: Opcode) -> InstData {
    InstData {
        opcode,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    }
}

fn state(f: &mut Function, values: &[Value]) -> FrameStateId {
    f.frame_states.add(FrameState {
        scopes: vec![FrameScope {
            function: 1,
            bcp: 0,
            locals: values.iter().map(|&value| source(value)).collect(),
            stack: vec![],
        }],
        remat: vec![],
    })
}

fn source(value: Value) -> ValueSource {
    ValueSource::Value {
        value,
        repr: ValueRepresentation::Tagged,
    }
}

fn load(f: &mut Function, b: Block, name: u16) -> Value {
    let fs = state(f, &[]);
    let mut d = data(Opcode::EnvironmentValue);
    d.aux = AuxData::EnvironmentName(name);
    d.flags = InstFlags {
        call: true,
        effectful: true,
        safepoint: true,
        ..Default::default()
    };
    d.frame_state = Some(fs);
    f.push_inst(b, d, &[(IRType::TOP, ValueRepresentation::Tagged)])
        .1[0]
}

fn store(f: &mut Function, b: Block, name: u16, value: Value) {
    let fs = state(f, &[value]);
    let mut d = data(Opcode::SetEnvironmentValue);
    d.aux = AuxData::EnvironmentName(name);
    d.args = vec![value];
    d.flags = InstFlags {
        call: true,
        effectful: true,
        safepoint: true,
        ..Default::default()
    };
    d.frame_state = Some(fs);
    f.push_inst(b, d, &[]);
}

fn finish(f: &mut Function, b: Block, value: Value) {
    let mut d = data(Opcode::Return);
    d.args = vec![value];
    f.set_terminator(b, d);
}

fn count(f: &Function, opcode: Opcode) -> usize {
    f.block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .filter(|&&i| f.inst(i).opcode == opcode)
        .count()
}

#[test]
fn lexical_forwarding_four_accesses_become_first_read_and_store() {
    let mut f = Function::new("lexical-update");
    let b = f.entry();
    let old = load(&mut f, b, 0);
    let mut one = data(Opcode::ConstFixnum);
    one.aux = AuxData::FixnumImm(1);
    let one = f
        .push_inst(
            b,
            one,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0];
    let old_state = state(&mut f, &[old]);
    let mut check = data(Opcode::Guard);
    check.args = vec![old];
    check.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
    check.flags.guard = true;
    check.flags.effectful = true;
    check.frame_state = Some(old_state);
    let checked = f
        .push_inst(
            b,
            check,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0];
    let mut add = data(Opcode::FixnumAdd);
    add.args = vec![checked, one];
    add.frame_state = Some(old_state);
    add.flags.guard = true;
    add.flags.effectful = true;
    let updated = f
        .push_inst(b, add, &[(IRType::TOP, ValueRepresentation::Tagged)])
        .1[0];
    store(&mut f, b, 0, updated);
    let clear = f.push_inst(b, data(Opcode::ClearMv), &[]).0;
    let discarded = load(&mut f, b, 0);
    let answer = load(&mut f, b, 0);
    let fs = state(&mut f, &[answer, discarded]);
    f.frame_states.get_mut(fs).remat.push(RematRecipe {
        op: RematOp::FixnumAdd,
        inputs: vec![
            source(answer),
            ValueSource::Const(egcl_rt::value::EgclVal::from_fixnum(0)),
        ],
        result_repr: ValueRepresentation::Tagged,
    });
    let mut guard = data(Opcode::Guard);
    guard.aux = AuxData::TypeTag(IRType::of(TypeBits::FIXNUM));
    guard.args = vec![answer];
    guard.frame_state = Some(fs);
    guard.flags.guard = true;
    // The test inspects recovery substitutions; no fabricated type proof is emitted.
    f.push_inst(
        b,
        guard,
        &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
    );
    finish(&mut f, b, answer);
    assert!(crate::t2::verify::verify(&f).is_ok());
    assert_eq!(forward(&mut f), 2);
    assert!(crate::t2::verify::verify(&f).is_ok());
    assert_eq!(count(&f, Opcode::EnvironmentValue), 1);
    assert_eq!(count(&f, Opcode::SetEnvironmentValue), 1);
    assert!(
        f.block(b).insts.contains(&clear),
        "MV reset remains in its original position"
    );
    assert_eq!(f.inst(f.terminator(b).unwrap()).args, vec![updated]);
    for source in f.frame_states.get(fs).scopes[0]
        .locals
        .iter()
        .chain(&f.frame_states.get(fs).remat[0].inputs[..1])
    {
        assert!(matches!(source, ValueSource::Value { value, .. } if *value == updated));
    }
}

#[test]
fn lexical_forwarding_store_alone_never_establishes_binding_existence() {
    let mut f = Function::new("unbound-after-store");
    let b = f.entry();
    let value = f.add_block_param(b, IRType::TOP, ValueRepresentation::Tagged);
    store(&mut f, b, 0, value);
    let answer = load(&mut f, b, 0);
    finish(&mut f, b, answer);
    let before = format!("{f:?}");
    assert_eq!(forward(&mut f), 0);
    assert_eq!(
        format!("{f:?}"),
        before,
        "first possible UNBOUND and completed store retained"
    );
}

#[test]
fn lexical_forwarding_calls_polls_and_scope_effects_are_barriers() {
    // The same test models a callee that mutates the captured binding, an
    // explicit cooperation point, and a scope/runtime operation. Flags alone
    // cannot make an unknown opcode transparent.
    for opcode in [
        Opcode::Call,
        Opcode::Invoke,
        Opcode::NativePoll,
        Opcode::CleanupSave,
        Opcode::SymbolValue,
        Opcode::SetSymbolValue,
    ] {
        let mut f = Function::new("lexical-barrier");
        let b = f.entry();
        let first = load(&mut f, b, 0);
        store(&mut f, b, 0, first);
        let mut barrier = data(opcode);
        barrier.flags.effectful = true;
        f.push_inst(b, barrier, &[]);
        let answer = load(&mut f, b, 0);
        finish(&mut f, b, answer);
        assert_eq!(forward(&mut f), 0, "{opcode:?}");
        assert_eq!(count(&f, Opcode::EnvironmentValue), 2);
    }
}

#[test]
fn lexical_forwarding_other_store_might_alias_and_kills_available_value() {
    let mut f = Function::new("different-index");
    let b = f.entry();
    let first = load(&mut f, b, 0);
    store(&mut f, b, 1, first);
    let answer = load(&mut f, b, 0);
    finish(&mut f, b, answer);
    assert_eq!(forward(&mut f), 0);
}

#[test]
fn lexical_forwarding_does_not_cross_branches_or_loop_edges() {
    let mut f = Function::new("lexical-block-boundary");
    let entry = f.entry();
    let later = f.make_block();
    let first = load(&mut f, entry, 0);
    let mut jump = data(Opcode::Jump);
    jump.targets = vec![BlockCall {
        block: later,
        args: vec![],
    }];
    f.set_terminator(entry, jump);
    let answer = load(&mut f, later, 0);
    store(&mut f, later, 0, first);
    finish(&mut f, later, answer);
    assert_eq!(forward(&mut f), 0);
}

#[test]
fn lexical_forwarding_keeps_first_load_and_forwards_only_later_reads() {
    let mut f = Function::new("repeated-read");
    let b = f.entry();
    let first = load(&mut f, b, 0);
    let answer = load(&mut f, b, 0);
    finish(&mut f, b, answer);
    assert_eq!(forward(&mut f), 1);
    assert_eq!(count(&f, Opcode::EnvironmentValue), 1);
    assert_eq!(f.inst(f.terminator(b).unwrap()).args, vec![first]);
}

#[test]
#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
fn lexical_forwarding_declined_ordinary_lowering_preserves_original_function() {
    let mut f = Function::new("declined-capture");
    let b = f.entry();
    let _first = load(&mut f, b, 0);
    let answer = load(&mut f, b, 0);
    finish(&mut f, b, answer);
    let mut add = data(Opcode::GenericAdd);
    add.args = vec![answer, answer];
    add.frame_state = Some(state(&mut f, &[answer]));
    add.flags.call = true;
    let term = f.block_mut(b).insts.pop().unwrap();
    f.push_inst(b, add, &[(IRType::TOP, ValueRepresentation::Tagged)]);
    f.block_mut(b).insts.push(term);
    let before = format!("{f:?}");
    // No private adapter is available. Whether this main revision rejects at
    // semantic lowering or at its verifier, the mapped source must stay intact.
    let result = crate::t2::emit::lower_ordinary_primitives(&mut f, |_| None);
    assert!(result.is_err());
    assert_eq!(format!("{f:?}"), before);
}

#[test]
fn lexical_forwarding_declines_alternate_osr_entry() {
    let mut f = Function::new("osr-read");
    let b = f.entry();
    let _first = load(&mut f, b, 0);
    let answer = load(&mut f, b, 0);
    let fs = state(&mut f, &[]);
    f.osr_entries.push(crate::t2::ir::OsrEntry {
        control_scopes: vec![],
        bcp: 0,
        block: b,
        frame_state: fs,
        checks: vec![],
    });
    finish(&mut f, b, answer);
    let before = format!("{f:?}");
    assert_eq!(forward(&mut f), 0);
    assert_eq!(format!("{f:?}"), before);
}

#[test]
fn lexical_forwarding_actual_special_binding_identity_is_a_barrier() {
    for enter in [false, true] {
        let mut f = Function::new("special-scope");
        let b = f.entry();
        let _first = load(&mut f, b, 0);
        let mut special = data(Opcode::Invoke);
        special.aux = AuxData::SpecialBinding { bcp: 0, enter };
        // Even malformed missing flags cannot turn a binder into a pure op.
        f.push_inst(b, special, &[]);
        let answer = load(&mut f, b, 0);
        finish(&mut f, b, answer);
        assert_eq!(forward(&mut f), 0);
    }
}

/// Exact bytecode shape captured from the measured source `(lambda (k)
/// (incf seed) (+ k seed))`, including SETF's primary binder and discarded read.
fn captured_update_bytecode() -> egcl_rt::bytecode::BytecodeFunction {
    use egcl_rt::bytecode::{BytecodeFunction, Instr, VarLoc};
    let plus = egcl_rt::symbols::intern("+");
    BytecodeFunction {
        code: vec![
            Instr::Const(0),
            Instr::StoreLocal(1),
            Instr::ClearMv,
            Instr::LoadEnvVar(0),
            Instr::LoadLocal(1),
            Instr::CallNamed {
                sym: plus,
                nargs: 2,
            },
            Instr::TakeValuesToLocals {
                nvars: 1,
                slot_base: 2,
            },
            Instr::LoadLocal(2),
            Instr::StoreEnvVar(0),
            Instr::ClearMv,
            Instr::LoadEnvVar(0),
            Instr::Pop,
            Instr::LoadLocal(0),
            Instr::LoadEnvVar(0),
            Instr::CallNamed {
                sym: plus,
                nargs: 2,
            },
            Instr::Return,
        ],
        constants: vec![egcl_rt::value::EgclVal::from_fixnum(1)],
        self_call_sites: vec![],
        notinline_sites: vec![],
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec!["SEED".into()],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![("K".into(), VarLoc::Slot(0))],
        param_types: vec![],
        has_env: true,
        n_locals: 3,
        max_stack: 2,
        arity: 1,
        name: "LEXICAL-UPDATE-BUILDER".into(),
        params_form: egcl_rt::value::NIL,
        min_args: 1,
        max_args: Some(1),
        variadic: false,
        binder_cache: Default::default(),
    }
}

#[test]
fn lexical_forwarding_real_bytecode_after_production_numeric_passes_is_four_to_two() {
    let body = captured_update_bytecode();
    let symbol = egcl_rt::symbols::intern("LEXICAL-UPDATE-BUILDER");
    let mut f = crate::t2::build::build_for_transfer_optimization(&body, symbol).unwrap();
    assert_eq!(
        crate::t2::speculate::speculate(&mut f, &|_| Some(crate::t2::speculate::SpecType::Fixnum)),
        2
    );
    // The semantic transfer builder matches the production background worker;
    // the native-scope builder has already legalized these operations.
    // Same ordering as the production background compiler before ordinary
    // environment lowering. Do not teach this pass purity of an opaque call.
    let mut passes = crate::t2::pass::PassManager::new();
    passes.add(Box::new(crate::t2::opt_fold::ConstFold));
    passes.add(Box::new(crate::t2::opt_gvn::Gvn));
    passes.add(Box::new(crate::t2::opt_guard::GuardElim));
    passes.add(Box::new(crate::t2::opt_clear_mv::ClearMvElim));
    passes.add(Box::new(crate::t2::opt_dce::Dce));
    passes.run(&mut f);
    crate::t2::verify::verify(&f).unwrap();
    assert_eq!(count(&f, Opcode::EnvironmentValue), 3);
    assert_eq!(count(&f, Opcode::SetEnvironmentValue), 1);
    let guards = f
        .block_order()
        .iter()
        .flat_map(|&b| &f.block(b).insts)
        .filter(|&&i| f.inst(i).flags.guard)
        .count();
    assert_eq!(
        forward(&mut f),
        2,
        "actual pass ordering must keep existence through guarded pure math"
    );
    assert_eq!(count(&f, Opcode::EnvironmentValue), 1);
    assert_eq!(count(&f, Opcode::SetEnvironmentValue), 1);
    assert_eq!(
        f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .filter(|&&i| f.inst(i).flags.guard)
            .count(),
        guards
    );
    crate::t2::verify::verify(&f).unwrap();
}

#[test]
fn lexical_forwarding_opaque_source_arithmetic_is_a_real_barrier() {
    let body = captured_update_bytecode();
    let symbol = egcl_rt::symbols::intern("LEXICAL-UPDATE-BUILDER");
    let mut f = crate::t2::build::build_for_transfer_optimization(&body, symbol).unwrap();
    // With no numeric rewrite, the first + is still a potentially allocating
    // arbitrary call. It kills the first-read proof. Only the two final reads
    // can share the later successful read: report three remaining accesses.
    assert_eq!(forward(&mut f), 1);
    assert_eq!(count(&f, Opcode::EnvironmentValue), 2);
    assert_eq!(count(&f, Opcode::SetEnvironmentValue), 1);
    crate::t2::verify::verify(&f).unwrap();
}

#[test]
fn lexical_forwarding_real_intervening_mutation_call_keeps_fresh_read() {
    use egcl_rt::bytecode::Instr;
    let mut body = captured_update_bytecode();
    let callback = egcl_rt::symbols::intern("LEXICAL-MUTATION-CALLBACK");
    body.code.insert(
        9,
        Instr::CallNamed {
            sym: callback,
            nargs: 0,
        },
    );
    body.code.insert(10, Instr::Pop);
    let owner = egcl_rt::symbols::intern("LEXICAL-UPDATE-BUILDER");
    let mut f = crate::t2::build::build_for_transfer_optimization(&body, owner).unwrap();
    assert_eq!(
        crate::t2::speculate::speculate(&mut f, &|_| Some(crate::t2::speculate::SpecType::Fixnum)),
        2
    );
    assert_eq!(
        forward(&mut f),
        1,
        "callback invalidates stored value; only subsequent reads share"
    );
    assert_eq!(count(&f, Opcode::EnvironmentValue), 2);
    assert!(
        f.block_order()
            .iter()
            .flat_map(|&b| &f.block(b).insts)
            .any(|&i| matches!(f.inst(i).aux, AuxData::CallTarget(sym) if sym == callback))
    );
    crate::t2::verify::verify(&f).unwrap();
}

#[test]
fn lexical_forwarding_duplicate_name_indexes_are_not_assumed_disjoint() {
    use egcl_rt::bytecode::Instr;
    let mut body = captured_update_bytecode();
    body.names.push(body.names[0].clone());
    body.code[8] = Instr::StoreEnvVar(1);
    let owner = egcl_rt::symbols::intern("LEXICAL-UPDATE-BUILDER");
    let mut f = crate::t2::build::build_for_transfer_optimization(&body, owner).unwrap();
    crate::t2::speculate::speculate(&mut f, &|_| Some(crate::t2::speculate::SpecType::Fixnum));
    assert_eq!(
        forward(&mut f),
        1,
        "other-index store kills old availability even when its name aliases"
    );
    assert_eq!(count(&f, Opcode::EnvironmentValue), 2);
    crate::t2::verify::verify(&f).unwrap();
}

#[test]
fn lexical_forwarding_dce_keeps_unused_first_read_before_independent_store() {
    let mut f = Function::new("first-read-error-precedes-store");
    let b = f.entry();
    let first = load(&mut f, b, 0);
    let crate::t2::ir::ValueDef::Result {
        inst: first_inst, ..
    } = f.value(first).def
    else {
        unreachable!()
    };
    let first_before = format!("{:?}", f.inst(first_inst));
    let mut constant = data(Opcode::ConstFixnum);
    constant.aux = AuxData::FixnumImm(17);
    let stored = f
        .push_inst(
            b,
            constant,
            &[(IRType::of(TypeBits::FIXNUM), ValueRepresentation::Tagged)],
        )
        .1[0];
    store(&mut f, b, 0, stored);
    let answer = load(&mut f, b, 0);
    finish(&mut f, b, answer);
    crate::t2::verify::verify(&f).unwrap();
    assert_eq!(forward(&mut f), 1);
    assert_eq!(f.inst(f.terminator(b).unwrap()).args, vec![stored]);
    // No SSA consumer or recovery slot needs `first` now. It nevertheless
    // must execute before the store, so an unbound binding still signals
    // without publishing the later write or any following effects.
    let mut passes = crate::t2::pass::PassManager::new();
    passes.add(Box::new(crate::t2::opt_dce::Dce));
    passes.run(&mut f);
    let insts = &f.block(b).insts;
    let first_position = insts.iter().position(|&inst| inst == first_inst).unwrap();
    let store_position = insts
        .iter()
        .position(|&inst| f.inst(inst).opcode == Opcode::SetEnvironmentValue)
        .unwrap();
    assert!(first_position < store_position);
    assert_eq!(
        format!("{:?}", f.inst(first_inst)),
        first_before,
        "retain the original first-read effect/call/safepoint flags and error FrameState"
    );
    assert_eq!(count(&f, Opcode::EnvironmentValue), 1);
    assert_eq!(count(&f, Opcode::SetEnvironmentValue), 1);
    crate::t2::verify::verify(&f).unwrap();
}
