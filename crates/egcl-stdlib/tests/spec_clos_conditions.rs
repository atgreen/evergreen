// Coverage umbrella: R5.44, R5.45.

use std::sync::{Arc, Mutex};
use std::thread;

use egcl_rt::error::EgclError;
use egcl_rt::gc::{alloc_typed, heap_stats};
use egcl_rt::object::type_id;
use egcl_rt::value::{NIL, T, EgclVal};
use egcl_stdlib::clos::{
    MethodCombinationType, MethodQualifier, add_method, define_class, get_effective_method,
    get_short_form_method, remove_method, shared_initialize_with_list,
};
use egcl_stdlib::conditions::{
    self, RestartSpec, SYMBOL_CONDITION, SYMBOL_CONTINUE, SYMBOL_ERROR, SYMBOL_MUFFLE_WARNING,
};
use egcl_stdlib::{
    allocate_instance, bootstrap_clos, change_class, class_direct_subclasses,
    class_direct_superclasses, class_name, class_of, class_slots, clear_funcall_hook,
    compute_applicable_methods, compute_class_precedence_list, compute_effective_method,
    compute_restarts, error_condition, find_restart, handler_bind_fn, handler_case,
    initialize_instance, invoke_restart, invoke_restart_interactively, make_generic_function,
    make_instance, make_simple_error, reinitialize_instance, set_debugger_hook, set_funcall_hook,
    set_method_specializers, set_slot_value, shared_initialize, signal_condition, slot_boundp,
    slot_makunbound, slot_value, warn_condition,
};

fn sym(i: u32) -> EgclVal {
    EgclVal::from_symbol_index(i)
}

fn fx(i: i64) -> EgclVal {
    EgclVal::from_fixnum(i)
}

fn test_lock() -> &'static Mutex<()> {
    static L: std::sync::OnceLock<Mutex<()>> = std::sync::OnceLock::new();
    L.get_or_init(|| Mutex::new(()))
}

/// Reset shared CLOS/GC state and return a serialization guard. Every test in
/// this binary holds it for its whole body: they share the process-global GC
/// heap and CLOS bootstrap state, and racing on it under cargo's parallel test
/// harness corrupts the heap -> SIGSEGV (bliss-110). Recover from a poisoned
/// lock so one panicking test does not cascade-fail the rest.
#[must_use = "hold the returned guard for the whole test to keep it serialized"]
fn reset_state() -> std::sync::MutexGuard<'static, ()> {
    let guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    bootstrap_clos().expect("bootstrap_clos");
    clear_funcall_hook();
    set_debugger_hook(None);
    guard
}

#[test]
fn clos_bootstrap_exposes_core_classes_and_rejects_builtin_instantiation() {
    let _guard = reset_state();

    // Per R5.87 and R11.03, built-in classes participate in the hierarchy
    // but are not user-instantiable through the public CLOS entrypoints.
    let fixnum_class = class_of(fx(7));
    let nil_class = class_of(NIL);
    let symbol_class = class_of(T);

    assert_ne!(fixnum_class, NIL);
    assert_ne!(nil_class, NIL);
    assert_ne!(symbol_class, NIL);
    assert_ne!(class_name(fixnum_class), NIL);
    assert!(!class_direct_superclasses(fixnum_class).is_empty());

    assert!(
        make_instance(fixnum_class, &[]).is_err(),
        "R5.87: built-in classes must not support MAKE-INSTANCE"
    );
}

#[test]
fn clos_class_definition_tracks_slots_subclasses_and_c3_order() {
    let _guard = reset_state();

    let class_a = fx(3001);
    let class_b = fx(3002);
    let class_c = fx(3003);
    let class_d = fx(3004);
    let slot_a = sym(3001);
    let slot_d = sym(3004);

    // Per R5.66 and R5.67, class metaobjects expose direct supers/subclasses,
    // direct slots, and a C3-based precedence list.
    define_class(sym(3101), class_a, &[], &[slot_a]).unwrap();
    define_class(sym(3102), class_b, &[class_a], &[]).unwrap();
    define_class(sym(3103), class_c, &[class_a], &[]).unwrap();
    define_class(sym(3104), class_d, &[class_b, class_c], &[slot_d]).unwrap();

    assert_eq!(class_slots(class_a), vec![slot_a]);
    assert_eq!(class_slots(class_d), vec![slot_d]);
    assert!(class_direct_subclasses(class_a).contains(&class_b));
    assert!(class_direct_subclasses(class_a).contains(&class_c));
    assert_eq!(class_direct_superclasses(class_d), vec![class_b, class_c]);

    let cpl = compute_class_precedence_list(class_d).unwrap();
    let pos_b = cpl.iter().position(|&c| c == class_b).unwrap();
    let pos_c = cpl.iter().position(|&c| c == class_c).unwrap();
    let pos_a = cpl.iter().position(|&c| c == class_a).unwrap();
    assert_eq!(cpl[0], class_d);
    assert!(pos_b < pos_c, "R5.67: left-to-right C3 ordering must hold");
    assert!(pos_a > pos_b && pos_a > pos_c);
}

#[test]
fn clos_initialization_protocol_filters_and_reapplies_initargs() {
    let _guard = reset_state();

    let class = fx(3200);
    let slot_a = sym(3201);
    let slot_b = sym(3202);
    define_class(sym(3203), class, &[], &[slot_a, slot_b]).unwrap();

    // Per R5.80 and R5.81, MAKE-INSTANCE goes through initialize-instance /
    // shared-initialize, and REINITIALIZE-INSTANCE reapplies explicit initargs.
    let instance = make_instance(class, &[slot_a, fx(10), slot_b, fx(20)]).unwrap();
    assert_eq!(slot_value(instance, slot_a).unwrap(), fx(10));
    assert_eq!(slot_value(instance, slot_b).unwrap(), fx(20));

    shared_initialize(instance, slot_a, &[slot_a, fx(11), slot_b, fx(99)]).unwrap();
    assert_eq!(slot_value(instance, slot_a).unwrap(), fx(11));
    assert_eq!(slot_value(instance, slot_b).unwrap(), fx(20));

    shared_initialize_with_list(instance, Some(&[slot_b]), &[slot_a, fx(50), slot_b, fx(21)])
        .unwrap();
    assert_eq!(slot_value(instance, slot_a).unwrap(), fx(11));
    assert_eq!(slot_value(instance, slot_b).unwrap(), fx(21));

    reinitialize_instance(instance, &[slot_b, fx(22)]).unwrap();
    assert_eq!(slot_value(instance, slot_b).unwrap(), fx(22));
}

#[test]
fn clos_slot_protocol_distinguishes_bound_unbound_and_missing_paths() {
    let _guard = reset_state();

    let class = fx(3300);
    let declared = sym(3301);
    let missing = sym(3302);
    define_class(sym(3303), class, &[], &[declared]).unwrap();
    let instance = allocate_instance(class).unwrap();

    // Per R5.70, R5.71, R5.72, and R5.86, slot access and boundp must
    // expose the observable unbound/missing-state behavior.
    assert!(!slot_boundp(instance, declared).unwrap());
    assert!(slot_value(instance, declared).is_err());
    assert!(!slot_boundp(instance, missing).unwrap());
    assert!(slot_value(instance, missing).is_err());

    set_slot_value(instance, declared, fx(5)).unwrap();
    assert!(slot_boundp(instance, declared).unwrap());
    assert_eq!(slot_value(instance, declared).unwrap(), fx(5));

    slot_makunbound(instance, declared).unwrap();
    assert!(!slot_boundp(instance, declared).unwrap());
    assert!(slot_value(instance, declared).is_err());
}

#[test]
fn clos_change_class_preserves_shared_slots_and_rebinds_class() {
    let _guard = reset_state();

    let old_class = fx(3400);
    let new_class = fx(3401);
    let shared = sym(3402);
    let old_only = sym(3403);
    let new_only = sym(3404);

    // Per R5.82, CHANGE-CLASS preserves same-named slots and rebinds the instance.
    define_class(sym(3405), old_class, &[], &[shared, old_only]).unwrap();
    define_class(sym(3406), new_class, &[], &[shared, new_only]).unwrap();

    let instance = make_instance(old_class, &[shared, fx(41), old_only, fx(42)]).unwrap();
    change_class(instance, new_class).unwrap();

    assert_eq!(class_of(instance), new_class);
    assert_eq!(slot_value(instance, shared).unwrap(), fx(41));
    assert!(!slot_boundp(instance, new_only).unwrap());
    assert!(slot_value(instance, old_only).is_err());
}

#[test]
fn clos_generic_dispatch_prefers_more_specific_methods_and_reflects_mutation() {
    let _guard = reset_state();

    let animal = fx(3500);
    let dog = fx(3501);
    let cat = fx(3502);
    define_class(sym(3500), animal, &[], &[]).unwrap();
    define_class(sym(3501), dog, &[animal], &[]).unwrap();
    define_class(sym(3502), cat, &[animal], &[]).unwrap();

    let dog_instance = make_instance(dog, &[]).unwrap();
    let cat_instance = make_instance(cat, &[]).unwrap();
    let gf = make_generic_function(sym(3503), NIL).unwrap();
    let animal_method = fx(3504);
    let dog_method = fx(3505);

    // Per R5.73, R5.74, and R5.75, applicable methods are ordered by specificity
    // and method-set mutations are reflected by the dispatch surface.
    add_method(gf, animal_method).unwrap();
    add_method(gf, dog_method).unwrap();
    set_method_specializers(animal_method, vec![animal], MethodQualifier::Primary);
    set_method_specializers(dog_method, vec![dog], MethodQualifier::Primary);

    assert_eq!(
        compute_applicable_methods(gf, &[dog_instance]),
        vec![dog_method, animal_method]
    );
    assert_eq!(
        compute_applicable_methods(gf, &[cat_instance]),
        vec![animal_method]
    );

    remove_method(gf, dog_method).unwrap();
    assert_eq!(
        compute_applicable_methods(gf, &[dog_instance]),
        vec![animal_method]
    );
}

#[test]
fn clos_effective_method_surfaces_standard_and_short_form_combinations() {
    let _guard = reset_state();

    let gf = make_generic_function(sym(3600), NIL).unwrap();
    let around = fx(3601);
    let before = fx(3602);
    let primary_a = fx(3603);
    let primary_b = fx(3604);
    let after = fx(3605);

    set_method_specializers(around, vec![], MethodQualifier::Around);
    set_method_specializers(before, vec![], MethodQualifier::Before);
    set_method_specializers(primary_a, vec![], MethodQualifier::Primary);
    set_method_specializers(primary_b, vec![], MethodQualifier::Primary);
    set_method_specializers(after, vec![], MethodQualifier::After);

    // Per R5.77, R5.78, and R5.79, the public effective-method protocol must
    // preserve standard qualifier structure and short-form combination metadata.
    let standard_key = compute_effective_method(
        gf,
        MethodCombinationType::Standard,
        &[around, before, primary_a, primary_b, after],
    )
    .unwrap();
    let standard = get_effective_method(standard_key).unwrap();
    assert_eq!(standard.0, vec![around]);
    assert_eq!(standard.1, vec![before]);
    assert_eq!(standard.2, vec![primary_a, primary_b]);
    assert_eq!(standard.3, vec![after]);

    let short_key = compute_effective_method(
        gf,
        MethodCombinationType::Append,
        &[around, primary_a, primary_b, after],
    )
    .unwrap();
    let short = get_short_form_method(short_key).unwrap();
    assert_eq!(short.0, MethodCombinationType::Append);
    assert_eq!(short.1, vec![primary_a, primary_b]);
}

#[test]
fn conditions_signal_runs_newest_matching_handlers_without_unwinding() {
    let _guard = reset_state();

    let log = Arc::new(Mutex::new(Vec::new()));
    let newest_handler = fx(4001);
    let oldest_handler = fx(4002);
    let log_for_hook = Arc::clone(&log);

    set_funcall_hook(move |function, args| {
        assert_eq!(args.len(), 1);
        let mut log = log_for_hook.lock().unwrap();
        log.push(function.as_fixnum());
        Ok(NIL)
    });

    let condition = make_simple_error("boom", &[]);

    // Per R5.92, R5.93, R5.94, and R5.102, HANDLER-BIND establishes dynamic
    // handler clusters, SIGNAL searches newest-first, and returning handlers
    // do not unwind the protected body.
    let result = handler_bind_fn(&[(sym(*SYMBOL_ERROR), oldest_handler)], || {
        handler_bind_fn(&[(sym(*SYMBOL_ERROR), newest_handler)], || {
            signal_condition(condition)?;
            Ok(fx(99))
        })
    })
    .unwrap();

    assert_eq!(result, fx(99));
    assert_eq!(*log.lock().unwrap(), vec![4001, 4002]);
}

#[test]
fn conditions_handler_case_matches_registered_conditions_and_passes_through_values() {
    let _guard = reset_state();

    let condition = make_simple_error("handler-case", &[]);

    // Per R5.95 and R11.02, the public HANDLER-CASE surface must route a
    // signalled condition to the matching clause and leave non-conditions alone.
    assert_eq!(
        handler_case(condition, &[(sym(*SYMBOL_ERROR), fx(77))]).unwrap(),
        fx(77)
    );
    assert_eq!(
        handler_case(fx(78), &[(sym(*SYMBOL_ERROR), fx(99))]).unwrap(),
        fx(78)
    );
}

#[test]
fn conditions_restarts_are_newest_first_filtered_invokable_and_thread_local() {
    let _guard = reset_state();

    let same_name = sym(4100);
    let hidden_name = sym(4101);
    let old_restart = fx(4102);
    let new_restart = fx(4103);
    let interactive = fx(4104);
    let test_true = fx(4105);
    let test_false = fx(4106);

    set_funcall_hook(move |function, args| {
        if function == test_true {
            return Ok(T);
        }
        if function == test_false {
            return Ok(NIL);
        }
        if function == interactive {
            return Ok(fx(1234));
        }
        if function == new_restart {
            return Ok(args.first().copied().unwrap_or(fx(9000)));
        }
        if function == old_restart {
            return Ok(fx(9001));
        }
        Ok(NIL)
    });

    let visible_old = RestartSpec {
        name: same_name,
        function: old_restart,
        report_function: None,
        interactive_function: None,
        test_function: Some(test_true),
    };
    let visible_new = RestartSpec {
        name: same_name,
        function: new_restart,
        report_function: None,
        interactive_function: Some(interactive),
        test_function: Some(test_true),
    };
    let hidden = RestartSpec {
        name: hidden_name,
        function: fx(4107),
        report_function: None,
        interactive_function: None,
        test_function: Some(test_false),
    };
    let condition = make_simple_error("restart-filter", &[]);

    // Per R5.96, R5.97, R5.98, R5.99, R5.100, and R5.108, restart clusters
    // are thread-local, newest-first, filter through :test, and are invokable
    // directly and interactively within their dynamic extent.
    let observed = conditions::restart_bind_fn(&[visible_old, visible_new, hidden], || {
        let names = compute_restarts(Some(condition));
        assert_eq!(names, vec![same_name, same_name]);
        assert_eq!(
            compute_restarts(None),
            vec![hidden_name, same_name, same_name]
        );

        let found = find_restart(same_name, Some(condition)).unwrap();
        assert_eq!(found, new_restart);
        assert_eq!(invoke_restart(found, &[fx(55)]).unwrap(), fx(55));
        assert_eq!(invoke_restart_interactively(same_name).unwrap(), fx(1234));

        let thread_names = thread::spawn(compute_restarts_none).join().unwrap();
        assert!(
            thread_names.is_empty(),
            "R5.108: restart stacks are thread-local"
        );

        Ok(fx(1))
    })
    .unwrap();

    assert_eq!(observed, fx(1));
    assert!(compute_restarts(None).is_empty());
}

fn compute_restarts_none() -> Vec<EgclVal> {
    compute_restarts(None)
}

#[test]
fn conditions_warn_and_cerror_expose_default_restarts() {
    let _guard = reset_state();

    let warning_handler = fx(4200);
    let continue_handler = fx(4201);

    set_funcall_hook(move |function, _args| {
        if function == warning_handler {
            let restart = find_restart(sym(*SYMBOL_MUFFLE_WARNING), None).unwrap();
            invoke_restart(restart, &[])?;
            return Ok(NIL);
        }
        if function == continue_handler {
            let restart = find_restart(sym(*SYMBOL_CONTINUE), None).unwrap();
            invoke_restart(restart, &[])?;
            return Ok(NIL);
        }
        Ok(NIL)
    });

    // Per R5.104 and R5.105, WARN establishes MUFFLE-WARNING and CERROR
    // establishes CONTINUE for recovery within the dynamic signalling context.
    handler_bind_fn(&[(sym(*SYMBOL_CONDITION), warning_handler)], || {
        warn_condition(make_simple_error("warn", &[]))?;
        Ok(NIL)
    })
    .unwrap();

    handler_bind_fn(&[(sym(*SYMBOL_ERROR), continue_handler)], || {
        conditions::cerror("continue", make_simple_error("cerror", &[]))?;
        Ok(NIL)
    })
    .unwrap();
}

#[test]
fn conditions_error_calls_debugger_hook_before_reporting_unhandled_error() {
    let _guard = reset_state();

    let hook = fx(4300);
    let calls = Arc::new(Mutex::new(Vec::<(EgclVal, Vec<EgclVal>)>::new()));
    let calls_for_hook = Arc::clone(&calls);

    set_funcall_hook(move |function, args| {
        calls_for_hook
            .lock()
            .unwrap()
            .push((function, args.to_vec()));
        Ok(NIL)
    });
    set_debugger_hook(Some(hook));

    let condition = make_simple_error("unhandled", &[]);

    // Per R5.101 and R5.103, ERROR must call *DEBUGGER-HOOK* before entering
    // the debugger for an unhandled condition and must not return normally.
    let result = error_condition(condition);
    assert!(result.is_err());

    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, hook);
    assert_eq!(calls[0].1, vec![condition, hook]);
}

#[test]
fn conditions_oom_path_uses_preallocated_storage_condition_instances() {
    let _guard = reset_state();
    conditions::initialize_condition_runtime_support().unwrap();

    let handler = fx(4301);
    let seen = Arc::new(Mutex::new(Vec::<EgclVal>::new()));
    let seen_for_hook = Arc::clone(&seen);
    set_funcall_hook(move |function, args| {
        if function == handler {
            seen_for_hook.lock().unwrap().push(args[0]);
        }
        Ok(NIL)
    });

    // Per R5.110, the runtime must signal a preallocated STORAGE-CONDITION
    // when heap allocation is exhausted instead of allocating a fresh
    // condition object on the failing path.
    handler_bind_fn(
        &[(sym(*conditions::SYMBOL_STORAGE_CONDITION), handler)],
        || {
            let first = conditions::signal_storage_condition_for_runtime_error(&EgclError::Oom)?;
            let second = conditions::signal_storage_condition_for_runtime_error(&EgclError::Oom)?;
            assert_ne!(first, NIL);
            assert_ne!(second, NIL);
            Ok(NIL)
        },
    )
    .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    assert_eq!(
        class_name(class_of(seen[0])),
        sym(*conditions::SYMBOL_STORAGE_CONDITION)
    );
    assert_eq!(
        class_name(class_of(seen[1])),
        sym(*conditions::SYMBOL_STORAGE_CONDITION)
    );
}

#[test]
fn real_allocator_oom_signals_preallocated_storage_condition() {
    let _guard = reset_state();
    conditions::initialize_condition_runtime_support().unwrap();
    let stats = heap_stats();
    let impossible_large_object =
        (stats.nursery_capacity + stats.old_gen_capacity + stats.large_object_bytes) as usize
            + 1024 * 1024;
    let error = alloc_typed(impossible_large_object, type_id::SIMPLE_BASE_STRING)
        .map(|_| ())
        .ok_or(EgclError::Oom)
        .expect_err("allocation larger than heap must fail");

    let handler = fx(4304);
    let seen = Arc::new(Mutex::new(Vec::<EgclVal>::new()));
    let seen_for_hook = Arc::clone(&seen);
    set_funcall_hook(move |function, args| {
        if function == handler {
            seen_for_hook.lock().unwrap().push(args[0]);
        }
        Ok(NIL)
    });

    handler_bind_fn(
        &[(sym(*conditions::SYMBOL_STORAGE_CONDITION), handler)],
        || {
            conditions::signal_storage_condition_for_runtime_error(&error)?;
            Ok(NIL)
        },
    )
    .unwrap();

    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(
        class_name(class_of(seen[0])),
        sym(*conditions::SYMBOL_STORAGE_CONDITION),
        "real allocator OOM must be signalled as STORAGE-CONDITION"
    );
}

// R5.110 / bliss-uh4.2: the storage-failure acquire path must be a startup
// invariant, never a lazy allocator.
#[test]
fn storage_condition_acquire_without_init_is_a_hard_error_not_lazy_alloc() {
    // The emergency reserve is process-wide: a new carrier must reuse it.
    // Explicitly clear it under the suite lock to exercise missing startup
    // initialization without depending on test order or thread-local state.
    let _guard = test_lock().lock().unwrap_or_else(|e| e.into_inner());
    conditions::reset_storage_condition_pool();
    let result = thread::spawn(conditions::acquire_preallocated_storage_condition)
        .join()
        .unwrap();
    assert!(
        result.is_err(),
        "acquire on the storage-failure path must not lazily initialize/allocate"
    );
}

#[test]
fn storage_condition_pool_rotates_and_reuses_preallocated_instances() {
    let _guard = reset_state();
    conditions::initialize_condition_runtime_support().unwrap();
    // STORAGE_CONDITION_POOL_SIZE is 4 (conditions.rs). Acquiring many more than
    // that must cycle through exactly those preallocated instances and never
    // allocate a fresh one.
    let mut distinct: Vec<EgclVal> = Vec::new();
    for _ in 0..12 {
        let c = conditions::acquire_preallocated_storage_condition().unwrap();
        assert_ne!(c, NIL);
        assert_eq!(
            class_name(class_of(c)),
            sym(*conditions::SYMBOL_STORAGE_CONDITION)
        );
        if !distinct.contains(&c) {
            distinct.push(c);
        }
    }
    assert_eq!(
        distinct.len(),
        4,
        "acquire must reuse the preallocated pool, not allocate a fresh instance each call"
    );
}

#[test]
fn clos_and_conditions_top_level_entrypoints_compose_in_an_acceptance_scenario() {
    let _guard = reset_state();

    let class = fx(4400);
    let slot = sym(4401);
    define_class(sym(4402), class, &[], &[slot]).unwrap();
    let instance = allocate_instance(class).unwrap();
    let handler = fx(4403);

    let observed = Arc::new(Mutex::new(Vec::<EgclVal>::new()));
    let observed_for_hook = Arc::clone(&observed);
    set_funcall_hook(move |function, args| {
        if function == handler {
            observed_for_hook.lock().unwrap().push(args[0]);
        }
        Ok(NIL)
    });

    // Per R11.02 and R11.03, the Rust-backed stdlib entrypoints for CLOS and
    // conditions must already compose into a usable bootstrap path.
    let result: Result<EgclVal, EgclError> =
        handler_bind_fn(&[(sym(*SYMBOL_ERROR), handler)], || {
            initialize_instance(instance, &[slot, fx(88)])?;
            assert_eq!(slot_value(instance, slot).unwrap(), fx(88));
            slot_makunbound(instance, slot)?;
            let err = make_simple_error("slot became unbound", &[]);
            signal_condition(err)?;
            Ok(instance)
        });

    assert_eq!(result.unwrap(), instance);
    assert_eq!(observed.lock().unwrap().len(), 1);
}

/// The STORAGE-CONDITION pool lives in the GC heap, pinned, so a collection never
/// moves or frees its preallocated instances (bliss-4v8 / D5.13). After a full
/// GC the same instances come back at the same addresses, still valid.
#[test]
fn storage_condition_pool_is_pinned_in_gc_heap_and_survives_collection() {
    let _guard = reset_state();
    conditions::initialize_condition_runtime_support().unwrap();

    // Snapshot the four pinned pool instances: claim all four distinct slots,
    // then release them so they can be re-claimed after the GC (bliss-wzw claim/
    // release protocol — claims are not recycled until released).
    let before: Vec<EgclVal> = (0..4)
        .map(|_| conditions::acquire_preallocated_storage_condition().unwrap())
        .collect();
    for c in &before {
        assert_ne!(*c, NIL);
        assert_eq!(
            class_name(class_of(*c)),
            sym(*conditions::SYMBOL_STORAGE_CONDITION)
        );
    }
    for c in &before {
        conditions::release_preallocated_storage_condition(*c);
    }

    // Churn the heap and force a full GC (minor + major). The pool is pinned, so
    // it must survive unmoved even as other objects are evacuated.
    for _ in 0..500 {
        let _ = egcl_rt::alloc_typed(16, egcl_rt::object::type_id::CONS);
    }
    egcl_rt::full_gc().expect("full_gc");

    let after: Vec<EgclVal> = (0..4)
        .map(|_| conditions::acquire_preallocated_storage_condition().unwrap())
        .collect();
    // Claim order is deterministic (lowest free slot first), so the same four
    // pinned instances come back in the same order at the same addresses.
    assert_eq!(
        before, after,
        "pinned pool instances kept their exact addresses across a full GC"
    );
    for c in &after {
        assert_eq!(
            class_name(class_of(*c)),
            sym(*conditions::SYMBOL_STORAGE_CONDITION),
            "pool instance is still a valid STORAGE-CONDITION after GC"
        );
    }
}
