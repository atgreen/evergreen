#![cfg(egcl_unix_fibers)]
// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Exercise the raw ABI separately from the scheduler: two live native stacks,
//! repeated returns to the suspension point, and floating-point register pressure.
use egcl_rt::context::{self, Context};
use std::cell::Cell;
use std::hint::black_box;

struct State {
    scheduler: Context,
    fiber: Context,
    low: usize,
    high: usize,
    rounds: usize,
    intact: bool,
}

thread_local! {
    static STATE: Cell<*mut State> = const { Cell::new(std::ptr::null_mut()) };
}

fn suspend(fiber: bool) {
    let state = STATE.get();
    // Do not keep a Rust reference to State live across the switch: the other
    // stack accesses it while this one is suspended.
    unsafe {
        if fiber {
            context::swap(&raw mut (*state).fiber, (*state).scheduler);
        } else {
            context::swap(&raw mut (*state).scheduler, (*state).fiber);
        }
    }
}

#[inline(never)]
fn live_values(seed: f64, fiber: bool) -> bool {
    // Separate locals let the compiler keep scalars in nonvolatile FP regs.
    // black_box after resumption prevents rematerializing the expected values.
    macro_rules! across {
        ($($name:ident = $offset:expr),+) => {{
            $(let $name = black_box(seed + $offset);)+
            let stack = black_box([seed.to_bits(); 257]);
            suspend(fiber);
            $(black_box($name) == seed + $offset)&&+
                && black_box(stack).iter().all(|&value| value == seed.to_bits())
        }};
    }
    across!(
        a = 0.0,
        b = 1.0,
        c = 2.0,
        d = 3.0,
        e = 4.0,
        f = 5.0,
        g = 6.0,
        h = 7.0,
        i = 8.0,
        j = 9.0,
        k = 10.0,
        l = 11.0,
        m = 12.0,
        n = 13.0,
        o = 14.0,
        p = 15.0
    )
}

extern "C" fn entry() {
    let local = 0u64;
    let address = &local as *const _ as usize;
    let state = STATE.get();
    unsafe { (*state).intact = address >= (*state).low && address < (*state).high };
    for round in 0..100 {
        let intact = live_values(1000.25 + round as f64, true);
        unsafe {
            (*state).intact &= intact;
            (*state).rounds += 1;
        }
    }
    suspend(true);
    std::process::abort(); // The completed stack must never be resumed.
}

#[test]
fn repeated_switches_preserve_both_stacks_and_live_values() {
    // Deliberately misalign both ends of the slice to exercise initial SP alignment.
    let mut stack = vec![0u8; 128 * 1024 + 7];
    let stack = &mut stack[3..];
    let mut state = State {
        scheduler: context::NULL,
        fiber: context::make(stack, entry),
        low: stack.as_ptr() as usize,
        high: stack.as_ptr() as usize + stack.len(),
        rounds: 0,
        intact: false,
    };
    assert!(!state.fiber.is_null());
    STATE.set(&raw mut state);
    for round in 0..101 {
        assert!(live_values(-2000.5 - round as f64, false));
        assert!(state.intact);
        assert_eq!(state.rounds, round);
    }
    STATE.set(std::ptr::null_mut());
}
