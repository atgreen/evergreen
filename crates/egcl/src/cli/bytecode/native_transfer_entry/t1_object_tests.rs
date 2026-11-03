// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::super::super::native_transfer_tests::ordinary_t2;
use super::*;
use egcl_rt::{Collector, HeapCollector};
use std::sync::atomic::Ordering;

fn mapped(native: &NativeCode) -> &Rc<TransferCode> {
    let NativeCodeStorage::Mapped(code) = &native._storage else {
        panic!("mapped native owner")
    };
    code
}
fn install_t1(env: &mut Env, name: &str, forms: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} (x) {forms})"), env).unwrap();
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
    assert!(!native.is_t2 && mapped(&native).lean_root_entry().is_some());
    publish_native(symbol, Some(*function), &native);
    (symbol, native)
}
fn install_parent(env: &mut Env, name: &str, params: &str, forms: &str) -> (u32, Rc<NativeCode>) {
    crate::cli::read_eval_all_env(&format!("(defun {name} {params} {forms})"), env).unwrap();
    let result = ordinary_t2::install(name, params, &format!("({forms})"), env);
    assert!(
        mapped(&result.1).ordinary_entry.is_none(),
        "actual mapped caller ABI"
    );
    result
}
fn run_parent(
    symbol: u32,
    native: &NativeCode,
    args: &[EgclVal],
    env: &mut Env,
) -> Result<EgclVal, EgclError> {
    mapped(native).run_callable(
        egcl_rt::symbols::symbol_function(symbol).unwrap(),
        args,
        env,
    )
}
fn heat(function: EgclVal, invokes: u32, edges: u32) {
    unsafe {
        (*egcl_rt::function::invocation_counter_address(function).unwrap())
            .store(invokes, Ordering::Relaxed);
        (*egcl_rt::function::back_edge_counter_address(function).unwrap())
            .store(edges, Ordering::Relaxed);
    }
}
struct NativeEnv(*mut Env);
impl NativeEnv {
    fn install(env: &mut Env) -> Self {
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnv {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}
struct Generation(u32, Option<u64>);
impl Drop for Generation {
    fn drop(&mut self) {
        REGISTRY_GENERATION.with(|map| {
            let mut map = map.borrow_mut();
            if let Some(value) = self.1 {
                map.insert(self.0, value);
            } else {
                map.remove(&self.0);
            }
        });
    }
}
struct LocalBody(u32, Option<Arc<BytecodeFunction>>);
impl Drop for LocalBody {
    fn drop(&mut self) {
        if let Some(body) = self.1.take() {
            REGISTRY.with(|map| map.borrow_mut().insert(self.0, body));
        }
    }
}

#[test]
fn t1_object_preparation_is_read_only_and_declines_stale_generation_and_promotion_boundaries() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_enabled() && !profiling_disabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install_t1(&mut env, "T1-OBJECT-PURE", "x");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let _native_env = NativeEnv::install(&mut env);
    heat(*function, 0, 0);
    for _ in 0..4 {
        assert!(t1_candidate(*function, 1).is_some());
    }
    assert_eq!(egcl_rt::function::invoke_count(*function), 0);
    assert!(
        native_target(*function, 1).is_none(),
        "T1 never enters T2 exports/table"
    );
    assert!(!is_cached_for_test(*function));
    let sizes = registry_sizes();
    let missing = Generation(
        symbol,
        REGISTRY_GENERATION.with(|map| map.borrow_mut().remove(&symbol)),
    );
    assert!(t1_candidate(*function, 1).is_none());
    assert!(
        REGISTRY_GENERATION.with(|map| !map.borrow().contains_key(&symbol)),
        "query cannot synchronize local generation"
    );
    assert!(NATIVE_REGISTRY.with(|map| {
        map.borrow()
            .get(&symbol)
            .is_some_and(|current| Rc::ptr_eq(current, &native))
    }));
    assert_eq!(registry_sizes().0, sizes.0);
    drop(missing);
    assert!(t1_candidate(*function, 1).is_some());
    for (invokes, edges) in [(t2_invoke_threshold() - 1, 0), (0, t2_backedge_threshold())] {
        heat(*function, invokes, edges);
        assert!(t1_candidate(*function, 1).is_none());
        assert_eq!(egcl_rt::function::invoke_count(*function), invokes);
        assert!(!T2_QUEUED.with(|queue| queue.borrow().contains_key(&symbol)));
    }
    heat(*function, 0, 0);
    assert!(t1_candidate(*function, 0).is_none());
    assert!(t1_candidate(*function, 2).is_none());
    egcl_rt::rooted!(private_name = egcl_rt::symbols::make_uninterned("T1-OBJECT-LEGACY"));
    egcl_rt::rooted!(legacy = egcl_rt::function::alloc_interpreted(NIL, NIL, NIL, *private_name));
    assert!(!egcl_rt::function::is_closure_instance(*legacy));
    assert!(
        t1_candidate(*legacy, 1).is_none(),
        "legacy representative aggregation is excluded"
    );
    egcl_rt::rooted!(wrapper = arena_cons(*private_name, NIL));
    assert!(
        t1_candidate(*wrapper, 1).is_none(),
        "cons wrappers keep their canonical dispatcher"
    );
    let old_depth = NATIVE_DEPTH.with(|slot| slot.replace(native_depth_cap()));
    let rejected = t1_candidate(*function, 1).is_none();
    NATIVE_DEPTH.with(|slot| slot.set(old_depth));
    assert!(rejected);
    env.locally_specials.push(0x8000_0104);
    assert!(t1_candidate(*function, 1).is_none());
    env.locally_specials.clear();
    let pin = ordinary_t2::RecursiveFixturePins::new(&["T1-OBJECT-PURE"]);
    assert!(t1_candidate(*function, 1).is_none());
    drop(pin);
    assert_eq!(egcl_rt::function::invoke_count(*function), 0);
    crate::cli::read_eval_all_env("(defun t1-object-pure (x) :replacement)", &mut env).unwrap();
    assert!(
        t1_candidate(*function, 1).is_none(),
        "retained old definition stays on canonical fallback"
    );
    assert_eq!(egcl_rt::function::invoke_count(*function), 0);
}

thread_local! { static NATIVE_ENTRIES: Cell<usize> = const { Cell::new(0) }; }
fn native_observer() {
    if t1_object_enabled() && std::env::var("EGCL_MAPPED_T1_ORDINARY").as_deref() != Ok("0") {
        assert_eq!(super::super::activation_counts_for_test(), (0, 0));
    }
    NATIVE_ENTRIES.with(|slot| slot.set(slot.get() + 1));
}
struct Observer(Option<fn()>);
impl Observer {
    fn install() -> Self {
        NATIVE_ENTRIES.with(|slot| slot.set(0));
        Self(ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(native_observer))))
    }
}
impl Drop for Observer {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        super::super::take_activation_counts_for_test();
    }
}
fn reset_entries(symbol: u32) {
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::take_nested_entries();
    super::super::mapped_generated::take_fast_entries_for_test();
    super::super::start_activation_counts_for_test(symbol);
}
fn assert_entries(count: usize) {
    assert_eq!(
        NATIVE_ENTRIES.with(Cell::get),
        count,
        "every actual native body executes its marker"
    );
    assert_eq!(mapped_ordinary::take_entries_for_test(), count);
    assert_eq!(mapped_ordinary::take_prepares_for_test(), 1);
    assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
    assert_eq!(super::super::take_nested_entries(), 0);
    assert_eq!(
        super::super::mapped_generated::take_fast_entries_for_test(),
        0,
        "T1 must never bypass canonical credit in a generated lane"
    );
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_t1_object_calls_enter_exact_native_body_and_credit_one_hundred_times() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t2_invoke_threshold() > 101 && !profiling_disabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun t1-object-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["T1-OBJECT-OBSERVE"]);
    let (symbol, child) = install_t1(&mut env, "T1-OBJECT-INNER", "(progn (t1-object-observe) x)");
    let calls = vec!["(funcall f x)"; 100].join(" ");
    let (parent_symbol, parent) = install_parent(
        &mut env,
        "T1-OBJECT-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        &format!("(progn {calls})"),
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    heat(*function, 0, 0);
    ordinary_t2::expect_entry(&child);
    reset_entries(symbol);
    let _observing = Observer::install();
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[*function, EgclVal::from_fixnum(47)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(47)
    );
    assert!(ordinary_t2::observed_entry() && !child.is_t2);
    assert_eq!(egcl_rt::function::invoke_count(*function), 100);
    if t1_object_enabled() && call_table::prefer_ordinary_child(&child, *function, t2_enabled()) {
        assert_entries(100);
    } else {
        assert!(
            mapped(&child).mapped_entry().is_none(),
            "this runtime-calling T1 fixture keeps original ordinary root fallback"
        );
        assert_eq!(NATIVE_ENTRIES.with(Cell::get), 100);
        assert_eq!(mapped_ordinary::take_entries_for_test(), 0);
        assert_eq!(mapped_ordinary::take_prepares_for_test(), 0);
        assert_eq!(super::super::take_lean_root_entries_for_test(), 100);
        assert_eq!(super::super::take_nested_entries(), 0);
        assert_eq!(
            super::super::mapped_generated::take_fast_entries_for_test(),
            0
        );
        assert_eq!(super::super::take_activation_counts_for_test(), (100, 100));
    }
    assert!(native_target(*function, 1).is_none());
    assert!(!is_cached_for_test(*function));
}

#[test]
#[ignore = "requires native entry and the normal asynchronous T2 broker"]
fn t1_object_threshold_calls_defer_before_credit_and_publish_real_t2_for_both_heat_sources() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_enabled() && !profiling_disabled());
    assert_ne!(std::env::var("EGCL_T2_NO_QUEUE").as_deref(), Ok("1"));
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    for edges_hot in [false, true] {
        let suffix = if edges_hot { "EDGE" } else { "CALL" };
        let (symbol, original) =
            install_t1(&mut env, &format!("T1-OBJECT-THRESHOLD-{suffix}"), "x");
        let (parent_symbol, parent) = install_parent(
            &mut env,
            &format!("T1-OBJECT-THRESHOLD-PARENT-{suffix}"),
            "(f x &key (k (1+ 0)) &aux (u 0))",
            "(funcall f x)",
        );
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        let initial = if edges_hot {
            0
        } else {
            t2_invoke_threshold() - 1
        };
        heat(
            *function,
            initial,
            if edges_hot {
                t2_backedge_threshold()
            } else {
                0
            },
        );
        {
            let _native_env = NativeEnv::install(&mut env);
            assert!(t1_candidate(*function, 1).is_none());
            assert_eq!(egcl_rt::function::invoke_count(*function), initial);
        }
        mapped_ordinary::take_entries_for_test();
        assert_eq!(
            run_parent(parent_symbol, &parent, &[*function, T], &mut env).unwrap(),
            T
        );
        assert_eq!(egcl_rt::function::invoke_count(*function), initial + 1);
        assert_eq!(
            mapped_ordinary::take_entries_for_test(),
            0,
            "crossing invocation stays on canonical cold route"
        );
        let replacement = wait_for_t2(symbol).expect("actual broker completion");
        assert!(replacement.is_t2 && !Rc::ptr_eq(&replacement, &original));
        let before = egcl_rt::function::invoke_count(*function);
        assert_eq!(
            run_parent(parent_symbol, &parent, &[*function, NIL], &mut env).unwrap(),
            NIL
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            before,
            "T2 owns no further T1 heat"
        );
        assert!(
            native_target(*function, 1).is_some(),
            "existing T2 export only after real T2 publication"
        );
    }
}

struct MovingProbe {
    owner: usize,
    function: *const EgclVal,
    old: *const EgclVal,
    next: *const EgclVal,
    observed: bool,
}
thread_local! { static MOVING: Cell<*mut MovingProbe> = const { Cell::new(std::ptr::null_mut()) }; }
fn collect_t1_template() {
    let probe = MOVING.with(Cell::get);
    if probe.is_null() || unsafe { (*probe).observed } {
        return;
    }
    let Some((_, owner)) = super::super::root_publication::active_anchor_for_test() else {
        return;
    };
    if owner != unsafe { (*probe).owner } {
        return;
    }
    assert_eq!(super::super::child_capture_for_test(), Some((owner, false)));
    assert_eq!(super::super::activation_counts_for_test(), (0, 0));
    let before = unsafe { [*(*probe).function, *(*probe).old, *(*probe).next] };
    unsafe {
        (*probe).observed = true;
    }
    HeapCollector::new().minor_gc().unwrap();
    let after = unsafe { [*(*probe).function, *(*probe).old, *(*probe).next] };
    for index in 0..3 {
        assert_ne!(
            before[index], after[index],
            "live callable/capture/argument home {index} must relocate in exact T1"
        );
    }
}
struct Publication(Option<fn()>);
impl Drop for Publication {
    fn drop(&mut self) {
        super::super::root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.0));
        MOVING.with(|slot| slot.set(std::ptr::null_mut()));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn t1_object_templates_share_heat_without_sharing_captures_and_move_exact_native_homes() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_invoke_threshold() > 102);
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun t1-object-template-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["T1-OBJECT-TEMPLATE-OBSERVE"]);
    egcl_rt::rooted!(params = reader::read_from_string("(start)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(
        "((let ((total start)) (lambda (next) (let ((old total)) (setq total next) (t1-object-template-observe) old))))").unwrap().0);
    let maker = Arc::new(
        compile_function(
            "T1-OBJECT-TEMPLATE-MAKER",
            *params,
            *forms,
            &env,
            true,
            false,
        )
        .unwrap(),
    );
    egcl_rt::rooted!(_maker = super::super::super::ActiveBytecodeRoot::new(&maker));
    let body = Arc::clone(&maker.nested_functions[0]);
    assert!(!body.has_env && body.code.iter().any(|op| matches!(op, Instr::ClosureEnv)));
    assert!(super::super::t1_ordinary_heap_access_body(&body));
    assert!(
        super::super::ordinary_capture_entry_profitable(&body),
        "heap captures use automatic !has_env cost policy, no force hook"
    );
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
    assert!(
        egcl_rt::function::is_closure_instance(*first)
            && egcl_rt::function::is_closure_instance(*second)
    );
    let symbol = egcl_rt::function::definition_index(*first);
    assert_eq!(symbol, egcl_rt::function::definition_index(*second));
    let child = super::super::install_t1(symbol, Arc::clone(&body), false)
        .expect("real ordinary T1 heap capture");
    publish_native(symbol, Some(*first), &child);
    assert!(
        Arc::ptr_eq(&mapped(&child).body, &body),
        "install and maker use the same nested Arc"
    );
    assert!(!child.is_t2 && mapped(&child).native_callable_slot.is_some());
    let calls = (0..100)
        .map(|i| {
            format!(
                "(funcall {} {i})",
                if i % 2 == 0 { "first" } else { "second" }
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let (parent_symbol, parent) = install_parent(
        &mut env,
        "T1-OBJECT-TEMPLATE-PARENT",
        "(first second &key (k (1+ 0)) &aux (u 0))",
        &format!("(list {calls})"),
    );
    {
        let _native_env = NativeEnv::install(&mut env);
        assert!(t1_candidate(*first, 1).is_some());
        let local = LocalBody(
            symbol,
            REGISTRY.with(|map| map.borrow_mut().remove(&symbol)),
        );
        assert!(t1_candidate(*first, 1).is_none());
        assert!(
            REGISTRY.with(|map| !map.borrow().contains_key(&symbol)),
            "query must not backfill immutable private body"
        );
        assert!(NATIVE_REGISTRY.with(|map| {
            map.borrow()
                .get(&symbol)
                .is_some_and(|value| Rc::ptr_eq(value, &child))
        }));
        drop(local);
    }
    heat(*first, 0, 0);
    ordinary_t2::expect_entry(&child);
    reset_entries(symbol);
    let observing = Observer::install();
    egcl_rt::rooted!(
        result = run_parent(parent_symbol, &parent, &[*first, *second], &mut env).unwrap()
    );
    let mut previous = [11, 22];
    let expected: Vec<_> = (0..100)
        .map(|i| {
            let prior = previous[i % 2];
            previous[i % 2] = i as i64;
            EgclVal::from_fixnum(prior)
        })
        .collect();
    assert_eq!(list_to_vec(*result), expected);
    assert!(ordinary_t2::observed_entry());
    assert_eq!(egcl_rt::function::invoke_count(*first), 100);
    assert_eq!(
        egcl_rt::function::invoke_count(*second),
        100,
        "shared prototype aggregates each call once"
    );
    assert_entries(100);
    drop(observing);
    assert!(native_target(*first, 1).is_none() && native_target(*second, 1).is_none());
    assert_eq!(egcl_rt::function::entry_word(*first), Some((0, 0)));
    assert_eq!(mapped(&child).template_claim_count_for_test(), 0);
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(
        fresh = heap_env::make_closure(&body, egcl_rt::function::closure_env(*first), &[], &env)
            .unwrap()
    );
    assert!(!egcl_rt::function::is_pinned(*fresh));
    egcl_rt::rooted!(old = arena_cons(EgclVal::from_fixnum(71), NIL));
    egcl_rt::rooted!(next = arena_cons(EgclVal::from_fixnum(72), NIL));
    let (depth, index) = body
        .code
        .iter()
        .find_map(|op| match op {
            Instr::EnvRef { depth, idx, .. } => Some((*depth, *idx)),
            _ => None,
        })
        .unwrap();
    assert!(egcl_rt::lexenv::store(
        egcl_rt::function::closure_env(*fresh),
        depth,
        index,
        *old
    ));
    let (moving_symbol, moving_parent) = install_parent(
        &mut env,
        "T1-OBJECT-MOVING-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(funcall f x)",
    );
    // Compile/setup before minting the nursery witness: a final minor here
    // would promote it and make an apparent stress pass vacuous.
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(
        fresh = heap_env::make_closure(&body, egcl_rt::function::closure_env(*fresh), &[], &env)
            .unwrap()
    );
    *old = arena_cons(EgclVal::from_fixnum(81), NIL);
    *next = arena_cons(EgclVal::from_fixnum(82), NIL);
    assert!(egcl_rt::lexenv::store(
        egcl_rt::function::closure_env(*fresh),
        depth,
        index,
        *old
    ));
    let mut probe = MovingProbe {
        owner: Rc::as_ptr(mapped(&child)) as usize,
        function: &*fresh,
        old: &*old,
        next: &*next,
        observed: false,
    };
    MOVING.with(|slot| slot.set(&mut probe));
    let _publication = Publication(
        super::super::root_publication::PROFILE_OBSERVE
            .with(|slot| slot.replace(Some(collect_t1_template))),
    );
    ordinary_t2::expect_entry(&child);
    reset_entries(symbol);
    egcl_rt::rooted!(
        answer = run_parent(moving_symbol, &moving_parent, &[*fresh, *next], &mut env).unwrap()
    );
    assert!(probe.observed && ordinary_t2::observed_entry());
    assert_eq!(*answer, *old);
    assert_eq!(cp(*answer), (EgclVal::from_fixnum(81), NIL));
    assert_eq!(
        egcl_rt::lexenv::load(egcl_rt::function::closure_env(*fresh), depth, index),
        Some(*next)
    );
    assert_eq!(egcl_rt::function::invoke_count(*first), 101);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn rust_outer_t1_object_calls_keep_root_ingress_and_retained_definition_semantics() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, child) = install_t1(&mut env, "T1-OBJECT-OUTER", "(+ x 1)");
    egcl_rt::rooted!(old = egcl_rt::symbols::symbol_function(symbol).unwrap());
    heat(*old, 0, 0);
    {
        let _native_env = NativeEnv::install(&mut env);
        assert!(
            t1_candidate(*old, 1).is_some(),
            "valid target alone is not ingress authority"
        );
    }
    assert!(apply(*old, &[EgclVal::from_fixnum(41)], &mut env, Admit::Native).is_none());
    assert_eq!(egcl_rt::function::invoke_count(*old), 0);
    mapped_ordinary::take_entries_for_test();
    super::super::take_lean_root_entries_for_test();
    let funcall = egcl_rt::symbols::intern("FUNCALL");
    assert_eq!(
        super::super::super::native_callable::invoke(
            EgclVal::from_symbol_index(funcall),
            &[*old, EgclVal::from_fixnum(41)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        0,
        "real Rust-owned FUNCALL record must not borrow a bridge"
    );
    assert_eq!(super::super::take_lean_root_entries_for_test(), 1);
    assert_eq!(egcl_rt::function::invoke_count(*old), 1);
    crate::cli::read_eval_all_env("(defun t1-object-outer (x) (+ x 100))", &mut env).unwrap();
    let (parent_symbol, parent) = install_parent(
        &mut env,
        "T1-OBJECT-RETAINED-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(funcall f x)",
    );
    mapped_ordinary::take_entries_for_test();
    assert_eq!(
        run_parent(
            parent_symbol,
            &parent,
            &[*old, EgclVal::from_fixnum(41)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(42)
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        0,
        "old private non-template identity stays on general fallback"
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*old),
        2,
        "retained fallback credits once"
    );
    let (arity_symbol, arity_parent) = install_parent(
        &mut env,
        "T1-OBJECT-ARITY-PARENT",
        "(f &key (k (1+ 0)) &aux (u 0))",
        "(funcall f)",
    );
    assert!(
        run_parent(arity_symbol, &arity_parent, &[*old], &mut env).is_err(),
        "valid parent invocation leaves actual callee arity signaling to general dispatch"
    );
    assert!(!child.is_t2);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn committed_t1_object_fifth_lane_miss_credits_once_and_keeps_native_root_fallback() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_invoke_threshold() > 2);
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let mut natives = Vec::new();
    egcl_rt::rooted!(functions = Vec::new());
    let mut symbols = Vec::new();
    for index in 0..5 {
        let (symbol, native) = install_t1(&mut env, &format!("T1-OBJECT-FIFTH-{index}"), "x");
        assert!(
            !native.has_deopt,
            "fixed identity entry cannot deopt to interpreter"
        );
        functions.push(egcl_rt::symbols::symbol_function(symbol).unwrap());
        symbols.push(symbol);
        natives.push(native);
    }
    let (parent_symbol, parent) = install_parent(
        &mut env,
        "T1-OBJECT-FIFTH-PARENT",
        "(a b c d e x &key (k (1+ 0)) &aux (u 0))",
        "(funcall a x) (funcall b x) (funcall c x) (funcall d x) (funcall e x)",
    );
    for function in functions.iter() {
        heat(*function, 0, 0);
    }
    reset_entries(symbols[4]);
    let mut args = functions.to_vec();
    args.push(EgclVal::from_fixnum(317));
    egcl_rt::rooted_ref!(_args = &mut args);
    assert_eq!(
        run_parent(parent_symbol, &parent, &args, &mut env).unwrap(),
        EgclVal::from_fixnum(317)
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        4,
        "four prepared code lanes exhaust the live-parent bound"
    );
    assert_eq!(
        super::super::take_lean_root_entries_for_test(),
        1,
        "the committed fifth call really executes existing native root ingress"
    );
    assert_eq!(super::super::take_nested_entries(), 0);
    assert_eq!(
        super::super::take_activation_counts_for_test(),
        (1, 1),
        "only the original root fallback prepares/recycles, once"
    );
    for function in functions.iter() {
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            1,
            "bridge miss cannot reenter apply and double-credit"
        );
        assert!(native_target(*function, 1).is_none());
    }
}

#[test]
#[ignore = "requires native entry and the normal asynchronous T2 broker"]
fn prepared_t1_object_retains_exact_body_across_redefinition_gc_and_ready_completion() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_enabled() && t2_invoke_threshold() > 2);
    assert_ne!(std::env::var("EGCL_T2_NO_QUEUE").as_deref(), Ok("1"));
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, original) = install_t1(
        &mut env,
        "T1-OBJECT-PREPARED-OLD",
        "'(t1-object-old-constant)",
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    heat(*function, 0, 0);
    let prepared = {
        let _native_env = NativeEnv::install(&mut env);
        t1_candidate(*function, 1).unwrap()
    };
    // Production does not allocate Lisp words between pure query and commit.
    // This test deliberately does; keep its prepared constants explicitly rooted.
    egcl_rt::rooted!(_old_body = super::super::super::ActiveBytecodeRoot::new(&prepared.body));
    crate::cli::read_eval_all_env(
        "(defun t1-object-prepared-old (x) :new-definition)",
        &mut env,
    )
    .unwrap();
    egcl_rt::gc::full_gc().unwrap();
    drop(_old_body);
    let _native_env = NativeEnv::install(&mut env);
    egcl_rt::rooted!(old = run_t1(prepared, *function, &[NIL], &mut env).unwrap());
    assert_eq!(cp(*old).1, NIL);
    assert!(crate::cli::sym_name(cp(*old).0).ends_with("T1-OBJECT-OLD-CONSTANT"));
    assert_eq!(egcl_rt::function::invoke_count(*function), 1);
    assert!(
        native_target(*function, 1).is_none(),
        "retained private body is not a T2/probe export"
    );
    drop(original);

    let (symbol, original) = install_t1(&mut env, "T1-OBJECT-PREPARED-T2", "x");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    heat(*function, 0, 0);
    let prepared = t1_candidate(*function, 1).unwrap();
    egcl_rt::rooted!(_ready_body = super::super::super::ActiveBytecodeRoot::new(&prepared.body));
    assert!(request_t2_compilation(symbol, 1));
    // Stage a real ready completion WITHOUT publishing it. The committed
    // canonical dispatcher must do that poll, rather than blindly run T1.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let done = loop {
        if let Some(done) = T2_COMPLETIONS.with(|slot| {
            slot.borrow()
                .as_ref()
                .and_then(|(_, receiver)| receiver.try_recv().ok())
        }) {
            assert_eq!(done.sym, symbol);
            assert!(!done.panicked && done.artifact.is_some());
            break done;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "real T2 job did not finish"
        );
        egcl_rt::poll_safepoint();
        std::thread::yield_now();
    };
    assert!(t2_completion_sender().send(done).is_ok());
    assert!(NATIVE_REGISTRY.with(|map| {
        map.borrow()
            .get(&symbol)
            .is_some_and(|native| Rc::ptr_eq(native, &original))
    }));
    egcl_rt::gc::full_gc().unwrap();
    assert_eq!(egcl_rt::function::invoke_count(*function), 0);
    drop(_ready_body);
    assert_eq!(
        run_t1(prepared, *function, &[EgclVal::from_fixnum(719)], &mut env).unwrap(),
        EgclVal::from_fixnum(719)
    );
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        1,
        "count once before canonical completion poll"
    );
    let current = NATIVE_REGISTRY
        .with(|map| map.borrow().get(&symbol).cloned())
        .unwrap();
    assert!(current.is_t2 && !Rc::ptr_eq(&current, &original));
    assert!(
        native_target(*function, 1).is_some(),
        "only actual T2 completion authorizes the old export"
    );
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn t1_object_bridge_preserves_multiple_values_and_error_cleanup_without_double_credit() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(setq *t1-object-cleanups* 0)
         (defun t1-object-mv-source (x) (values x (list x)))
         (defun t1-object-error-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin =
        ordinary_t2::RecursiveFixturePins::new(&["T1-OBJECT-MV-SOURCE", "T1-OBJECT-ERROR-OBSERVE"]);
    let (mv_symbol, mv_child) = install_t1(
        &mut env,
        "T1-OBJECT-MV",
        "(locally (declare (notinline t1-object-mv-source)) (t1-object-mv-source x))",
    );
    let (error_symbol, error_child) = install_t1(
        &mut env,
        "T1-OBJECT-ERROR",
        "(progn (t1-object-error-observe) (car x))",
    );
    let (mv_parent_symbol, mv_parent) = install_parent(
        &mut env,
        "T1-OBJECT-MV-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(multiple-value-list (funcall f x))",
    );
    let (error_parent_symbol, error_parent) = install_parent(
        &mut env,
        "T1-OBJECT-ERROR-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(unwind-protect (funcall f x) (setq *t1-object-cleanups* (1+ *t1-object-cleanups*)))",
    );
    egcl_rt::rooted!(mv_function = egcl_rt::symbols::symbol_function(mv_symbol).unwrap());
    egcl_rt::rooted!(error_function = egcl_rt::symbols::symbol_function(error_symbol).unwrap());
    heat(*mv_function, 0, 0);
    heat(*error_function, 0, 0);
    egcl_rt::rooted!(payload = arena_cons(EgclVal::from_fixnum(91), NIL));
    reset_entries(mv_symbol);
    egcl_rt::rooted!(
        result = run_parent(
            mv_parent_symbol,
            &mv_parent,
            &[*mv_function, *payload],
            &mut env
        )
        .unwrap()
    );
    let values = list_to_vec(*result);
    assert_eq!(values.len(), 2);
    assert_eq!(values[0], *payload);
    assert_eq!(cp(values[1]), (*payload, NIL));
    drop(values);
    assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    assert_eq!(egcl_rt::function::invoke_count(*mv_function), 1);
    assert!(!mv_child.is_t2);
    ordinary_t2::expect_entry(&error_child);
    reset_entries(error_symbol);
    assert!(
        run_parent(
            error_parent_symbol,
            &error_parent,
            &[*error_function, EgclVal::from_fixnum(17)],
            &mut env
        )
        .is_err()
    );
    assert!(ordinary_t2::observed_entry());
    assert_eq!(mapped_ordinary::take_entries_for_test(), 1);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    assert_eq!(egcl_rt::function::invoke_count(*error_function), 1);
    assert_eq!(
        crate::cli::read_eval_all_env("*t1-object-cleanups*", &mut env).unwrap(),
        EgclVal::from_fixnum(1)
    );
}
