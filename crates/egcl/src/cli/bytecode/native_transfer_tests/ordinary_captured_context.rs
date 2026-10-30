// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0
use super::*;
use std::cell::{Cell, RefCell};

pub(super) fn template(forms: &str, env: &Env) -> Arc<BytecodeFunction> {
    egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(forms).unwrap().0);
    let name = crate::cli::sym_name(resolve_sym("ORDINARY-NATIVE-CELL").unwrap());
    let captures = std::collections::HashSet::from([name]);
    let empty = std::collections::HashSet::new();
    Arc::new(
        compile_function_in(
            "ORDINARY-CAPTURED-CONTEXT",
            *params,
            *forms,
            env,
            &HashMap::new(),
            false,
            false,
            &empty,
            &empty,
            &empty,
            &captures,
            None,
        )
        .expect("real capturing bytecode"),
    )
}

pub(super) fn closure(body: &Arc<BytecodeFunction>, value: EgclVal, env: &mut Env) -> EgclVal {
    let mut construction = env.child();
    construction.frame = Arc::new(SharedCell::new(EnvFrame {
        vars: crate::cli::VecMap::default(),
        symbol_vars: crate::cli::VecMap::default(),
        function_vars: crate::cli::VecMap::default(),
        parent: None,
    }));
    egcl_rt::rooted_ref!(_construction = &mut construction);
    let name = body
        .names
        .iter()
        .find(|name| name.ends_with("ORDINARY-NATIVE-CELL"))
        .expect("captured source name");
    bind_boxed_param(&construction.frame, name, value);
    // construction owns every capture before make_bytecode_closure allocates;
    // the caller's original environment remains independently rooted as well.
    make_bytecode_closure(body, Some(Arc::clone(&construction.frame)))
}

fn install(function: EgclVal) -> Rc<NativeCode> {
    native_transfer_entry::with_forced_captured_entry_for_test(|| install_automatic(function))
}

pub(super) fn install_automatic(function: EgclVal) -> Rc<NativeCode> {
    let symbol = egcl_rt::function::definition_index(function);
    closure_compilation_owner(symbol, 1);
    let input = snapshot_t2_input(symbol, 0).expect("captured T2 snapshot");
    let generation = input.generation;
    let input = egcl_rt::CrossThreadRoot::new(input);
    let artifact = input
        .with_gc_stable_mutator(compile_t2_artifact)
        .expect("captured T2 artifact");
    install_t2_completion(T2Completion {
        panicked: false,
        _permit: None,
        dependencies: Vec::new(),
        sym: symbol,
        generation,
        artifact: Some(artifact),
        input,
    })
    .expect("captured T2 install")
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_captured_instances_share_code_without_sharing_lexical_cells() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(11), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
    let code = install(*first);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("mapped owner")
    };
    assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
    assert_eq!(
        owner.ordinary_descriptor(0).unwrap().environment,
        egcl_rt::call_table::ORDINARY_ENV_CLOSURE
    );
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    let _ = native_transfer_entry::take_ordinary_lexical_leaf_hits_for_test();
    native_transfer_entry::ordinary_closure_context::PUBLISHED.with(|count| count.set(0));
    assert_eq!(
        owner
            .run_callable(*first, &[EgclVal::from_fixnum(13)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(13)
    );
    assert_eq!(
        owner
            .run_callable(*second, &[EgclVal::from_fixnum(24)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(24)
    );
    assert!(
        native_transfer_entry::take_ordinary_lexical_leaf_hits_for_test() > 0,
        "actual instances must execute native lexical leaves"
    );
    assert_eq!(
        native_transfer_entry::ordinary_closure_context::PUBLISHED.with(Cell::get),
        0,
        "neither successful instance may require a full lexical publication"
    );
    let name = body
        .names
        .iter()
        .find(|name| name.ends_with("ORDINARY-NATIVE-CELL"))
        .unwrap();
    assert_eq!(
        Env::lookup_frame(&closure_captured_env(*first).unwrap(), name),
        Some(EgclVal::from_fixnum(13))
    );
    assert_eq!(
        Env::lookup_frame(&closure_captured_env(*second).unwrap(), name),
        Some(EgclVal::from_fixnum(24))
    );
    assert_eq!(
        native_transfer_entry::take_lean_root_entries_for_test(),
        2,
        "both actual captured instances must execute the ordinary root entry"
    );
}

thread_local! {
    static FRAME: RefCell<Option<Arc<SharedCell<EnvFrame>>>> = const { RefCell::new(None) };
    static MOVED: Cell<Option<(u64, u64, bool)>> = const { Cell::new(None) };
    static MAJOR_RESULT: Cell<Option<bool>> = const { Cell::new(None) };
}
fn collect_major_in_capture() {
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    MAJOR_RESULT.with(|result| result.set(Some(egcl_rt::gc::full_gc().is_ok())));
}
fn collect_capture() {
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
    let frame = FRAME.with(|slot| slot.borrow().clone()).unwrap();
    let value = || {
        frame
            .borrow()
            .vars
            .iter()
            .next()
            .map(|(_, value)| value.0)
            .unwrap()
    };
    let before = value();
    let ok = egcl_rt::gc::collect_t0_minor().is_ok();
    let after = value();
    MOVED.with(|slot| slot.set(Some((before, after, ok))));
}
struct Reset;
impl Drop for Reset {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        FRAME.with(|slot| *slot.borrow_mut() = None);
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_captured_value_moves_during_published_lexical_read() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(warm = closure(&body, EgclVal::from_fixnum(1), &mut env));
    let code = install(*warm);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("mapped owner")
    };
    assert!(owner.ordinary_entry.is_some() && owner.native_callable_slot.is_some());
    assert_eq!(
        owner.run_callable(*warm, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
    egcl_rt::gc::collect_t0_minor().unwrap();
    egcl_rt::rooted!(fresh = closure(&body, NIL, &mut env));
    let frame = closure_captured_env(*fresh).unwrap();
    // Allocate AFTER closure construction and warming. No Lisp allocation
    // intervenes between publishing this young datum and the observed call.
    egcl_rt::rooted!(value = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    let name = body
        .names
        .iter()
        .find(|name| name.ends_with("ORDINARY-NATIVE-CELL"))
        .unwrap();
    assert!(Env::set_frame_var(&frame, name, *value));
    *value = NIL;
    FRAME.with(|slot| *slot.borrow_mut() = Some(frame));
    MOVED.with(|slot| slot.set(None));
    let _reset = Reset;
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(Some(collect_capture)));
    let result = owner.run_callable(*fresh, &[NIL], &mut env).unwrap();
    let (before, after, ok) = MOVED
        .with(Cell::get)
        .expect("real published native lexical operation");
    assert!(ok);
    assert_ne!(
        before, after,
        "the young captured datum must actually relocate"
    );
    assert_eq!(result.0, after);
    assert_eq!(cp(result).0, EgclVal::from_fixnum(71));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_funcall_probe_enters_distinct_captured_instances_without_rust_child_activation() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(11), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
    let code = install(*first);
    let NativeCodeStorage::Mapped(callee) = &code._storage else {
        panic!("mapped captured owner")
    };
    assert!(callee.native_callable_slot.is_some());
    let (first_owner, first_code, first_selected, _) =
        native_transfer_entry::funcall_fast::native_target(*first, 1)
            .expect("first captured target");
    let (second_owner, second_code, second_selected, _) =
        native_transfer_entry::funcall_fast::native_target(*second, 1)
            .expect("second captured target");
    assert_eq!(
        first_owner, second_owner,
        "one publication identity per compiled template"
    );
    assert!(Rc::ptr_eq(&first_code, &second_code));
    assert_eq!(first_selected, *first);
    assert_eq!(second_selected, *second);
    assert_ne!(
        first_selected, second_selected,
        "instances keep distinct frame identities"
    );
    let (symbol, caller) = ordinary_t2::install(
        "ORDINARY-CAPTURED-PROBE-CALLER",
        "(f next)",
        "((funcall f next))",
        &mut env,
    );
    let NativeCodeStorage::Mapped(owner) = &caller._storage else {
        panic!("mapped caller")
    };
    assert!(owner.ordinary_entry.is_some());
    for _ in 0..4 {
        for (function, expected) in [(*first, 11), (*second, 22)] {
            assert_eq!(
                run_native(&caller, symbol, &[function, NIL], &mut env).unwrap(),
                EgclVal::from_fixnum(expected)
            );
        }
    }
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    for (function, expected) in [(*first, 11), (*second, 22)] {
        assert_eq!(
            run_native(&caller, symbol, &[function, NIL], &mut env).unwrap(),
            EgclVal::from_fixnum(expected)
        );
    }
    assert_eq!(
        native_transfer_entry::take_lean_root_entries_for_test(),
        2,
        "only the two outer callers may construct Rust root activations"
    );
    assert_eq!(
        native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
        0,
        "both captured calls must take the generated ordinary probe, not its Rust native fallback"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_captured_child_guard_preserves_completed_store_and_exact_instance() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell (cons next ordinary-native-cell)) (+ next 1))",
        &env,
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, NIL, &mut env));
    egcl_rt::rooted!(second = closure(&body, NIL, &mut env));
    let code = install(*first);
    let NativeCodeStorage::Mapped(callee) = &code._storage else {
        panic!("mapped captured owner")
    };
    assert!(callee.ordinary_entry.is_some() && callee.native_callable_slot.is_some());
    let (symbol, caller) = native_transfer_entry::with_ordinary_non_tail_emission_for_test(|| {
        ordinary_t2::install(
            "ORDINARY-CAPTURED-GUARD-CALLER",
            "(f next)",
            "((funcall f next))",
            &mut env,
        )
    });
    egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(2.5));
    for _ in 0..4 {
        assert_eq!(
            run_native(
                &caller,
                symbol,
                &[*second, EgclVal::from_fixnum(2)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(3)
        );
    }
    let name = body
        .names
        .iter()
        .find(|name| name.ends_with("ORDINARY-NATIVE-CELL"))
        .unwrap();
    let first_frame = closure_captured_env(*first).unwrap();
    let second_frame = closure_captured_env(*second).unwrap();
    assert!(Env::set_frame_var(&second_frame, name, NIL));
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    let before = callee.deopt_count();
    let stack = egcl_rt::current_stack();
    let anchor = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    egcl_rt::rooted!(result = run_native(&caller, symbol, &[*second, *input], &mut env).unwrap());
    assert_eq!(result.as_double_float(), 3.5);
    assert_eq!(
        callee.deopt_count(),
        before + 1,
        "must execute the precise native guard continuation"
    );
    assert_eq!(
        native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
        0,
        "the failing child must enter through the generated ordinary probe"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        anchor
    );
    assert_eq!(
        Env::lookup_frame(&first_frame, name),
        Some(NIL),
        "code sharing must not select the first instance's frame"
    );
    let effect = Env::lookup_frame(&second_frame, name).unwrap();
    assert_eq!(cp(effect).0, *input);
    assert_eq!(
        cp(effect).1,
        NIL,
        "completed cons/store must not replay during recovery"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_named_captured_ingress_normalizes_current_object_and_retains_old_identity() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(11), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
    let code = install(*first);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("mapped captured owner")
    };
    assert!(owner.native_callable_slot.is_some());
    egcl_rt::rooted!(name = resolve_sym("ORDINARY-NAMED-CAPTURED-INGRESS").unwrap());
    egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), *first);
    assert_eq!(
        owner.run_callable(*name, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(11)
    );
    egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), *second);
    assert_eq!(
        owner.run_callable(*name, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(22)
    );
    assert_eq!(
        owner.run_callable(*first, &[NIL], &mut env).unwrap(),
        EgclVal::from_fixnum(11)
    );
    egcl_rt::symbols::set_symbol_function(name.as_symbol_index(), NIL);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_shared_code_survives_first_instance_without_retaining_its_frame() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );

    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(first = closure(&body, *payload, &mut env));
    *payload = NIL;
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
    let first_id = egcl_rt::function::definition_index(*first);
    let second_id = egcl_rt::function::definition_index(*second);
    let first_frame = Arc::downgrade(&closure_captured_env(*first).unwrap());
    let second_frame = Arc::downgrade(&closure_captured_env(*second).unwrap());
    let code = install(*first);
    let NativeCodeStorage::Mapped(owner) = &code._storage else {
        panic!("mapped captured owner")
    };
    assert!(owner.native_callable_slot.is_some());
    *first = NIL;
    MAJOR_RESULT.with(|result| result.set(None));
    let _reset = Reset;
    native_transfer_entry::root_publication::OBSERVE
        .with(|hook| hook.set(Some(collect_major_in_capture)));
    // The profiling/code owner remains A, but only actual B owns a frame
    // during this activation. A must die even while its shared code executes.
    assert_eq!(
        run_native_callable(
            &code,
            first_id,
            *second,
            &[EgclVal::from_fixnum(22)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(22)
    );
    assert_eq!(MAJOR_RESULT.with(Cell::get), Some(true));
    assert!(
        first_frame.upgrade().is_none(),
        "native metadata must not retain the first instance's captured payload"
    );
    assert!(!closure_envs().borrow().contains_key(&first_id));
    assert!(
        !closure_bodies().borrow().contains_key(&first_id),
        "instance records and code ownership are separate"
    );
    assert!(registry_get(first_id).is_some_and(|saved| Arc::ptr_eq(&saved, &body)));
    let (selected_owner, selected_code, selected, _) =
        native_transfer_entry::funcall_fast::native_target(*second, 1)
            .expect("surviving instance retains compiled template");
    assert_eq!(selected_owner, first_id);
    assert!(
        Rc::ptr_eq(owner, &selected_code),
        "major collection must not force a redundant compilation"
    );
    assert_eq!(selected, *second);
    assert_eq!(
        owner
            .run_callable(*second, &[EgclVal::from_fixnum(31)], &mut env)
            .unwrap(),
        EgclVal::from_fixnum(31)
    );
    *second = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(second_frame.upgrade().is_none());
    assert!(registry_get(first_id).is_none());
    assert!(registry_get(second_id).is_none());
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&first_id)));
    assert!(!INVOKE_COUNTS.with(|counts| counts.borrow().contains_key(&first_id)));
    assert!(
        !CLOSURE_COMPILATION
            .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
    );
    // External Rust code/body Arcs still exist here. Cache-only tracing must
    // not manufacture a semantic owner and keep dispatch metadata alive.
    assert!(Rc::ptr_eq(owner, &selected_code));
}

#[test]
fn shared_owner_lease_rejects_mutable_function_edge_cycles() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("((+ next ordinary-native-cell))", &env);
    assert!(closure_body_has_acyclic_owner_lease(&body));
    // A cache-owned constant vector/cons could point back to an instance of
    // this template. Such a graph must not acquire the new persistent lease.
    egcl_rt::rooted!(edge = crate::cli::arena_cons(NIL, NIL));
    egcl_rt::rooted!(private = egcl_rt::symbols::make_uninterned("PRIVATE-LEASE-EDGE"));
    let mut mutable = (*body).clone();
    mutable.constants.push(*edge);
    assert!(!closure_body_has_acyclic_owner_lease(&mutable));
    mutable.constants.pop();
    mutable.load_time_values.push((0, NIL));
    assert!(!closure_body_has_acyclic_owner_lease(&mutable));
    mutable.load_time_values.clear();
    mutable.nested_functions.push(Arc::clone(&body));
    assert!(!closure_body_has_acyclic_owner_lease(&mutable));
    mutable.nested_functions.clear();
    mutable.constants.push(*private);
    assert!(!closure_body_has_acyclic_owner_lease(&mutable));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn named_factory_owns_compiled_template_after_every_instance_dies() {
    assert_named_factory_owns_compiled_template(true);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mapped_factory_keeps_compiled_template_after_every_instance_dies() {
    assert_named_factory_owns_compiled_template(false);
}

fn assert_named_factory_owns_compiled_template(force_capability: bool) {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        if force_capability {
            "((setq ordinary-native-cell next) ordinary-native-cell)"
        } else {
            "((dotimes (i 2) (setq ordinary-native-cell next)) ordinary-native-cell)"
        },
        &env,
    );
    assert!(
        native_transfer_entry::ordinary_code_owner_body(&body),
        "both cohorts retain structural metadata ownership"
    );
    assert!(
        closure_body_has_acyclic_owner_lease(&body),
        "the named factory must own a lease-eligible template graph"
    );
    let mut factory = (*body).clone();
    let factory_name = if force_capability {
        "ORDINARY-CAPTURED-SEMANTIC-FACTORY"
    } else {
        "MAPPED-CAPTURED-SEMANTIC-FACTORY"
    };
    factory.name = factory_name.to_owned();
    factory.code = vec![
        Instr::MakeClosure {
            func: 0,
            capture_env: true,
        },
        Instr::Return,
    ];
    factory.constants.clear();
    factory.nested_functions = vec![Arc::clone(&body)];
    let factory = Arc::new(factory);
    // This is an actual named-definition owner, not a cached native-body Arc.
    let name = resolve_sym(factory_name).unwrap().symbol_index().unwrap();
    egcl_rt::rooted!(factory_function = make_bytecode_closure(&factory, None));
    egcl_rt::symbols::set_symbol_function(name, *factory_function);
    registry_put(name, factory);
    *factory_function = NIL;
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(1), &mut env));
    let first_id = egcl_rt::function::definition_index(*first);
    let first_frame = Arc::downgrade(&closure_captured_env(*first).unwrap());
    let code = if force_capability {
        install(*first)
    } else {
        install_automatic(*first)
    };
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("native owner")
    };
    assert_eq!(mapped.ordinary_entry.is_some(), force_capability);
    if !force_capability {
        assert!(
            mapped.mapped_entry().is_some(),
            "the real finite backedge must retain mapped execution"
        );
    }
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(first_frame.upgrade().is_none());
    assert!(registry_get(first_id).is_some_and(|saved| Arc::ptr_eq(&saved, &body)));
    egcl_rt::rooted!(next = closure(&body, EgclVal::from_fixnum(2), &mut env));
    let next_frame = Arc::downgrade(&closure_captured_env(*next).unwrap());
    assert_eq!(
        closure_compilation_owner(egcl_rt::function::definition_index(*next), 1).0,
        first_id
    );
    let (_, reused, _, _) = native_transfer_entry::funcall_fast::native_target(*next, 1).unwrap();
    let NativeCodeStorage::Mapped(original) = &code._storage else {
        panic!("native owner")
    };
    assert!(Rc::ptr_eq(original, &reused));
    assert_eq!(
        run_native_callable(
            &code,
            first_id,
            *next,
            &[EgclVal::from_fixnum(31)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(31)
    );
    *next = NIL;
    egcl_rt::symbols::set_symbol_function(name, egcl_rt::value::UNBOUND);
    registry_remove(name);
    egcl_rt::gc::full_gc().unwrap();
    assert!(next_frame.upgrade().is_none());
    assert!(registry_get(first_id).is_none());
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&first_id)));
    assert!(
        !CLOSURE_COMPILATION
            .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
    );
    // Retained external code/Arc values do not grant semantic persistence.
    assert!(Rc::ptr_eq(original, &reused));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn stale_shared_owner_lease_does_not_remove_redefined_metadata() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(1), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(2), &mut env));
    let owner = egcl_rt::function::definition_index(*first);
    // The replacement is genuinely live through its explicitly held symbol;
    // a plain u32 diagnostic ID would not own a private symbol across major GC.
    egcl_rt::rooted!(_replacement_name = EgclVal::from_symbol_index(owner));
    let _old_code = install(*first);
    let replacement = template("((+ next ordinary-native-cell))", &env);
    registry_put(owner, Arc::clone(&replacement));
    INVOKE_COUNTS.with(|counts| {
        counts.borrow_mut().insert(owner, 123);
    });
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(registry_get(owner).is_some_and(|current| Arc::ptr_eq(&current, &replacement)));
    assert_eq!(
        INVOKE_COUNTS.with(|counts| counts.borrow().get(&owner).copied()),
        Some(123)
    );
    assert_eq!(
        closure_compilation_owner(egcl_rt::function::definition_index(*second), 1).0,
        egcl_rt::function::definition_index(*second)
    );
    // The old owner's different current body is not evidence for sharing.
    assert!(native_transfer_entry::funcall_fast::native_target(*second, 1).is_none());
    registry_remove(owner);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn mutated_owner_cells_do_not_turn_code_cache_into_a_closure_root() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    for plist in [false, true] {
        let mut env = Env::new(false);
        egcl_rt::rooted_ref!(_env = &mut env);
        let body = template(
            "((setq ordinary-native-cell next) ordinary-native-cell)",
            &env,
        );
        egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(71), &mut env));
        egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(22), &mut env));
        let owner = egcl_rt::function::definition_index(*first);
        let frame = Arc::downgrade(&closure_captured_env(*first).unwrap());
        let code = install(*first);
        assert!(egcl_rt::symbols::symbol_global_cells_are_pristine(owner));
        if plist {
            egcl_rt::rooted!(properties = crate::cli::arena_cons(*first, NIL));
            egcl_rt::symbols::set_symbol_plist(owner, *properties);
        } else {
            egcl_rt::symbols::set_symbol_value(owner, *first);
        }
        assert!(!egcl_rt::symbols::symbol_global_cells_are_pristine(owner));
        *first = NIL;
        // An executing old owner may temporarily retain its mutated name's
        // graph. Once it returns, cached native metadata must not do so.
        MAJOR_RESULT.with(|result| result.set(None));
        let _reset = Reset;
        native_transfer_entry::root_publication::OBSERVE
            .with(|hook| hook.set(Some(collect_major_in_capture)));
        assert_eq!(
            run_native_callable(&code, owner, *second, &[EgclVal::from_fixnum(23)], &mut env)
                .unwrap(),
            EgclVal::from_fixnum(23)
        );
        assert_eq!(MAJOR_RESULT.with(Cell::get), Some(true));
        assert!(
            frame.upgrade().is_some(),
            "active code temporarily owns its mutated symbol graph"
        );
        egcl_rt::gc::full_gc().unwrap();
        assert!(
            frame.upgrade().is_none(),
            "cache-only owner symbol must not root its value/plist cycle"
        );
        assert!(registry_get(owner).is_none());
        assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
        assert!(
            !CLOSURE_COMPILATION
                .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
        );
        assert!(
            native_transfer_entry::funcall_fast::native_target(*second, 1).is_none(),
            "a contaminated metadata lease falls back to recompilation"
        );
        assert!(
            code.is_t2,
            "retained external native code is not semantic ownership"
        );
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn late_owner_contamination_retires_metadata_after_instance_records_are_gone() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(1), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(2), &mut env));
    let owner = egcl_rt::function::definition_index(*first);
    let second_frame = Arc::downgrade(&closure_captured_env(*second).unwrap());
    let code = install(*first);
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(!closure_bodies().borrow().contains_key(&owner));
    assert!(NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    egcl_rt::symbols::set_symbol_value(owner, *second);
    *second = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(second_frame.upgrade().is_none());
    assert!(registry_get(owner).is_none());
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(!INVOKE_COUNTS.with(|counts| counts.borrow().contains_key(&owner)));
    assert!(
        !CLOSURE_COMPILATION
            .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
    );
    assert!(code.is_t2, "an external code cache is not a semantic owner");
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn late_function_cell_change_retires_old_code_without_removing_current_binding() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell next) ordinary-native-cell)",
        &env,
    );
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(1), &mut env));
    egcl_rt::rooted!(second = closure(&body, EgclVal::from_fixnum(2), &mut env));
    let owner = egcl_rt::function::definition_index(*first);
    let replacement_id = egcl_rt::function::definition_index(*second);
    let code = install(*first);
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(!closure_bodies().borrow().contains_key(&owner));
    assert!(NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    // A genuinely retained user symbol owns its new function binding. Only
    // the former compilation-owner cache is eligible for eviction.
    egcl_rt::rooted!(_name = EgclVal::from_symbol_index(owner));
    egcl_rt::symbols::set_symbol_function(owner, *second);
    *second = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(!REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(
        !CLOSURE_COMPILATION
            .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
    );
    let current = egcl_rt::symbols::symbol_function(owner).unwrap();
    assert!(egcl_rt::function::is_interpreted_function(current));
    assert_eq!(egcl_rt::function::definition_index(current), replacement_id);
    assert!(registry_get(replacement_id).is_some_and(|current| Arc::ptr_eq(&current, &body)));
    assert!(closure_captured_env(current).is_some());
    assert!(code.is_t2);
    egcl_rt::symbols::set_symbol_function(owner, egcl_rt::value::UNBOUND);
}

#[test]
fn captured_ordinary_admission_requires_a_surviving_callable_protocol() {
    use egcl_compiler::t2::ir::{
        AuxData, Function, IRType, InstData, InstFlags, Opcode, ValueRepresentation,
    };
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    let plain = |opcode| InstData {
        opcode,
        args: vec![],
        results: vec![],
        aux: AuxData::None,
        flags: InstFlags::default(),
        targets: vec![],
        frame_state: None,
        source_pos: 0,
    };
    for explicit in [false, true] {
        let mut ir = Function::new("REMOVED-CAPTURED-ACCESS");
        let entry = ir.entry();
        let argument = ir.add_block_param(entry, IRType::TOP, ValueRepresentation::Tagged);
        if explicit {
            ir.push_inst(
                entry,
                plain(Opcode::CurrentCallable),
                &[(IRType::TOP, ValueRepresentation::Tagged)],
            );
        }
        let mut ret = plain(Opcode::Return);
        ret.args.push(argument);
        ret.flags.terminator = true;
        ir.set_terminator(entry, ret);
        assert!(egcl_compiler::t2::verify::verify(&ir).is_ok());
        assert_eq!(
            native_transfer_entry::ordinary_capture_protocol_present(&body, &ir),
            explicit
        );
        let mut uncaptured = (*body).clone();
        uncaptured.has_env = false;
        assert!(native_transfer_entry::ordinary_capture_protocol_present(
            &uncaptured,
            &ir
        ));
    }
    let root = resolve_sym("CAPTURED-PROTOCOL-ADMISSION")
        .unwrap()
        .as_symbol_index();
    let semantic = egcl_compiler::t2::build::build_for_transfer_optimization(&body, root).unwrap();
    assert!(native_transfer_entry::ordinary_capture_protocol_present(
        &body, &semantic
    ));
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn native_registry_retires_private_admission_before_retained_code_dies() {
    use std::sync::atomic::Ordering;
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(function = closure(&body, EgclVal::from_fixnum(7), &mut env));
    let native = install(*function);
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped code")
    };
    let revision = code.private_entry_revision();
    let weak = Rc::downgrade(&native);
    let mut registry = NativeRegistry::default();
    registry.insert(1, Rc::clone(&native));
    let initial = revision.load(Ordering::Acquire);
    registry.insert(1, Rc::clone(&native));
    assert_eq!(
        revision.load(Ordering::Acquire),
        initial,
        "identical publication stays admitted"
    );
    registry.remove(&1);
    assert!(revision.load(Ordering::Acquire) > initial);
    assert!(
        weak.upgrade().is_some(),
        "retirement must precede NativeCode destruction"
    );
    for operation in 0..4 {
        registry.insert(1, Rc::clone(&native));
        let before = revision.load(Ordering::Acquire);
        match operation {
            0 => {
                registry.retain(|_, _| false);
            }
            1 => registry.clear(),
            2 => {
                let mut replacement = NativeRegistry::default();
                std::mem::swap(&mut registry, &mut replacement);
                drop(replacement);
            }
            _ => {
                registry.insert(2, Rc::clone(&native));
                registry.remove(&2);
            }
        }
        assert!(revision.load(Ordering::Acquire) > before);
        assert!(weak.upgrade().is_some());
    }
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn private_probe_lease_keeps_physical_maps_but_not_native_installation_alive() {
    use std::sync::atomic::Ordering;
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("(ordinary-native-cell)", &env);
    egcl_rt::rooted!(function = closure(&body, EgclVal::from_fixnum(9), &mut env));
    let symbol = egcl_rt::function::definition_index(*function);
    let native = install(*function);
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped code")
    };
    let code = Rc::clone(code);
    let weak = Rc::downgrade(&native);
    assert!(
        egcl_rt::call_table::resolve_ordinal(symbol).is_none(),
        "private owners must not consume named ordinals"
    );
    let segment = call_table::enter_ordinary_segment();
    let directory = segment.directory();
    let (descriptor, revision, seen) =
        call_table::private_probe_target(Rc::clone(&code), weak.clone())
            .expect("private descriptor lease");
    let descriptor =
        unsafe { &*(descriptor as *const egcl_rt::call_table::OrdinaryEntryDescriptor) };
    let pc = descriptor.code_start;
    assert!(directory.code_for_pc(pc).is_some());
    let admission = unsafe { &*(revision as *const std::sync::atomic::AtomicU64) };
    assert_eq!(admission.load(Ordering::Acquire), seen);
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().remove(&symbol));
    assert_ne!(
        admission.load(Ordering::Acquire),
        seen,
        "removal revokes a still-live NativeCode"
    );
    assert!(weak.upgrade().is_some());
    drop(native);
    assert!(
        weak.upgrade().is_none(),
        "directory must not own NativeCode"
    );
    assert!(
        directory.code_for_pc(pc).is_some(),
        "active frames retain physical maps"
    );
    drop(segment);
    assert!(
        directory.code_for_pc(pc).is_none(),
        "quiescent exit retires invalid private descriptors"
    );
}

thread_local! {
    static EPHEMERAL_FRAME: RefCell<std::sync::Weak<SharedCell<EnvFrame>>> = const { RefCell::new(std::sync::Weak::new()) };
    static EPHEMERAL_FACTORY_SLOT: Cell<u32> = const { Cell::new(0) };
    static EPHEMERAL_CHILD: Cell<usize> = const { Cell::new(0) };
    static EPHEMERAL_PARENT: Cell<usize> = const { Cell::new(0) };
    static EPHEMERAL_ALIVE: Cell<Option<bool>> = const { Cell::new(None) };
}
// This leaf neither allocates nor polls. The global owns B until this exact
// read/clear; the native stub then transports the return bits without a call.
unsafe extern "C" fn ephemeral_factory_leaf() -> u64 {
    let slot = EPHEMERAL_FACTORY_SLOT.with(Cell::get);
    let value = egcl_rt::symbols::symbol_value(slot).expect("one-shot callable slot");
    egcl_rt::symbols::set_symbol_value(slot, NIL);
    value.0
}
fn collect_before_ephemeral_guard_resume(_words: *const u64, _count: usize, _raw: bool) {
    assert!(
        native_transfer_entry::root_publication::published_frame_has_owner_for_test(
            EPHEMERAL_CHILD.with(Cell::get)
        ),
        "the precise child frame must be visible"
    );
    assert!(
        !native_transfer_entry::root_publication::published_frame_has_owner_for_test(
            EPHEMERAL_PARENT.with(Cell::get)
        ),
        "tail caller must not retain an outgoing callable copy"
    );
    let weak = EPHEMERAL_FRAME.with(|slot| slot.borrow().clone());
    egcl_rt::gc::full_gc().expect("major at the exact deoptimizing child boundary");
    EPHEMERAL_ALIVE.with(|result| result.set(Some(weak.upgrade().is_some())));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn generated_ephemeral_child_keeps_its_capture_through_deoptimizing_hidden_home() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((if next (+ ordinary-native-cell 1) ordinary-native-cell))",
        &env,
    );
    egcl_rt::rooted!(_body = ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(first = closure(&body, EgclVal::from_fixnum(3), &mut env));
    let code = install(*first);
    let NativeCodeStorage::Mapped(callee) = &code._storage else {
        panic!("mapped captured code")
    };
    assert!(callee.ordinary_entry.is_some() && callee.native_callable_slot.is_some());
    egcl_rt::rooted!(payload = egcl_rt::gc::alloc_double_float(2.5));
    egcl_rt::rooted!(fresh = closure(&body, *payload, &mut env));
    let weak = Arc::downgrade(&closure_captured_env(*fresh).unwrap());
    EPHEMERAL_FRAME.with(|slot| *slot.borrow_mut() = weak.clone());
    EPHEMERAL_CHILD.with(|slot| slot.set(Rc::as_ptr(callee) as usize));
    let slot = resolve_sym("*EPHEMERAL-CAPTURED-SLOT*")
        .unwrap()
        .as_symbol_index();
    egcl_rt::symbols::set_symbol_value(slot, NIL);
    EPHEMERAL_FACTORY_SLOT.with(|value| value.set(slot));
    let (primer_symbol, primer) = ordinary_t2::install(
        "EPHEMERAL-CAPTURED-PRIMER",
        "(f)",
        "((funcall f nil))",
        &mut env,
    );
    egcl_rt::rooted!(caller_forms = reader::read_from_string("(nil)").unwrap().0);
    let caller_body = Arc::new(
        compile_function(
            "EPHEMERAL-CAPTURED-CALLER",
            NIL,
            *caller_forms,
            &env,
            false,
            false,
        )
        .expect("zero-argument fixture body"),
    );
    let mut parent = Box::new(native_transfer_entry::TransferCode::compile(caller_body).unwrap());
    // Finish every target publication before priming the existing shared PIC.
    for _ in 0..4 {
        assert_eq!(
            run_native(&primer, primer_symbol, &[*fresh], &mut env)
                .unwrap()
                .as_double_float(),
            2.5
        );
    }
    let funcall = resolve_sym("FUNCALL").unwrap().as_symbol_index();
    let cell = call_table::resolve(funcall).expect("canonical FUNCALL cell");
    let descriptor =
        unsafe { &*cell.ordinary_entry_address() }.load(std::sync::atomic::Ordering::Acquire);
    assert_ne!(descriptor, 0);
    let entry =
        unsafe { &*(descriptor as *const egcl_rt::call_table::OrdinaryEntryDescriptor) }.entry;
    // Ordinary no-arg entry: aligned noncollecting factory CALL, followed by
    // the production FUNCALL probe. Restore RSP before the tail JMP, preserve
    // every callee-saved register/R13, and never hold B in a host argument.
    let mut stub = vec![0x48, 0x83, 0xec, 0x08, 0x48, 0xb8];
    stub.extend_from_slice(&(ephemeral_factory_leaf as *const () as u64).to_le_bytes());
    stub.extend_from_slice(&[
        0xff, 0xd0, 0x48, 0x83, 0xc4, 0x08, 0x48, 0x89, 0xc7, 0x48, 0xbe,
    ]);
    stub.extend_from_slice(&egcl_rt::value::T.0.to_le_bytes());
    stub.extend_from_slice(&[0x48, 0xc7, 0xc1, 2, 0, 0, 0, 0x48, 0xb8]);
    stub.extend_from_slice(&NIL.0.to_le_bytes());
    stub.extend_from_slice(&[0x49, 0xbb]);
    stub.extend_from_slice(&(entry as u64).to_le_bytes());
    stub.extend_from_slice(&[0x41, 0xff, 0xe3]);
    // SAFETY: the only CALL is the nonallocating leaf above. The frame is
    // already retired when the real child's precise guard can collect.
    let _original_code = unsafe { parent.replace_nongc_tail_entry_for_test(&stub) }.unwrap();
    EPHEMERAL_PARENT.with(|value| value.set(std::ptr::from_ref(&*parent) as usize));
    egcl_rt::symbols::set_symbol_value(slot, *fresh);
    *fresh = NIL;
    *first = NIL;
    *payload = NIL;
    EPHEMERAL_ALIVE.with(|result| result.set(None));
    let before = callee.deopt_count();
    let _ = native_transfer_entry::funcall_fast::take_fast_calls_for_test();
    let result = native_transfer_entry::with_ordinary_guard_payload_observer_for_test(
        collect_before_ephemeral_guard_resume,
        || parent.run_ordinary_root(0, NIL, &[], &mut env),
    );
    assert_eq!(result.unwrap().as_double_float(), 3.5);
    assert_eq!(EPHEMERAL_ALIVE.with(Cell::get), Some(true));
    assert_eq!(callee.deopt_count(), before + 1);
    assert_eq!(
        native_transfer_entry::funcall_fast::take_fast_calls_for_test().0,
        0,
        "the child must enter by generated probe, without a Rust callable root"
    );
    assert_eq!(egcl_rt::symbols::symbol_value(slot), Some(NIL));
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        weak.upgrade().is_none(),
        "the hidden callable must stop retaining its frame after return"
    );
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn named_factory_keeps_flat_noenv_code_but_not_its_first_instance_alive() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((1+ next))").unwrap().0);
    let body = Arc::new(
        compile_function("FLAT-NOENV-TEMPLATE", *params, *forms, &env, false, false).unwrap(),
    );
    assert!(!body.has_env && closure_body_has_acyclic_owner_lease(&body));
    let mut factory = (*body).clone();
    factory.name = "FLAT-NOENV-SEMANTIC-FACTORY".to_owned();
    factory.code = vec![
        Instr::MakeClosure {
            func: 0,
            capture_env: false,
        },
        Instr::Return,
    ];
    factory.constants.clear();
    factory.nested_functions = vec![Arc::clone(&body)];
    let factory = Arc::new(factory);
    let name = resolve_sym("FLAT-NOENV-SEMANTIC-FACTORY")
        .unwrap()
        .as_symbol_index();
    egcl_rt::rooted!(factory_function = make_bytecode_closure(&factory, None));
    egcl_rt::symbols::set_symbol_function(name, *factory_function);
    registry_put(name, factory);
    *factory_function = NIL;
    egcl_rt::rooted!(first = make_bytecode_closure(&body, None));
    let owner = egcl_rt::function::definition_index(*first);
    let code = install(*first);
    assert!(code.is_t2);
    *first = NIL;
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        !closure_bodies().borrow().contains_key(&owner),
        "dead instance records must disappear"
    );
    assert!(registry_get(owner).is_some_and(|current| Arc::ptr_eq(&current, &body)));
    assert!(NATIVE_REGISTRY.with(|registry| {
        registry
            .borrow()
            .get(&owner)
            .is_some_and(|current| Rc::ptr_eq(current, &code))
    }));
    egcl_rt::rooted!(second = make_bytecode_closure(&body, None));
    assert_eq!(
        closure_compilation_owner(egcl_rt::function::definition_index(*second), 1).0,
        owner
    );
    assert_eq!(
        run_native_callable(&code, owner, *second, &[EgclVal::from_fixnum(8)], &mut env).unwrap(),
        EgclVal::from_fixnum(9)
    );
    *second = NIL;
    egcl_rt::symbols::set_symbol_function(name, egcl_rt::value::UNBOUND);
    registry_remove(name);
    egcl_rt::gc::full_gc().unwrap();
    assert!(!REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(
        !CLOSURE_COMPILATION
            .with(|cache| cache.borrow().contains_key(&(Arc::as_ptr(&body) as usize)))
    );
    assert!(
        code.is_t2,
        "an external code Rc must not manufacture a semantic owner"
    );
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn private_identity_cache_reuses_until_collection_and_clears_retired_external_code() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(
        params = reader::read_from_string("(next unused1 unused2 unused3)")
            .unwrap()
            .0
    );
    egcl_rt::rooted!(forms = reader::read_from_string("((1+ next))").unwrap().0);
    // Arity four deliberately retains the fallback identity-cache ABI.
    // Fixed zero-to-three argument leaves use a precise callable home instead.
    let body = Arc::new(
        compile_function("PRIVATE-CACHE-REUSE", *params, *forms, &env, false, false).unwrap(),
    );
    egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
    let owner = egcl_rt::function::definition_index(*function);
    let code = install(*function);
    let _ = call_table::take_root_identities_for_test();
    for _ in 0..2 {
        assert_eq!(
            run_native_callable(
                &code,
                owner,
                *function,
                &[EgclVal::from_fixnum(8), NIL, NIL, NIL],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(9)
        );
    }
    assert_eq!(
        call_table::take_root_identities_for_test(),
        1,
        "steady calls must reuse the active identity allocation"
    );
    egcl_rt::gc::full_gc().unwrap();
    for _ in 0..2 {
        assert_eq!(
            run_native_callable(
                &code,
                owner,
                *function,
                &[EgclVal::from_fixnum(8), NIL, NIL, NIL],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(9)
        );
    }
    assert_eq!(
        call_table::take_root_identities_for_test(),
        1,
        "a collection evicts once, then reuse resumes"
    );
    NATIVE_REGISTRY.with(|registry| registry.borrow_mut().remove(&owner));
    *function = NIL;
    // The external NativeCode Rc still owns its cache, but it is no longer in
    // NATIVE_REGISTRY. The weak dirty token must cover this owner as well.
    egcl_rt::gc::full_gc().unwrap();
    assert!(!closure_bodies().borrow().contains_key(&owner));
    assert!(!REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(code.is_t2);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn private_identity_cache_requeues_a_spare_after_collection_inside_its_active_guard() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted!(params = reader::read_from_string("(next)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string("((1+ next))").unwrap().0);
    let body = Arc::new(
        compile_function(
            "PRIVATE-CACHE-ACTIVE-GC",
            *params,
            *forms,
            &env,
            false,
            false,
        )
        .unwrap_or_else(|| {
            panic!(
                "private cache fixture compilation failed: {:?}",
                last_bail_reason()
            )
        }),
    );
    egcl_rt::rooted!(function = make_bytecode_closure(&body, None));
    let owner = egcl_rt::function::definition_index(*function);
    let code = install(*function);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("mapped private code")
    };
    let before = mapped.deopt_count();
    egcl_rt::rooted!(input = egcl_rt::gc::alloc_double_float(2.5));
    MAJOR_RESULT.with(|result| result.set(None));
    fn collect_after_guard_payload_rooted(_: *const u64, _: usize, _: bool) {
        MAJOR_RESULT.with(|result| result.set(Some(egcl_rt::gc::full_gc().is_ok())));
    }
    // This boundary has rooted the serialized native payload, but has not yet
    // recovered the callable through the active directory identity.
    let result = native_transfer_entry::with_ordinary_guard_payload_observer_for_test(
        collect_after_guard_payload_rooted,
        || run_native_callable(&code, owner, *function, &[*input], &mut env),
    );
    assert_eq!(result.unwrap().as_double_float(), 3.5);
    assert_eq!(
        mapped.deopt_count(),
        before + 1,
        "real private guard recovery must survive cache eviction"
    );
    assert_eq!(MAJOR_RESULT.with(Cell::get), Some(true));
    *function = NIL;
    // No intervening call may refill last_callable. Only the spare identity
    // cached when the active guard returned can keep the callable alive here.
    egcl_rt::gc::full_gc().unwrap();
    assert!(!closure_bodies().borrow().contains_key(&owner));
    assert!(!REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(!NATIVE_REGISTRY.with(|registry| registry.borrow().contains_key(&owner)));
    assert!(code.is_t2);
}

#[test]
fn captured_entry_cost_policy_counts_accesses_and_real_loop_backedges() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let read = template("((+ next ordinary-native-cell))", &env);
    let mutate = template(
        "((setq ordinary-native-cell (+ ordinary-native-cell 1)) (+ next ordinary-native-cell))",
        &env,
    );
    let repeated = template(
        "((let ((sum 0)) (dotimes (i 2 sum) (setq sum (+ sum ordinary-native-cell)))))",
        &env,
    );
    assert!(native_transfer_entry::ordinary_capture_entry_profitable(
        &read
    ));
    assert!(native_transfer_entry::ordinary_capture_entry_profitable(
        &mutate
    ));
    assert!(!native_transfer_entry::ordinary_capture_entry_profitable(
        &repeated
    ));
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn automatic_capture_policy_uses_ordinary_entry_for_one_access() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template("((+ next ordinary-native-cell))", &env);
    egcl_rt::rooted!(function = closure(&body, EgclVal::from_fixnum(7), &mut env));
    let owner = egcl_rt::function::definition_index(*function);
    let code = install_automatic(*function);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("native code")
    };
    assert!(code.is_t2 && mapped.ordinary_entry.is_some() && mapped.native_callable_slot.is_some());
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    assert_eq!(
        run_native_callable(
            &code,
            owner,
            *function,
            &[EgclVal::from_fixnum(3)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(10)
    );
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 1);
}

#[test]
#[ignore = "requires a platform-supported native compiler"]
fn automatic_capture_policy_uses_ordinary_leaf_for_multiple_accesses() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let body = template(
        "((setq ordinary-native-cell (+ ordinary-native-cell 1)) (+ next ordinary-native-cell))",
        &env,
    );
    egcl_rt::rooted!(function = closure(&body, EgclVal::from_fixnum(7), &mut env));
    let owner = egcl_rt::function::definition_index(*function);
    let code = install_automatic(*function);
    let NativeCodeStorage::Mapped(mapped) = &code._storage else {
        panic!("native code")
    };
    assert!(code.is_t2 && mapped.ordinary_entry.is_some() && mapped.native_callable_slot.is_some());
    let _ = native_transfer_entry::take_lean_root_entries_for_test();
    assert_eq!(
        run_native_callable(
            &code,
            owner,
            *function,
            &[EgclVal::from_fixnum(3)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(11)
    );
    assert_eq!(native_transfer_entry::take_lean_root_entries_for_test(), 1);
}
