// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use egcl_compiler::t2::{build, ir::AuxData, verify};
use egcl_rt::Collector;
use native_transfer_entry::root_publication;
use std::cell::Cell;

thread_local! {
    static OWNER: Cell<usize> = const { Cell::new(0) };
    static STATE: Cell<u32> = const { Cell::new(0) };
    static ROOTS: Cell<Option<[*const EgclVal; 3]>> = const { Cell::new(None) };
    static MOVED: Cell<bool> = const { Cell::new(false) };
    static VISITS: Cell<usize> = const { Cell::new(0) };
}

fn observe() {
    if !root_publication::published_frame_has_owner_for_test(OWNER.with(Cell::get)) {
        return;
    }
    VISITS.with(|n| n.set(n.get() + 1));
    if MOVED.with(Cell::get) {
        return;
    }
    let Some(roots) = ROOTS.with(Cell::get) else {
        return;
    };
    // Wait until both nested bindings have been installed. The old global
    // value is now also retained in the native activation's binding ledger.
    if egcl_rt::symbols::symbol_value(STATE.with(Cell::get)) != Some(unsafe { *roots[2] }) {
        return;
    }
    native_transfer_entry::local_exits::reject_corrupt_catch_for_test();
    let before = roots.map(|p| unsafe { (*p).to_raw() });
    egcl_rt::collect_t0_minor().unwrap();
    for (p, old) in roots.into_iter().zip(before) {
        assert_ne!(
            unsafe { (*p).to_raw() },
            old,
            "fixture payload must actually relocate"
        );
    }
    assert_eq!(
        egcl_rt::symbols::symbol_value(STATE.with(Cell::get)),
        Some(unsafe { *roots[2] })
    );
    MOVED.with(|moved| moved.set(true));
}

struct Recording;
impl Drop for Recording {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(None));
        ROOTS.with(|roots| roots.set(None));
        OWNER.with(|owner| owner.set(0));
    }
}

fn fixture_body(env: &Env, name: &str, source: &str) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(
        params = reader::read_from_string("(marker outer inner tag throwp number)")
            .unwrap()
            .0
    );
    egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
    Arc::new(compile_function(name, *params, *forms, env, false, false).expect(source))
}

fn return_source(values: &str) -> String {
    format!(
        "((declare (special *native-local-state*))
      (catch tag
        (let ((*native-local-state* outer))
          (multiple-value-bind (a b c)
              (block done
                (catch tag
                  (let ((*native-local-state* inner))
                    (if throwp (throw tag {values}) (return-from done {values})))))
            (throw tag (cons (+ (car number) 1) (list a b c *native-local-state*)))))))"
    )
}

fn owner(code: &NativeCode) -> usize {
    let NativeCodeStorage::Mapped(native) = &code._storage else {
        panic!("mapped native owner")
    };
    Rc::as_ptr(native) as usize
}

#[test]
fn local_exit_ir_retains_original_scope_identity_and_refuses_other_unwinds() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    crate::cli::read_eval_all_env("(setq *native-local-state* nil)", &mut env).unwrap();
    let body = fixture_body(
        &env,
        "LOCAL-EXIT-IR",
        &return_source("(values marker inner outer)"),
    );
    let _root = ActiveBytecodeRoot::new(&body);
    let scopes = egcl_compiler::control_scope::ScopeMap::analyze_function(&body).unwrap();
    let ir = build::build_from_bytecode_for_native_cleanups(&body).unwrap();
    verify::verify(&ir).unwrap();
    let sites: Vec<_> = ir
        .block_order()
        .iter()
        .flat_map(|&b| &ir.block(b).insts)
        .copied()
        .filter(|&i| matches!(ir.inst(i).aux, AuxData::DirectExit { .. }))
        .collect();
    assert_eq!(sites.len(), 1);
    let call = sites[0];
    let AuxData::DirectExit { bcp } = ir.inst(call).aux else {
        unreachable!()
    };
    assert!(matches!(body.code[bcp as usize], Instr::ReturnFrom { .. }));
    let exit = scopes.exit_at(bcp).unwrap();
    assert!(exit.removed.iter().any(|s| matches!(
        s.kind,
        egcl_compiler::control_scope::ScopeKind::Catch { .. }
    )));
    assert!(exit.removed.iter().any(|s| matches!(
        s.kind,
        egcl_compiler::control_scope::ScopeKind::SpecialBinding { .. }
    )));
    assert_eq!(
        ir.frame_states
            .get(ir.inst(call).frame_state.unwrap())
            .scopes
            .last()
            .unwrap()
            .bcp,
        bcp
    );
    let cold = ir.terminator(ir.inst(call).targets[1].block).unwrap();
    assert!(
        matches!(&ir.inst(cold).aux, AuxData::TransferSite { origin_bcp, scopes: old }
        if *origin_bcp == bcp && old == scopes.before(bcp).unwrap())
    );
    let mut wrong = ir.clone();
    wrong.inst_mut(call).aux = AuxData::DirectExit { bcp: bcp + 1 };
    assert!(
        verify::verify(&wrong).is_err(),
        "retained original BCP is mandatory"
    );
    assert!(
        build::build_from_bytecode_for_transfers(&body).is_err(),
        "legacy ABI remains fail-closed"
    );

    for source in [
        "((block done (unwind-protect (return-from done marker) nil)))",
        "((block done (handler-bind ((error (lambda (e) (declare (ignore e)) nil))) (return-from done marker))))",
        "((block done (handler-case (return-from done marker) (error () nil))))",
        "((block done (restart-case (return-from done marker) (retry () nil))))",
        "((block done (let ((captured marker)) (flet ((escape () captured)) (return-from done #'escape)))))",
    ] {
        let unsupported = fixture_body(&env, "UNSUPPORTED-LOCAL-EXIT", source);
        let _unsupported_root = ActiveBytecodeRoot::new(&unsupported);
        assert!(
            build::build_from_bytecode_for_native_cleanups(&unsupported).is_err(),
            "{source}"
        );
    }
}

#[test]
fn mapped_local_return_restores_catches_bindings_and_values_after_actual_gc_and_deopt() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    egcl_rt::rooted!(env = Env::new(false));
    crate::cli::read_eval_all_env("(setq *native-local-state* nil)", &mut env).unwrap();
    let state = resolve_sym("*NATIVE-LOCAL-STATE*")
        .unwrap()
        .as_symbol_index();
    let _mapped = native_transfer_entry::MappedOnlyForTest::new();
    for tier2 in [false, true] {
        for (case, expression) in ["(values)", "(values marker)", "(values marker inner outer)"]
            .into_iter()
            .enumerate()
        {
            let source = return_source(expression);
            let name = format!("NATIVE-LOCAL-RETURN-{tier2}-{case}");
            let (symbol, code) = if tier2 {
                native_transfer_tests::ordinary_t2::install(
                    &name,
                    "(marker outer inner tag throwp number)",
                    &source,
                    &mut env,
                )
            } else {
                let body = fixture_body(&env, &name, &source);
                let symbol = egcl_rt::symbols::intern(&name);
                registry_put(symbol, Arc::clone(&body));
                (
                    symbol,
                    native_transfer_entry::install_t1(symbol, body, false).expect("real mapped T1"),
                )
            };
            assert_eq!(code.is_t2, tier2);
            // Initialize entry/call storage before creating the moving payload.
            egcl_rt::rooted!(warm_number = crate::cli::arena_cons(EgclVal::from_fixnum(7), NIL));
            run_native(
                &code,
                symbol,
                &[NIL, NIL, NIL, EgclVal::from_fixnum(991), NIL, *warm_number],
                &mut env,
            )
            .unwrap();
            // Empty the nursery, then create dead prefix so compaction cannot
            // leave the fixture at the same address by coincidental reuse.
            egcl_rt::HeapCollector::new().minor_gc().unwrap();
            for _ in 0..32 {
                let _ = crate::cli::arena_cons(NIL, NIL);
            }
            egcl_rt::rooted!(marker = crate::cli::arena_cons(EgclVal::from_fixnum(701), NIL));
            egcl_rt::rooted!(outer = crate::cli::arena_cons(EgclVal::from_fixnum(702), NIL));
            egcl_rt::rooted!(inner = crate::cli::arena_cons(EgclVal::from_fixnum(703), NIL));
            egcl_rt::rooted!(number_cell = crate::cli::arena_cons(EgclVal::from_fixnum(7), NIL));
            egcl_rt::symbols::set_symbol_value(state, *marker);
            OWNER.with(|v| v.set(owner(&code)));
            STATE.with(|v| v.set(state));
            ROOTS.with(|v| v.set(Some([&*marker, &*outer, &*inner])));
            MOVED.with(|v| v.set(false));
            VISITS.with(|v| v.set(0));
            let _recording = Recording;
            root_publication::PROFILE_OBSERVE.with(|hook| hook.set(Some(observe)));
            for throwing in [false, true] {
                native_transfer_entry::local_exits::COMPLETED.with(|v| v.set(0));
                let before_deopts = deopt_count();
                egcl_rt::rooted!(
                    answer = run_native(
                        &code,
                        symbol,
                        &[
                            *marker,
                            *outer,
                            *inner,
                            EgclVal::from_fixnum(991),
                            if throwing { T } else { NIL },
                            *number_cell
                        ],
                        &mut env
                    )
                    .unwrap()
                );
                let fields = list_to_vec(*answer);
                let expected = match case {
                    0 => [NIL, NIL, NIL],
                    1 => [*marker, NIL, NIL],
                    _ => [*marker, *inner, *outer],
                };
                assert_eq!(
                    fields,
                    vec![
                        EgclVal::from_fixnum(8),
                        expected[0],
                        expected[1],
                        expected[2],
                        *outer
                    ]
                );
                assert_eq!(egcl_rt::symbols::symbol_value(state), Some(*marker));
                assert!(env.catch_stack.is_empty(), "no stale catch survives return");
                assert_eq!(
                    native_transfer_entry::local_exits::COMPLETED.with(Cell::get),
                    usize::from(!throwing)
                );
                assert_eq!(
                    deopt_count(),
                    before_deopts,
                    "valid execution must remain native"
                );
            }
            assert!(
                MOVED.with(Cell::get),
                "published native payloads must actually move"
            );
            assert!(
                VISITS.with(Cell::get) > 0,
                "physical owner publication required"
            );
            if tier2 {
                // The arithmetic operand comes from a heap read after the
                // committed exit, so its numeric guard cannot move to entry.
                // Recovery must not execute that exit twice or revive its catch.
                egcl_rt::rooted!(wide = egcl_rt::bignum::alloc_bignum_cli(1, &[0, 1 << 36]));
                egcl_rt::rooted!(expected = egcl_rt::bignum::alloc_bignum_cli(1, &[1, 1 << 36]));
                egcl_rt::rooted!(wide_cell = crate::cli::arena_cons(*wide, NIL));
                let before = deopt_count();
                native_transfer_entry::local_exits::COMPLETED.with(|v| v.set(0));
                egcl_rt::rooted!(
                    answer = run_native(
                        &code,
                        symbol,
                        &[
                            *marker,
                            *outer,
                            *inner,
                            EgclVal::from_fixnum(991),
                            NIL,
                            *wide_cell
                        ],
                        &mut env
                    )
                    .unwrap()
                );
                let fields = list_to_vec(*answer);
                assert!(crate::cli::eql_values(fields[0], *expected));
                assert!(
                    deopt_count() > before,
                    "the post-exit guard must really deopt"
                );
                assert_eq!(
                    native_transfer_entry::local_exits::COMPLETED.with(Cell::get),
                    1,
                    "committed exit cannot replay on deopt"
                );
                assert_eq!(egcl_rt::symbols::symbol_value(state), Some(*marker));
                assert!(env.catch_stack.is_empty());
            }
        }
    }
}

#[test]
fn mapped_local_go_removes_inner_catch_and_special_without_losing_outer_scopes() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    egcl_rt::rooted!(env = Env::new(false));
    crate::cli::read_eval_all_env("(setq *native-local-state* nil)", &mut env).unwrap();
    let _mapped = native_transfer_entry::MappedOnlyForTest::new();
    let source = "((declare (special *native-local-state*))
      (catch tag
        (let ((*native-local-state* outer) (answer nil))
          (tagbody
            (catch tag
              (let ((*native-local-state* inner))
                (setq answer marker)
                (go done)))
            (setq answer nil)
            done)
          (throw tag (list answer *native-local-state*)))))";
    for tier2 in [false, true] {
        let name = format!("NATIVE-LOCAL-GO-{tier2}");
        let (symbol, code) = if tier2 {
            native_transfer_tests::ordinary_t2::install(
                &name,
                "(marker outer inner tag throwp number)",
                source,
                &mut env,
            )
        } else {
            let body = fixture_body(&env, &name, source);
            let symbol = egcl_rt::symbols::intern(&name);
            registry_put(symbol, Arc::clone(&body));
            (
                symbol,
                native_transfer_entry::install_t1(symbol, body, false).expect("native GO"),
            )
        };
        egcl_rt::rooted!(marker = crate::cli::arena_cons(EgclVal::from_fixnum(811), NIL));
        egcl_rt::rooted!(outer = crate::cli::arena_cons(EgclVal::from_fixnum(812), NIL));
        egcl_rt::rooted!(inner = crate::cli::arena_cons(EgclVal::from_fixnum(813), NIL));
        OWNER.with(|v| v.set(owner(&code)));
        ROOTS.with(|v| v.set(None));
        VISITS.with(|v| v.set(0));
        let _recording = Recording;
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(Some(observe)));
        native_transfer_entry::local_exits::COMPLETED.with(|v| v.set(0));
        let before = deopt_count();
        egcl_rt::rooted!(
            answer = run_native(
                &code,
                symbol,
                &[
                    *marker,
                    *outer,
                    *inner,
                    EgclVal::from_fixnum(992),
                    NIL,
                    EgclVal::from_fixnum(7)
                ],
                &mut env
            )
            .unwrap()
        );
        assert_eq!(list_to_vec(*answer), vec![*marker, *outer]);
        assert!(
            VISITS.with(Cell::get) > 0,
            "actual native owner must publish"
        );
        assert_eq!(
            native_transfer_entry::local_exits::COMPLETED.with(Cell::get),
            1
        );
        assert!(env.catch_stack.is_empty());
        assert_eq!(deopt_count(), before);
    }
}
