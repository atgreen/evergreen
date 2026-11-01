// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Owner-managed requests and publication; compiler workers never run Lisp.
use super::*;
use std::sync::{Condvar, Mutex, OnceLock, mpsc};

#[cfg(all(target_arch = "x86_64", target_os = "linux"))]
use super::native_transfer_entry::{CompiledT1, PreparedT1};
#[cfg(all(target_arch = "s390x", target_os = "linux"))]
use super::s390x::{EmittedT1 as CompiledT1, PreparedT1};

struct Job {
    permit: CompilerPermit,
    symbol: u32,
    generation: u64,
    ticket: u64,
    prepared: PreparedT1,
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    counter: Option<Box<std::sync::atomic::AtomicU32>>,
    completion: mpsc::Sender<Completion>,
}
struct Completion {
    _permit: CompilerPermit,
    symbol: u32,
    generation: u64,
    ticket: u64,
    artifact: Result<Option<CompiledT1>, ()>,
    roots: egcl_rt::CrossThreadRoot<T2InstalledBodies>,
    counter: Option<Box<std::sync::atomic::AtomicU32>>,
}
struct Owner {
    sender: mpsc::Sender<Completion>,
    receiver: mpsc::Receiver<Completion>,
    pending: HashMap<u32, (u64, u64, bool)>,
    generic: HashMap<u32, u64>,
    direct_calls: HashMap<(u32, u64), Vec<Rc<NativeCode>>>,
}
impl Owner {
    fn new() -> Self {
        let (sender, receiver) = mpsc::channel();
        Self {
            sender,
            receiver,
            pending: HashMap::new(),
            generic: HashMap::new(),
            direct_calls: HashMap::new(),
        }
    }
}
static OWNER: egcl_rt::execution_local::ExecutionLocal<RefCell<Owner>> =
    unsafe { egcl_rt::execution_local::ExecutionLocal::new(|| RefCell::new(Owner::new())) };
struct Queue {
    outstanding: Arc<std::sync::atomic::AtomicUsize>,
    dropped: std::sync::atomic::AtomicU64,
    jobs: Mutex<std::collections::VecDeque<Job>>,
    ready: Condvar,
    capacity: usize,
}
impl Queue {
    fn submit(&self, job: Job) -> bool {
        let mut jobs = self.jobs.lock().unwrap_or_else(|e| e.into_inner());
        if jobs.len() >= self.capacity {
            return false;
        }
        jobs.push_back(job);
        self.ready.notify_one();
        true
    }
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
            outstanding: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            dropped: std::sync::atomic::AtomicU64::new(0),
            jobs: Mutex::new(std::collections::VecDeque::new()),
            ready: Condvar::new(),
            capacity: positive_env(&["EGCL_COMPILE_QUEUE_SIZE"], 64) as usize,
        });
        let mut started = 0;
        for index in 0..positive_env(&["EGCL_T1_THREADS"], 1).min(32) {
            let queue = Arc::clone(&queue);
            if std::thread::Builder::new()
                .name(format!("egcl-t1-{index}"))
                .spawn(move || {
                    loop {
                        let Job {
                            permit,
                            symbol,
                            generation,
                            ticket,
                            prepared,
                            roots,
                            counter,
                            completion,
                        } = queue.take();
                        let artifact =
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                #[cfg(test)]
                                super::compiler_test_pause(symbol, false);
                                #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
                                return native_transfer_entry::compile_background_t1(prepared);
                                #[cfg(all(target_arch = "s390x", target_os = "linux"))]
                                return s390x::compile_background_t1(prepared);
                            }))
                            .map_err(|_| ());
                        let _ = completion.send(Completion {
                            _permit: permit,
                            symbol,
                            generation,
                            ticket,
                            artifact,
                            roots,
                            counter,
                        });
                    }
                })
                .is_ok()
            {
                started += 1;
            }
        }
        (started != 0).then_some(queue)
    })
}

pub(super) fn pending(sym: u32) -> bool {
    OWNER.with(|owner| owner.borrow().pending.contains_key(&sym))
}

pub(super) fn pending_any() -> bool {
    OWNER.with(|owner| !owner.borrow().pending.is_empty())
}

pub(super) fn request(sym: u32) -> bool {
    request_with_speculation(sym, true)
}

pub(super) fn request_with_speculation(sym: u32, allow_speculation: bool) -> bool {
    if cfg!(egcl_no_t1) || is_profile_pinned(sym) || t1_declined(sym) {
        return false;
    }
    if DEOPT_BLACKLIST.with(|s| s.borrow().contains(&sym)) {
        T1_DECLINED.with(|s| s.borrow_mut().insert(sym));
        return false;
    }
    let Some(body) = registry_get(sym) else {
        return false;
    };
    let generation = REGISTRY_GENERATION.with(|g| g.borrow().get(&sym).copied().unwrap_or(0));
    let allow_speculation = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        if !allow_speculation {
            owner.generic.insert(sym, generation);
        }
        if owner
            .generic
            .get(&sym)
            .is_some_and(|&saved| saved != generation)
        {
            owner.generic.remove(&sym);
        }
        allow_speculation && owner.generic.get(&sym) != Some(&generation)
    });
    if OWNER.with(|owner| {
        owner
            .borrow()
            .pending
            .get(&sym)
            .is_some_and(|&(current, _, speculative)| {
                current == generation && (allow_speculation || !speculative)
            })
    }) {
        return false;
    }
    static TICKETS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let ticket = TICKETS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    if t1_would_lose_captured_control(&body) {
        egcl_rt::blog!(
            "compile",
            egcl_rt::log::TRACE,
            "[T1] {}: declined: a closure it creates needs captured block/tag control; retain interpreter",
            sym_label(sym)
        );
        T1_DECLINED.with(|s| s.borrow_mut().insert(sym));
        return false;
    }
    let Some(queue) = queue() else {
        return false;
    };
    let Some(permit) = CompilerPermit::acquire(&queue.outstanding, queue.capacity) else {
        queue
            .dropped
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        egcl_rt::blog!(
            "compile",
            egcl_rt::log::TRACE,
            "[T1] {}: queue full; retain T0",
            sym_label(sym)
        );
        return false;
    };
    let roots = egcl_rt::CrossThreadRoot::new(T2InstalledBodies {
        bodies: vec![Arc::clone(&body)],
    });
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let counter = None;
    #[cfg(all(target_arch = "s390x", target_os = "linux"))]
    let counter = Some(Box::new(std::sync::atomic::AtomicU32::new(0)));
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let prepared =
        native_transfer_entry::prepare_background_t1(sym, Arc::clone(&body), allow_speculation)
            .map(|prepared| (prepared, Vec::new()));
    #[cfg(all(target_arch = "s390x", target_os = "linux"))]
    let prepared = {
        let address = std::ptr::from_ref(&**counter.as_ref().unwrap()) as u64;
        s390x::prepare_background_t1(sym, &body, allow_speculation, address)
    };
    let Some((prepared, direct_calls)) = prepared else {
        T1_DECLINED.with(|s| s.borrow_mut().insert(sym));
        return false;
    };
    let sender = OWNER.with(|owner| owner.borrow().sender.clone());
    if !queue.submit(Job {
        permit,
        symbol: sym,
        generation,
        ticket,
        prepared,
        roots,
        counter,
        completion: sender,
    }) {
        return false;
    }
    OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        owner
            .pending
            .insert(sym, (generation, ticket, allow_speculation));
        owner.direct_calls.insert((sym, ticket), direct_calls);
    });
    true
}

fn install(done: Completion) -> Option<Rc<NativeCode>> {
    let (current, direct_calls) = OWNER.with(|owner| {
        let mut owner = owner.borrow_mut();
        let current = owner
            .pending
            .get(&done.symbol)
            .is_some_and(|&(generation, ticket, _)| {
                generation == done.generation && ticket == done.ticket
            });
        let permitted = current
            && !owner
                .pending
                .get(&done.symbol)
                .is_some_and(|&(_, _, speculative)| {
                    speculative && owner.generic.get(&done.symbol) == Some(&done.generation)
                });
        if current {
            owner.pending.remove(&done.symbol);
        }
        (
            permitted,
            owner
                .direct_calls
                .remove(&(done.symbol, done.ticket))
                .unwrap_or_default(),
        )
    });
    if !current {
        return None;
    }
    let body = registry_get(done.symbol)?;
    let generation =
        REGISTRY_GENERATION.with(|g| g.borrow().get(&done.symbol).copied().unwrap_or(0));
    if generation != done.generation
        || is_profile_pinned(done.symbol)
        || DEOPT_BLACKLIST.with(|s| s.borrow().contains(&done.symbol))
    {
        return None;
    }
    // OSR or explicit compilation may have installed a newer tier meanwhile.
    if let Some(current) = NATIVE_REGISTRY.with(|r| r.borrow().get(&done.symbol).cloned()) {
        return Some(current);
    }
    let artifact = match done.artifact {
        Err(()) => {
            egcl_rt::blog!(
                "compile",
                egcl_rt::log::TRACE,
                "[T1] {}: compiler panic; retain T0 and allow retry",
                sym_label(done.symbol)
            );
            return None;
        }
        Ok(artifact) => artifact,
    };
    let Some(artifact) = artifact else {
        egcl_rt::blog!(
            "compile",
            egcl_rt::log::TRACE,
            "[T1] {}: background compilation declined",
            sym_label(done.symbol)
        );
        T1_DECLINED.with(|s| s.borrow_mut().insert(done.symbol));
        return None;
    };
    #[cfg(all(target_arch = "x86_64", target_os = "linux"))]
    let code = {
        let _ = (&done.roots, &done.counter, direct_calls);
        native_transfer_entry::install_background_t1(done.symbol, body, artifact)?
    };
    #[cfg(all(target_arch = "s390x", target_os = "linux"))]
    let code = install_checked_t1_artifact(
        done.symbol,
        body,
        NativeEmission {
            env_names: artifact.env_names,
            code: artifact.code,
            osr_entries: artifact.osr_entries,
            bcp_offsets: artifact.bcp_offsets,
            has_deopt: artifact.has_deopt,
            direct_calls,
        },
        Some(done.roots),
        done.counter,
    )?;
    mark_fresh_promotion(done.symbol);
    let function = egcl_rt::symbols::symbol_function(done.symbol)
        .filter(|&v| egcl_rt::function::is_interpreted_function(v));
    publish_native(done.symbol, function, &code);
    Some(code)
}

pub(super) fn poll() {
    loop {
        let done = OWNER.with(|owner| owner.borrow().receiver.try_recv().ok());
        let Some(done) = done else {
            return;
        };
        let _ = install(done);
    }
}

pub(super) fn wait(sym: u32) -> Option<Rc<NativeCode>> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        poll();
        if let Some(code) = NATIVE_REGISTRY.with(|r| r.borrow().get(&sym).cloned()) {
            return Some(code);
        }
        if !pending(sym) || std::time::Instant::now() >= deadline {
            return None;
        }
        compiler_wait_tick();
    }
}

#[cfg(test)]
pub(super) fn force_full_queue_for_test() -> Vec<CompilerPermit> {
    let queue = queue().as_ref().unwrap();
    let mut reservations = Vec::new();
    while let Some(permit) = CompilerPermit::acquire(&queue.outstanding, queue.capacity) {
        reservations.push(permit);
    }
    reservations
}
