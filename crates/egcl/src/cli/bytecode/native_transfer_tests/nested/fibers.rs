// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use super::*;
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

static NEXT_CHILD: AtomicUsize = AtomicUsize::new(0);
static MODE: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "native-transfer-test-hooks")]
static REFUSE_TARGET: std::sync::Mutex<Option<egcl_rt::FiberId>> = std::sync::Mutex::new(None);
#[cfg(feature = "native-transfer-test-hooks")]
static REFUSAL_RESUMES: AtomicUsize = AtomicUsize::new(0);
static SETUP: AtomicUsize = AtomicUsize::new(0);
static WARMED: AtomicUsize = AtomicUsize::new(0);
static SNAPSHOT: AtomicBool = AtomicBool::new(false);
static SNAPSHOTTED: AtomicUsize = AtomicUsize::new(0);
static MOVED: AtomicUsize = AtomicUsize::new(0);
static CHILD_MIGRATIONS: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Default)]
struct Probe {
    mode: usize,
    code: usize,
    argument: usize,
    hits: usize,
}
// Only the owning execution reads its live argument location; the pointer is
// published after its Vec is rooted and never survives that invocation.
static PROBE: egcl_rt::execution_local::ExecutionLocal<Cell<Probe>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| Cell::new(Probe::default())) };
static PAUSED: std::sync::Mutex<Vec<egcl_rt::FiberId>> = std::sync::Mutex::new(Vec::new());
static RELEASE: AtomicBool = AtomicBool::new(false);

struct Cohort<'a> {
    group: &'a egcl_rt::SchedulerGroup,
    finished: bool,
}
impl Cohort<'_> {
    fn finish(&mut self) -> Result<Vec<EgclVal>, EgclError> {
        RELEASE.store(true, Ordering::Release);
        let results = self.group.finish();
        self.finished = true;
        #[cfg(feature = "native-transfer-test-hooks")]
        egcl_rt::native_transfer::test_hooks::disarm();
        results
    }
}
impl Drop for Cohort<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // A failing assertion must not leave carriers or earlier fibers
            // mutating the next cohort's shared observations.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.finish()));
        }
    }
}

pub(in crate::cli::bytecode::native_transfer_tests) fn pause() -> Result<EgclVal, EgclError> {
    let probe = PROBE.with(|slot| {
        let mut probe = slot.get();
        probe.hits += 1;
        slot.set(probe);
        probe
    });
    assert_eq!(probe.hits, 1, "child pause must execute exactly once");
    let capture = super::super::super::native_transfer_entry::child_capture_for_test();
    if matches!(probe.mode, 3 | 5) {
        assert!(
            capture.is_none(),
            "cold recovery must mask its abandoned capture"
        );
    } else {
        assert_eq!(
            capture,
            Some((probe.code, true)),
            "pause must belong to the selected child"
        );
    }
    let segment = egcl_rt::native_transfer::current_segment();
    assert!(!segment.is_null());
    let fiber = egcl_rt::current_fiber_id().unwrap();
    assert_eq!(
        unsafe { (*segment).owner() },
        egcl_rt::native_transfer::SegmentOwner::Fiber(fiber)
    );
    egcl_rt::rooted!(value = unsafe { *(probe.argument as *const EgclVal) });
    egcl_rt::rooted!(held = super::super::super::super::arena_cons(*value, NIL));
    PAUSED.lock().unwrap().push(fiber);
    let mut before = None;
    #[cfg(feature = "native-transfer-test-hooks")]
    let mut refusal_armed = false;
    while !RELEASE.load(Ordering::Acquire) {
        if before.is_none() && SNAPSHOT.load(Ordering::Acquire) {
            before = Some(held.to_raw());
            SNAPSHOTTED.fetch_add(1, Ordering::Release);
        }
        #[cfg(feature = "native-transfer-test-hooks")]
        if !refusal_armed && *REFUSE_TARGET.lock().unwrap() == Some(fiber) {
            // The driver selects this child only after every child has paused.
            // No other Lisp allocation can promote this fresh relocation probe.
            *held = super::super::super::super::arena_cons(*value, NIL);
            egcl_rt::native_transfer::test_hooks::arm(vec![fiber]);
            refusal_armed = true;
        }
        let carrier = egcl_rt::current_thread_id();
        #[cfg(feature = "native-transfer-test-hooks")]
        let before_refusal = held.to_raw();
        egcl_rt::fiber_sleep(std::time::Duration::from_millis(10))?;
        #[cfg(feature = "native-transfer-test-hooks")]
        if let Some((required, refused)) =
            egcl_rt::native_transfer::test_hooks::take_rejection(fiber)
        {
            assert_eq!(egcl_rt::current_thread_id(), required);
            assert_ne!(required, refused);
            assert_ne!(
                held.to_raw(),
                before_refusal,
                "refusal collection must move the child's live root"
            );
            REFUSAL_RESUMES.fetch_add(1, Ordering::Release);
        }
        if carrier != egcl_rt::current_thread_id() {
            CHILD_MIGRATIONS.fetch_add(1, Ordering::Relaxed);
        }
        assert_eq!(egcl_rt::native_transfer::current_segment(), segment);
        assert_eq!(
            unsafe { (*segment).carrier() },
            egcl_rt::current_thread_id()
        );
        assert_eq!(
            super::super::super::native_transfer_entry::child_capture_for_test(),
            capture
        );
    }
    if before.is_some_and(|before| held.to_raw() != before) {
        MOVED.fetch_add(1, Ordering::Relaxed);
    }
    assert_eq!(cp(*held), (*value, NIL));
    assert_eq!(*value, unsafe { *(probe.argument as *const EgclVal) });
    Ok(NIL)
}

fn child_fiber() -> EgclVal {
    // Assertions must unwind within Rust, never across the scheduler trampoline.
    std::panic::catch_unwind(child_fiber_checked).unwrap_or(EgclVal::from_fixnum(0))
}

fn child_fiber_checked() -> EgclVal {
    let case = NEXT_CHILD.fetch_add(1, Ordering::Relaxed);
    let mode = MODE.load(Ordering::Relaxed);
    let mut env = Env::new_impl(false, false, false);
    egcl_rt::rooted_ref!(_env = &mut env);
    let name = format!("MIGRATING-CHILD-{case}");
    let pause = format!("CHILD-PAUSE-{case}");
    let handler = format!("CHILD-HANDLER-{case}");
    let finish = format!("CHILD-FINISH-{case}");
    let restart = format!("CHILD-RESUME-{case}");
    let source = match mode {
        0 => format!("((catch :child ({pause} x active) (values x (list :body))))"),
        1 => format!(
            "((unwind-protect (if active (throw :parent (values x (list :exceptional-cleanup))) (values x (list :warm))) ({pause} x active)))"
        ),
        2 => format!(
            "((restart-case (handler-bind ((type-error #'{handler})) (if active (symbol-value x) (values x (list :warm)))) ({restart} (v) (values v (list :handler)))))"
        ),
        3 => format!(
            "((catch :child (if active (throw :child x) nil)) ({pause} x active) (values x (list :cold)))"
        ),
        4 => format!("((unwind-protect (values x (list :normal-cleanup)) ({pause} x active)))"),
        5 => format!("((+ n 1) ({pause} x active) ({finish} x))"),
        _ => unreachable!(),
    };
    super::super::super::super::read_eval_all_env(
        &format!(
            "(defun {pause} (x active)
           (declare (ignore x))
           (if active (%native-child-suspend-for-test) nil))
         (defun {handler} (c)
           ({pause} (slot-value c 'datum) t)
           (invoke-restart '{restart} (slot-value c 'datum)))
         (defun {finish} (x) (values x (list :deopt)))
         (defun {name} (x active n) {})",
            &source[1..source.len() - 1]
        ),
        &mut env,
    )
    .unwrap();
    egcl_rt::rooted!(params = reader::read_from_string("(x active n)").unwrap().0);
    egcl_rt::rooted!(forms = reader::read_from_string(&source).unwrap().0);
    let symbol = egcl_rt::symbols::intern(&name);
    let body = Arc::new(compile_function(&name, *params, *forms, &env, false, false).unwrap());
    registry_put(symbol, Arc::clone(&body));
    let mut code = if mode == 5 {
        let code = TransferCode::compile(Arc::clone(&body)).unwrap();
        assert!(
            code.has_deopt,
            "fiber must suspend in a real guard continuation"
        );
        code
    } else {
        TransferCode::compile_nested_protected(Arc::clone(&body)).unwrap()
    };
    if mode == 3 {
        let push = body
            .code
            .iter()
            .position(|op| matches!(op, Instr::PushCatch { .. }))
            .unwrap() as u32;
        code = code.without_catch_destination(push);
    }
    let installed =
        super::super::super::native_transfer_entry::install_baseline_code(symbol, code).unwrap();
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &installed,
    );
    let caller = compile_caller(
        "(x active n)",
        &format!("((catch :parent ({name} x active n)))"),
        &env,
    );
    // Global publication invalidates call cells, so finish every setup before
    // resolving the measured callers. Warm helpers before the final barrier too.
    caller
        .run(&[NIL, NIL, EgclVal::from_fixnum(1)], &mut env)
        .unwrap();
    SETUP.fetch_add(1, Ordering::Release);
    while SETUP.load(Ordering::Acquire) != 8 {
        assert!(!RELEASE.load(Ordering::Acquire), "setup aborted");
        egcl_rt::fiber_sleep(std::time::Duration::from_millis(1)).unwrap();
    }
    caller
        .run(&[NIL, NIL, EgclVal::from_fixnum(1)], &mut env)
        .unwrap();
    WARMED.fetch_add(1, Ordering::Release);
    while WARMED.load(Ordering::Acquire) != 8 {
        assert!(!RELEASE.load(Ordering::Acquire), "warmup aborted");
        egcl_rt::fiber_sleep(std::time::Duration::from_millis(1)).unwrap();
    }
    take_nested_entries();
    super::super::super::native_transfer_entry::take_native_fallback_count();
    let stack = egcl_rt::current_stack();
    let fp = stack.fp();
    let sp = stack.sp();
    let depth = NATIVE_DEPTH.with(|depth| depth.get());
    let fiber = egcl_rt::current_fiber_id();
    egcl_rt::rooted!(
        args = vec![
            super::super::super::super::arena_cons(EgclVal::from_fixnum(case as i64), NIL),
            T,
            EgclVal::from_single_float(1.5)
        ]
    );
    let address = args[0].to_raw();
    let NativeCodeStorage::Mapped(mapped) = &installed._storage else {
        panic!("mapped baseline");
    };
    PROBE.with(|slot| {
        slot.set(Probe {
            mode,
            code: Rc::as_ptr(mapped) as usize,
            argument: args.as_mut_ptr() as usize,
            hits: 0,
        })
    });
    egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
    assert_eq!(PROBE.with(|slot| slot.get().hits), 1);
    if mode == 5 {
        assert_eq!(mapped.deopt_count(), 1);
    }
    assert_eq!(
        take_nested_entries(),
        1,
        "the suspended invocation must use a mapped child"
    );
    if mode == 3 {
        assert_eq!(
            super::super::super::native_transfer_entry::take_native_fallback_count(),
            1
        );
    }
    assert_eq!(*result, args[0]);
    assert_ne!(
        args[0].to_raw(),
        address,
        "the child's live argument must relocate"
    );
    assert_eq!(cp(args[0]).0, EgclVal::from_fixnum(case as i64));
    assert!(env.mv_active);
    assert_eq!(env.mv.len(), 2);
    assert_eq!(env.mv[0], args[0]);
    let expected = reader::read_from_string(
        [
            ":body",
            ":exceptional-cleanup",
            ":handler",
            ":cold",
            ":normal-cleanup",
            ":deopt",
        ][mode],
    )
    .unwrap()
    .0;
    assert_eq!(cp(env.mv[1]), (expected, NIL));
    assert_eq!(egcl_rt::current_fiber_id(), fiber);
    assert_eq!(stack.fp(), fp);
    assert_eq!(stack.sp(), sp);
    assert_eq!(NATIVE_DEPTH.with(|depth| depth.get()), depth);
    assert!(egcl_rt::native_transfer::current_segment().is_null());
    assert!(env.handlers.is_empty());
    assert!(env.restarts.is_empty());
    assert!(env.catch_stack.is_empty());
    EgclVal::from_fixnum(1)
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_protected_children_survive_fiber_suspension_and_migration() {
    run_children(false);
}

#[cfg(feature = "native-transfer-test-hooks")]
#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_protected_children_reject_incompatible_carriers() {
    run_children(true);
}

fn run_children(refuse: bool) {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    for num_workers in if refuse { &[4][..] } else { &[1, 4][..] } {
        let num_workers = *num_workers;
        for mode in 0..6 {
            MODE.store(mode, Ordering::Relaxed);
            #[cfg(feature = "native-transfer-test-hooks")]
            {
                *REFUSE_TARGET.lock().unwrap() = None;
            }
            SETUP.store(0, Ordering::Relaxed);
            WARMED.store(0, Ordering::Relaxed);
            SNAPSHOT.store(false, Ordering::Relaxed);
            SNAPSHOTTED.store(0, Ordering::Relaxed);
            MOVED.store(0, Ordering::Relaxed);
            CHILD_MIGRATIONS.store(0, Ordering::Relaxed);
            RELEASE.store(false, Ordering::Release);
            PAUSED.lock().unwrap().clear();
            let group =
                egcl_rt::SchedulerGroup::init(&egcl_rt::SchedulerConfig { num_workers }).unwrap();
            let mut cohort = Cohort {
                group: &group,
                finished: false,
            };
            let mut fibers = Vec::new();
            for _ in 0..8 {
                let entry =
                    unsafe { EgclVal::from_function_ptr(child_fiber as *const () as *mut u8) };
                let fiber = egcl_rt::thread::make_fiber(entry).unwrap();
                fibers.push(fiber);
                group.submit(fiber).unwrap();
            }
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
            loop {
                let paused = PAUSED.lock().unwrap().clone();
                if paused.len() == fibers.len()
                    && paused.iter().all(|fiber| {
                        matches!(
                            egcl_rt::thread::fiber_state(*fiber),
                            Some(
                                egcl_rt::thread::FiberState::Waiting
                                    | egcl_rt::thread::FiberState::Blocked
                            )
                        )
                    })
                {
                    break;
                }
                egcl_rt::poll_safepoint();
                assert!(
                    std::time::Instant::now() < deadline,
                    "all selected child continuations must pause (mode {mode}, reached {})",
                    paused.len()
                );
                std::thread::yield_now();
            }
            #[cfg(feature = "native-transfer-test-hooks")]
            if refuse {
                REFUSAL_RESUMES.store(0, Ordering::Relaxed);
                *REFUSE_TARGET.lock().unwrap() = Some(fibers[0]);
                while REFUSAL_RESUMES.load(Ordering::Acquire) != 1 {
                    egcl_rt::poll_safepoint();
                    assert!(
                        std::time::Instant::now() < deadline,
                        "child must resume after refusal"
                    );
                    std::thread::yield_now();
                }
                assert_eq!(
                    egcl_rt::native_transfer::test_hooks::observations(),
                    (1, 1, false)
                );
            }
            SNAPSHOT.store(true, Ordering::Release);
            while SNAPSHOTTED.load(Ordering::Acquire) != fibers.len()
                || (num_workers == 4 && CHILD_MIGRATIONS.load(Ordering::Relaxed) == 0)
            {
                egcl_rt::poll_safepoint();
                assert!(
                    std::time::Instant::now() < deadline,
                    "children must snapshot and migrate"
                );
                std::thread::yield_now();
            }
            if !refuse {
                HeapCollector::new().minor_gc().unwrap();
            }
            RELEASE.store(true, Ordering::Release);
            assert_eq!(
                cohort.finish().unwrap(),
                vec![EgclVal::from_fixnum(1); 8],
                "mode {mode}, workers {num_workers}"
            );
            assert!(
                refuse || MOVED.load(Ordering::Relaxed) > 0,
                "external collection must relocate a paused child's precise root"
            );
            if num_workers == 4 {
                assert!(
                    CHILD_MIGRATIONS.load(Ordering::Relaxed) > 0,
                    "mode {mode} must actually migrate"
                );
            }
        }
    }
}
