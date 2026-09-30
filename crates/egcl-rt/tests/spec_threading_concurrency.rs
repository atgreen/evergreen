// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::fs;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use std::time::Instant;
use egcl_rt::error::EgclError;
use egcl_rt::object::type_id;
use egcl_rt::runtime::{check_sigint, install_signal_handlers};
use egcl_rt::safepoint::{
    enter_safepoint, poll_safepoint, resume_all_threads, wait_for_all_threads,
};
use egcl_rt::scheduler::{Scheduler, SchedulerConfig};
use egcl_rt::thread::{
    all_thread_ids, current_thread, current_thread_id, interrupt_thread, join_thread, make_fiber,
    make_thread, safepoint_participant_count_excluding, thread_is_carrier,
};
use egcl_rt::value::{NIL, T, EgclVal};
use egcl_rt::{Collector, GcConfig, HeapCollector, alloc_typed};

static SLOW_THREAD_STARTED: AtomicBool = AtomicBool::new(false);
static RELEASE_SLOW_THREAD: AtomicBool = AtomicBool::new(false);
static SAFETY_LOOP_EXIT: AtomicBool = AtomicBool::new(false);
static POLL_ITERATIONS: AtomicUsize = AtomicUsize::new(0);
static T0_ALLOC_LOOP_EXIT: AtomicBool = AtomicBool::new(false);
static T0_ALLOC_ITERATIONS: AtomicUsize = AtomicUsize::new(0);
#[cfg(feature = "c-ffi")]
static NATIVE_FFI_STARTED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "c-ffi")]
static NATIVE_FFI_FINISHED: AtomicBool = AtomicBool::new(false);
#[cfg(feature = "c-ffi")]
static USLEEP_FN: OnceLock<usize> = OnceLock::new();

/// Serializes every test that touches PROCESS-GLOBAL runtime state (bliss-z11t).
///
/// `cargo test` runs the tests in one binary concurrently, but the thread
/// registry, the safepoint handshake and the GC are one per process, and these
/// tests each drive them as though they owned it. Concurrently they invalidate
/// each other in three different ways, which is why this target produced three
/// different symptoms rather than one:
///
///   - Count assertions break. `lazily_registered_host_threads_...` samples
///     `safepoint_participant_count_excluding` before/during/after, and
///     `make_thread_join_...` bounds `all_thread_ids().len()`; another test
///     spawning or joining a thread moves those global numbers.
///   - Stop-the-world assertions break. `stop_the_world_...` and
///     `t0_alloc_typed_...` call `wait_for_all_threads()` / `resume_all_threads()`
///     and then assert their thread stayed parked. A concurrent test's
///     `resume_all_threads()` restarts it, and
///     `failed_safepoint_handshake_is_reported_and_cleans_up` deliberately
///     creates a thread that NEVER polls -- so while it runs, every other
///     rendezvous in the process is meant to fail.
///   - The run hangs or faults. Two overlapping stop-the-world rendezvous leave
///     the coordinator waiting on threads the other side already resumed.
///
/// So this is interference over a single global coordinator, not a race the
/// production runtime can reach: there is one collector, and nothing outside a
/// test harness drives two simultaneous handshakes. Tests that only read
/// thread-local or per-object state deliberately do NOT take this lock, so they
/// still run in parallel.
fn runtime_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

/// Hold the global-state lock for the rest of the test. Poisoning is recovered
/// rather than propagated: one test panicking must not cascade into spurious
/// failures in every later test that takes the lock.
macro_rules! serialize_global_runtime {
    () => {
        let _runtime_guard = runtime_lock().lock().unwrap_or_else(|e| e.into_inner());
    };
}

fn gc_config() -> GcConfig {
    GcConfig {
        heap_size: 64 * 1024,
        heap_max: 128 * 1024,
        nursery_size: 8 * 1024,
        tlab_size: 256,
        region_size: 4 * 1024,
        promotion_threshold: 1,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.5,
    }
}

fn heap_value_with_marker(marker: u64) -> EgclVal {
    let body = alloc_typed(16, type_id::BIGNUM).expect("heap value allocation");
    unsafe { *(body as *mut u64) = marker };
    unsafe { EgclVal::from_heap_ptr(body.sub(8)) }
}

fn heap_marker(value: EgclVal) -> u64 {
    unsafe { *((value.as_ptr().add(8)) as *const u64) }
}

fn value_returning_entry() -> EgclVal {
    EgclVal::from_fixnum(1234)
}

fn tls_isolated_entry() -> EgclVal {
    let thread = current_thread();
    thread.tls_set(0, EgclVal::from_fixnum(99));
    thread.tls_get(0)
}

fn slow_interruptible_entry() -> EgclVal {
    SLOW_THREAD_STARTED.store(true, Ordering::Release);
    while !RELEASE_SLOW_THREAD.load(Ordering::Acquire) {
        std::thread::yield_now();
    }
    T
}

fn polling_entry() -> EgclVal {
    while !SAFETY_LOOP_EXIT.load(Ordering::Acquire) {
        POLL_ITERATIONS.fetch_add(1, Ordering::AcqRel);
        poll_safepoint();
        std::thread::yield_now();
    }
    EgclVal::from_fixnum(POLL_ITERATIONS.load(Ordering::Acquire) as i64)
}

fn t0_allocating_entry() -> EgclVal {
    while !T0_ALLOC_LOOP_EXIT.load(Ordering::Acquire) {
        alloc_typed(16, type_id::BIGNUM).expect("T0 allocation should succeed");
        T0_ALLOC_ITERATIONS.fetch_add(1, Ordering::AcqRel);
        std::thread::yield_now();
    }
    EgclVal::from_fixnum(T0_ALLOC_ITERATIONS.load(Ordering::Acquire) as i64)
}

#[cfg(feature = "c-ffi")]
fn native_ffi_entry() -> EgclVal {
    NATIVE_FFI_STARTED.store(true, Ordering::Release);
    let usleep = *USLEEP_FN.get().expect("usleep must be configured") as *const ();
    unsafe {
        egcl_rt::ffi::ffi_call(
            usleep,
            &egcl_rt::ffi::AlienType::Int {
                signed: true,
                bits: 32,
            },
            &[egcl_rt::ffi::AlienType::Int {
                signed: false,
                bits: 32,
            }],
            &[200_000],
        )
        .expect("usleep FFI call must succeed");
    }
    NATIVE_FFI_FINISHED.store(true, Ordering::Release);
    T
}

unsafe fn fn_entry(function: fn() -> EgclVal) -> EgclVal {
    unsafe { EgclVal::from_function_ptr(function as usize as *mut u8) }
}

fn read_runtime_source(path: &str) -> String {
    fs::read_to_string(path).unwrap_or_else(|err| panic!("failed to read {}: {}", path, err))
}

#[test]
fn fiber_api_design_is_complete_and_distinct_from_exposed_carrier_threads() {
    // bliss-jtc.14.4: lock the JVM-style public split into an executable
    // specification contract before the remaining scheduler implementation is
    // filled in. Carrier/platform threads and fibers are intentionally not
    // aliases and are introspected through different packages.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let extensions = fs::read_to_string(repo.join("spec/09-extensions.md"))
        .expect("read extension specification");
    let runtime = fs::read_to_string(repo.join("spec/02-runtime-core.md"))
        .expect("read runtime specification");
    let concurrency = fs::read_to_string(repo.join("spec/13-concurrency.md"))
        .expect("read concurrency specification");

    for api in [
        "EGCL-THREAD",
        "EGCL-FIBER",
        "make-fiber",
        "submit-fiber",
        "run-fibers",
        "start-fibers",
        "finish-fibers",
        "fiber-join",
        "fiber-yield",
        "fiber-park",
        "fiber-sleep",
        "fiber-pin",
        "fiber-state",
        "print-fiber-backtrace",
        "scheduler-group-carriers",
    ] {
        assert!(
            extensions
                .to_ascii_lowercase()
                .contains(&api.to_ascii_lowercase()),
            "fiber API contract must specify {api}"
        );
    }
    assert!(extensions.contains("MAKE-THREAD") && extensions.contains("OS thread"));
    assert!(runtime.contains("distinct public object"));
    assert!(concurrency.contains("A thread and a fiber MUST NOT be aliases"));
    assert!(concurrency.contains("exposed carrier"));
}

#[test]
fn make_thread_join_and_registry_cleanup_follow_the_public_thread_api() {
    serialize_global_runtime!();
    // Per R2.04 and R13.18, user-visible fibers run via the public make/join entrypoints.
    let before = all_thread_ids();
    let id = make_thread(T).expect("thread creation must succeed");
    assert!(all_thread_ids().contains(&id));

    let result = join_thread(id).expect("join must return the thread result");
    assert_eq!(result, T);
    assert!(!all_thread_ids().contains(&id));
    assert!(all_thread_ids().len() <= before.len() + 1);
}

#[test]
fn lazily_registered_host_threads_participate_in_gc_safepoints() {
    serialize_global_runtime!();
    // Host/test threads that enter the runtime through CURRENT-THREAD have no Lisp
    // entry function, but they can still allocate on the GC heap and must be counted
    // as native mutators until their TLS registration is dropped.
    let current = current_thread_id();
    let before = safepoint_participant_count_excluding(current);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        let id = current_thread_id();
        ready_tx.send(id).expect("send registered thread id");
        release_rx.recv().expect("wait for release");
    });

    let worker = ready_rx.recv().expect("worker should register");
    assert_ne!(worker, current);
    let during = safepoint_participant_count_excluding(current);
    assert!(
        during > before,
        "a lazily registered NIL-entry host thread must count as a GC mutator"
    );

    release_tx.send(()).expect("release worker");
    handle.join().expect("worker exits cleanly");
    assert!(
        safepoint_participant_count_excluding(current) < during,
        "lazy host thread TLS drop should remove it from GC safepoint participation"
    );
}

#[test]
fn exited_lazy_host_threads_are_pruned_from_public_registry_views() {
    serialize_global_runtime!();
    // h6z.6: lazy host/test thread records must not be removed from TLS drop
    // with an OrderedMutex, but they should disappear on later safe registry
    // access once their TLS registration has ended.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();

    let handle = std::thread::spawn(move || {
        ready_tx
            .send(current_thread_id())
            .expect("send registered thread id");
    });

    let worker = ready_rx.recv().expect("worker should register");
    handle.join().expect("worker exits cleanly");

    assert!(
        !all_thread_ids().contains(&worker),
        "exited lazy host thread must not leak through ALL-THREADS"
    );
    assert_eq!(
        thread_is_carrier(worker),
        None,
        "exited lazy host thread must not remain queryable as a public thread"
    );
}

#[test]
fn function_entries_execute_on_worker_threads_and_return_values() {
    serialize_global_runtime!();
    // Per R2.04, fibers are scheduled onto the carrier pool.
    // Per R13.18, MAKE-THREAD and JOIN-THREAD expose observable thread execution.
    let id = make_thread(unsafe { fn_entry(value_returning_entry) }).expect("thread creation");
    let value = join_thread(id).expect("join must succeed");
    assert_eq!(value, EgclVal::from_fixnum(1234));
}

#[test]
fn invalid_thread_entry_surfaces_a_result_error_not_a_panic() {
    serialize_global_runtime!();
    // Per R2.18, runtime failures propagate through Result rather than panicking.
    let id = make_thread(EgclVal::from_fixnum(17)).expect("thread creation");
    let err = join_thread(id).expect_err("non-function, non-special thread entry must fail");
    assert!(matches!(err, EgclError::TypeError { expected, .. } if expected == "function"));
}

#[test]
fn thread_local_storage_is_isolated_between_green_threads() {
    serialize_global_runtime!();
    // Per R2.05, each fiber has its own control/value stack and thread-local state.
    current_thread().tls_set(0, EgclVal::from_fixnum(7));
    let id = make_thread(unsafe { fn_entry(tls_isolated_entry) }).expect("thread creation");
    let child_value = join_thread(id).expect("join must succeed");

    assert_eq!(child_value, EgclVal::from_fixnum(99));
    assert_eq!(current_thread().tls_get(0), EgclVal::from_fixnum(7));
}

#[test]
fn native_thread_tls_roots_survive_minor_gc() {
    // h6z.5: native thread-owned TLS slots live outside the heap but must be
    // scanned and rewritten by a moving minor collection.
    serialize_global_runtime!();
    egcl_rt::init_heap(&gc_config()).expect("init_heap");

    const MARKER: u64 = 0x5100_0000_0000_0001;
    current_thread().tls_set(0, heap_value_with_marker(MARKER));

    HeapCollector::new().minor_gc().expect("minor GC");

    assert_eq!(heap_marker(current_thread().tls_get(0)), MARKER);
    current_thread().tls_set(0, NIL);
}

#[test]
fn interrupt_delivery_changes_the_join_result_of_a_live_thread() {
    serialize_global_runtime!();
    // Per R9.04 and R13.18, INTERRUPT-THREAD is delivered to the target thread.
    SLOW_THREAD_STARTED.store(false, Ordering::Release);
    RELEASE_SLOW_THREAD.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(slow_interruptible_entry) }).expect("thread creation");
    while !SLOW_THREAD_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    interrupt_thread(id, EgclVal::from_fixnum(444)).expect("interrupt must be accepted");
    RELEASE_SLOW_THREAD.store(true, Ordering::Release);

    let result = join_thread(id).expect("join must succeed");
    assert_eq!(result, EgclVal::from_fixnum(444));
}

#[test]
fn all_threads_reports_a_live_thread_until_join_completes() {
    serialize_global_runtime!();
    // Per R9.04 and R13.18, the runtime exposes the set of live thread handles.
    SLOW_THREAD_STARTED.store(false, Ordering::Release);
    RELEASE_SLOW_THREAD.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(slow_interruptible_entry) }).expect("thread creation");
    while !SLOW_THREAD_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    assert!(all_thread_ids().contains(&id));
    RELEASE_SLOW_THREAD.store(true, Ordering::Release);
    assert_eq!(join_thread(id).expect("join must succeed"), T);
    assert!(!all_thread_ids().contains(&id));
}

#[test]
fn scheduler_group_exposes_carriers_and_runs_real_fibers() {
    serialize_global_runtime!();
    // Per R2.03, the runtime supports a configurable worker-thread pool.
    // Per R13.08, scheduling is exposed through the runtime scheduler surface.
    let scheduler = Scheduler::init(&SchedulerConfig { num_workers: 3 })
        .expect("scheduler with explicit worker count must initialize");
    let fiber = make_fiber(T).expect("fiber creation");
    scheduler.submit(fiber).expect("submit must succeed");
    assert!(!scheduler.carrier_thread_ids().is_empty());
    assert!(
        scheduler
            .carrier_thread_ids()
            .iter()
            .all(|id| all_thread_ids().contains(id))
    );
    assert_eq!(scheduler.finish().expect("finish must succeed"), vec![T]);
}

#[test]
fn fiber_interrupt_roots_survive_minor_gc() {
    // h6z.5: fiber-owned interrupt storage is also an external execution root,
    // including while the fiber is only queued in the scheduler registry.
    serialize_global_runtime!();
    egcl_rt::init_heap(&gc_config()).expect("init_heap");

    const MARKER: u64 = 0x5100_0000_0000_0002;
    let fiber = make_fiber(T).expect("fiber creation");
    egcl_rt::interrupt_fiber(fiber, heap_value_with_marker(MARKER)).expect("interrupt fiber");

    HeapCollector::new().minor_gc().expect("minor GC");

    let scheduler =
        Scheduler::init(&SchedulerConfig { num_workers: 1 }).expect("scheduler creation");
    scheduler.submit(fiber).expect("submit fiber");
    let results = scheduler.finish().expect("finish scheduler");
    assert_eq!(results.len(), 1);
    assert_eq!(heap_marker(results[0]), MARKER);
}

#[test]
fn enter_safepoint_publishes_the_current_stack_top() {
    // Per R2.07 and R13.10, safepoints use cooperative polling rather than async suspension.
    let stack = current_thread().stack();
    assert_eq!(stack.published_sp(), 0);
    assert!(stack.published_fp().is_null());

    enter_safepoint();

    assert_eq!(stack.published_sp(), stack.used());
    assert_eq!(stack.published_fp(), stack.fp());
}

#[test]
fn stop_the_world_waits_for_polling_threads_and_resumes_them() {
    serialize_global_runtime!();
    // Per R2.07, safepoint polls must be observed by running threads.
    // Per R13.10, stop-the-world uses the safepoint handshake rather than async suspension.
    SAFETY_LOOP_EXIT.store(false, Ordering::Release);
    POLL_ITERATIONS.store(0, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(polling_entry) }).expect("thread creation");
    while POLL_ITERATIONS.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }

    wait_for_all_threads().expect("safepoint rendezvous must succeed");
    let iterations_during_stop = POLL_ITERATIONS.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        POLL_ITERATIONS.load(Ordering::Acquire),
        iterations_during_stop,
        "the polling thread should be parked while the world is stopped"
    );

    resume_all_threads().expect("resume must succeed");
    while POLL_ITERATIONS.load(Ordering::Acquire) == iterations_during_stop {
        std::thread::yield_now();
    }

    SAFETY_LOOP_EXIT.store(true, Ordering::Release);
    let result = join_thread(id).expect("polling thread must finish");
    assert!(result.as_fixnum() >= iterations_during_stop as i64);
}

#[test]
fn failed_safepoint_handshake_is_reported_and_cleans_up() {
    serialize_global_runtime!();
    // h6z.4: a moving GC must never proceed after a failed safepoint
    // handshake. A running thread that never polls should make the handshake
    // fail, and the failure path must resume/clear coordination state so the
    // target can still be joined.
    install_signal_handlers().expect("signal handlers must install before SIGUSR1 nudging");
    SLOW_THREAD_STARTED.store(false, Ordering::Release);
    RELEASE_SLOW_THREAD.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(slow_interruptible_entry) }).expect("thread creation");
    while !SLOW_THREAD_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    let err = wait_for_all_threads().expect_err("non-polling mutator must fail handshake");
    assert!(
        format!("{err}").contains("safepoint handshake failed"),
        "unexpected safepoint error: {err}"
    );

    RELEASE_SLOW_THREAD.store(true, Ordering::Release);
    assert_eq!(join_thread(id).expect("join must succeed after cleanup"), T);
}

#[test]
fn t0_alloc_typed_observes_safepoints_before_touching_its_tlab() {
    serialize_global_runtime!();
    // h6z.3: T0 allocation is a native-mutator entry point. A thread that only
    // allocates through alloc_typed must still park at a GC safepoint before it
    // touches its TLAB again.
    T0_ALLOC_LOOP_EXIT.store(false, Ordering::Release);
    T0_ALLOC_ITERATIONS.store(0, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(t0_allocating_entry) }).expect("thread creation");
    while T0_ALLOC_ITERATIONS.load(Ordering::Acquire) == 0 {
        std::thread::yield_now();
    }

    wait_for_all_threads().expect("alloc_typed-only mutator should reach safepoint");
    let iterations_during_stop = T0_ALLOC_ITERATIONS.load(Ordering::Acquire);
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        T0_ALLOC_ITERATIONS.load(Ordering::Acquire),
        iterations_during_stop,
        "alloc_typed must poll and park before touching the T0 TLAB"
    );

    resume_all_threads().expect("resume must succeed");
    while T0_ALLOC_ITERATIONS.load(Ordering::Acquire) == iterations_during_stop {
        std::thread::yield_now();
    }

    T0_ALLOC_LOOP_EXIT.store(true, Ordering::Release);
    let result = join_thread(id).expect("allocating thread must finish");
    assert!(result.as_fixnum() >= iterations_during_stop as i64);
}

#[test]
fn current_thread_identity_is_stable_within_the_calling_thread() {
    // Per R13.18, CURRENT-THREAD is a stable handle for the running thread.
    let thread = current_thread();
    assert_eq!(thread.id(), current_thread_id());
    assert_eq!(thread.id(), current_thread_id());
}

#[test]
fn sigint_delivery_is_observable_through_the_runtime_interrupt_flag() {
    serialize_global_runtime!();
    // Per R2.10, SIGINT must be surfaced to the runtime rather than crashing the process.
    // Per R8.10 and R13.13, signal handlers defer non-trivial work by setting a flag.
    install_signal_handlers().expect("signal handlers must install");
    assert!(!check_sigint(), "signal flag should start clear");

    unsafe {
        libc::raise(libc::SIGINT);
    }

    let deadline = Instant::now() + Duration::from_millis(250);
    let mut observed = false;
    while Instant::now() < deadline && !observed {
        observed = check_sigint();
        std::thread::yield_now();
    }

    assert!(
        observed,
        "SIGINT must become observable through the runtime flag"
    );
    assert!(
        !check_sigint(),
        "check_sigint must clear the pending interrupt after observing it"
    );
}

#[cfg(feature = "c-ffi")]
#[test]
fn safepoint_wait_does_not_block_on_a_thread_executing_native_ffi() {
    serialize_global_runtime!();
    // Per R2.15 and R13.11, a thread in Native FFI state must not block a safepoint handshake.
    let libc = egcl_rt::ffi::load_foreign_library("libc.so.6")
        .or_else(|_| egcl_rt::ffi::load_foreign_library("libSystem.B.dylib"))
        .or_else(|_| egcl_rt::ffi::load_foreign_library("libc.so"))
        .expect("expected a libc-compatible shared library");
    let usleep =
        unsafe { egcl_rt::ffi::foreign_symbol(libc, "usleep") }.expect("libc must export usleep");
    let _ = USLEEP_FN.set(usleep as usize);

    NATIVE_FFI_STARTED.store(false, Ordering::Release);
    NATIVE_FFI_FINISHED.store(false, Ordering::Release);

    let id = make_thread(unsafe { fn_entry(native_ffi_entry) }).expect("thread creation");
    while !NATIVE_FFI_STARTED.load(Ordering::Acquire) {
        std::thread::yield_now();
    }

    let started = Instant::now();
    wait_for_all_threads().expect("safepoint request must return");
    let elapsed = started.elapsed();
    resume_all_threads().expect("resume must succeed");

    assert!(
        elapsed < Duration::from_millis(150),
        "native threads should be excluded from the handshake, observed {:?}",
        elapsed
    );
    assert_eq!(join_thread(id).expect("native thread must finish"), T);
}

#[test]
fn runtime_sources_define_the_boot_sequence_and_walkable_stack_metadata() {
    // Per R2.01 and R2.02, startup must parse configuration, initialize the runtime,
    // and enter user-visible evaluation entrypoints.
    // Per R2.06, stack frames must remain walkable through prev-fp metadata and safepoint maps.
    // Per R2.16 and R2.17, environment parsing and shutdown hooks must be part of the runtime surface.
    // Per R2.20, stack overflow is represented as a recoverable runtime error.
    let runtime_source = read_runtime_source("crates/egcl-rt/src/runtime.rs");
    let stack_source = read_runtime_source("crates/egcl-rt/src/stack.rs");
    let error_source = read_runtime_source("crates/egcl-rt/src/error.rs");

    assert!(runtime_source.contains("pub fn from_env() -> Result<Self, EgclError>"));
    assert!(runtime_source.contains("pub fn apply_cli_args(&mut self, args: &[String])"));
    assert!(runtime_source.contains("pub fn parse_cli"));
    assert!(runtime_source.contains("pub fn shutdown(&mut self) -> Result<(), EgclError>"));
    assert!(
        runtime_source.contains("SIGSEGV"),
        "runtime must install a SIGSEGV path"
    );
    assert!(stack_source.contains("pub prev_fp: *mut Frame"));
    assert!(stack_source.contains("pub fn publish_top(&self)"));
    assert!(stack_source.contains("pub fn stack_map(&self, pc_offset: usize)"));
    assert!(error_source.contains("StackOverflow(FiberId)"));
    assert!(
        read_runtime_source("crates/egcl-rt/src/thread.rs").contains("100 000"),
        "threading source must account for the 100,000-thread scalability target"
    );
}

#[test]
fn concurrency_sources_expose_atomic_ordering_locking_and_thread_primitives() {
    // Per R13.01 and R13.02, per-thread and cross-thread state must use explicit ordering.
    // Per R13.04, compare-and-swap operations must exist with acquire-release semantics.
    // Per R13.05 and R13.06, lock ordering must be represented explicitly in the runtime.
    // Per R13.07, key runtime coordination cells must be lock-free atomics.
    // Per R13.09, scheduling must expose a yield/preemption flag checked at safepoints.
    // Per R13.12 and R13.14, blocked threads must publish their stacks and long syscalls need a SIGUSR1 fallback.
    // Per R13.15 and R13.16, WITH-ATOMIC support must exist as a distinct preemption-control surface.
    // Per R13.19, mutex, condition-variable, read-write-lock, and semaphore primitives must be exposed.
    let thread_source = read_runtime_source("crates/egcl-rt/src/thread.rs");
    let safepoint_source = read_runtime_source("crates/egcl-rt/src/safepoint.rs");
    let scheduler_source = read_runtime_source("crates/egcl-rt/src/scheduler.rs");
    let lib_source = read_runtime_source("crates/egcl-rt/src/lib.rs");

    assert!(thread_source.contains("Ordering::Acquire"));
    assert!(thread_source.contains("Ordering::Release"));
    assert!(thread_source.contains("compare_exchange"));
    assert!(thread_source.contains("yield_requested"));
    assert!(safepoint_source.contains("Mutex"));
    assert!(safepoint_source.contains("Condvar"));
    assert!(
        safepoint_source.contains("publish stack top") || safepoint_source.contains("publish_top")
    );
    assert!(
        safepoint_source.contains("SIGUSR1"),
        "blocked-syscall fallback must be represented"
    );
    assert!(scheduler_source.contains("work-stealing"));
    assert!(
        lib_source.contains("RwLock"),
        "public lock primitives must include RW locks"
    );
    assert!(
        lib_source.contains("Semaphore"),
        "public thread primitives must include semaphores"
    );
    assert!(
        lib_source.contains("with_atomic"),
        "WITH-ATOMIC must have a runtime surface"
    );
}

#[test]
fn thread_sources_describe_thread_local_dynamic_bindings() {
    // Per R13.17, each fiber must have its own special-variable binding stack.
    let thread_source = read_runtime_source("crates/egcl-rt/src/thread.rs");
    let stack_source = read_runtime_source("crates/egcl-rt/src/stack.rs");

    assert!(
        thread_source.contains("binding"),
        "thread runtime must store dynamic bindings"
    );
    assert!(
        stack_source.contains("FrameType::Special"),
        "special-binding frames must be walkable"
    );
}
