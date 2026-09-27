#![cfg(all(target_arch = "x86_64", any(unix, windows)))]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use torcl_rt::scheduler::{SchedulerConfig, SchedulerGroup};
use torcl_rt::thread::{current_thread_id, fiber_yield, make_fiber};
use torcl_rt::value::TorclVal;

static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn rooted_fiber() -> TorclVal {
    torcl_rt::rooted!(value = heap_value(42.0));
    let mut borrowed = heap_value(17.0);
    torcl_rt::rooted_ref!(_borrowed = &mut borrowed);
    let mut carrier = current_thread_id();
    for _ in 0..200 {
        fiber_yield().unwrap();
        let next = current_thread_id();
        if next != carrier {
            MIGRATIONS.fetch_add(1, Ordering::Relaxed);
            carrier = next;
        }
        assert_eq!(marker(*value), 42.0);
        assert_eq!(marker(borrowed), 17.0);
    }
    TorclVal::from_fixnum(42)
}

#[test]
fn root_guards_survive_fiber_suspension_and_carrier_migration() {
    let _serial = TEST_LOCK.lock().unwrap();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 4 }).unwrap();
    for _ in 0..64 {
        let entry = unsafe { TorclVal::from_function_ptr(rooted_fiber as *const () as *mut u8) };
        group.submit(make_fiber(entry).unwrap()).unwrap();
    }
    assert_eq!(group.finish().unwrap(), vec![TorclVal::from_fixnum(42); 64]);
    assert!(
        MIGRATIONS.load(Ordering::Relaxed) > 0,
        "must exercise migration"
    );
}

fn heap_value(marker: f64) -> TorclVal {
    let body = torcl_rt::alloc_typed(8, torcl_rt::object::type_id::DOUBLE_FLOAT).unwrap();
    unsafe {
        body.cast::<f64>().write(marker);
        TorclVal::from_heap_ptr(body.sub(8))
    }
}

fn marker(value: TorclVal) -> f64 {
    unsafe { value.as_ptr().add(8).cast::<f64>().read() }
}

fn parked_heap_roots() -> TorclVal {
    torcl_rt::rooted!(owned = heap_value(42.0));
    // Stress may move this object during the next allocation, before parking;
    // the ordinary run moves it during the explicit suspended collection.
    let original = owned.to_raw();
    let mut borrowed = heap_value(17.0);
    torcl_rt::rooted_ref!(_borrowed = &mut borrowed);
    torcl_rt::thread::park_current_fiber().unwrap();
    assert_eq!(marker(*owned), 42.0);
    assert_eq!(marker(borrowed), 17.0);
    assert_ne!(owned.to_raw(), original, "GC must relocate the live root");
    TorclVal::from_fixnum(42)
}

#[test]
fn moving_gc_updates_owned_and_borrowed_roots_on_suspended_fiber_stacks() {
    let _serial = TEST_LOCK.lock().unwrap();
    torcl_rt::init_heap(&torcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 1024 * 1024,
        tlab_size: 4096,
        region_size: 4096,
        promotion_threshold: 3,
        pause_target_ms: 10,
        gc_workers: 1,
        satb_buffer_size: 32,
        old_occupancy_trigger: 0.9,
    })
    .unwrap();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 4 }).unwrap();
    let mut fibers = Vec::new();
    for _ in 0..16 {
        let entry =
            unsafe { TorclVal::from_function_ptr(parked_heap_roots as *const () as *mut u8) };
        let id = make_fiber(entry).unwrap();
        group.submit(id).unwrap();
        fibers.push(id);
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fibers
        .iter()
        .any(|&id| torcl_rt::thread::fiber_state(id) != Some(torcl_rt::thread::FiberState::Blocked))
    {
        torcl_rt::poll_safepoint();
        assert!(std::time::Instant::now() < deadline, "fibers did not park");
        std::thread::yield_now();
    }
    torcl_rt::gc::collect_t0_minor().unwrap();
    for id in fibers {
        group.unpark(id).unwrap();
    }
    assert_eq!(group.finish().unwrap(), vec![TorclVal::from_fixnum(42); 16]);
}
