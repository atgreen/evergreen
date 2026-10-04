// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Process CPU accounting includes worker threads, unlike the caller's clock.
use egcl_rt::syscall::{process_cpu_time_ns, thread_cpu_time_ns};
use std::time::{Duration, Instant};

#[test]
fn process_cpu_time_includes_a_worker_while_the_caller_waits() {
    let process_start = process_cpu_time_ns().unwrap();
    let caller_start = thread_cpu_time_ns().unwrap();
    let consumed = std::thread::spawn(|| {
        let deadline = Instant::now() + Duration::from_secs(10);
        let start = thread_cpu_time_ns().unwrap();
        loop {
            for n in 0..10_000_u64 {
                std::hint::black_box(n.wrapping_mul(n));
            }
            let consumed = thread_cpu_time_ns().unwrap() - start;
            if consumed >= 50_000_000 {
                return consumed;
            }
            assert!(
                Instant::now() < deadline,
                "worker CPU clock did not advance"
            );
        }
    })
    .join()
    .unwrap();
    let caller = thread_cpu_time_ns().unwrap() - caller_start;
    let process = process_cpu_time_ns().unwrap() - process_start;
    assert!(process >= consumed, "worker={consumed} process={process}");
    assert!(caller < process / 2, "caller={caller} process={process}");
}
