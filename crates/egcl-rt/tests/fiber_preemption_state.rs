#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use std::sync::atomic::{AtomicUsize, Ordering};
use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclVal};

static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);

fn allocating_fiber() -> EgclVal {
    use egcl_rt::{runtime, syscall, thread};
    let fiber = thread::current_fiber().unwrap();
    let identity = fiber.id();
    egcl_rt::rooted!(value = egcl_rt::gc::alloc_double_float(42.0));
    let mut wrong = 0;
    for _ in 0..200 {
        let before = syscall::cached_tid();
        runtime::set_sigsegv_recovery_ips(0x1000 + identity.0 as usize * 16, 0);
        fiber.request_yield();
        // No explicit yield: allocating must honor the pending preemption.
        let allocated = egcl_rt::gc::alloc_double_float(17.0);
        let after = syscall::cached_tid();
        let actual = syscall::gettid();
        if actual != before {
            MIGRATIONS.fetch_add(1, Ordering::Relaxed);
        }
        if after != actual || thread::current_fiber_id() != Some(identity) {
            wrong |= 1;
        }
        if runtime::current_sigsegv_null_guard_recovery_ip() != 0x1000 + identity.0 as usize * 16 {
            wrong |= 2;
        }
        unsafe {
            if value.as_ptr().add(8).cast::<f64>().read() != 42.0
                || allocated.as_ptr().add(8).cast::<f64>().read() != 17.0
            {
                wrong |= 4;
            }
        }
    }
    runtime::set_sigsegv_recovery_ips(0, 0);
    EgclVal::from_fixnum(wrong)
}

#[test]
fn allocation_preemption_refreshes_carrier_state_and_preserves_fiber_state() {
    egcl_rt::init_heap(&egcl_rt::GcConfig {
        heap_size: 4 * 1024 * 1024,
        heap_max: 16 * 1024 * 1024,
        nursery_size: 64 * 1024,
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
    for _ in 0..64 {
        let entry =
            unsafe { EgclVal::from_function_ptr(allocating_fiber as *const () as *mut u8) };
        group
            .submit(egcl_rt::thread::make_fiber(entry).unwrap())
            .unwrap();
    }
    assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(0); 64]);
    assert!(
        MIGRATIONS.load(Ordering::Relaxed) > 0,
        "must exercise migration"
    );
}
