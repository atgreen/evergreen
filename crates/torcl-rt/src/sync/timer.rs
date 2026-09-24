use crate::error::TorclError;
use crate::thread::FiberId;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy)]
struct TimerEntry {
    deadline: Instant,
    fiber: FiberId,
    token: u64,
}

struct TimerService {
    queue: Arc<(Mutex<Vec<TimerEntry>>, Condvar)>,
    started: bool,
}

fn timer_service() -> &'static TimerService {
    static SERVICE: OnceLock<TimerService> = OnceLock::new();
    SERVICE.get_or_init(|| {
        let queue = Arc::new((Mutex::new(Vec::<TimerEntry>::new()), Condvar::new()));
        let worker_queue = Arc::clone(&queue);
        let started = std::thread::Builder::new()
            .name("torcl-fiber-timer".into())
            .spawn(move || timer_loop(worker_queue))
            .is_ok();
        TimerService { queue, started }
    })
}

pub(crate) fn schedule(fiber: FiberId, token: u64, deadline: Instant) -> Result<(), TorclError> {
    let service = timer_service();
    if !service.started {
        return Err(TorclError::Internal(
            "failed to start fiber deadline service".into(),
        ));
    }
    let (queue, wake) = &*service.queue;
    queue.lock().unwrap().push(TimerEntry {
        deadline,
        fiber,
        token,
    });
    wake.notify_one();
    Ok(())
}

fn timer_loop(shared: Arc<(Mutex<Vec<TimerEntry>>, Condvar)>) {
    let (queue, wake) = &*shared;
    loop {
        let mut guard = queue.lock().unwrap();
        while guard.is_empty() {
            guard = wake.wait(guard).unwrap();
        }
        let next = guard
            .iter()
            .map(|entry| entry.deadline)
            .min()
            .unwrap_or_else(Instant::now);
        let now = Instant::now();
        if next > now {
            let (new_guard, _) = wake.wait_timeout(guard, next - now).unwrap();
            drop(new_guard);
            continue;
        }
        let mut expired = Vec::new();
        let now = Instant::now();
        let mut index = 0;
        while index < guard.len() {
            if guard[index].deadline <= now {
                expired.push(guard.swap_remove(index));
            } else {
                index += 1;
            }
        }
        drop(guard);
        for entry in expired {
            crate::thread::wake_fiber_wait(entry.fiber, entry.token);
        }
    }
}
