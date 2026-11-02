// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Verify machine execution behind escaped tree-created namespace callbacks.
use super::*;

fn native_for_tree_function(function: EgclVal) -> Rc<NativeCode> {
    assert!(crate::cli::is_closure_cons(function));
    let id = crate::cli::cp(function).1.as_fixnum() as u64;
    egcl_rt::rooted!(
        compiled = crate::cli::settled_tree_closure(id).expect("escaped closure must compile")
    );
    let owner = egcl_rt::function::definition_index(*compiled);
    let native = NATIVE_REGISTRY
        .with(|registry| registry.borrow().get(&owner).cloned())
        .or_else(|| try_promote_to_t1(owner))
        .expect("actual native installation");
    assert!(matches!(&native._storage, NativeCodeStorage::Mapped(_)));
    native
}

fn collect_at_verified_native_entry() {
    // ordinary_t2's existing observer first checks the real native segment and
    // its exact owner/capture; this hook executes only after that proof.
    HeapCollector::new().minor_gc().unwrap();
}
struct ResetObserver;
impl Drop for ResetObserver {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(None));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn escaped_captured_namespace_calls_and_function_references_execute_native_code_and_move_roots() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for call in ["(captured-local inner)", "(funcall #'captured-local inner)"] {
        // Call the tree evaluator directly: no top-level creator compiler can
        // replace this setup and make the captured-namespace proof vacuous.
        let source = format!(
            r#"
          (let ((held (list 17)))
            (flet ((captured-local (check)
                     (if check (%ordinary-t2-entry-for-test)) held))
              (list (lambda (outer inner)
                      (if outer (%ordinary-t2-entry-for-test))
                      (list {call} #'captured-local))
                    #'captured-local
                    (lambda (value) (setq held value)))))"#
        );
        egcl_rt::rooted!(form = reader::read_from_string(&source).unwrap().0);
        egcl_rt::rooted!(created = crate::cli::eval_form(*form, &mut env).unwrap());
        egcl_rt::rooted!(functions = list_to_vec(*created));
        assert_eq!(functions.len(), 3);
        assert!(
            functions
                .iter()
                .all(|function| crate::cli::is_closure_cons(*function)),
            "the creator must produce interpreted closure identities"
        );
        let id = crate::cli::cp(functions[0]).1.as_fixnum() as u64;
        assert!(
            env.closures
                .borrow()
                .get(&id)
                .unwrap()
                .captured_funs
                .as_ref()
                .is_some_and(|funs| !funs.borrow().is_empty())
        );
        for _ in 0..lazy_compile_threshold().saturating_add(2) {
            egcl_rt::rooted!(result = apply_function(functions[0], &[NIL, NIL], &mut env).unwrap());
            egcl_rt::rooted!(values = list_to_vec(*result));
            assert_eq!(
                values[1], functions[1],
                "FUNCTION must retain the canonical local identity"
            );
            assert_eq!(crate::cli::cp(values[0]), (EgclVal::from_fixnum(17), NIL));
        }
        // Compile/prime all setter and marker paths before allocating the
        // fresh relocation probe; initial helper publication may allocate pins.
        for _ in 0..lazy_compile_threshold().saturating_add(2) {
            egcl_rt::rooted!(result = apply_function(functions[0], &[NIL, NIL], &mut env).unwrap());
            egcl_rt::rooted!(values = list_to_vec(*result));
            egcl_rt::rooted!(
                _stored = apply_function(functions[2], &[values[0]], &mut env).unwrap()
            );
        }
        let outer_native = native_for_tree_function(functions[0]);
        let inner_native = native_for_tree_function(functions[1]);
        let _setter_native = native_for_tree_function(functions[2]);
        ordinary_t2::expect_entry(&outer_native);
        egcl_rt::rooted!(_prime_outer = apply_function(functions[0], &[T, NIL], &mut env).unwrap());
        assert!(ordinary_t2::observed_entry());
        ordinary_t2::expect_entry(&inner_native);
        egcl_rt::rooted!(_prime_inner = apply_function(functions[0], &[NIL, T], &mut env).unwrap());
        assert!(ordinary_t2::observed_entry());
        HeapCollector::new().minor_gc().unwrap();
        egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(73), NIL));
        egcl_rt::rooted!(stored = apply_function(functions[2], &[*payload], &mut env).unwrap());
        assert_eq!(*stored, *payload);
        let before = payload.to_raw();
        ordinary_t2::expect_entry(&outer_native);
        ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|hook| hook.set(Some(collect_at_verified_native_entry)));
        let _reset = ResetObserver;
        egcl_rt::rooted!(outer_result = apply_function(functions[0], &[T, NIL], &mut env).unwrap());
        assert!(
            ordinary_t2::observed_entry(),
            "escaped callback itself must reach its exact native owner"
        );
        assert_ne!(
            payload.to_raw(),
            before,
            "collection in native code must actually move the shared capture"
        );
        egcl_rt::rooted!(outer_values = list_to_vec(*outer_result));
        assert_eq!(*outer_values, vec![*payload, functions[1]]);
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(None));
        ordinary_t2::expect_entry(&inner_native);
        egcl_rt::rooted!(inner_result = apply_function(functions[0], &[NIL, T], &mut env).unwrap());
        assert!(
            ordinary_t2::observed_entry(),
            "local namespace target must itself execute native code"
        );
        egcl_rt::rooted!(inner_values = list_to_vec(*inner_result));
        assert_eq!(*inner_values, vec![*payload, functions[1]]);
        assert_eq!(crate::cli::cp(*payload), (EgclVal::from_fixnum(73), NIL));
    }
}
