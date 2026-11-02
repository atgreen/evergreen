// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

thread_local! {
    static MOVED: Cell<Option<[(u64, u64); 2]>> = const { Cell::new(None) };
}

unsafe fn collect_in_creator(args: *mut EgclVal, count: usize) {
    assert_eq!(count, 3, "BCP, shared parent and one flat capture");
    let before = unsafe { (args.add(1).read().to_raw(), args.add(2).read().to_raw()) };
    HeapCollector::new().minor_gc().unwrap();
    let after = unsafe { (args.add(1).read().to_raw(), args.add(2).read().to_raw()) };
    MOVED.with(|slot| slot.set(Some([(before.0, after.0), (before.1, after.1)])));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_heap_creators_enter_t1_and_t2_with_moving_parent_and_flat_captures() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for tier in [1, 2] {
        let name = format!("ORDINARY-HEAP-CREATOR-T{tier}");
        let source = "((let ((shared parent))
            (lambda (next) (let ((old (list shared flat))) (setq shared next) old))))";
        crate::cli::read_eval_all_env(
            &format!("(defun {name} (parent flat) (progn . {source}))"),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(params = reader::read_from_string("(parent flat)").unwrap().0);
        egcl_rt::rooted!(forms = reader::read_from_string(source).unwrap().0);
        let body = Arc::new(
            compile_function(&name, *params, *forms, &env, true, false)
                .unwrap_or_else(|| panic!("heap creator: {:?}", last_bail_reason())),
        );
        assert!(
            body.code
                .iter()
                .any(|op| matches!(op, Instr::MakeEnvClosure { nflat: 1, .. }))
        );
        let symbol = egcl_rt::symbols::intern(&name);
        registry_put(symbol, Arc::clone(&body));
        let code = if tier == 1 {
            try_promote_to_t1(symbol).expect("T1 ordinary creator")
        } else {
            ordinary_t2::install_body(symbol, body).1
        };
        assert_eq!(code.is_t2, tier == 2);
        let NativeCodeStorage::Mapped(mapped) = &code._storage else {
            panic!("native transfer owner")
        };
        assert!(mapped.ordinary_entry.is_some() && mapped.ordinary_root_allowed());
        assert!(
            mapped.mapped_entry().is_none(),
            "no fallback may satisfy the creator proof"
        );
        egcl_rt::rooted!(old_callable = egcl_rt::symbols::symbol_function(symbol).unwrap());
        HeapCollector::new().minor_gc().unwrap();
        egcl_rt::rooted!(parent = arena_cons(EgclVal::from_fixnum(11), NIL));
        egcl_rt::rooted!(flat = arena_cons(EgclVal::from_fixnum(22), NIL));
        MOVED.with(|slot| slot.set(None));
        native_transfer_entry::ordinary_heap_creator::OBSERVE
            .with(|slot| slot.set(Some(collect_in_creator)));
        native_transfer_entry::take_lean_root_entries_for_test();
        egcl_rt::rooted!(first = run_native(&code, symbol, &[*parent, *flat], &mut env).unwrap());
        assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 1);
        let moved = MOVED
            .with(Cell::get)
            .expect("the actual ordinary creator adapter ran");
        assert_ne!(
            moved[0].0, moved[0].1,
            "the parent environment must relocate"
        );
        assert_ne!(moved[1].0, moved[1].1, "the flat capture must relocate");
        assert_eq!(flat.to_raw(), moved[1].1);
        egcl_rt::rooted!(second = run_native(&code, symbol, &[*parent, *flat], &mut env).unwrap());
        assert_ne!(*first, *second, "each evaluation creates a fresh instance");
        egcl_rt::rooted!(
            answer = crate::cli::apply_function_impl(*first, &[EgclVal::from_fixnum(33)], &mut env)
                .unwrap()
        );
        assert_eq!(cp(*answer).0, *parent);
        assert_eq!(cp(cp(*answer).1).0, *flat);
        egcl_rt::rooted!(
            answer = crate::cli::apply_function_impl(*first, &[NIL], &mut env).unwrap()
        );
        assert_eq!(cp(*answer).0, EgclVal::from_fixnum(33));
        egcl_rt::rooted!(
            answer = crate::cli::apply_function_impl(*second, &[NIL], &mut env).unwrap()
        );
        assert_eq!(
            cp(*answer).0,
            *parent,
            "instances keep separate shared slots"
        );
        // Replace the public name with the same creator shape but a different
        // nested template. Calling retained code must still choose its old
        // template from the physical owner, never from that mutable name.
        crate::cli::read_eval_all_env(
            &format!(
                "(defun {name} (parent flat) (let ((shared parent))
                (lambda (next) (let ((old (list :replacement flat)))
                    (setq shared next) old))))"
            ),
            &mut env,
        )
        .unwrap();
        egcl_rt::rooted!(replacement = egcl_rt::symbols::symbol_function(symbol).unwrap());
        assert_ne!(*old_callable, *replacement);
        native_transfer_entry::take_lean_root_entries_for_test();
        egcl_rt::rooted!(
            retained = mapped
                .run_callable(*old_callable, &[*parent, *flat], &mut env)
                .unwrap()
        );
        assert_eq!(
            native_transfer_entry::take_lean_root_entries_for_test(),
            1,
            "retained old creator must execute ordinary native code"
        );
        egcl_rt::rooted!(
            answer = crate::cli::apply_function_impl(*retained, &[NIL], &mut env).unwrap()
        );
        assert_eq!(
            cp(*answer).0,
            *parent,
            "retained owner selects the old nested template"
        );
        egcl_rt::rooted!(
            current =
                crate::cli::apply_function_impl(*replacement, &[*parent, *flat], &mut env).unwrap()
        );
        egcl_rt::rooted!(
            answer = crate::cli::apply_function_impl(*current, &[NIL], &mut env).unwrap()
        );
        assert_eq!(
            cp(*answer).0,
            resolve_sym(":REPLACEMENT").unwrap(),
            "the new public factory has observably different nested code"
        );
    }
}
