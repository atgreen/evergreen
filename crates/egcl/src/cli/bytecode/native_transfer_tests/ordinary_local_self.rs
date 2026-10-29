// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;

thread_local! {
    static COLD: Cell<usize> = const { Cell::new(0) };
    static MARKER: Cell<*mut EgclVal> = const { Cell::new(std::ptr::null_mut()) };
}

fn request_self_poll() {
    // Arrive after the helper's return poll so the generated self edge
    // must still publish its caller and take cold admission.
    ordinary_t2::request_yield_after_return();
}

fn collect_in_self_cold() {
    COLD.with(|count| count.set(count.get() + 1));
    let marker = MARKER.with(Cell::get);
    let before = unsafe { (*marker).to_raw() };
    egcl_rt::gc::full_gc().unwrap();
    assert_ne!(
        unsafe { (*marker).to_raw() },
        before,
        "local self cold admission must actually relocate the marker"
    );
}

struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        native_transfer_entry::ordinary_calls::YIELD_AFTER_RETURN_TO_OWNER
            .with(|target| target.set(0));
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(None));
        native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER.with(|hook| hook.set(None));
        MARKER.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_labels_self_cold_retains_real_local_callable() {
    let _lock = super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let forms = "((labels ((walk (n marker)
        (if (= n 0) (car marker)
            (progn (local-self-observer) (+ (walk 0 marker) 0)))))
        (walk n marker)))";
    super::super::super::read_eval_all_env(
        &format!(
            "(defun local-self-observer () (%ordinary-t2-entry-for-test))
         (defun ordinary-labels-owner (n marker) {})",
            &forms[1..forms.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    let (outer_symbol, outer) =
        ordinary_t2::install("ORDINARY-LABELS-OWNER", "(n marker)", forms, &mut env);
    let local_symbol = outer
        .body
        .as_ref()
        .unwrap()
        .code
        .iter()
        .find_map(|instruction| {
            let Instr::CallNamed { sym, .. } = instruction else {
                return None;
            };
            let name = super::super::super::sym_name(EgclVal::from_symbol_index(*sym));
            name.contains("__FLET__").then_some(*sym)
        })
        .expect("actual noncapturing LABELS call must target its lexical registration");
    egcl_rt::rooted!(
        local = egcl_rt::symbols::symbol_function(local_symbol)
            .expect("LABELS registration must own a callable object")
    );
    assert!(egcl_rt::function::is_interpreted_function(*local));
    let body = registry_get(local_symbol).unwrap();
    assert_eq!(
        body.self_call_sites.len(),
        1,
        "real LABELS source proves its self call"
    );
    let input = snapshot_t2_input(local_symbol, 0).expect("local T2 snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("local T2 artifact");
    let local_code = install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: local_symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("local ordinary T2 installation");
    let NativeCodeStorage::Mapped(owner) = &local_code._storage else {
        panic!("local native owner")
    };
    assert!(owner.ordinary_entry.is_some());
    assert!(local_code.is_t2);
    ordinary_t2::expect_entry(&local_code);
    egcl_rt::rooted!(marker = super::super::super::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(args = vec![EgclVal::from_fixnum(1), *marker]);
    MARKER.with(|slot| slot.set(&mut *marker));
    COLD.with(|count| count.set(0));
    let _reset = Reset;
    ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|hook| hook.set(Some(request_self_poll)));
    native_transfer_entry::ordinary_calls::SELF_COLD_OBSERVER
        .with(|hook| hook.set(Some(collect_in_self_cold)));
    let result = run_native(&outer, outer_symbol, &args, &mut env).unwrap();
    assert_eq!(result, EgclVal::from_fixnum(71));
    assert!(
        ordinary_t2::observed_entry(),
        "must execute the installed local ordinary owner"
    );
    assert_eq!(
        COLD.with(Cell::get),
        1,
        "real lexical self edge must reach cold admission"
    );
}
