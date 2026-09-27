#![cfg(all(target_arch = "x86_64", any(unix, windows)))]

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use torcl_rt::thread::{FiberId, fiber_state, make_fiber};
use torcl_rt::{SchedulerConfig, SchedulerGroup, TorclVal};

static FIRST: AtomicU64 = AtomicU64::new(0);
static ORIGINAL: AtomicUsize = AtomicUsize::new(0);

fn first() -> TorclVal {
    let value = torcl_rt::gc::alloc_double_float(42.0);
    ORIGINAL.store(value.to_raw() as usize, Ordering::Release);
    value
}

fn second() -> TorclVal {
    // Wait until finish has consumed the first result and removed its registry
    // root. The only remaining owner should be finish's result accumulator.
    while fiber_state(FiberId(FIRST.load(Ordering::Acquire))).is_some() {
        torcl_rt::poll_safepoint();
        std::thread::yield_now();
    }
    if torcl_rt::collect_t0_minor().is_err() {
        return TorclVal::from_fixnum(-1);
    }
    TorclVal::from_fixnum(17)
}

#[test]
fn finish_roots_early_results_while_later_fibers_collect() {
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 2 }).unwrap();
    let entry = unsafe { TorclVal::from_function_ptr(first as *const () as *mut u8) };
    let id = make_fiber(entry).unwrap();
    FIRST.store(id.0, Ordering::Release);
    group.submit(id).unwrap();
    let entry = unsafe { TorclVal::from_function_ptr(second as *const () as *mut u8) };
    group.submit(make_fiber(entry).unwrap()).unwrap();
    let results = group.finish().unwrap();
    assert_eq!(
        results[1],
        TorclVal::from_fixnum(17),
        "collector handshake failed"
    );
    assert_ne!(
        results[0].to_raw() as usize,
        ORIGINAL.load(Ordering::Acquire),
        "the accumulated result must be relocated, not left pointing at freed nursery storage"
    );
    assert_eq!(
        unsafe { results[0].as_ptr().add(8).cast::<f64>().read() },
        42.0
    );
}
