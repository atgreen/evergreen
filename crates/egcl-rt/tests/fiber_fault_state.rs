#![cfg(egcl_fibers)]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::runtime::{
    check_sigsegv_null_guard, check_sigsegv_stack_guard, current_sigsegv_null_guard_recovery_ip,
    current_sigsegv_stack_guard_recovery_ip, post_sigsegv_null_guard, post_sigsegv_stack_guard,
    set_sigsegv_recovery_ips,
};
use egcl_rt::thread::{current_thread_id, fiber_yield, make_fiber};
use egcl_rt::{EgclVal, SchedulerConfig, SchedulerGroup};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(1);
static MIGRATIONS: AtomicUsize = AtomicUsize::new(0);

fn probe() -> EgclVal {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let mut wrong = 0;
    if current_sigsegv_null_guard_recovery_ip() != 0
        || current_sigsegv_stack_guard_recovery_ip() != 0
        || check_sigsegv_null_guard()
        || check_sigsegv_stack_guard()
    {
        wrong |= 1;
    }
    let mut carrier = current_thread_id();
    for _ in 0..200 {
        // Synthetic targets: no hardware exception is raised by this test.
        set_sigsegv_recovery_ips(id * 16, id * 16 + 8);
        post_sigsegv_null_guard();
        post_sigsegv_stack_guard();
        fiber_yield().unwrap();
        let next = current_thread_id();
        if next != carrier {
            MIGRATIONS.fetch_add(1, Ordering::Relaxed);
            carrier = next;
        }
        if current_sigsegv_null_guard_recovery_ip() != id * 16
            || current_sigsegv_stack_guard_recovery_ip() != id * 16 + 8
        {
            wrong |= 2;
        }
        if !check_sigsegv_null_guard() || !check_sigsegv_stack_guard() {
            wrong |= 4;
        }
    }
    set_sigsegv_recovery_ips(0, 0);
    EgclVal::from_fixnum(wrong)
}

#[test]
fn recovery_targets_and_pending_faults_follow_the_fiber() {
    for carriers in [1, 4] {
        let group = SchedulerGroup::init(&SchedulerConfig {
            num_workers: carriers,
        })
        .unwrap();
        for _ in 0..64 {
            let entry = native_entry::entry(probe);
            group.submit(make_fiber(entry).unwrap()).unwrap();
        }
        assert_eq!(group.finish().unwrap(), vec![EgclVal::from_fixnum(0); 64]);
    }
    assert!(MIGRATIONS.load(Ordering::Relaxed) > 0);
}

#[path = "support/native_entry.rs"]
mod native_entry;
