// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Acyclic heap captures enter actual ordinary T1 with each instance's callable.

use super::*;

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t1_heap_access_keeps_instances_distinct_and_relocates_live_values() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(start)").unwrap().0);
    egcl_rt::rooted!(
        forms = reader::read_from_string(
            "((let ((total start))
             (lambda (next)
               (let ((old total)) (setq total next) (%force-minor-gc-for-test) old))))"
        )
        .unwrap()
        .0
    );
    let maker = Arc::new(
        compile_function("T1-HEAP-MAKER", *params, *forms, &env, true, false)
            .unwrap_or_else(|| panic!("T1 heap maker: {:?}", last_bail_reason())),
    );
    egcl_rt::rooted!(_maker = ActiveBytecodeRoot::new(&maker));
    let body = Arc::clone(&maker.nested_functions[0]);
    assert!(
        !body.has_env,
        "the maker must rewrite its nested capture to the heap"
    );
    let predicates: [fn(&Instr) -> bool; 3] = [
        |op: &Instr| matches!(op, Instr::ClosureEnv),
        |op: &Instr| matches!(op, Instr::EnvRef { .. }),
        |op: &Instr| matches!(op, Instr::EnvSet { .. }),
    ];
    for predicate in predicates {
        assert!(body.code.iter().any(predicate), "{:?}", body.code);
    }
    egcl_rt::rooted!(
        first = run_with_sym(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(11)],
            NIL,
            u32::MAX,
            &mut env
        )
        .unwrap()
    );
    egcl_rt::rooted!(
        second = run_with_sym(
            Arc::clone(&maker),
            &[EgclVal::from_fixnum(22)],
            NIL,
            u32::MAX,
            &mut env
        )
        .unwrap()
    );
    let symbol = egcl_rt::function::definition_index(*first);
    assert_eq!(
        symbol,
        egcl_rt::function::definition_index(*second),
        "one template owns the code"
    );
    let code = try_promote_to_t1(symbol).expect("T1 heap capture installation");
    assert!(!code.is_t2);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("native transfer representation");
    };
    assert!(mapped.ordinary_entry.is_some() && mapped.native_callable_slot.is_some());
    assert!(mapped.ordinary_root_allowed());
    assert!(
        mapped.mapped_entry().is_none(),
        "no mapped fallback may satisfy the test"
    );
    native_transfer_entry::take_lean_root_entries_for_test();
    for (use_first, next, previous) in [
        (true, 13, 11),
        (false, 24, 22),
        (true, 15, 13),
        (false, 26, 24),
    ] {
        let function = if use_first { *first } else { *second };
        assert_eq!(
            mapped
                .run_callable(function, &[EgclVal::from_fixnum(next)], &mut env)
                .unwrap(),
            EgclVal::from_fixnum(previous)
        );
    }
    assert_eq!(
        native_transfer_entry::take_lean_root_entries_for_test(),
        4,
        "each actual instance entered ordinary T1"
    );

    // The native caller holds the old captured cons across its collecting call.
    // Rooted external copies are address witnesses, never substitutes for its
    // returned native home: require relocation and the exact moved return.
    HeapCollector::new().minor_gc().unwrap();
    // The original instances may already be promoted by compilation and the
    // preceding calls. Make a fresh instance only after that setup, so the
    // collector inside this actual native call must move the callable too.
    egcl_rt::rooted!(
        fresh = heap_env::make_closure(&body, egcl_rt::function::closure_env(*first), &[], &env)
            .unwrap()
    );
    assert!(!egcl_rt::function::is_pinned(*fresh));
    egcl_rt::rooted!(old = arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(next = arena_cons(EgclVal::from_fixnum(72), NIL));
    let environment = egcl_rt::function::closure_env(*fresh);
    let (depth, idx) = body
        .code
        .iter()
        .find_map(|op| match op {
            Instr::EnvRef { depth, idx, .. } => Some((*depth, *idx)),
            _ => None,
        })
        .unwrap();
    assert!(egcl_rt::lexenv::store(environment, depth, idx, *old));
    let before = (old.to_raw(), next.to_raw());
    let callable_before = fresh.to_raw();
    egcl_rt::rooted!(result = mapped.run_callable(*fresh, &[*next], &mut env).unwrap());
    assert_ne!(
        fresh.to_raw(),
        callable_before,
        "the executing callable must relocate"
    );
    assert_ne!(old.to_raw(), before.0, "old captured value must relocate");
    assert_ne!(next.to_raw(), before.1, "new captured value must relocate");
    assert_eq!(*result, *old, "native return uses the relocated home");
    assert_eq!(
        egcl_rt::lexenv::load(egcl_rt::function::closure_env(*fresh), depth, idx),
        Some(*next)
    );
}
