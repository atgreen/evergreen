// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

use egcl_rt::{EgclError, EgclVal, symbols};
use std::sync::{Arc, Barrier};

#[test]
fn concurrent_assignment_cannot_overwrite_a_published_constant() {
    // Intern before starting either participant, so the racing section cannot
    // trigger a collection while a native thread waits at the test barrier.
    let indices: Vec<_> = (0..512)
        .map(|i| symbols::intern(&format!("PUBLICATION-CONSTANT-{i}")))
        .collect();
    let barrier = Arc::new(Barrier::new(2));
    let writer_barrier = barrier.clone();
    let writer_indices = indices.clone();
    let writer = std::thread::spawn(move || {
        egcl_rt::thread::current_thread_id();
        let mut failures = Vec::new();
        for idx in writer_indices {
            writer_barrier.wait();
            for _ in 0..64 {
                match symbols::set_symbol_value_checked(idx, EgclVal::from_fixnum(9)) {
                    Ok(()) => {}
                    Err(EgclError::ProgramError(_)) => {
                        if symbols::symbol_value(idx) != Some(EgclVal::from_fixnum(7)) {
                            failures.push(format!("constant {idx} changed after rejection"));
                        }
                    }
                    Err(error) => failures.push(format!("unexpected assignment error: {error:?}")),
                }
            }
            writer_barrier.wait();
        }
        failures
    });
    let mut unpublished = Vec::new();
    for idx in indices {
        barrier.wait();
        symbols::define_symbol_constant(idx, EgclVal::from_fixnum(7));
        barrier.wait();
        if !symbols::symbol_is_constant(idx)
            || symbols::symbol_value(idx) != Some(EgclVal::from_fixnum(7))
        {
            unpublished.push(idx);
        }
    }
    let failures = writer.join().unwrap();
    assert!(
        unpublished.is_empty(),
        "incorrect publication: {unpublished:?}"
    );
    assert!(failures.is_empty(), "{failures:?}");
}
