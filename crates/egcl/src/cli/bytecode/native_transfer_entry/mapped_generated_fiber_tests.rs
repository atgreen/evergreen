// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! A generated mapped-to-ordinary hot call remains walkable while its fiber is unmounted.
//! Holding the real origin carrier until completion proves actual migration.

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
    observed: bool,
}
// The probe and publication observer follow the execution, including if its
// continuation resumes on another carrier. The pointer borrows this fiber's
// stable Rust stack only until Observing is dropped before Probe.
static PROBE: egcl_rt::execution_local::ExecutionLocal<Cell<*mut Probe>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(std::ptr::null_mut())) };
fn observe_hot_middle() {
    let probe = PROBE.with(Cell::get);
    if probe.is_null() || mapped_generated::fast_entries_for_test() == 0 {
        return;
    }
    if root_publication::published_frame_has_owner_for_test(unsafe { (*probe).owner }) {
        unsafe {
            (*probe).observed = true;
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

const READY: &str = "*GENERATED-HOT-FIBER-READY*";
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

fn generated_fiber() -> EgclVal {
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let (_, middle) = install(
        "GENERATED-HOT-FIBER-MIDDLE",
        "(a b c d)",
        "((generated-hot-fiber-terminal b d a c))",
        &mut env,
    );
    assert!(owner(&middle).ordinary_entry.is_some());
    let middle_owner = Rc::as_ptr(owner(&middle)) as usize;
    let (symbol, outer) = install_mapped(
        &mut env,
        "GENERATED-HOT-FIBER-OUTER",
        "(a b c d &key (seed (1+ 0)))",
        "(progn (generated-hot-fiber-middle 1 2 3 4) (generated-hot-fiber-middle d c b a))",
    );
    assert!(owner(&outer).ordinary_entry.is_none());
    let warm = [11, 22, 33, 44].map(EgclVal::from_fixnum);
    for _ in 0..2 {
        assert_eq!(
            run_mapped(symbol, &outer, &warm, &mut env).unwrap(),
            warm[1]
        );
    }
    // Do not keep the foreign owner alive with a test-owned Rc after its cell
    // is invalidated. The suspended segment's directory must retain it.
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
    mapped_generated::take_fast_entries_for_test();
    let mut probe = Probe {
        owner: middle_owner,
        observed: false,
    };
    let _observing = observing(&mut probe);
    let result = run_mapped(symbol, &outer, &arguments, &mut env).unwrap();
    let origin = PARKED_ORIGIN.load(Ordering::Acquire);
    assert_ne!(
        origin, 0,
        "external controller must record the actual parked carrier"
    );
    assert_ne!(
        egcl_rt::current_thread_id().0,
        origin,
        "the generated call must return on a different actual carrier"
    );
    assert!(
        mapped_generated::take_fast_entries_for_test() > 0,
        "the real heap call follows the generated lane filled by the same activation"
    );
    assert!(
        probe.observed,
        "the hot route must publish this physical middle owner"
    );
    assert_eq!(result, arguments[2]);
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
    // An already active old owner completed above; the next call must use the
    // new definition published by the other execution while this fiber slept.
    assert_eq!(
        run_mapped(symbol, &outer, &arguments, &mut env).unwrap(),
        EgclVal::from_fixnum(909)
    );
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
                    .is_some_and(|name| name.ends_with("GENERATED-HOT-FIBER-MIDDLE"))
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
                .is_some_and(|name| name.ends_with("GENERATED-HOT-FIBER-MIDDLE"))
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
            .is_some_and(|name| name.ends_with("GENERATED-HOT-FIBER-OUTER"))),
        "{rows:?}"
    );
    // The variadic mapped OUTER may not expose original actuals.
    arguments.clone()
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn generated_hot_call_survives_migration_redefinition_and_external_gc() {
    let _lock = crate::cli::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    PARKED_ORIGIN.store(0, Ordering::Release);
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    egcl_rt::symbols::set_symbol_value_checked(egcl_rt::symbols::intern(READY), NIL).unwrap();
    crate::cli::read_eval_all_env(
        "(defun generated-hot-fiber-terminal (a b c d)
           (if (consp a)
               (progn
                 (setq *generated-hot-fiber-ready* t)
                 (eval '(%park-native-fiber-for-test))
                 (values a b c d))
               d))
         (defun generated-hot-fiber-middle (a b c d)
           (generated-hot-fiber-terminal b d a c))",
        &mut startup,
    )
    .unwrap();
    let group =
        egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers: 2 }).unwrap();
    let entry = unsafe { EgclVal::from_function_ptr(generated_fiber as *const () as *mut u8) };
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
    // Redefinition clears the cell while old middle/outer PCs are still live.
    crate::cli::read_eval_all_env(
        "(defun generated-hot-fiber-middle (a b c d) 909)",
        &mut startup,
    )
    .unwrap();
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
        "resumed generated call must complete while origin stays held",
    );
    assert!(
        !hold.timed_out.load(Ordering::Acquire),
        "origin hold must cover completion"
    );
    let resumed = egcl_rt::thread::fiber_carrier_thread(fiber).expect("completed carrier");
    assert_ne!(
        resumed, origin,
        "the actual generated continuation must migrate"
    );
    assert_eq!(cohort.finish(), vec![EgclVal::from_fixnum(1)]);
    HeapCollector::new().full_gc().unwrap();
    for (value, index) in native_arguments(&rows).iter().zip([3, 2, 1, 0]) {
        assert_eq!(
            crate::cli::cp(*value),
            (EgclVal::from_fixnum(200 + index), NIL)
        );
    }
}
