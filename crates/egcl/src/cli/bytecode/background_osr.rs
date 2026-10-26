// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Checked OSR compiles an exact live body, independently of its current name.
use super::*;
use std::sync::{Condvar, Mutex, OnceLock, atomic::AtomicUsize, mpsc};

struct Job {
    key: usize,
    label: u32,
    prepared: s390x::PreparedT1,
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    permit: CompilerPermit,
    completion: mpsc::Sender<Completion>,
}

struct Completion {
    key: usize,
    // None means unsupported bytecode; Err means a worker panic, which must
    // not turn this body into a permanent structural decline.
    artifact: Result<Option<s390x::EmittedT1>, ()>,
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    _permit: CompilerPermit,
}

struct Pending {
    body: Arc<BytecodeFunction>,
    label: u32,
    direct_calls: Vec<Rc<NativeCode>>,
    counter: Box<std::sync::atomic::AtomicU32>,
}

struct Owner {
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    pending: HashMap<usize, Pending>,
}

impl Owner {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            pending: HashMap::new(),
        }
    }
}

static OWNER: egcl_rt::execution_local::ExecutionLocal<RefCell<Owner>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Owner::new())) };

struct Queue {
    jobs: Mutex<std::collections::VecDeque<Job>>,
    ready: Condvar,
    outstanding: Arc<AtomicUsize>,
    capacity: usize,
    dropped: std::sync::atomic::AtomicU64,
}

impl Queue {
    fn take(&self) -> Job {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(job) = jobs.pop_front() {
                return job;
            }
            jobs = self.ready.wait(jobs).unwrap_or_else(|e| e.into_inner());
        }
    }
}

fn queue() -> &'static Option<Arc<Queue>> {
    static QUEUE: OnceLock<Option<Arc<Queue>>> = OnceLock::new();
    QUEUE.get_or_init(|| {
        let queue = Arc::new(Queue {
            jobs: Mutex::new(std::collections::VecDeque::new()),
            ready: Condvar::new(),
            outstanding: Arc::new(AtomicUsize::new(0)),
            capacity: positive_env(&["EGCL_COMPILE_QUEUE_SIZE"], 64) as usize,
            dropped: std::sync::atomic::AtomicU64::new(0),
        });
        let worker_queue = Arc::clone(&queue);
        std::thread::Builder::new()
            .name("egcl-osr".into())
            .spawn(move || {
                loop {
                    let Job {
                        key,
                        label,
                        prepared,
                        roots,
                        permit,
                        completion,
                    } = worker_queue.take();
                    let artifact = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        #[cfg(test)]
                        super::compiler_test_pause(label, false);
                        let _ = label;
                        s390x::compile_background_t1(prepared)
                    }))
                    .map_err(|_| ());
                    // A dead owner drops the artifact, roots and permit here. No
                    // pointer captured in the snapshot is dereferenced by emission.
                    let _ = completion.send(Completion {
                        key,
                        artifact,
                        roots,
                        _permit: permit,
                    });
                }
            })
            .ok()?;
        Some(queue)
    })
}

fn poll() {
    loop {
        let done = OWNER.with(|owner| owner.borrow().receiver.try_recv().ok());
        let Some(done) = done else { return };
        let pending = OWNER.with(|owner| owner.borrow_mut().pending.remove(&done.key));
        let Some(pending) = pending else { continue };
        let artifact = match done.artifact {
            Ok(Some(artifact)) if !artifact.osr_entries.is_empty() => artifact,
            Ok(_) => {
                egcl_rt::blog!(
                    "compile",
                    egcl_rt::log::TRACE,
                    "[OSR] {}: background compilation declined",
                    pending.body.name
                );
                ANON_OSR_REGISTRY.with(|cache| cache.borrow_mut().insert(done.key, None));
                continue;
            }
            Err(()) => {
                egcl_rt::blog!(
                    "compile",
                    egcl_rt::log::TRACE,
                    "[OSR] {}: compiler failed; retry remains eligible",
                    pending.body.name
                );
                continue;
            }
        };
        let code = install_osr_emission(
            &pending.body,
            pending.label,
            NativeEmission {
                env_names: artifact.env_names,
                code: artifact.code,
                osr_entries: artifact.osr_entries,
                bcp_offsets: artifact.bcp_offsets,
                has_deopt: artifact.has_deopt,
                direct_calls: pending.direct_calls,
            },
            Some(done.roots),
            Some(pending.counter),
        );
        // Resource/publication failures remain retryable. A name redefinition
        // does not invalidate this exact body's OSR entry: an old activation
        // must finish its own body, not the replacement definition.
        if let Some(code) = code {
            ANON_OSR_REGISTRY.with(|cache| cache.borrow_mut().insert(done.key, Some(code)));
        }
    }
}

pub(super) fn compile(body: &Arc<BytecodeFunction>, label: u32) -> Option<Rc<OsrCode>> {
    poll();
    let key = Arc::as_ptr(body) as usize;
    if let Some(cached) = ANON_OSR_REGISTRY.with(|cache| cache.borrow().get(&key).cloned()) {
        return cached;
    }
    if OWNER.with(|owner| owner.borrow().pending.contains_key(&key)) {
        return None;
    }
    if cfg!(egcl_no_t1) || native_would_lose_captured_control(body) {
        ANON_OSR_REGISTRY.with(|cache| cache.borrow_mut().insert(key, None));
        return None;
    }
    let queue = queue().as_ref()?;
    // Capacity includes running and completed jobs until publication, so a
    // dormant owner cannot accumulate unlimited roots and machine code.
    let Some(permit) = CompilerPermit::acquire(&queue.outstanding, queue.capacity) else {
        queue
            .dropped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return None;
    };
    let roots = egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
        bodies: vec![Arc::clone(body)],
    });
    let counter = Box::new(std::sync::atomic::AtomicU32::new(0));
    let counter_address = std::ptr::from_ref(&*counter) as u64;
    let Some((prepared, direct_calls)) =
        s390x::prepare_background_osr(body, counter_address, osr_traps_enabled())
    else {
        ANON_OSR_REGISTRY.with(|cache| cache.borrow_mut().insert(key, None));
        return None;
    };
    let completion = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner.pending.insert(
            key,
            Pending {
                body: Arc::clone(body),
                label,
                direct_calls,
                counter,
            },
        );
        owner.sender.clone()
    });
    queue
        .jobs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push_back(Job {
            key,
            label,
            prepared,
            roots,
            permit,
            completion,
        });
    queue.ready.notify_one();
    None
}
