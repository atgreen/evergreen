// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Process-epoch request to published generated-poll observation, not full GC
//! rendezvous latency. Hooks and synchronization remain inside the measurement.

use super::*;
use std::cell::RefCell;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

struct Pending {
    sequence: usize,
    epoch: u64,
    posted: Instant,
    observed: Option<Instant>,
}

#[derive(Debug)]
struct Sample {
    sequence: usize,
    requested_epoch: u64,
    acknowledged_epoch: Option<u64>,
    posted_ns: u128,
    observed_ns: u128,
    completed_ns: Option<u128>,
}

#[derive(Default)]
struct State {
    warmup_observed: bool,
    ready: bool,
    finished: bool,
    requested: usize,
    completed: usize,
    timeouts: usize,
    pending: Option<Pending>,
    samples: Vec<Sample>,
    relocated: bool,
    relocated_value: Option<u64>,
    failure: Option<String>,
}

type Shared = Arc<(Mutex<State>, Condvar, Instant)>;

thread_local! {
    static ACTIVE: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

fn shared() -> Shared {
    ACTIVE.with(|slot| slot.borrow().as_ref().unwrap().clone())
}

fn warmup() {
    if native_transfer_entry::root_publication::native_poll_root_for_test(71).is_some() {
        // This forcing hook is removed before readiness is released. Every
        // measured arrival must pass the generated request checks naturally.
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        shared().0.lock().unwrap().warmup_observed = true;
    }
}

fn observe_request() {
    let shared = shared();
    let mut state = shared.0.lock().unwrap();
    let Some(pending) = state.pending.as_ref() else {
        return;
    };
    if pending.observed.is_some() {
        return;
    }
    let seen = egcl_rt::runtime::current_seen_signal_activity();
    let current = unsafe { &*egcl_rt::runtime::process_signal_activity_address() }
        .load(std::sync::atomic::Ordering::Acquire);
    if seen.is_none_or(|seen| seen >= pending.epoch) || current < pending.epoch {
        state.failure =
            Some("request was already consumed or not yet published at observation".into());
        return;
    }
    let Some(root) = native_transfer_entry::root_publication::native_poll_root_for_test(71) else {
        state.failure =
            Some("request did not reach a generated poll with the native-only root".into());
        return;
    };
    let observed = Instant::now();
    let pending = state.pending.as_mut().unwrap();
    pending.observed = Some(observed);
    let sample = Sample {
        sequence: pending.sequence,
        requested_epoch: pending.epoch,
        acknowledged_epoch: None,
        posted_ns: pending.posted.duration_since(shared.2).as_nanos(),
        observed_ns: observed.duration_since(shared.2).as_nanos(),
        completed_ns: None,
    };
    state.samples.push(sample);
    let collect = !state.relocated;
    drop(state);
    if collect {
        let before = unsafe { root.read() }.to_raw();
        let result = egcl_rt::collect_t0_minor();
        let after = unsafe { root.read() }.to_raw();
        let mut state = shared.0.lock().unwrap();
        match result {
            Ok(()) if before != after => {
                state.relocated = true;
                state.relocated_value = Some(after);
            }
            Ok(()) => {
                state.failure =
                    Some("first requested poll did not relocate its native-only root".into())
            }
            Err(error) => {
                state.failure = Some(format!("requested-poll collection failed: {error}"))
            }
        }
    }
}

fn after_refresh() {
    let shared = shared();
    let mut state = shared.0.lock().unwrap();
    if state.warmup_observed && !state.ready {
        state.ready = true;
        shared.1.notify_all();
        return;
    }
    let Some(pending) = state.pending.as_ref() else {
        return;
    };
    if pending.observed.is_none() {
        return;
    }
    if egcl_rt::runtime::current_seen_signal_activity().is_none_or(|seen| seen < pending.epoch) {
        state.failure = Some("runtime returned without acknowledging the observed epoch".into());
        shared.1.notify_all();
        return;
    }
    let sequence = pending.sequence;
    let sample = state.samples.last_mut().unwrap();
    sample.acknowledged_epoch = egcl_rt::runtime::current_seen_signal_activity();
    sample.completed_ns = Some(Instant::now().duration_since(shared.2).as_nanos());
    state.completed = sequence;
    state.pending = None;
    shared.1.notify_all();
}

fn post_requests(shared: Shared, count: usize) {
    let mut state = shared.0.lock().unwrap();
    let (guard, wait) = shared
        .1
        .wait_timeout_while(state, Duration::from_secs(5), |state| {
            !state.ready && !state.finished
        })
        .unwrap();
    state = guard;
    if wait.timed_out() || !state.ready {
        state.timeouts += usize::from(wait.timed_out());
        state.failure = Some("native generated-loop warmup never became ready".into());
        return;
    }
    for sequence in 1..=count {
        drop(state);
        // Space requests outside the timed interval so the loop has an idle
        // interval. This is not the clock used to calculate observation latency.
        std::thread::sleep(Duration::from_micros(200));
        state = shared.0.lock().unwrap();
        if state.finished || state.failure.is_some() {
            return;
        }
        assert!(
            state.pending.is_none(),
            "only one request may be outstanding"
        );
        let posted = Instant::now();
        // Perform the real producer's atomic increment, retaining its return
        // value. A separate load after mark_process_signal_activity() could
        // accidentally label another producer's later epoch as this request.
        let epoch = unsafe { &*egcl_rt::runtime::process_signal_activity_address() }
            .fetch_add(1, std::sync::atomic::Ordering::Release)
            .wrapping_add(1);
        egcl_rt::execution_poll::request_all();
        // Hold the mutex over timestamp, real request and sequence publication:
        // the observer can never see a request with missing timing metadata.
        state.pending = Some(Pending {
            sequence,
            epoch,
            posted,
            observed: None,
        });
        state.requested = sequence;
        let (guard, wait) = shared
            .1
            .wait_timeout_while(state, Duration::from_secs(5), |state| {
                state.completed < sequence && !state.finished && state.failure.is_none()
            })
            .unwrap();
        state = guard;
        if wait.timed_out() {
            state.timeouts += 1;
            state.failure = Some(format!("request {sequence} acknowledgment timed out"));
            return;
        }
        if state.finished || state.failure.is_some() {
            return;
        }
    }
}

struct RestoreHooks;
impl Drop for RestoreHooks {
    fn drop(&mut self) {
        native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(None));
        native_transfer_entry::native_poll::OBSERVE_REQUEST.with(|hook| hook.set(None));
        native_transfer_entry::native_poll::AFTER_REFRESH.with(|hook| hook.set(None));
        ACTIVE.with(|slot| *slot.borrow_mut() = None);
    }
}

#[test]
fn poll_latency_rejects_an_already_consumed_epoch() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // Do not publish a new request: deliberately present a counter this
    // carrier has already consumed. It must never produce a latency sample.
    egcl_rt::runtime::take_process_signal_activity();
    let epoch = egcl_rt::runtime::current_seen_signal_activity().unwrap();
    let now = Instant::now();
    let shared: Shared = Arc::new((
        Mutex::new(State {
            pending: Some(Pending {
                sequence: 1,
                epoch,
                posted: now,
                observed: None,
            }),
            ..State::default()
        }),
        Condvar::new(),
        now,
    ));
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
    observe_request();
    ACTIVE.with(|slot| *slot.borrow_mut() = None);
    let state = shared.0.lock().unwrap();
    assert!(state.samples.is_empty());
    assert_eq!(
        state.failure.as_deref(),
        Some("request was already consumed or not yet published at observation")
    );
}

#[test]
fn poll_latency_acknowledges_request_after_later_epoch_activity() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    egcl_rt::runtime::mark_process_signal_activity();
    let requested = unsafe { &*egcl_rt::runtime::process_signal_activity_address() }
        .load(std::sync::atomic::Ordering::Acquire);
    egcl_rt::runtime::mark_process_signal_activity();
    egcl_rt::runtime::take_process_signal_activity();
    let acknowledged = egcl_rt::runtime::current_seen_signal_activity().unwrap();
    assert!(acknowledged > requested);
    let now = Instant::now();
    let shared: Shared = Arc::new((
        Mutex::new(State {
            pending: Some(Pending {
                sequence: 1,
                epoch: requested,
                posted: now,
                observed: Some(now),
            }),
            samples: vec![Sample {
                sequence: 1,
                requested_epoch: requested,
                acknowledged_epoch: None,
                posted_ns: 0,
                observed_ns: 0,
                completed_ns: None,
            }],
            ..State::default()
        }),
        Condvar::new(),
        now,
    ));
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
    after_refresh();
    ACTIVE.with(|slot| *slot.borrow_mut() = None);
    let state = shared.0.lock().unwrap();
    assert!(state.failure.is_none());
    assert_eq!(state.completed, 1);
    assert!(state.pending.is_none());
    assert_eq!(state.samples[0].acknowledged_epoch, Some(acknowledged));
    assert!(state.samples[0].completed_ns.is_some());
}

#[test]
#[ignore = "requires actual x86 mapped T2 execution; reports instrumentation-inclusive latency"]
fn ordinary_t2_generated_poll_request_observation_latency() {
    let _lock = super::super::super::super::heap_test_lock()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let mut env = Env::new(false);
    egcl_rt::rooted_ref!(_env = &mut env);
    super::super::super::super::read_eval_all_env(
        "(defun latency-root-allocator () (%ordinary-t2-root-for-test))",
        &mut env,
    )
    .unwrap();
    let (symbol, code) = install(
        "GENERATED-POLL-LATENCY",
        "(n)",
        "((let ((held (latency-root-allocator)))
            (values nil)
            (let ((sum 0)) (dotimes (i n) (setq sum (+ sum i))))
            held))",
        &mut env,
    );
    assert_mapped_t2(&code);
    expect_entry(&code);
    egcl_rt::collect_t0_minor().unwrap();
    let shared: Shared = Arc::new((Mutex::new(State::default()), Condvar::new(), Instant::now()));
    ACTIVE.with(|slot| *slot.borrow_mut() = Some(shared.clone()));
    let _restore = RestoreHooks;
    native_transfer_entry::root_publication::OBSERVE.with(|hook| hook.set(Some(warmup)));
    native_transfer_entry::native_poll::OBSERVE_REQUEST
        .with(|hook| hook.set(Some(observe_request)));
    native_transfer_entry::native_poll::AFTER_REFRESH.with(|hook| hook.set(Some(after_refresh)));
    native_transfer_entry::native_poll::take_generated_skips();
    native_transfer_entry::reset_poll_paths_for_test();
    let samples = 16;
    let requester = {
        let shared = shared.clone();
        std::thread::spawn(move || post_requests(shared, samples))
    };
    let result = run_native(
        &code,
        symbol,
        &[EgclVal::from_fixnum(100_000_000)],
        &mut env,
    );
    {
        let mut state = shared.0.lock().unwrap();
        state.finished = true;
        shared.1.notify_all();
    }
    requester.join().unwrap();
    let generated_skips = native_transfer_entry::native_poll::take_generated_skips();
    let (rust_idle, published) = native_transfer_entry::take_poll_paths_for_test();
    eprintln!(
        "POLL-LATENCY-PATH code_id={} mapped_t2={} generated_idle_skips={} rust_idle_preflights={} published_polls={}",
        code.code_id, code.is_t2, generated_skips, rust_idle, published
    );
    let state = shared.0.lock().unwrap();
    let elapsed_ns: Vec<_> = state
        .samples
        .iter()
        .map(|sample| sample.observed_ns - sample.posted_ns)
        .collect();
    let delayed = elapsed_ns.iter().filter(|&&ns| ns > 100_000).count();
    eprintln!(
        "POLL-LATENCY requested={} observed={} completed={} missed={} unposted={} timeouts={} delayed_over_100us={} relocated={} elapsed_ns={:?}",
        state.requested,
        state.samples.len(),
        state.completed,
        state.requested - state.samples.len(),
        samples - state.requested,
        state.timeouts,
        delayed,
        state.relocated,
        elapsed_ns
    );
    for sample in &state.samples {
        eprintln!(
            "POLL-LATENCY-SAMPLE sequence={} requested_epoch={} acknowledged_epoch={:?} posted_ns={} observed_ns={} completed_ns={:?}",
            sample.sequence,
            sample.requested_epoch,
            sample.acknowledged_epoch,
            sample.posted_ns,
            sample.observed_ns,
            sample.completed_ns
        );
    }
    if !elapsed_ns.is_empty() {
        let mut sorted = elapsed_ns.clone();
        sorted.sort_unstable();
        let percentile = |percent: usize| sorted[(sorted.len() * percent).div_ceil(100) - 1];
        eprintln!(
            "POLL-LATENCY-SUMMARY min_ns={} p50_ns={} p95_ns={} p99_ns={} max_ns={} method=nearest-rank",
            sorted[0],
            percentile(50),
            percentile(95),
            percentile(99),
            sorted.last().unwrap()
        );
    }
    assert!(state.failure.is_none(), "{:?}", state.failure);
    assert_eq!(
        state.completed, samples,
        "finite generated loop ended before all requests completed"
    );
    assert!(
        state.relocated,
        "the real requested cold edge must preserve a moving root"
    );
    assert!(
        generated_skips > 0,
        "nonforcing observers must leave real generated idle skips between requests"
    );
    let result = result.unwrap();
    assert_eq!(
        Some(result.to_raw()),
        state.relocated_value,
        "generated code must return the relocated home, not an old address with intact payload"
    );
    assert_eq!(
        super::super::super::super::cp(result),
        (EgclVal::from_fixnum(71), NIL)
    );
}
