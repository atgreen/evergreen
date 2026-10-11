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
static RECURSIVE_ARGUMENT_MOVES: AtomicUsize = AtomicUsize::new(0);

#[derive(Clone, Copy, Default)]
struct Probe {
    mode: usize,
    code: usize,
    argument: usize,
    hits: usize,
    recursive_entries: usize,
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
    let recursive = probe.mode >= 6;
    let stack = egcl_rt::current_stack();
    let suspended_frame = stack.fp();
    let suspended_sp = stack.sp();
    let mut argument_before = None;
    if recursive {
        let entered = super::super::super::native_transfer_entry::take_recursive_entries();
        assert_eq!(
            entered, 4,
            "four direct native recursive frames must be live before suspension"
        );
        PROBE.with(|slot| {
            let mut state = slot.get();
            state.recursive_entries = entered;
            slot.set(state);
        });
    }
    let capture = super::super::super::native_transfer_entry::child_capture_for_test();
    let cold_recovery = matches!(probe.mode, 3 | 5);
    // A Rust pause callback can own one or more outer adapter segments. Follow
    // their verified physical ancestry to the selected mapped child. Cold
    // recovery deliberately retired its capture; its fallback/deopt and frame
    // retirement assertions below remain the evidence for that separate path.
    let adapter_callers = if cold_recovery {
        None
    } else {
        super::super::super::native_callable::outer::suspended_callers_for_test()
    };
    let selected_capture = if let Some(callers) = &adapter_callers {
        assert!(capture.is_none(), "outer pause adapter must hide CAPTURE");
        let (last, intermediates) = callers
            .split_last()
            .expect("pause adapter retains its caller");
        assert!(intermediates.iter().all(|(_, capture)| capture.is_none()));
        assert_ne!(last.0, 0, "selected mapped child segment must remain live");
        last.1
    } else {
        capture
    };
    if cold_recovery {
        assert!(
            capture.is_none(),
            "cold recovery must mask its abandoned capture"
        );
    } else {
        assert_eq!(
            selected_capture,
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
            if recursive {
                argument_before = Some(unsafe { *(probe.argument as *const EgclVal) }.to_raw());
            }
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
        if recursive {
            assert!(
                std::ptr::eq(egcl_rt::current_stack(), stack),
                "fiber migration must retain the recursive activation's owned Lisp stack"
            );
            assert_eq!(stack.fp(), suspended_frame);
            assert_eq!(stack.sp(), suspended_sp);
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
        if !cold_recovery {
            assert_eq!(
                super::super::super::native_callable::outer::suspended_callers_for_test(),
                adapter_callers,
                "migration must preserve every outer adapter and its selected mapped child"
            );
        }
    }
    if before.is_some_and(|before| held.to_raw() != before) {
        MOVED.fetch_add(1, Ordering::Relaxed);
    }
    if argument_before.is_some_and(|before| unsafe { *(probe.argument as *const EgclVal) }.to_raw() != before) {
        RECURSIVE_ARGUMENT_MOVES.fetch_add(1, Ordering::Relaxed);
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
        6 | 7 | 8 | 9 => format!(
            "((if (= n 0) (progn ({pause} x active) ({finish} x active))
                (progn ({name} x active (- n 1))
                       (if (= (car x) {case}) x (error \"recursive caller value changed\")))))"
        ),
        _ => unreachable!(),
    };
    let finish_definition = if mode >= 6 {
        if matches!(mode, 7 | 9) {
            format!(
                "(defun {finish} (x active) (if active (throw :parent (values x (list :recursive-transfer))) x))"
            )
        } else {
            format!("(defun {finish} (x active) (declare (ignore active)) x)")
        }
    } else {
        format!("(defun {finish} (x) (values x (list :deopt)))")
    };
    super::super::super::super::read_eval_all_env(
        &format!(
            "(defun {pause} (x active)
           (declare (ignore x))
           (if active (%native-child-suspend-for-test) nil))
         (defun {handler} (c)
           ({pause} (slot-value c 'datum) t)
           (invoke-restart '{restart} (slot-value c 'datum)))
         {finish_definition}
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
    let mut code = if mode >= 5 {
        let code = TransferCode::compile(Arc::clone(&body)).unwrap();
        assert!(
            mode != 5 || code.has_deopt,
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
    let _trace_pins = (mode >= 8).then(||
        super::super::ordinary_t2::RecursiveFixturePins::new(&[&pause, &finish]));
    let installed = if mode >= 8 {
        let (selected, installed) = super::super::ordinary_t2::install(
            &name, "(x active n)", &source, &mut env);
        assert_eq!(selected, symbol);
        let NativeCodeStorage::Mapped(mapped) = &installed._storage else { panic!("mapped T2") };
        assert!(installed.is_t2 && mapped.native_entry_kind_slot.is_some());
        installed
    } else {
        super::super::super::native_transfer_entry::install_baseline_code(symbol, code).unwrap()
    };
    publish_native(
        symbol,
        egcl_rt::symbols::symbol_function(symbol),
        &installed,
    );
    let caller_source = if mode >= 6 {
        format!(
            "((catch :parent (let ((answer ({name} x active n))) (values answer (list :recursive-return)))))"
        )
    } else {
        format!("((catch :parent ({name} x active n)))")
    };
    let caller = compile_caller("(x active n)", &caller_source, &env);
    let warm_depth = if mode >= 6 { 0 } else { 1 };
    // Global publication invalidates call cells, so finish every setup before
    // resolving the measured callers. Warm helpers before the final barrier too.
    caller
        .run(&[NIL, NIL, EgclVal::from_fixnum(warm_depth)], &mut env)
        .unwrap();
    SETUP.fetch_add(1, Ordering::Release);
    while SETUP.load(Ordering::Acquire) != 8 {
        assert!(!RELEASE.load(Ordering::Acquire), "setup aborted");
        egcl_rt::fiber_sleep(std::time::Duration::from_millis(1)).unwrap();
    }
    caller
        .run(&[NIL, NIL, EgclVal::from_fixnum(warm_depth)], &mut env)
        .unwrap();
    WARMED.fetch_add(1, Ordering::Release);
    while WARMED.load(Ordering::Acquire) != 8 {
        assert!(!RELEASE.load(Ordering::Acquire), "warmup aborted");
        egcl_rt::fiber_sleep(std::time::Duration::from_millis(1)).unwrap();
    }
    take_nested_entries();
    super::super::super::native_transfer_entry::take_recursive_entries();
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
            if mode >= 6 {
                EgclVal::from_fixnum(4)
            } else {
                EgclVal::from_single_float(1.5)
            }
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
            recursive_entries: 0,
        })
    });
    egcl_rt::rooted!(result = caller.run(&args, &mut env).unwrap());
    assert_eq!(PROBE.with(|slot| slot.get().hits), 1);
    if mode >= 6 {
        assert_eq!(PROBE.with(|slot| slot.get().recursive_entries), 4);
        assert_eq!(
            super::super::super::native_transfer_entry::take_recursive_entries(),
            0,
            "resuming must retire the original frames without replaying recursion"
        );
        assert_eq!(
            super::super::super::native_transfer_entry::take_native_fallback_count(),
            0,
            "recursive suspension/retirement must not fall back to the interpreter"
        );
    }
    if mode == 5 {
        assert_eq!(mapped.deopt_count(), 1);
    }
    if mode >= 8 {
        assert_eq!(mapped.deopt_count(), 0,
            "normal return and THROW must retire the original native chain without deoptimization");
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
            ":recursive-return",
            ":recursive-transfer",
            ":recursive-return",
            ":recursive-transfer",
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

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn native_v2_recursive_frames_retire_after_fiber_migration() {
    run_child_modes(false, 6..8);
}

fn stopped_native_rows(
    fiber: egcl_rt::FiberId,
    deadline: std::time::Instant,
) -> Vec<egcl_rt::debug_stack::LogicalFrame> {
    loop {
        // capture_fiber owns the mount/state exclusion. A sampled FiberState
        // alone would not authorize dereferencing a published native stack.
        if let Some(frames) = egcl_rt::debug_stack::capture_fiber(fiber, 64).unwrap() {
            return frames;
        }
        egcl_rt::poll_safepoint();
        assert!(std::time::Instant::now() < deadline, "native fiber did not stop");
        std::thread::yield_now();
    }
}

fn assert_stopped_native_rows(frames: &[egcl_rt::debug_stack::LogicalFrame]) -> u64 {
    use egcl_rt::debug_stack::FrameOrigin;
    let rows: Vec<_> = frames.iter().filter(|frame| frame.function.as_deref()
        .is_some_and(|name| name.starts_with("MIGRATING-CHILD-"))).collect();
    assert_eq!(rows.len(), 5, "four physical recursive children and one real managed anchor");
    let name = rows[0].function.as_deref().unwrap();
    let case: i64 = name.strip_prefix("MIGRATING-CHILD-").unwrap().parse().unwrap();
    let argument = rows[0].arguments.as_ref().expect("original native arguments")[0];
    assert_eq!(cp(argument), (EgclVal::from_fixnum(case), NIL));
    for (depth, row) in rows.iter().enumerate() {
        assert_eq!(row.function.as_deref(), Some(name));
        assert_eq!(row.origin, if depth < 4 { FrameOrigin::Native } else { FrameOrigin::Managed });
        assert_eq!(row.arguments.as_deref(),
            Some(&[argument, T, EgclVal::from_fixnum(depth as i64)][..]));
    }
    let leaf_name = format!("CHILD-PAUSE-{case}");
    let leaf = frames.iter().position(|frame| frame.function.as_deref() == Some(leaf_name.as_str())).unwrap();
    let native = frames.iter().position(|frame| frame.function.as_deref() == Some(name)).unwrap();
    assert!(leaf < native, "suspended interpreted leaf must precede native callers");
    argument.to_raw()
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_backtraces_survive_stopped_fiber_gc_and_migration() {
    run_child_modes(false, 8..9);
}

#[test]
#[ignore = "requires a platform-supported native segment transition"]
fn ordinary_t2_recursive_throw_retires_native_frames_after_fiber_migration() {
    run_child_modes(false, 9..10);
}

fn run_children(refuse: bool) {
    run_child_modes(refuse, 0..6);
}

fn run_child_modes(refuse: bool, modes: std::ops::Range<usize>) {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    assert!(egcl_rt::native_transfer::is_supported());
    let mut startup = Env::new(false);
    egcl_rt::rooted_ref!(_startup = &mut startup);
    for num_workers in if refuse { &[4][..] } else { &[1, 4][..] } {
        let num_workers = *num_workers;
        for mode in modes.clone() {
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
            RECURSIVE_ARGUMENT_MOVES.store(0, Ordering::Relaxed);
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
            // Do not retain the first argument copies as roots: the suspended
            // execution alone must preserve and relocate its native arguments.
            let trace_before = if mode >= 8 {
                fibers.iter().map(|&fiber|
                    assert_stopped_native_rows(&stopped_native_rows(fiber, deadline)))
                    .collect::<Vec<_>>()
            } else { Vec::new() };
            if !refuse {
                HeapCollector::new().minor_gc().unwrap();
            }
            egcl_rt::rooted!(traces = Vec::<Vec<egcl_rt::debug_stack::LogicalFrame>>::new());
            if mode >= 8 {
                for &fiber in &fibers {
                    traces.push(stopped_native_rows(fiber, deadline));
                }
                let after: Vec<_> = traces.iter().map(|rows| assert_stopped_native_rows(rows)).collect();
                assert!(trace_before.iter().zip(after).any(|(before, after)| *before != after),
                    "native original arguments must actually relocate while their fibers are stopped");
            }
            RELEASE.store(true, Ordering::Release);
            assert_eq!(
                cohort.finish().unwrap(),
                vec![EgclVal::from_fixnum(1); 8],
                "mode {mode}, workers {num_workers}"
            );
            if mode >= 8 {
                HeapCollector::new().minor_gc().unwrap();
                assert_eq!(traces.len(), fibers.len());
                for rows in traces.iter() {
                    assert_stopped_native_rows(rows);
                }
            }
            assert!(
                refuse || MOVED.load(Ordering::Relaxed) > 0,
                "external collection must relocate a paused child's precise root"
            );
            if mode >= 6 {
                assert!(
                    RECURSIVE_ARGUMENT_MOVES.load(Ordering::Relaxed) > 0,
                    "GC must relocate a native recursive caller's argument while suspended"
                );
            }
            if num_workers == 4 {
                assert!(
                    CHILD_MIGRATIONS.load(Ordering::Relaxed) > 0,
                    "mode {mode} must actually migrate"
                );
            }
        }
    }
}
