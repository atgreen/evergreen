#![cfg(all(target_arch = "x86_64", any(unix, windows)))]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{SchedulerConfig, SchedulerGroup, EgclVal};

fn collecting() -> EgclVal {
    egcl_rt::rooted!(value = egcl_rt::gc::alloc_double_float(42.0));
    match egcl_rt::collect_t0_minor() {
        Ok(()) => {
            EgclVal::from_fixnum(unsafe { value.as_ptr().add(8).cast::<f64>().read() as i64 })
        }
        Err(error) => {
            eprintln!("collection while joined failed: {error:?}");
            EgclVal::from_fixnum(-1)
        }
    }
}

#[test]
fn registered_native_joiner_does_not_obstruct_its_fibers_gc() {
    egcl_rt::thread::current_thread_id();
    let group = SchedulerGroup::init(&SchedulerConfig { num_workers: 1 }).unwrap();
    let entry = unsafe { EgclVal::from_function_ptr(collecting as *const () as *mut u8) };
    group
        .submit(egcl_rt::thread::make_fiber(entry).unwrap())
        .unwrap();
    assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(42)]);
}
