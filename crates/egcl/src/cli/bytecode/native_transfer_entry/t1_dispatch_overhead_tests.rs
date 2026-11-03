// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_tests::ordinary_t2;
use super::*;

struct Queries;
impl Queries {
    fn start(symbol: u32) -> Self {
        DISPATCH_QUERIES.with(|slot| slot.set((Some(symbol), 0, 0)));
        Self
    }
    fn counts(&self) -> (usize, usize) {
        DISPATCH_QUERIES.with(|slot| {
            let (_, selects, candidates) = slot.get();
            (selects, candidates)
        })
    }
}
impl Drop for Queries {
    fn drop(&mut self) {
        DISPATCH_QUERIES.with(|slot| slot.set((None, 0, 0)));
    }
}
fn mapped(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped native code")
    };
    code
}
fn install_t1(env: &mut Env, name: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} (x) x)"), env).unwrap();
    let symbol = egcl_rt::symbols::intern(name);
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    assert!(lazy_compile_defun(
        symbol,
        name,
        egcl_rt::function::lambda_list(*function),
        egcl_rt::function::body(*function),
        env
    ));
    let native = super::super::install_t1(symbol, registry_get(symbol).unwrap(), false).unwrap();
    publish_native(symbol, Some(*function), &native);
    assert!(!native.is_t2 && !native.has_deopt);
    (symbol, native)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn admitted_t1_object_calls_skip_failing_t2_selection_and_preserve_exact_heat() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_invoke_threshold() > 33);
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, child) = install_t1(&mut env, "T1-DISPATCH-OVERHEAD-CHILD");
    crate::cli::read_eval_all_env(
        "(defun t1-dispatch-overhead-parent (f x &key (seed (1+ 0)) &aux (u 0))
           (dotimes (i 32 x) (funcall f x)))",
        &mut env,
    )
    .unwrap();
    let (parent_symbol, parent) = ordinary_t2::install(
        "T1-DISPATCH-OVERHEAD-PARENT",
        "(f x &key (seed (1+ 0)) &aux (u 0))",
        "((dotimes (i 32 x) (funcall f x)))",
        &mut env,
    );
    assert!(mapped(&parent).ordinary_entry.is_none());
    assert!(mapped(&child).lean_root_entry().is_some());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let before = egcl_rt::function::invoke_count(*function);
    let queries = Queries::start(symbol);
    super::super::start_activation_counts_for_test(symbol);
    mapped_ordinary::take_entries_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::take_nested_entries();
    assert_eq!(
        mapped(&parent)
            .run_callable(
                egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
                &[*function, EgclVal::from_fixnum(317)],
                &mut env
            )
            .unwrap(),
        EgclVal::from_fixnum(317)
    );
    assert_eq!(
        queries.counts().0,
        0,
        "no T2 fill/selection for the admitted T1 receiver"
    );
    assert!(
        queries.counts().1 >= 32,
        "actual T1 preparation/committed queries ran"
    );
    assert_eq!(egcl_rt::function::invoke_count(*function), before + 32);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 32);
    assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
    assert_eq!(super::super::take_nested_entries(), 0);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    assert!(
        !is_cached_for_test(*function),
        "T1 still has no T2 table entry"
    );
    // Native identity body has no guard/deopt or managed route; the real
    // ordinary bridge entries above, not installed metadata alone, are required.
    assert!(!child.is_t2 && !child.has_deopt);
}

#[test]
fn admissible_t2_cache_hits_skip_t1_queries_in_both_preparation_functions() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for child in [false, true] {
        let name = if child {
            "T2-DISPATCH-CACHED-CHILD"
        } else {
            "T2-DISPATCH-CACHED-ORDINARY"
        };
        let params = "(x)";
        // A live handler scope requires managed scope ledgers. A keyword
        // parameter alone does not guarantee a mapped child entry, even when
        // its ordinary root is unavailable. Keep both cohorts fixed-arity.
        let body = if child {
            "(handler-case (car x) (error () :handled))"
        } else {
            "x"
        };
        crate::cli::read_eval_all_env(&format!("(defun {name} {params} {body})"), &mut env)
            .unwrap();
        let (symbol, native) = ordinary_t2::install(name, params, &format!("({body})"), &mut env);
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        assert!(native.is_t2);
        if child {
            assert!(
                native
                    .body
                    .as_ref()
                    .unwrap()
                    .code
                    .iter()
                    .any(|op| matches!(op, Instr::PushHandlerCase { .. }))
            );
            assert!(
                mapped(&native).ordinary_entry.is_none(),
                "handler body must retain managed ABI"
            );
            assert!(
                mapped(&native).mapped_entry().is_some(),
                "genuine managed child entry required"
            );
            assert!(!call_table::prefer_ordinary_child(
                &native, *function, false
            ));
        }
        let selected = lookup_or_fill(*function, 1).expect("real cached T2 selection");
        let Target::Native {
            native: cached_native,
            owner,
            function: cached_function,
            ..
        } = &selected
        else {
            panic!("exact native cache target");
        };
        assert!(Rc::ptr_eq(cached_native, &native));
        assert_eq!(*owner, symbol);
        assert_eq!(*cached_function, *function);
        assert_eq!(enters_as_child(&selected), child);
        egcl_rt::rooted!(args = [*function, T]);
        let mut context = egcl_rt::call_table::NativeCallContext {
            request: std::ptr::null_mut(),
            capture: std::ptr::null(),
            args: args.as_mut_ptr(),
            nargs: args.len(),
        };
        // Preparation-only fixture: every field is an integer/raw pointer,
        // Returned=0 is valid, and generated storage is opaque/unentered. Only
        // context.args/nargs are read. Never publish or execute this record.
        let mut record: MappedCallRecord = unsafe { std::mem::zeroed() };
        record.context = &mut context;
        let queries = Queries::start(symbol);
        for _ in 0..16 {
            assert_eq!(unsafe { cached(&mut record) }, !child);
            assert_eq!(unsafe { prepare(&mut record) }, !child);
        }
        assert_eq!(
            queries.counts(),
            (0, 0),
            "admissible T2 ordinary AND child hits precede both T1 query and fill"
        );
    }
}

#[test]
fn canonical_credit_returns_exact_retained_body_and_stops_for_private_t2_despite_stale_tier() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(!profiling_disabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install_t1(&mut env, "T1-DISPATCH-CREDIT-OLD");
    egcl_rt::rooted!(old = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let body = Arc::clone(native.body.as_ref().unwrap());
    egcl_rt::rooted!(_body = super::super::super::ActiveBytecodeRoot::new(&body));
    let before = egcl_rt::function::invoke_count(*old);
    let (owner, selected) = record_callable_invocation_and_body(*old).unwrap();
    assert_eq!(owner, symbol);
    assert!(Arc::ptr_eq(&body, &selected));
    assert_eq!(egcl_rt::function::invoke_count(*old), before + 1);
    crate::cli::read_eval_all_env("(defun t1-dispatch-credit-old (x) :new)", &mut env).unwrap();
    let private = egcl_rt::function::definition_index(*old);
    let (owner, selected) = record_callable_invocation_and_body(*old).unwrap();
    assert_eq!(
        owner, private,
        "canonical helper resolves retained owner, not public replacement"
    );
    assert!(Arc::ptr_eq(&body, &selected));
    assert_eq!(egcl_rt::function::invoke_count(*old), before + 2);
    let (_, private_t2) = ordinary_t2::install_body(private, Arc::clone(&body));
    assert!(private_t2.is_t2);
    // A saved object can have stale shared tier bits; heat policy is based on
    // the exact retained body/owner, never a cheap tier-bit shortcut.
    egcl_rt::function::set_tier(*old, 0);
    let (owner, selected) = record_callable_invocation_and_body(*old).unwrap();
    assert_eq!(owner, private);
    assert!(Arc::ptr_eq(&body, &selected));
    record_callable_invocation(*old);
    assert_eq!(egcl_rt::function::invoke_count(*old), before + 2);
}
