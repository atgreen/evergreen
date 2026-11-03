// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A published object FUNCALL reuses its native bridge across a real forced
//! carrier change. Owner-filtered activation counters follow the execution,
//! while native argument maps stay valid during external moving collection.

use super::super::native_transfer_tests::ordinary_t2::install as install_ordinary;
use super::mapped_ordinary_tests::{install as install_mapped, owner, run_mapped};
use super::*;
use egcl_rt::debug_stack::{FrameOrigin, LogicalFrame};
use egcl_rt::{Collector, HeapCollector};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

// Keep the ordinary child frame live while its terminal adapter suspends.
fn install(name: &str, params: &str, forms: &str, env: &mut Env) -> (u32, Rc<NativeCode>) {
    with_ordinary_non_tail_emission_for_test(|| install_ordinary(name, params, forms, env))
}

struct Probe {
    owner: usize,
    tier: u8,
    third_entry: usize,
    observed: bool,
    observed_after_resume: bool,
    control: u64,
}
// The probe and publication observer follow the execution, including if its
// continuation resumes on another carrier. The pointer borrows this fiber's
// stable Rust stack only until Observing is dropped before Probe.
static PROBE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut Probe>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };
fn observe_hot_middle() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null() {
        return;
    }
    if root_publication::published_frame_has_owner_for_test(unsafe { (*probe).owner }) {
        unsafe {
            (*probe).observed = true;
            // The physical publication resolves to this exact code owner,
            // independently of the function object's installed tier metadata.
            let code = &*((*probe).owner as *const TransferCode);
            assert_eq!(
                code.is_t2,
                (*probe).tier == 2,
                "real published middle frame must execute the requested compiler tier"
            );
            let execution = egcl_rt::thread::current_execution_context_for_native()
                .expect("current bridge execution");
            let current_control = (&*execution).active_native_control();
            assert_ne!(current_control, 0);
            if (*probe).control == 0 {
                (*probe).control = current_control;
            }
            assert_eq!(
                current_control,
                (*probe).control,
                "same parent lane reuses its control"
            );
            // A third call begins only after the parked second call resumes.
            // Its entry poll may publish before refreshing stale snapshots;
            // require a later real publication with current consumed state.
            if *mapped_ordinary::entry_counter() == (*probe).third_entry
                && PARKED_ORIGIN.load(Ordering::Acquire) != 0
                && egcl_rt::current_thread_id().0 != PARKED_ORIGIN.load(Ordering::Acquire)
            {
                let control =
                    &*(current_control as *const egcl_compiler::t2::emit::NativeRecursiveControl);
                assert_eq!(control.execution, execution);
                if control.seen_carrier == (&*execution).carrier_id()
                    && Some(control.seen_signal_epoch)
                        == egcl_rt::runtime::current_seen_signal_activity()
                {
                    (*probe).observed_after_resume = true;
                }
            }
            assert_eq!(
                activation_counts_for_test(),
                (0, 0),
                "no callee activation setup/teardown while native bridge is live"
            );
        }
    }
}
struct Observing(Option<fn()>);
impl Drop for Observing {
    fn drop(&mut self) {
        root_publication::PROFILE_OBSERVE.with(|hook| hook.set(self.0));
        PROBE.with(|slot| slot.set(std::ptr::null_mut()));
    }
}
fn observing(probe: &mut Probe) -> Observing {
    PROBE.with(|slot| slot.set(probe));
    Observing(root_publication::PROFILE_OBSERVE.with(|hook| hook.replace(Some(observe_hot_middle))))
}

const READY: &str = "*MF-OBJECT-FIBER-READY*";
static PARKED_ORIGIN: AtomicU64 = AtomicU64::new(0);
#[derive(Default)]
struct CarrierHold {
    entered: AtomicBool,
    release: AtomicBool,
    timed_out: AtomicBool,
}

struct Cohort<'a> {
    group: &'a egcl_rt::SchedulerGroup,
    fiber: egcl_rt::FiberId,
    hold: Arc<CarrierHold>,
    finished: bool,
}

impl Cohort<'_> {
    fn finish(&mut self) -> Vec<EgclVal> {
        // The origin must be released before finish joins carrier threads.
        self.hold.release.store(true, Ordering::Release);
        self.group.set_idle_hook(None);
        let values = self.group.finish().unwrap();
        self.finished = true;
        values
    }
}

impl Drop for Cohort<'_> {
    fn drop(&mut self) {
        self.hold.release.store(true, Ordering::Release);
        self.group.set_idle_hook(None);
        if self.finished {
            return;
        }
        // A failure may precede the fiber's park. Revisit it until completion,
        // without joining an indefinitely stranded continuation during unwind.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match egcl_rt::thread::fiber_state(self.fiber) {
                Some(
                    egcl_rt::thread::FiberState::Blocked | egcl_rt::thread::FiberState::Waiting,
                ) => {
                    let _ = self.group.unpark(self.fiber);
                }
                Some(egcl_rt::thread::FiberState::Dead) => {
                    let _ = self.group.finish();
                    break;
                }
                _ => {}
            }
            if Instant::now() >= deadline {
                break;
            }
            egcl_rt::poll_safepoint();
            std::thread::yield_now();
        }
    }
}

fn wait_until(mut ready: impl FnMut() -> bool, message: &str) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !ready() {
        egcl_rt::poll_safepoint();
        assert!(Instant::now() < deadline, "{message}");
        std::thread::yield_now();
    }
}

fn object_fiber() -> EgclVal {
    object_fiber_for_tier(2, 0)
}
fn object_t1_fiber() -> EgclVal {
    object_fiber_for_tier(1, 0)
}
fn object_overflow_fiber() -> EgclVal {
    object_fiber_for_tier(2, 16)
}
fn object_t1_overflow_fiber() -> EgclVal {
    object_fiber_for_tier(1, 16)
}
fn object_spill_fiber() -> EgclVal {
    object_fiber_for_tier(2, 4)
}
fn object_t1_spill_fiber() -> EgclVal {
    object_fiber_for_tier(1, 4)
}
fn object_fiber_for_tier(tier: u8, fillers: usize) -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (middle_symbol, mut middle) = install(
        "MF-OBJECT-FIBER-MIDDLE",
        "(a b c d)",
        "((mf-object-fiber-terminal b d a c))",
        &mut env,
    );
    if tier == 1 {
        let body = Arc::clone(middle.body.as_ref().unwrap());
        let baseline = with_ordinary_non_tail_emission_for_test(|| {
            install_t1(middle_symbol, Arc::clone(&body), false)
        })
        .expect("real non-tail T1 ordinary middle");
        assert!(!baseline.is_t2);
        assert!(Arc::ptr_eq(baseline.body.as_ref().unwrap(), &body));
        publish_native(
            middle_symbol,
            egcl_rt::symbols::symbol_function(middle_symbol),
            &baseline,
        );
        middle = baseline;
    }
    assert_eq!(middle.is_t2, tier == 2);
    assert!(owner(&middle).ordinary_entry.is_some());
    assert!(owner(&middle).lean_root_entry().is_some());
    let middle_owner = Rc::as_ptr(owner(&middle)) as usize;
    egcl_rt::rooted!(function = egcl_rt::symbols::symbol_function(middle_symbol).unwrap());
    if fillers != 0 {
        assert!(mapped_ordinary::overflow_enabled());
    }
    for index in 0..fillers {
        // Publish source callable identity as well as native filler code.
        install_mapped(&mut env, &format!("MF-OVERFLOW-FILL-{index}"), "(x)", "x");
    }
    let prefix = (0..fillers)
        .map(|i| format!("(mf-overflow-fill-{i} {i})"))
        .collect::<Vec<_>>()
        .join(" ");
    let declarations = if fillers == 0 {
        String::new()
    } else {
        let names = (0..fillers)
            .map(|i| format!("mf-overflow-fill-{i}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!("(declare (notinline {names}))")
    };
    let forms = format!(
        "(locally {declarations}
        (progn {prefix} (funcall f 1 2 3 4)
          (multiple-value-prog1 (funcall f d c b a) (funcall f 5 6 7 8))))"
    );
    let (symbol, outer) = install_mapped(
        &mut env,
        "MF-OBJECT-FIBER-OUTER",
        "(f a b c d &key (seed (1+ 0)))",
        &forms,
    );
    assert!(owner(&outer).ordinary_entry.is_none());
    let heat_before_warm = egcl_rt::function::invoke_count(*function);
    if tier == 1 {
        assert!(
            t2_invoke_threshold() > heat_before_warm.saturating_add(9),
            "migration test must not cross a promotion boundary"
        );
        assert!(
            funcall_fast::native_target(*function, 4).is_none(),
            "T1 must not enter the T2 export/probe path"
        );
    }
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    mapped_ordinary::take_storage_births_for_test();
    for _ in 0..2 {
        assert_eq!(
            run_mapped(
                symbol,
                &outer,
                &[*function, warm[0], warm[1], warm[2], warm[3]],
                &mut env
            )
            .unwrap(),
            warm[1]
        );
    }
    assert_eq!(
        mapped_ordinary::take_storage_births_for_test(),
        fillers + 1,
        "two separate warm parents reuse bounded Rust backing"
    );
    if tier == 1 {
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            heat_before_warm + 6,
            "two warm parents each make three canonically counted T1 object calls"
        );
        assert!(funcall_fast::native_target(*function, 4).is_none());
    }
    let storage = mapped_ordinary::idle_storage_for_test(owner(&middle));
    assert_eq!(storage.0, 1);
    // The exact callable stays rooted; the active segment/directory also owns
    // the native physical version across suspension and external collection.
    drop(middle);
    egcl_rt::rooted!(arguments = Vec::new());
    for index in 0..4 {
        arguments.push(crate::cli::arena_cons(
            EgclVal::from_fixnum(200 + index),
            NIL,
        ));
    }
    let stack = egcl_rt::current_stack();
    let before = (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get));
    let execution = egcl_rt::thread::current_execution_context_for_native().unwrap();
    let control = unsafe { &*execution }.active_native_control();
    let segment = egcl_rt::native_transfer::current_segment();
    egcl_rt::poll_safepoint();
    assert_eq!(super::super::native_loop_should_exit(), 0);
    mapped_ordinary::take_entries_for_test();
    mapped_ordinary::take_prepares_for_test();
    take_lean_root_entries_for_test();
    mapped_ordinary::take_uncached_calls_for_test();
    mapped_ordinary::take_cache_shape_for_test();
    start_activation_counts_for_test(middle_symbol);
    let heat_before_live = egcl_rt::function::invoke_count(*function);
    let mut probe = Probe {
        owner: middle_owner,
        tier,
        third_entry: fillers + 3,
        observed: false,
        observed_after_resume: false,
        control: 0,
    };
    let _observing = observing(&mut probe);
    egcl_rt::rooted!(
        result = run_mapped(
            symbol,
            &outer,
            &[
                *function,
                arguments[0],
                arguments[1],
                arguments[2],
                arguments[3]
            ],
            &mut env
        )
        .unwrap()
    );
    let origin = PARKED_ORIGIN.load(Ordering::Acquire);
    assert_ne!(
        origin, 0,
        "external controller must record the actual parked carrier"
    );
    assert_ne!(
        egcl_rt::current_thread_id().0,
        origin,
        "the object FUNCALL bridge must return on a different actual carrier"
    );
    assert_eq!(
        mapped_ordinary::take_entries_for_test(),
        fillers + 3,
        "third object call starts after suspension, after all filler calls"
    );
    assert_eq!(
        mapped_ordinary::take_prepares_for_test(),
        if fillers >= 16 {
            fillers + 3
        } else {
            fillers + 1
        },
        "spill leases prepare once; targets above sixteen prepare a local lease per call"
    );
    let (cached, generated) = mapped_ordinary::take_cache_shape_for_test();
    assert_eq!(cached, (fillers + 1).min(16));
    assert!(generated <= 4);
    assert_eq!(take_lean_root_entries_for_test(), 0);
    assert_eq!(
        mapped_ordinary::take_uncached_calls_for_test(),
        if fillers >= 16 { (3, 16) } else { (0, 0) },
        "only targets above sixteen use the real uncached route"
    );
    assert_eq!(
        mapped_ordinary::take_storage_births_for_test(),
        0,
        "the migrating parent leases backing from an earlier retired parent"
    );
    if tier == 1 {
        assert_eq!(
            egcl_rt::function::invoke_count(*function),
            heat_before_live + 3,
            "warm, suspended and post-resume calls each credit T1 exactly once"
        );
        assert!(funcall_fast::native_target(*function, 4).is_none());
        // The initial strong NativeCode was dropped before the live run. Look
        // up only after return in this fiber's execution-local native registry.
        let current = NATIVE_REGISTRY
            .with(|registry| registry.borrow().get(&middle_symbol).cloned())
            .expect("same execution retains exact T1 installation");
        assert!(!current.is_t2);
        assert_eq!(Rc::as_ptr(owner(&current)) as usize, middle_owner);
        assert_eq!(
            mapped_ordinary::idle_storage_for_test(owner(&current)),
            storage
        );
    } else {
        let (_, current, _, _) = funcall_fast::native_target(*function, 4).unwrap();
        assert_eq!(mapped_ordinary::idle_storage_for_test(&current), storage);
    }
    assert_eq!(
        take_activation_counts_for_test(),
        (0, 0),
        "execution-local callee counter migrates; typed backing bypasses general recycle"
    );
    assert!(
        probe.observed,
        "the hot route must publish this physical middle owner"
    );
    assert!(
        probe.observed_after_resume,
        "third object call must publish on the new carrier with refreshed control/poll snapshots"
    );
    assert_eq!(*result, arguments[2]);
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 4);
    for (actual, index) in env.mv.iter().zip([2, 0, 3, 1]) {
        assert_eq!(*actual, arguments[index]);
        assert_eq!(
            crate::cli::cp(*actual),
            (EgclVal::from_fixnum(200 + index as i64), NIL)
        );
    }
    assert_eq!(
        (stack.fp(), stack.sp(), NATIVE_DEPTH.with(Cell::get)),
        before
    );
    assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
    assert_eq!(unsafe { &*execution }.active_native_control(), control);
    EgclVal::from_fixnum(1)
}

fn stopped_rows(fiber: egcl_rt::FiberId, deadline: Instant) -> Vec<LogicalFrame> {
    loop {
        // This API holds mount exclusion. Merely observing Waiting does not
        // authorize reading another execution's native stack or directory.
        if let Some(rows) = egcl_rt::debug_stack::capture_fiber(fiber, 64).unwrap() {
            if rows.iter().any(|row| {
                row.function
                    .as_deref()
                    .is_some_and(|name| name.ends_with("MF-OBJECT-FIBER-MIDDLE"))
            }) {
                return rows;
            }
        }
        egcl_rt::poll_safepoint();
        assert!(Instant::now() < deadline, "ordinary chain did not suspend");
        std::thread::yield_now();
    }
}

fn native_arguments(rows: &[LogicalFrame]) -> Vec<EgclVal> {
    let middle: Vec<_> = rows
        .iter()
        .filter(|row| {
            row.function
                .as_deref()
                .is_some_and(|name| name.ends_with("MF-OBJECT-FIBER-MIDDLE"))
        })
        .collect();
    assert_eq!(middle.len(), 1, "{rows:?}");
    assert_eq!(middle[0].origin, FrameOrigin::Native);
    let arguments = middle[0]
        .arguments
        .as_ref()
        .expect("ordinary child argument homes");
    assert_eq!(
        arguments.len(),
        4,
        "fourth actual exercises the overflow ABI home"
    );
    assert!(
        rows.iter().any(|row| row
            .function
            .as_deref()
            .is_some_and(|name| name.ends_with("MF-OBJECT-FIBER-OUTER"))),
        "{rows:?}"
    );
    // The variadic mapped OUTER may not expose original actuals.
    arguments.clone()
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_object_funcall_bridge_survives_actual_migration_and_external_gc() {
    run_migration(object_fiber, 2);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn published_t1_object_funcall_bridge_preserves_heat_across_actual_migration_and_external_gc() {
    run_migration(object_t1_fiber, 1);
}

#[test]
#[ignore = "requires native migration and external moving collection"]
fn uncached_t2_object_bridge_survives_migration_external_gc_and_post_resume_call() {
    run_migration(object_overflow_fiber, 2);
}
#[test]
#[ignore = "requires native migration and external moving collection"]
fn uncached_t1_object_bridge_preserves_heat_through_migration_and_external_gc() {
    run_migration(object_t1_overflow_fiber, 1);
}

#[test]
#[ignore = "requires native migration and external moving collection"]
fn spill_t2_object_bridge_survives_migration_external_gc_and_post_resume_call() {
    run_migration(object_spill_fiber, 2);
}
#[test]
#[ignore = "requires native migration and external moving collection"]
fn spill_t1_object_bridge_preserves_heat_through_migration_and_external_gc() {
    run_migration(object_t1_spill_fiber, 1);
}

fn run_migration(entry: fn() -> EgclVal, tier: u8) {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    PARKED_ORIGIN.store(0, Ordering::Release);
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(READY), NIL).unwrap();
    crate::cli::read_eval_all_env(
        "(defun mf-object-fiber-terminal (a b c d)
           (if (consp a)
               (progn
                 (setq *mf-object-fiber-ready* t)
                 (eval '(%park-native-fiber-for-test))
                 (values a b c d))
               d))
         (defun mf-object-fiber-middle (a b c d)
           (mf-object-fiber-terminal b d a c))",
        &mut startup,
    )
    .unwrap();
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
    let entry = unsafe { EgclVal::from_function_ptr(entry as *const () as *mut u8) };
    let fiber = egcl_rt::thread::make_fiber(entry).unwrap();
    let hold = Arc::new(CarrierHold::default());
    let mut cohort = Cohort {
        group: &group,
        fiber,
        hold: Arc::clone(&hold),
        finished: false,
    };
    group.submit(fiber).unwrap();
    let deadline = Instant::now() + Duration::from_secs(15);
    let ready = egcl_rt::symbols::intern(READY);
    while egcl_rt::symbols::symbol_value(ready) != Some(T) {
        egcl_rt::poll_safepoint();
        assert!(
            Instant::now() < deadline,
            "terminal did not reach its suspension gate"
        );
        std::thread::yield_now();
    }
    wait_until(
        || egcl_rt::thread::fiber_state(fiber) == Some(egcl_rt::thread::FiberState::Blocked),
        "target must finish its explicit one-shot park",
    );
    let origin = egcl_rt::thread::fiber_carrier_thread(fiber).expect("parked origin carrier");
    PARKED_ORIGIN.store(origin.0, Ordering::Release);
    let held = Arc::clone(&hold);
    group.set_idle_hook(Some(Arc::new(move || {
        if egcl_rt::current_thread_id() != origin || held.release.load(Ordering::Acquire) {
            return;
        }
        // This real idle carrier holds no Lisp references/borrows. Publishing
        // native blocking state permits the external collector to stop safely.
        let _blocked = unsafe { egcl_rt::safepoint::NativeBlockingScope::enter() };
        held.entered.store(true, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !held.release.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                held.timed_out.store(true, Ordering::Release);
                break;
            }
            std::thread::yield_now();
        }
    })));
    wait_until(
        || hold.entered.load(Ordering::Acquire),
        "origin carrier must acknowledge hold",
    );
    let middle_symbol = egcl_rt::symbols::intern("MF-OBJECT-FIBER-MIDDLE");
    if tier == 1 {
        assert_eq!(
            egcl_rt::function::tier(egcl_rt::symbols::symbol_function(middle_symbol).unwrap()),
            1,
            "stopped native debug row plus exact published T1 owner prove the T1 path"
        );
    }
    start_activation_counts_for_test(middle_symbol);
    assert_eq!(
        activation_counts_for_test(),
        (0, 0),
        "controller starts with its own counter state"
    );
    // Drop the captured rows before GC: a copied backtrace must not provide
    // extra roots for the suspended execution whose physical homes we test.
    let before: Vec<u64> = native_arguments(&stopped_rows(fiber, deadline))
        .iter()
        .map(|value| value.to_raw())
        .collect();
    HeapCollector::new().full_gc().unwrap();
    egcl_rt::rooted!(rows = stopped_rows(fiber, deadline));
    let after = native_arguments(&rows);
    assert!(
        after
            .iter()
            .zip(before.iter())
            .any(|(value, bits)| value.to_raw() != *bits),
        "external collection must actually relocate an original native argument"
    );
    for (value, index) in after.iter().zip([3, 2, 1, 0]) {
        assert_eq!(
            crate::cli::cp(*value),
            (EgclVal::from_fixnum(200 + index), NIL)
        );
    }
    group.unpark(fiber).unwrap();
    wait_until(
        || egcl_rt::thread::fiber_state(fiber) == Some(egcl_rt::thread::FiberState::Dead),
        "resumed object call must complete while origin stays held",
    );
    assert!(
        !hold.timed_out.load(Ordering::Acquire),
        "origin hold must cover completion"
    );
    let resumed = egcl_rt::thread::fiber_carrier_thread(fiber).expect("completed carrier");
    assert_ne!(
        resumed, origin,
        "the actual object-call continuation must migrate"
    );
    assert_eq!(cohort.finish(), vec![EgclVal::from_fixnum(1)]);
    assert_eq!(
        take_activation_counts_for_test(),
        (0, 0),
        "fiber's final recycle must never mutate the controller execution counter"
    );
    HeapCollector::new().full_gc().unwrap();
    for (value, index) in native_arguments(&rows).iter().zip([3, 2, 1, 0]) {
        assert_eq!(
            crate::cli::cp(*value),
            (EgclVal::from_fixnum(200 + index), NIL)
        );
    }
}
