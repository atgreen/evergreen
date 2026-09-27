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
}
