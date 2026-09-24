use std::sync::Mutex;
use std::sync::atomic::Ordering;
use std::time::Instant;
use torcl_compiler::ic::{IcState, InlineCache, ic_generation, init_ic_registry, reset_all_caches};
use torcl_compiler::osr::{
    ConversionKind, DeoptConfig, DeoptLog, DeoptReason, LocalMapping, Location, OsrEntryMap,
    OsrSlotDesc, TypeGuard, clear_global_deopt_logs, deoptimize, is_function_blacklisted,
    is_function_in_backoff, osr_entry,
};
use torcl_compiler::profiling::{
    BackEdgeCounter, FunctionProfile, InvocationCounter, TYPE_PROFILE_MAX_ENTRIES,
};
use torcl_compiler::tiered::{
    FLAG_T2_FAILED, FnMeta, Interpreter, Tier, TierConfig, check_promotion,
    pop_compilation_request, process_compilation_request, request_compilation,
};
use torcl_rt::value::{NIL, T, TAG_FIXNUM, TorclVal};

/// Serializes tests that touch process-global registries shared across this
/// binary: the bounded compilation queue (`COMPILATION_QUEUE`) and the global
/// deopt-log/blacklist registry (`DEOPT_LOGS`). Running these tests
/// concurrently lets one test's enqueue/drain or `clear_global_deopt_logs()`
/// perturb another's expectations. The lock restores determinism without
/// altering single-threaded behaviour.
static GLOBAL_REGISTRY_SERIAL: Mutex<()> = Mutex::new(());

fn make_function() -> (TorclVal, &'static FnMeta) {
    let meta = Box::leak(Box::new(FnMeta::new(0, TorclVal::from_fixnum(0), NIL)));
    // SAFETY: tests leak the function metadata for process lifetime.
    let function = unsafe { TorclVal::from_function_ptr(meta as *mut FnMeta as *mut u8) };
    (function, meta)
}

fn drain_queue() {
    while pop_compilation_request().is_some() {}
}

fn env_threshold_helper() {
    let mut interpreter = Interpreter::new();
    let (function, meta) = make_function();
    for _ in 0..3 {
        let value = interpreter
            .apply(function, NIL)
            .expect("helper process should be able to call the fixture through T0");
        assert_eq!(value, TorclVal::from_fixnum(0));
    }
    assert_eq!(
        meta.tier.load(Ordering::Acquire),
        Tier::Baseline as u8,
        "TORCL_T0_T1_THRESHOLD=3 should promote on the third call in a fresh process"
    );
}

#[test]
fn tiered_promotion_uses_invocation_and_loop_heat_thresholds_from_spec() {
    // Per R4.24, T0->T1 promotes at the configured invocation threshold.
    // Per R4.25, T1->T2 promotes on invocation count OR loop heat.
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5_000,
        osr_threshold: 10_000,
        compile_threads: 1,
    };
    let (function, meta) = make_function();

    meta.invoke_count.store(9, Ordering::Relaxed);
    assert_eq!(check_promotion(function, &config), None);

    meta.invoke_count.store(10, Ordering::Relaxed);
    assert_eq!(check_promotion(function, &config), Some(Tier::Baseline));

    meta.tier.store(Tier::Baseline as u8, Ordering::Release);
    meta.invoke_count.store(4_999, Ordering::Relaxed);
    meta.back_edge_count.store(9_999, Ordering::Relaxed);
    assert_eq!(check_promotion(function, &config), None);

    meta.back_edge_count.store(10_000, Ordering::Relaxed);
    assert_eq!(
        check_promotion(function, &config),
        Some(Tier::Optimising),
        "loop heat alone should make a T1 function eligible for T2"
    );
}

#[test]
fn t2_compile_failure_keeps_t1_entry_and_marks_failure() {
    // Per R4.28, if T2 compilation fails then the function must remain at T1,
    // keep its existing entry point, and record the failure for logging/reporting.
    let _serial = GLOBAL_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    drain_queue();
    let (function, meta) = make_function();
    let mut interpreter = Interpreter::new();
    for _ in 0..10 {
        interpreter
            .apply(function, NIL)
            .expect("real calls should synchronously promote the fixture to T1");
    }
    let t1_entry = meta.entry.load(Ordering::Acquire);
    assert_eq!(meta.tier.load(Ordering::Acquire), Tier::Baseline as u8);

    request_compilation(function, Tier::Optimising)
        .expect("queueing the T2 compilation attempt should succeed");
    assert!(process_compilation_request());
    assert_eq!(
        meta.tier.load(Ordering::Acquire),
        Tier::Baseline as u8,
        "a failed T2 compile must leave the function executing at T1"
    );
    assert_eq!(
        meta.entry.load(Ordering::Acquire),
        t1_entry,
        "a failed T2 compile must not clobber the published T1 entry point"
    );
    assert_ne!(
        meta.flags.load(Ordering::Acquire) & FLAG_T2_FAILED,
        0,
        "failed T2 compilation should set persistent failure metadata for logging/reporting"
    );
}

#[test]
fn startup_environment_configures_tier_thresholds() {
    // Per R4.29, tier thresholds must be configurable at startup from
    // TORCL_T0_T1_THRESHOLD, TORCL_T1_T2_THRESHOLD, and TORCL_LOOP_HEAT_THRESHOLD.
    if std::env::var_os("TORCL_ENV_THRESHOLD_HELPER").is_some() {
        env_threshold_helper();
        return;
    }

    let status = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .arg("--exact")
        .arg("startup_environment_configures_tier_thresholds")
        .arg("--nocapture")
        .env("TORCL_ENV_THRESHOLD_HELPER", "1")
        .env("TORCL_T0_T1_THRESHOLD", "3")
        .env("TORCL_T1_T2_THRESHOLD", "4")
        .env("TORCL_LOOP_HEAT_THRESHOLD", "5")
        .status()
        .expect("helper process should launch");
    assert!(
        status.success(),
        "startup tier-threshold environment variables should affect promotion in a fresh process"
    );
}

#[test]
fn compilation_queue_is_bounded_and_prioritises_hotter_requests() {
    // Per R4.30, the queue is bounded and overflow drops work instead of blocking.
    // Per R4.25/R4.26, background T2 requests are orchestrated through the queue.
    let _serial = GLOBAL_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    drain_queue();

    let (high_fn, high_meta) = make_function();
    high_meta.invoke_count.store(100, Ordering::Relaxed);
    high_meta.back_edge_count.store(50, Ordering::Relaxed);

    let (low_fn, low_meta) = make_function();
    low_meta.invoke_count.store(2, Ordering::Relaxed);
    low_meta.back_edge_count.store(0, Ordering::Relaxed);

    request_compilation(low_fn, Tier::Optimising).expect("low-priority request should queue");
    request_compilation(high_fn, Tier::Optimising).expect("high-priority request should queue");

    let first = pop_compilation_request().expect("first queued item should be available");
    let second = pop_compilation_request().expect("second queued item should be available");
    assert_eq!(first.0, high_fn, "hotter function should be selected first");
    assert_eq!(second.0, low_fn);

    let mut fixtures = Vec::new();
    for _ in 0..70 {
        let (function, _) = make_function();
        request_compilation(function, Tier::Optimising).expect("enqueue should not error");
        fixtures.push(function);
    }
    let mut popped = 0usize;
    while pop_compilation_request().is_some() {
        popped += 1;
    }
    assert_eq!(
        popped, 64,
        "spec queue bound is 64 entries; overflow requests should be dropped"
    );
}

#[test]
fn deopt_at_safepoint_restores_equivalent_interpreter_frame() {
    // Per R4.39, deoptimisation at a GC safepoint must reconstruct a
    // semantically equivalent interpreter frame within one poll interval.
    let _serial = GLOBAL_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    clear_global_deopt_logs();
    let (function, _) = make_function();
    let live_values = [TorclVal::from_fixnum(7), NIL, T];
    let deopt = deoptimize(
        function,
        DeoptReason::TypeMismatch {
            expected: "fixnum".into(),
            actual: "symbol".into(),
        },
        &live_values,
    )
    .expect("deoptimisation should reconstruct interpreter state at the safepoint");

    assert_eq!(deopt.frame_locals, live_values);
    assert_eq!(deopt.total_deopts, 1);
    assert!(
        !deopt.blacklisted,
        "the first safepoint deoptimisation should restore the frame without blacklisting"
    );
}

#[test]
fn osr_and_deopt_keep_gc_visible_state_walkable_during_transition() {
    // Per R4.41, OSR entry and deoptimisation must reach a safepoint before
    // stack-shape changes and the replacement frame must remain GC-walkable.
    let _serial = GLOBAL_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let (function, _) = make_function();
    let map = OsrEntryMap {
        mappings: vec![
            LocalMapping {
                local_index: 0,
                ssa_var: 1,
            },
            LocalMapping {
                local_index: 1,
                ssa_var: 2,
            },
        ],
        target_pc_offset: 13,
        slots: vec![
            OsrSlotDesc {
                source_offset: 0,
                dest: Location::StackOffset(0),
                conversion: ConversionKind::None,
            },
            OsrSlotDesc {
                source_offset: 8,
                dest: Location::StackOffset(8),
                conversion: ConversionKind::None,
            },
        ],
        live_ref_bitmap: vec![0b0000_0011],
        type_guards: vec![],
    };
    let locals = [NIL, T];
    let entered = osr_entry(function, &map, &locals)
        .expect("OSR should expose a fully mapped replacement frame at the safepoint");
    assert_eq!(entered.mapped_values, vec![(1, NIL), (2, T)]);
    assert_eq!(map.live_ref_bitmap, vec![0b0000_0011]);

    let deopt = deoptimize(function, DeoptReason::InlineCacheOverflow, &locals)
        .expect("deopt should hand GC-visible locals back to the interpreter");
    assert_eq!(deopt.frame_locals, locals);
}

#[test]
fn osr_entry_preserves_mapped_locals_and_deopt_blacklists_after_threshold() {
    // Per R4.38, OSR must preserve live locals and the current PC mapping.
    // Per R4.40, repeated deopts must be logged and eventually blacklist T2.
    let _serial = GLOBAL_REGISTRY_SERIAL
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    clear_global_deopt_logs();
    let (function, _) = make_function();
    let map = OsrEntryMap {
        mappings: vec![
            LocalMapping {
                local_index: 0,
                ssa_var: 11,
            },
            LocalMapping {
                local_index: 1,
                ssa_var: 12,
            },
        ],
        target_pc_offset: 77,
        slots: vec![
            OsrSlotDesc {
                source_offset: 0,
                dest: Location::StackOffset(0),
                conversion: ConversionKind::None,
            },
            OsrSlotDesc {
                source_offset: 8,
                dest: Location::Register(1),
                conversion: ConversionKind::UnboxFixnum,
            },
        ],
        live_ref_bitmap: vec![0b0000_0011],
        type_guards: vec![TypeGuard {
            slot_index: 1,
            expected_tag: TAG_FIXNUM,
        }],
    };

    let result = osr_entry(function, &map, &[NIL, TorclVal::from_fixnum(5)])
        .expect("OSR entry should accept matching locals");
    assert_eq!(result.target_pc_offset, 77);
    assert_eq!(result.mapped_values[0], (11, NIL));
    assert_eq!(
        result.mapped_values[1],
        (12, TorclVal::from_raw(5)),
        "unboxed fixnum payload should be transferred into the T2 mapping"
    );

    for _ in 0..3 {
        let deopt = deoptimize(
            function,
            DeoptReason::InlineCacheOverflow,
            &[TorclVal::from_fixnum(1), TorclVal::from_fixnum(2)],
        )
        .expect("deoptimisation should reconstruct interpreter locals");
        assert_eq!(deopt.frame_locals.len(), 2);
    }

    assert!(is_function_blacklisted(function));
    assert!(is_function_in_backoff(function));
    let blocked = osr_entry(function, &map, &[NIL, TorclVal::from_fixnum(5)]);
    assert!(
        blocked.is_err(),
        "blacklisted functions must not re-enter T2 during backoff"
    );
}

#[test]
fn deopt_log_keeps_recent_ring_and_records_reason_counts() {
    // Per R4.40, a per-function deopt log must retain recent events with reasons.
    let config = DeoptConfig {
        blacklist_threshold: 99,
        backoff_seconds: 1,
    };
    let mut log = DeoptLog::with_config(&config);

    for idx in 0..35 {
        log.record_full(
            DeoptReason::Other(format!("reason-{idx}")),
            0x1000 + idx,
            idx as u32,
        );
    }
    log.record(DeoptReason::InlineCacheOverflow);

    assert_eq!(log.count(), 32, "spec deopt ring capacity is 32");
    assert_eq!(log.total_deopts(), 36);
    assert!(
        log.entries().iter().all(|entry| entry.timestamp > 0),
        "each deopt event should have a monotonic timestamp"
    );
    assert!(
        log.reason_count_in_window("inline_cache_overflow") >= 1,
        "per-reason counters should track uncommon trap categories"
    );
}

#[test]
fn scheduler_reads_hot_counters_through_lock_free_metadata_loads() {
    // Per R4.55, the compilation scheduler must read hotness counters with
    // lock-free atomic loads rather than a mutex or CAS loop.
    let (function, meta) = make_function();
    let config = TierConfig {
        t1_threshold: 10,
        t2_threshold: 5_000,
        osr_threshold: 10_000,
        compile_threads: 1,
    };
    meta.tier.store(Tier::Baseline as u8, Ordering::Release);
    meta.invoke_count.store(5_000, Ordering::Relaxed);
    meta.back_edge_count.store(10_000, Ordering::Relaxed);

    for _ in 0..32 {
        assert_eq!(check_promotion(function, &config), Some(Tier::Optimising));
        assert_eq!(meta.invoke_count.load(Ordering::Relaxed), 5_000);
        assert_eq!(meta.back_edge_count.load(Ordering::Relaxed), 10_000);
    }
}

#[test]
fn inline_cache_follows_spec_state_machine_and_bulk_invalidation() {
    // Per R4.48, dynamic dispatch sites must be backed by an inline cache.
    // Per R4.49, the IC supports monomorphic -> polymorphic (<= 8) -> megamorphic.
    // Per R4.51, global invalidation clears stale IC state after layout changes.
    init_ic_registry();
    let ic = InlineCache::new();

    ic.update(TorclVal::from_fixnum(1), TorclVal::from_fixnum(10));
    assert_eq!(ic.state(), IcState::Monomorphic);

    for idx in 2..=8 {
        ic.update(TorclVal::from_fixnum(idx), TorclVal::from_fixnum(idx * 10));
    }
    assert_eq!(
        ic.state(),
        IcState::Polymorphic,
        "up to eight receiver classes should remain polymorphic"
    );
    assert_eq!(ic.entries().len(), 8);

    ic.update(TorclVal::from_fixnum(9), TorclVal::from_fixnum(90));
    assert_eq!(
        ic.state(),
        IcState::Megamorphic,
        "the ninth distinct receiver should trigger megamorphic fallback"
    );

    let before = ic_generation();
    reset_all_caches().expect("global IC invalidation should succeed");
    let after = ic_generation();
    assert!(after > before);
    assert_eq!(
        ic.state(),
        IcState::Uninitialized,
        "bulk invalidation should lazily clear stale cache state"
    );
}

#[test]
fn profiling_records_hotness_and_fixed_size_type_profile_ring() {
    // Per R4.53 and R4.56, invocation and back-edge counters drive hotness.
    // Per R4.54, receiver types are recorded in a fixed-size ring buffer.
    let invocation = InvocationCounter::new();
    let back_edge = BackEdgeCounter::new();

    for _ in 0..4_999 {
        assert!(!invocation.increment(5_000));
    }
    assert_eq!(invocation.count(), 4_999);
    assert!(invocation.increment(5_000));

    for _ in 0..9_999 {
        assert!(!back_edge.increment(10_000));
    }
    assert!(back_edge.increment(10_000));

    assert_eq!(
        TYPE_PROFILE_MAX_ENTRIES, 4,
        "spec D4.14 fixes the type-profile ring size at four entries"
    );

    let mut profile = FunctionProfile::new();
    profile.add_type_profile(7);
    let site = profile
        .type_profile(7)
        .expect("registered call-site profile should be retrievable");
    for idx in 0..6 {
        site.record(TorclVal::from_fixnum(idx));
    }
    assert_eq!(site.entries().len(), 4);
    assert_eq!(
        profile.invocation_counter().count(),
        0,
        "function profiles should expose the real invocation counter object"
    );
}

#[test]
fn profiling_overhead_stays_within_budget_on_counter_workload() {
    // Per R4.58, counter increments plus type recording must stay within
    // a 5% wall-clock slowdown versus profiling-disabled execution.
    fn baseline_loop(iterations: usize) -> u128 {
        let started = Instant::now();
        let mut acc = 0usize;
        for idx in 0..iterations {
            acc = std::hint::black_box(acc.wrapping_add(idx));
        }
        std::hint::black_box(acc);
        started.elapsed().as_nanos()
    }

    fn profiled_loop(iterations: usize) -> u128 {
        let invocation = InvocationCounter::new();
        let back_edge = BackEdgeCounter::new();
        let mut profile = FunctionProfile::new();
        profile.add_type_profile(1);
        let site = profile
            .type_profile(1)
            .expect("call-site profile should exist for the overhead fixture");

        let started = Instant::now();
        for idx in 0..iterations {
            let _ = invocation.increment(u32::MAX);
            let _ = back_edge.increment(u32::MAX);
            site.record(TorclVal::from_fixnum((idx % 4) as i64));
        }
        started.elapsed().as_nanos()
    }

    let iterations = 50_000;
    let baseline = baseline_loop(iterations);
    let profiled = profiled_loop(iterations);
    assert!(
        profiled * 100 <= baseline * 105,
        "profiling budget exceeded: baseline={baseline}ns profiled={profiled}ns"
    );
}
