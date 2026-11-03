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
    assert_eq!(
        queries.counts().1,
        32,
        "one full T1 proof per committed call; preparation only hints"
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

struct NativeEnvForHint(*mut Env);
impl NativeEnvForHint {
    fn install(env: &mut Env) -> Self {
        Self(NATIVE_ENV.with(|slot| slot.replace(env)))
    }
}
impl Drop for NativeEnvForHint {
    fn drop(&mut self) {
        NATIVE_ENV.with(|slot| slot.set(self.0));
    }
}

#[test]
fn preparation_hint_borrows_without_credit_and_full_proof_rejects_false_positives() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_enabled() && !profiling_disabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install_t1(&mut env, "T1-HINT-PURE");
    let (_, other) = install_t1(&mut env, "T1-HINT-OTHER-BODY");
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    let _native_env = NativeEnvForHint::install(&mut env);
    let queries = Queries::start(symbol);
    let before = egcl_rt::function::invoke_count(*function);
    let owners = Rc::strong_count(&native);
    let bodies = Arc::strong_count(native.body.as_ref().unwrap());
    for _ in 0..16 {
        assert!(t1_preparation_hint(*function, 1));
    }
    assert_eq!(queries.counts(), (0, 0));
    assert_eq!(Rc::strong_count(&native), owners);
    assert_eq!(Arc::strong_count(native.body.as_ref().unwrap()), bodies);
    assert_eq!(egcl_rt::function::invoke_count(*function), before);
    assert!(!is_cached_for_test(*function));
    assert!(!t1_preparation_hint(*function, 0));
    assert!(!t1_preparation_hint(*function, 2));
    assert!(t1_candidate(*function, 1).is_some());

    // Deliberately stale routing hints cannot mint an execution capability.
    let generation = REGISTRY_GENERATION.with(|map| map.borrow_mut().remove(&symbol));
    let hinted = t1_preparation_hint(*function, 1);
    let rejected = t1_candidate(*function, 1).is_none();
    assert!(REGISTRY_GENERATION.with(|map| !map.borrow().contains_key(&symbol)));
    REGISTRY_GENERATION.with(|map| {
        if let Some(generation) = generation {
            map.borrow_mut().insert(symbol, generation);
        }
    });
    assert!(hinted && rejected);
    let old = REGISTRY.with(|map| {
        map.borrow_mut()
            .insert(symbol, Arc::clone(other.body.as_ref().unwrap()))
            .unwrap()
    });
    let hinted = t1_preparation_hint(*function, 1);
    let rejected = t1_candidate(*function, 1).is_none();
    REGISTRY.with(|map| map.borrow_mut().insert(symbol, old));
    assert!(hinted && rejected);
    env.locally_specials.push(symbol);
    let hinted = t1_preparation_hint(*function, 1);
    let rejected = t1_candidate(*function, 1).is_none();
    env.locally_specials.pop();
    assert!(hinted && rejected);
    let invocation = egcl_rt::function::invocation_counter_address(*function).unwrap();
    unsafe {
        (*invocation).store(
            t2_invoke_threshold() - 1,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
    let hinted = t1_preparation_hint(*function, 1);
    let rejected = t1_candidate(*function, 1).is_none();
    assert_eq!(
        egcl_rt::function::invoke_count(*function),
        t2_invoke_threshold() - 1
    );
    unsafe { (*invocation).store(before, std::sync::atomic::Ordering::Relaxed) };
    assert!(hinted && rejected);
    assert_eq!(
        queries.counts().0,
        0,
        "hint and full proof do not fill T2 TABLE"
    );
    assert_eq!(egcl_rt::function::invoke_count(*function), before);
    assert!(!is_cached_for_test(*function));
    assert!(!T2_QUEUED.with(|queue| queue.borrow().contains_key(&symbol)));
}

// Scalar-only hook state; no callable or code owner is retained by this probe.
thread_local! {
    static HINT_SPECIAL: Cell<(usize, u32, bool)> = const { Cell::new((0, 0, false)) };
}
fn install_hint_special_after_parent_entry() {
    let (owner, symbol, _) = HINT_SPECIAL.with(Cell::get);
    if owner == 0
        || !ordinary_t2::observed_entry()
        || super::super::root_publication::active_anchor_for_test().map(|(_, owner)| owner)
            != Some(owner)
    {
        return;
    }
    let env = NATIVE_ENV.with(Cell::get);
    assert!(!env.is_null());
    HINT_SPECIAL.with(|slot| slot.set((owner, symbol, true)));
    // Actual parent entry has already cleared its inherited declarations. The
    // pinned marker proves its physical frame before this FUNCALL publication.
    // Reestablish at every exact parent publication: the T0 marker wrapper
    // has its own declaration guard and can restore the previous vector.
    unsafe {
        let declarations = &mut (*env).locally_specials;
        if !declarations.contains(&symbol) {
            declarations.push(symbol);
        }
    }
}
struct HintSpecialPublication {
    hook: Option<fn()>,
    probe: (usize, u32, bool),
    env: *mut Env,
    declarations: Vec<u32>,
}
impl HintSpecialPublication {
    fn install(parent: &NativeCode, symbol: u32, env: &mut Env) -> Self {
        let owner = Rc::as_ptr(mapped(parent)) as usize;
        let declarations = env.locally_specials.clone();
        Self {
            hook: super::super::root_publication::PROFILE_OBSERVE
                .with(|slot| slot.replace(Some(install_hint_special_after_parent_entry))),
            probe: HINT_SPECIAL.with(|slot| slot.replace((owner, symbol, false))),
            env,
            declarations,
        }
    }
    fn installed(&self) -> bool {
        HINT_SPECIAL.with(|slot| slot.get().2)
    }
}
impl Drop for HintSpecialPublication {
    fn drop(&mut self) {
        super::super::root_publication::PROFILE_OBSERVE.with(|slot| slot.set(self.hook));
        HINT_SPECIAL.with(|slot| slot.set(self.probe));
        unsafe { (*self.env).locally_specials = std::mem::take(&mut self.declarations) };
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn false_positive_preparation_hints_use_once_counted_checked_fallback() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(t1_object_enabled() && t2_enabled() && !profiling_disabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun t1-hint-parent-observe () (%ordinary-t2-entry-for-test))
         (defun t1-hint-fallback-parent (f x &key (seed (1+ 0)) &aux (u 0))
           (t1-hint-parent-observe) (funcall f x))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["T1-HINT-PARENT-OBSERVE"]);
    let (parent_symbol, parent) = ordinary_t2::install(
        "T1-HINT-FALLBACK-PARENT",
        "(f x &key (seed (1+ 0)) &aux (u 0))",
        "((t1-hint-parent-observe) (funcall f x))",
        &mut env,
    );
    assert!(mapped(&parent).ordinary_entry.is_none());
    for promotion in [false, true] {
        let name = if promotion {
            "T1-HINT-PROMOTION"
        } else {
            "T1-HINT-SPECIAL"
        };
        let (symbol, child) = install_t1(&mut env, name);
        assert!(mapped(&child).lean_root_entry().is_some());
        egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
        if promotion {
            unsafe {
                (*egcl_rt::function::invocation_counter_address(*function).unwrap()).store(
                    t2_invoke_threshold() - 1,
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
        }
        let _native_env = NativeEnvForHint::install(&mut env);
        assert!(t1_preparation_hint(*function, 1));
        if promotion {
            assert!(t1_candidate(*function, 1).is_none());
        }
        let before = egcl_rt::function::invoke_count(*function);
        mapped_ordinary::take_entries_for_test();
        ordinary_t2::expect_entry(&parent);
        let special =
            (!promotion).then(|| HintSpecialPublication::install(&parent, symbol, &mut env));
        let result = mapped(&parent).run_callable(
            egcl_rt::symbols::symbol_function(parent_symbol).unwrap(),
            &[*function, EgclVal::from_fixnum(418)],
            &mut env,
        );
        if let Some(scope) = special.as_ref() {
            assert!(
                scope.installed(),
                "SPECIAL injected at real parent publication after entry"
            );
        }
        drop(special);
        assert!(
            env.locally_specials.is_empty(),
            "declaration scope restored"
        );
        assert_eq!(result.unwrap(), EgclVal::from_fixnum(418));
        assert!(
            ordinary_t2::observed_entry(),
            "exact parent physical native owner observed"
        );
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            before + 1,
            "checked fallback owns exactly one canonical credit"
        );
        assert_eq!(
            mapped_ordinary::take_entries_for_test(),
            0,
            "routing hint cannot bypass committed SPECIAL/promotion guard"
        );
    }
}

#[test]
fn validated_t1_offset_is_reused_only_for_exact_selected_owner() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, prepared) = install_t1(&mut env, "T1-VALIDATED-OFFSET");
    let offset = mapped(&prepared).lean_root_entry().unwrap();
    assert_eq!(
        t1_selected_offset(&prepared, offset, &prepared),
        Some(offset)
    );
    let (_, replacement) =
        ordinary_t2::install_body(symbol, Arc::clone(prepared.body.as_ref().unwrap()));
    assert!(replacement.is_t2 && !Rc::ptr_eq(&prepared, &replacement));
    let replacement_offset = mapped(&replacement).lean_root_entry().unwrap();
    // A deliberately invalid prepared offset makes accidental cross-owner
    // reuse visible even when two real code buffers place their entries alike.
    assert_ne!(replacement_offset, usize::MAX);
    assert_eq!(
        t1_selected_offset(&prepared, usize::MAX, &replacement),
        Some(replacement_offset)
    );
    for code in [&prepared, &replacement] {
        let old = mapped(code).recompile.replace(true);
        let denied = t1_selected_offset(&prepared, offset, code);
        mapped(code).recompile.set(old);
        assert!(
            denied.is_none(),
            "same and replacement owners honor recompile requests"
        );
    }
}

#[test]
fn combined_t1_roots_keep_owned_body_and_writable_receiver_and_argument_homes() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (symbol, native) = install_t1(&mut env, "T1-COMBINED-ROOTS");
    let body = Arc::clone(native.body.as_ref().unwrap());
    let before = Arc::strong_count(&body);
    let mut roots = T1InvocationRoots {
        function: egcl_rt::symbols::symbol_function(symbol).unwrap(),
        args: EntryArgs::new(&[EgclVal::from_fixnum(581)]),
        active: super::super::ActiveCallableRoot(symbol),
        body: super::super::ActiveBytecodeRoot::from_owned(body),
    };
    assert_eq!(
        Arc::strong_count(&roots.body._function),
        before,
        "body root consumes the existing lease instead of cloning it"
    );
    let function_home = std::ptr::addr_of_mut!(roots.function);
    let argument_home = roots.args.as_slice().as_ptr() as *mut EgclVal;
    let mut homes = (false, false);
    egcl_rt::gc::TraceHostRoots::trace_host_roots(&mut roots, &mut |home| {
        if home == function_home {
            homes.0 = true;
            unsafe { *home = T };
        }
        if home == argument_home {
            homes.1 = true;
            unsafe { *home = EgclVal::from_fixnum(582) };
        }
    });
    assert_eq!(
        homes,
        (true, true),
        "both writable receiver/argument homes traced"
    );
    assert_eq!(roots.function, T);
    assert_eq!(roots.args.as_slice(), &[EgclVal::from_fixnum(582)]);
    assert_eq!(roots.active.0, symbol);
    assert!(Arc::ptr_eq(
        &roots.body._function,
        native.body.as_ref().unwrap()
    ));
    // Existing actual GC/poison/migration tests exercise this combined root
    // while canonical polling and ordinary machine frames are live.
}
