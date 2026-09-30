// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

#[path = "../src/lifecycle.rs"]
mod lifecycle;
use lifecycle::ActivityState;
use std::sync::{Arc, mpsc};
use std::time::Duration;

#[test]
fn destroying_a_surface_waits_for_renderer_release() {
    let state = Arc::new(ActivityState::default());
    state.set_window(42);
    assert_eq!(state.wait_window(), 42);
    let (sent, received) = mpsc::channel();
    let other = state.clone();
    let destroy = std::thread::spawn(move || {
        other.destroy_window();
        sent.send(()).unwrap();
    });
    while state.running() {
        std::thread::yield_now();
    }
    assert!(received.recv_timeout(Duration::from_millis(30)).is_err());
    state.finish_window();
    received.recv_timeout(Duration::from_secs(1)).unwrap();
    destroy.join().unwrap();
    state.set_window(99);
    assert_eq!(state.wait_window(), 99);
    state.finish_window();
    state.shutdown();
    assert_eq!(state.wait_window(), 0);
}

#[test]
fn shutdown_wakes_a_waiting_interpreter() {
    let state = Arc::new(ActivityState::default());
    let other = state.clone();
    let worker = std::thread::spawn(move || other.wait_window());
    state.shutdown();
    assert_eq!(worker.join().unwrap(), 0);
}

#[test]
fn input_is_bounded_and_pause_is_observable() {
    let state = ActivityState::default();
    state.set_paused(true);
    assert!(state.paused());
    state.set_paused(false);
    assert!(!state.paused());
    for i in 0..1000 {
        state.touch(i, i as f32, 2.0);
    }
    let events: Vec<_> = std::iter::from_fn(|| state.poll_touch()).collect();
    assert_eq!(events.len(), 64);
    assert_eq!(events.last().unwrap().0, 999);
}
