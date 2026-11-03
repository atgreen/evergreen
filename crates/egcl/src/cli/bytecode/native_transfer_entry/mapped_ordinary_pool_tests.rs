// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Each iteration retires its mapped parent; these are not 100 calls inside
//! one activation. Storage identity and actual native entry are separate gates.
use super::super::super::native_transfer_tests::{ordinary_captured_context, ordinary_t2};
use super::super::mapped_ordinary_tests::{install, owner, run_mapped};
use super::*;
use egcl_rt::{Collector, HeapCollector};

fn idle_address(code: &TransferCode) -> usize {
    let pool = code.bridge_pool.idle.borrow();
    assert_eq!(pool.len(), 1);
    let bridge = pool[0].as_ref().get_ref();
    assert!(!bridge.context.live, "idle context has been dropped");
    assert!(!bridge.busy && !bridge.clean_return && bridge.generated.is_none());
    assert_eq!(bridge.function, NIL);
    assert_eq!(bridge.dispatch_function, NIL);
    assert!(bridge.args.as_slice().is_empty());
    assert!(bridge.children.is_empty());
    assert!(bridge.state.as_ref().unwrap().is_pristine_native_child());
    bridge as *const Bridge as usize
}

thread_local! {
    static RETIRING: Cell<*const TransferCode> = const { Cell::new(std::ptr::null()) };
}
fn retire_live_code() {
    let code = RETIRING.with(Cell::get);
    assert!(!code.is_null());
    unsafe { (&*code).retire_private_entries() };
}
struct Retiring(Option<fn()>);
impl Drop for Retiring {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        RETIRING.with(|slot| slot.set(std::ptr::null()));
    }
}

thread_local! {
    // Only addresses/counters, never an unrooted Lisp value.
    static EXPECTED_ENV: Cell<(usize, usize, usize)> = const { Cell::new((0, 0, 0)) };
}
fn observe_current_env_control() {
    let (mv_active, expected_execution, seen) = EXPECTED_ENV.with(Cell::get);
    assert_ne!(mv_active, 0);
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    assert_eq!(execution as usize, expected_execution);
    let control = unsafe { (&*execution).active_native_control() };
    assert_ne!(control, 0);
    let control = unsafe { &*(control as *const egcl_compiler::t2::emit::NativeRecursiveControl) };
    assert_eq!(control.execution, execution);
    assert_eq!(
        control.mv_active as usize, mv_active,
        "each lease must bind the current Env's MV flag"
    );
    EXPECTED_ENV.with(|slot| slot.set((mv_active, expected_execution, seen + 1)));
}
struct EnvProbe(Option<fn()>);
impl Drop for EnvProbe {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        EXPECTED_ENV.with(|slot| slot.set((0, 0, 0)));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn hundred_separate_parents_reuse_one_native_storage_without_general_child_recycle() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled(), "run this proof with the pool enabled");
    let mut env = Env::new(false);
    let mut second_env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    egcl_rt::rooted_ref!(_second = &mut second_env);
    crate::cli::read_eval_all_env(
        "(defun bp-observe () (%ordinary-t2-entry-for-test))
         (defun bp-produce (x) (values (+ x 1) x 77))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["BP-OBSERVE", "BP-PRODUCE"]);
    let (symbol, child) = install(
        &mut env,
        "BP-CHILD",
        "(x)",
        "(progn (bp-observe) (locally (declare (notinline bp-produce)) (bp-produce x)))",
    );
    let (parent_symbol, parent) = install(
        &mut env,
        "BP-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(funcall f x)",
    );
    assert!(owner(&parent).ordinary_entry.is_none());
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    ordinary_t2::expect_entry(&child);
    funcall_fast::apply(
        *function,
        &[EgclVal::from_fixnum(0)],
        &mut env,
        funcall_fast::Admit::Native,
    )
    .unwrap()
    .unwrap();
    take_storage_births_for_test();
    take_prepares_for_test();
    take_entries_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::start_activation_counts_for_test(symbol);
    let observing = EnvProbe(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|slot| slot.replace(Some(observe_current_env_control))),
    );
    let mut address = 0;
    for i in 0..100 {
        ordinary_t2::expect_entry(&child);
        // The same execution can use different Env objects. Context reset must
        // not retain the previous Env's frame, MV buffer or control pointers.
        let (current, other) = if i % 2 == 0 {
            (&mut env, &mut second_env)
        } else {
            (&mut second_env, &mut env)
        };
        let sentinel = [EgclVal::from_fixnum(-900 - i), T];
        other.set_mv(sentinel.to_vec());
        current.set_mv(vec![EgclVal::from_fixnum(-1)]);
        let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
        EXPECTED_ENV.with(|slot| {
            let (_, _, seen) = slot.get();
            slot.set((
                std::ptr::from_ref(&current.mv_active) as usize,
                execution as usize,
                seen,
            ));
        });
        assert_eq!(
            run_mapped(
                parent_symbol,
                &parent,
                &[*function, EgclVal::from_fixnum(i)],
                current
            )
            .unwrap(),
            EgclVal::from_fixnum(i + 1)
        );
        assert!(
            ordinary_t2::observed_entry(),
            "actual callee body entry {i}"
        );
        assert!(current.mv_active);
        assert_eq!(
            current.mv,
            [
                EgclVal::from_fixnum(i + 1),
                EgclVal::from_fixnum(i),
                EgclVal::from_fixnum(77)
            ]
        );
        assert!(
            other.mv_active,
            "current call cannot clear the other Env's MV flag"
        );
        assert_eq!(
            other.mv, sentinel,
            "current call cannot mutate the other Env's MV buffer"
        );
        let next = idle_address(owner(&child));
        if i == 0 {
            address = next;
        } else {
            assert_eq!(next, address);
        }
        assert_eq!(super::super::activation_counts_for_test(), (0, 0));
    }
    assert_eq!(EXPECTED_ENV.with(|slot| slot.get().2), 100);
    drop(observing);
    assert_eq!(take_entries_for_test(), 100);
    assert_eq!(
        take_prepares_for_test(),
        100,
        "one necessary context reset per parent"
    );
    assert_eq!(
        take_storage_births_for_test(),
        1,
        "four backing boxes allocated only once"
    );
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
    assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
    // Explicit retirement during an actual native body must preserve this
    // active old lease, then forbid its return to the retired version's pool.
    RETIRING.with(|slot| slot.set(Rc::as_ptr(owner(&child))));
    let _retiring = Retiring(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(retire_live_code))),
    );
    ordinary_t2::expect_entry(&child);
    assert_eq!(
        run_mapped(
            parent_symbol,
            &parent,
            &[*function, EgclVal::from_fixnum(909)],
            &mut env
        )
        .unwrap(),
        EgclVal::from_fixnum(910)
    );
    assert!(ordinary_t2::observed_entry());
    assert!(owner(&child).bridge_pool.retired.get());
    assert!(owner(&child).bridge_pool.idle.borrow().is_empty());
}

struct Weak(Box<egcl_rt::gc::WeakPointer>);
impl Weak {
    fn new(value: EgclVal) -> Self {
        let mut pointer = Box::new(egcl_rt::gc::WeakPointer::new(
            egcl_rt::gc::finalizer_key(value).unwrap(),
        ));
        egcl_rt::gc::register_weak_pointer(&mut pointer);
        Self(pointer)
    }
}
impl Drop for Weak {
    fn drop(&mut self) {
        egcl_rt::gc::unregister_weak_pointer(&self.0);
    }
}
thread_local! {
    static MOVING: Cell<*const EgclVal> = const { Cell::new(std::ptr::null()) };
    static MOVED: Cell<bool> = const { Cell::new(false) };
}
fn collect_live_capture() {
    let pointer = MOVING.with(Cell::get);
    assert!(!pointer.is_null());
    let before = unsafe { *pointer };
    HeapCollector::new().minor_gc().unwrap();
    MOVED.with(|moved| moved.set(unsafe { *pointer } != before));
}
struct Moving(Option<fn()>);
impl Drop for Moving {
    fn drop(&mut self) {
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.set(self.0));
        MOVING.with(|slot| slot.set(std::ptr::null()));
    }
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn idle_storage_releases_escaped_receiver_and_capture_after_real_native_movement() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun bp-cap-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["BP-CAP-OBSERVE"]);
    let body = ordinary_captured_context::template("((bp-cap-observe) ordinary-native-cell)", &env);
    egcl_rt::rooted!(_body = super::super::super::ActiveBytecodeRoot::new(&body));
    egcl_rt::rooted!(warm = ordinary_captured_context::closure(&body, NIL, &mut env));
    let child = super::super::with_forced_captured_entry_for_test(|| {
        ordinary_captured_context::install_automatic(*warm)
    });
    let (parent_symbol, parent) = install(
        &mut env,
        "BP-CAP-PARENT",
        "(f &key (k (1+ 0)) &aux (u 0))",
        "(funcall f nil)",
    );
    ordinary_t2::expect_entry(&child);
    funcall_fast::apply(*warm, &[NIL], &mut env, funcall_fast::Admit::Native)
        .unwrap()
        .unwrap();
    ordinary_t2::expect_entry(&child);
    run_mapped(parent_symbol, &parent, &[*warm], &mut env).unwrap();
    let address = idle_address(owner(&child));
    HeapCollector::new().minor_gc().unwrap();
    egcl_rt::rooted!(payload = crate::cli::arena_cons(EgclVal::from_fixnum(782), NIL));
    egcl_rt::rooted!(function = ordinary_captured_context::closure(&body, *payload, &mut env));
    let weak_function = Weak::new(*function);
    let weak_payload = Weak::new(*payload);
    MOVING.with(|slot| slot.set(&*payload));
    MOVED.with(|slot| slot.set(false));
    let moving = Moving(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER.with(|slot| slot.replace(Some(collect_live_capture))),
    );
    ordinary_t2::expect_entry(&child);
    take_storage_births_for_test();
    egcl_rt::rooted!(result = run_mapped(parent_symbol, &parent, &[*function], &mut env).unwrap());
    assert!(ordinary_t2::observed_entry() && MOVED.with(Cell::get));
    assert_eq!(*result, *payload);
    assert_eq!(cp(*result), (EgclVal::from_fixnum(782), NIL));
    assert_eq!(idle_address(owner(&child)), address);
    assert_eq!(take_storage_births_for_test(), 0);
    drop(moving);
    drop(result);
    drop(function);
    drop(payload);
    env.clear_mv();
    egcl_rt::gc::full_gc().unwrap();
    assert!(
        weak_function.0.is_broken(),
        "idle backing must not keep last callable alive"
    );
    assert!(
        weak_payload.0.is_broken(),
        "idle backing must not keep its lexical frame alive"
    );
    assert_eq!(idle_address(owner(&child)), address);
}

#[test]
fn detached_pool_has_no_code_cycle_and_retirement_rejects_outstanding_lease() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, installed) = install(&mut env, "BP-RETIRE", "(x)", "x");
    // A separately compiled physical version has no registry/cache/directory
    // owners, so the Weak assertion isolates exactly the pool ownership edge.
    let code = Rc::new(
        TransferCode::compile_variant(Arc::clone(&owner(&installed).body), true, false, None)
            .unwrap(),
    );
    let weak = Rc::downgrade(&code);
    let frame = egcl_rt::current_stack()
        .push_frame(NIL, std::ptr::null(), 0, 0)
        .unwrap();
    let _frame = FrameGuard(Some(frame));
    let mut lease = Lease::new(Rc::clone(&code), &mut env, frame);
    lease.bridge_mut().clean_return = true; // Structural pool fixture, not a native execution claim.
    drop(lease);
    idle_address(&code);
    drop(code);
    assert!(
        weak.upgrade().is_none(),
        "even an undrained idle pool cannot keep code alive"
    );
    let code = Rc::new(
        TransferCode::compile_variant(Arc::clone(&owner(&installed).body), true, false, None)
            .unwrap(),
    );
    let weak = Rc::downgrade(&code);
    let mut active = Lease::new(Rc::clone(&code), &mut env, frame);
    active.bridge_mut().clean_return = true;
    code.retire_private_entries();
    assert!(code.bridge_pool.idle.borrow().is_empty());
    drop(active);
    assert!(
        code.bridge_pool.idle.borrow().is_empty(),
        "retired version cannot be repopulated"
    );
    drop(code);
    assert!(
        weak.upgrade().is_none(),
        "code-owned storage must have no strong backedge"
    );
}

#[test]
fn idle_pool_is_bounded_and_rejects_dirty_or_incomplete_native_storage() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, installed) = install(&mut env, "BP-BOUNDED", "(x)", "x");
    let code = Rc::new(
        TransferCode::compile_variant(Arc::clone(&owner(&installed).body), true, false, None)
            .unwrap(),
    );
    let frame = egcl_rt::current_stack()
        .push_frame(NIL, std::ptr::null(), 0, 0)
        .unwrap();
    let _frame = FrameGuard(Some(frame));
    // These structural leases make no native execution claim. None may be
    // offered for reuse until it has a clean completion and pristine ledgers.
    drop(Lease::new(Rc::clone(&code), &mut env, frame));
    assert!(code.bridge_pool.idle.borrow().is_empty());
    let mut dirty = Lease::new(Rc::clone(&code), &mut env, frame);
    dirty.bridge_mut().clean_return = true;
    dirty.bridge_mut().state.as_mut().unwrap().ready_osr =
        Some(ActivationState::native_child(&env));
    drop(dirty);
    assert!(code.bridge_pool.idle.borrow().is_empty());
    let mut leases: Vec<_> = (0..6)
        .map(|_| Lease::new(Rc::clone(&code), &mut env, frame))
        .collect();
    for lease in &mut leases {
        lease.bridge_mut().clean_return = true;
    }
    drop(leases);
    assert_eq!(code.bridge_pool.idle.borrow().len(), 4);
    code.retire_private_entries();
    assert!(code.bridge_pool.idle.borrow().is_empty());
}

fn panic_in_guarded_wrapper() {
    panic!("bridge pool guarded observer panic");
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn guarded_observer_panic_discards_storage_then_retry_constructs_and_reuses() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(pooling_enabled());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    crate::cli::read_eval_all_env(
        "(defun bp-panic-observe () (%ordinary-t2-entry-for-test))",
        &mut env,
    )
    .unwrap();
    // This observer runs inside the existing pinned T0 wrapper and its c2i
    // panic guard. PROFILE_OBSERVE runs outside that guard and is unsuitable.
    let _pin = ordinary_t2::RecursiveFixturePins::new(&["BP-PANIC-OBSERVE"]);
    let (symbol, child) = install(
        &mut env,
        "BP-PANIC-CHILD",
        "(x)",
        "(progn (bp-panic-observe) (+ x 1))",
    );
    let (parent_symbol, parent) = install(
        &mut env,
        "BP-PANIC-PARENT",
        "(f x &key (k (1+ 0)) &aux (u 0))",
        "(funcall f x)",
    );
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(symbol).unwrap());
    ordinary_t2::expect_entry(&child);
    funcall_fast::apply(
        *function,
        &[EgclVal::from_fixnum(40)],
        &mut env,
        funcall_fast::Admit::Native,
    )
    .unwrap()
    .unwrap();
    assert!(owner(&child).bridge_pool.idle.borrow().is_empty());
    take_storage_births_for_test();
    take_entries_for_test();
    super::super::take_lean_root_entries_for_test();
    super::super::start_activation_counts_for_test(symbol);
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let panicking = Moving(
        ordinary_t2::RECURSIVE_LEAF_OBSERVER
            .with(|slot| slot.replace(Some(panic_in_guarded_wrapper))),
    );
    ordinary_t2::expect_entry(&child);
    let error = run_mapped(
        parent_symbol,
        &parent,
        &[*function, EgclVal::from_fixnum(40)],
        &mut env,
    )
    .unwrap_err();
    assert!(
        ordinary_t2::observed_entry(),
        "panic must come from actual native callee body"
    );
    assert!(
        format!("{error:?}").contains("bridge pool guarded observer panic"),
        "{error:?}"
    );
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert!(!native_error_pending());
    assert!(
        owner(&child).bridge_pool.idle.borrow().is_empty(),
        "failed storage cannot become idle"
    );
    assert_eq!(take_storage_births_for_test(), 1);
    drop(panicking);
    for expected_births in [1, 0] {
        ordinary_t2::expect_entry(&child);
        assert_eq!(
            run_mapped(
                parent_symbol,
                &parent,
                &[*function, EgclVal::from_fixnum(40)],
                &mut env
            )
            .unwrap(),
            EgclVal::from_fixnum(41)
        );
        assert!(ordinary_t2::observed_entry());
        assert_eq!(take_storage_births_for_test(), expected_births);
        idle_address(owner(&child));
        assert_eq!(
            (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
            before
        );
    }
    assert_eq!(take_entries_for_test(), 3);
    assert_eq!(super::super::take_lean_root_entries_for_test(), 0);
    assert_eq!(super::super::take_activation_counts_for_test(), (0, 0));
}
