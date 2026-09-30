// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Surface ownership handshake between Android's main thread and Lisp.
use std::collections::VecDeque;
use std::sync::{Condvar, Mutex};

#[derive(Default)]
struct State {
    window: usize,
    rendering: bool,
    stopped: bool,
    shutdown: bool,
    paused: bool,
    touches: VecDeque<(i32, f32, f32)>,
    keys: VecDeque<(i32, i32, i32)>,
    /// What Lisp last asked us to keep, handed to Android from
    /// `onSaveInstanceState`.
    saved: Vec<u8>,
    /// What Android handed back when this Activity was created, for Lisp to
    /// read once.
    restored: Vec<u8>,
}

#[derive(Default)]
pub struct ActivityState {
    state: Mutex<State>,
    changed: Condvar,
}

impl ActivityState {
    pub fn set_window(&self, window: usize) {
        let mut state = self.state.lock().unwrap();
        assert!(!state.rendering);
        state.window = window;
        state.stopped = false;
        self.changed.notify_all();
    }

    pub fn wait_window(&self) -> usize {
        let mut state = self.state.lock().unwrap();
        while state.window == 0 && !state.shutdown {
            state = self.changed.wait(state).unwrap();
        }
        if state.shutdown {
            return 0;
        }
        state.rendering = true;
        state.window
    }

    pub fn running(&self) -> bool {
        let state = self.state.lock().unwrap();
        state.rendering && !state.stopped && !state.shutdown
    }

    pub fn finish_window(&self) {
        let mut state = self.state.lock().unwrap();
        state.rendering = false;
        state.window = 0;
        self.changed.notify_all();
    }

    pub fn destroy_window(&self) {
        let mut state = self.state.lock().unwrap();
        state.stopped = true;
        state.window = 0;
        while state.rendering {
            state = self.changed.wait(state).unwrap();
        }
        state.touches.clear();
    }

    pub fn shutdown(&self) {
        let mut state = self.state.lock().unwrap();
        state.shutdown = true;
        self.changed.notify_all();
    }

    /// Keep BYTES for the next `onSaveInstanceState`.
    ///
    /// Lisp PUSHES its state rather than being asked for it, because
    /// `onSaveInstanceState` arrives on the main thread at a moment Android
    /// chooses and the interpreter may be anywhere -- mid-frame, inside a JNI
    /// call, or waiting on the main-thread gate. Asking it synchronously would
    /// deadlock against that gate; asking it asynchronously would miss the
    /// deadline. So the answer is always ready before the question.
    pub fn set_saved(&self, bytes: &[u8]) {
        self.state.lock().unwrap().saved = bytes.to_vec();
    }

    pub fn saved(&self) -> Vec<u8> {
        self.state.lock().unwrap().saved.clone()
    }

    pub fn set_restored(&self, bytes: &[u8]) {
        self.state.lock().unwrap().restored = bytes.to_vec();
    }

    pub fn restored(&self) -> Vec<u8> {
        self.state.lock().unwrap().restored.clone()
    }

    pub fn set_paused(&self, paused: bool) {
        self.state.lock().unwrap().paused = paused;
    }
    pub fn paused(&self) -> bool {
        self.state.lock().unwrap().paused
    }

    pub fn touch(&self, action: i32, x: f32, y: f32) {
        let mut state = self.state.lock().unwrap();
        if state.touches.len() == 64 {
            state.touches.pop_front();
        }
        state.touches.push_back((action, x, y));
    }

    pub fn poll_touch(&self) -> Option<(i32, f32, f32)> {
        self.state.lock().unwrap().touches.pop_front()
    }

    /// Queue a key event: action, key code, meta state.
    ///
    /// Bounded exactly like `touch`, and for the same reason: a Lisp worker that
    /// stops polling must not grow this without limit. Dropping the OLDEST is
    /// the right end to drop from for keys as well as touches -- a backlog that
    /// has outrun the application is stale, and the recent keystrokes are the
    /// ones still worth delivering.
    pub fn key(&self, action: i32, code: i32, meta: i32) {
        let mut state = self.state.lock().unwrap();
        if state.keys.len() == 64 {
            state.keys.pop_front();
        }
        state.keys.push_back((action, code, meta));
    }

    pub fn poll_key(&self) -> Option<(i32, i32, i32)> {
        self.state.lock().unwrap().keys.pop_front()
    }
}
