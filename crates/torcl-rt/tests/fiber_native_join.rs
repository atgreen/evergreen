#![cfg(all(target_arch = "x86_64", any(unix, windows)))]

use torcl_rt::{SchedulerConfig, SchedulerGroup, TorclVal};

fn collecting() -> TorclVal {
    torcl_rt::rooted!(value = torcl_rt::gc::alloc_double_float(42.0));
    match torcl_rt::collect_t0_minor() {
        Ok(()) => {
            TorclVal::from_fixnum(unsafe { value.as_ptr().add(8).cast::<f64>().read() as i64 })
        }
        Err(error) => {
            eprintln!("collection while joined failed: {error:?}");
            TorclVal::from_fixnum(-1)
        }
    }
}

#[test]
fn registered_native_joiner_does_not_obstruct_its_fibers_gc() {
    torcl_rt::thread::current_thread_id();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let entry = unsafe { TorclVal::from_function_ptr(collecting as *const () as *mut u8) };
    group
        .submit(torcl_rt::thread::make_fiber(entry).unwrap())
        .unwrap();
    assert_eq!(group.finish().unwrap(), vec![TorclVal::from_fixnum(42)]);
}
