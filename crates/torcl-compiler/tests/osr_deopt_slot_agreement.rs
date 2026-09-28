//! bliss-ht4 — the OSR entry map and the deopt frame state must describe one
//! safepoint identically.
//!
//! OSR is deoptimisation in reverse: the deopt stack map EXPORTS live
//! interpreter slots out of optimised code on a failed guard, and the OSR entry
//! map IMPORTS the same slots back in at a loop header. If the two compute
//! liveness or per-slot representation by separate code paths they will drift —
//! one calling a slot `UnboxedFixnum` while the other calls it `Tagged` — and
//! the result is a miscompile that every non-OSR test passes, because the
//! export path runs constantly and the import path only on a loop that actually
//! OSRs.
//!
//! These tests pin the agreement to the shared producer, `t2::slot_map`.

use torcl_compiler::osr::ConversionKind;
use torcl_compiler::t2::build::build_from_bytecode;
use torcl_compiler::t2::deopt::{self, SlotDescriptor};
use torcl_compiler::t2::ir::ValueRepresentation;
use torcl_compiler::t2::mach::{Location, StackSlot};
use torcl_compiler::t2::slot_map;
use torcl_rt::bytecode::{BytecodeFunction, Instr};
use torcl_rt::value::TorclVal;

fn bf(name: &str, code: Vec<Instr>, constants: Vec<TorclVal>, n_locals: u16) -> BytecodeFunction {
    BytecodeFunction {
        code,
        constants,
        load_time_values: vec![],
        handler_cases: vec![],
        handler_binds: vec![],
        names: vec![],
        restart_cases: vec![],
        nested_functions: vec![],
        param_layout: vec![],
        param_types: vec![],
        has_env: false,
        n_locals,
        max_stack: 2,
        arity: 0,
        name: name.to_string(),
        params_form: torcl_rt::value::NIL,
        min_args: 0,
        max_args: Some(0),
        variadic: false,
    }
}

/// A counted loop whose back-edge is `back`. Both an explicit tagbody `Go` and
/// an ordinary `Br` must be recognised as OSR safepoints: the condition is that
/// the *target* has an empty operand stack, not which instruction jumps to it
/// (bliss-izt.4).
///
/// Establish a TAGBODY before the loop and retire it on normal exit. Even the
/// synthetic GO fixture must have an active lexical target.
fn counted_loop(back: Instr) -> BytecodeFunction {
    bf(
        "osr_loop",
        vec![
            Instr::Const(0),
            Instr::StoreLocal(0),
            Instr::PushTag {
                tagbody_id: 0,
                sp_restore: 0,
            },
            Instr::LoadLocal(0),
            Instr::BrIfFalse(8),
            Instr::Const(1),
            Instr::StoreLocal(0),
            back,
            Instr::PopHandler,
            Instr::LoadLocal(0),
            Instr::Return,
        ],
        vec![TorclVal::from_fixnum(3), TorclVal::from_fixnum(0)],
        1,
    )
}

/// The two back-edge shapes a counted loop can have: a tagbody `Go` and the
/// ordinary `Br` that DO/DOTIMES/DOLIST lower to.
fn back_edges() -> Vec<(&'static str, Instr)> {
    vec![
        (
            "Go",
            Instr::Go {
                tagbody_id: 0,
                target_bcp: 3,
            },
        ),
        ("Br", Instr::Br(3)),
    ]
}

/// An ordinary backward `Br` must yield an OSR safepoint just like a tagbody
/// `Go`. Before bliss-izt.4 only `Go` was scanned for, so DO/DOTIMES/DOLIST
/// loops — which lower to `Br` — got no OSR entry at all and could never be
/// entered at T2 once already running.
#[test]
fn every_backward_branch_shape_yields_an_osr_safepoint() {
    for (label, back) in back_edges() {
        let f = build_from_bytecode(&counted_loop(back)).expect("builds");
        assert_eq!(
            f.osr_entries.len(),
            1,
            "a counted loop with a backward {label} must produce exactly one              OSR safepoint at the loop header"
        );
        assert_eq!(
            f.osr_entries[0].bcp, 3,
            "the OSR safepoint belongs at the loop header (bcp 3), not at the              back-edge, for a backward {label}"
        );
    }
}

/// The bead's explicit ask: for a given safepoint, the OSR import descriptor and
/// the deopt export descriptor agree on slot count and per-slot representation.
#[test]
fn osr_import_and_deopt_export_agree_on_slot_count_and_representation() {
    for (label, back) in back_edges() {
        let f = build_from_bytecode(&counted_loop(back)).expect("builds");
        assert!(
            !f.osr_entries.is_empty(),
            "the backward {label} must be captured as an OSR safepoint, otherwise \
         this test proves nothing"
        );

        for osr in &f.osr_entries {
            let fs = f.frame_states.get(osr.frame_state);
            let scope = fs.scopes.first().expect("OSR frame state has a scope");

            // ── Import side: what OSR transfers in. ──
            let imports = slot_map::slot_specs(scope, fs);

            // ── Export side: what deopt writes back out. Every value is allocated
            //    to a distinct stack location so lowering can succeed; the location
            //    itself is irrelevant here, only the frame SHAPE is under test. ──
            let exported = deopt::lower_one(0, fs, &|v| Some(Location::Stack(StackSlot(v.0))))
                .expect("frame state lowers");
            let exported_scope = exported.scopes.first().expect("one scope");

            // (1) Same number of slots.
            assert_eq!(
                imports.len(),
                exported_scope.slots.len(),
                "OSR import and deopt export disagree on slot COUNT at bcp {}",
                osr.bcp
            );

            // (2) Same locals/stack split, so slot i means the same thing to both.
            assert_eq!(
                imports.iter().filter(|s| s.is_local).count(),
                exported_scope.num_locals,
                "OSR import and deopt export disagree on the locals/stack split at \
             bcp {}",
                osr.bcp
            );

            // (3) Per-slot representation: the import conversion must be the exact
            //     inverse of the export rebox, slot by slot.
            for (i, spec) in imports.iter().enumerate() {
                let import = ConversionKind::for_repr(spec.repr);
                let export = match &exported_scope.slots[i] {
                    SlotDescriptor::InLocation(_, rebox) => Some(*rebox),
                    // Const/Unbound/Remat slots are materialised, not transferred
                    // through a machine location, so they have no rebox to invert.
                    _ => None,
                };
                if let Some(rebox) = export {
                    assert_eq!(
                        rebox,
                        deopt::Rebox::for_repr(spec.repr),
                        "slot {i} at bcp {}: deopt export rebox does not match the \
                     representation the OSR import would use",
                        osr.bcp
                    );
                    assert!(
                        import.is_some(),
                        "slot {i} at bcp {}: deopt can export representation {:?} \
                     but OSR has no inverse import conversion — the OSR entry \
                     must be declined, not emitted",
                        osr.bcp,
                        spec.repr
                    );
                }
            }
        }
    }
}

/// Today the builder records every frame-state slot as `Tagged`, which is what
/// makes the plain word-move OSR stub in `emit` correct. This test states that
/// dependency out loud: if a representation-changing pass (unboxing) ever runs
/// before OSR emission, this fails and points at the stub that must learn to
/// convert — rather than the stub silently transferring a tagged word into a
/// register the loop reads as a raw integer.
#[test]
fn osr_safepoint_slots_are_all_tagged_so_the_word_move_stub_is_valid() {
    for (_, back) in back_edges() {
        let f = build_from_bytecode(&counted_loop(back)).expect("builds");
        for osr in &f.osr_entries {
            let fs = f.frame_states.get(osr.frame_state);
            let scope = fs.scopes.first().expect("scope");
            for spec in slot_map::slot_specs(scope, fs) {
                assert_eq!(
                    spec.repr,
                    ValueRepresentation::Tagged,
                    "slot {} at bcp {} is {:?}; the emit OSR stub moves a raw word \
                 out of the interpreter frame and can only do that for Tagged \
                 slots. Teach the stub the D4.09 conversion (or keep declining \
                 the entry) before allowing unboxed OSR slots.",
                    spec.index,
                    osr.bcp,
                    spec.repr
                );
                assert_eq!(
                    ConversionKind::for_repr(spec.repr),
                    Some(ConversionKind::None),
                    "a Tagged slot must need no import conversion"
                );
            }
        }
    }
}

#[test]
fn builder_rejects_go_without_an_active_tagbody() {
    use torcl_compiler::control_scope::ScopeError;
    use torcl_compiler::t2::build::BuildError;
    let mut body = counted_loop(Instr::Go {
        tagbody_id: 0,
        target_bcp: 3,
    });
    body.code[2] = Instr::Br(3);
    assert!(matches!(
        build_from_bytecode(&body),
        Err(BuildError::InvalidScopes(
            ScopeError::InactiveTarget { .. } | ScopeError::EmptyPop { .. }
        ))
    ));
}
