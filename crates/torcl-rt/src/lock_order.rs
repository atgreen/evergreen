//! Debug-checked global runtime lock ordering (D13.01, R13.05–R13.06).
//!
//! Long-lived subsystem locks use these wrappers.  User-visible mutexes and
//! short-lived condvar coordination locks are deliberately outside this global
//! hierarchy: their ordering is controlled by the program/protocol rather than
//! by runtime subsystem nesting.

// The lock-order checker itself is compiled only under debug_assertions, so
// its imports are gated the same way or release builds warn on them.
#[cfg(debug_assertions)]
use std::cell::RefCell;
#[cfg(debug_assertions)]
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::{LockResult, PoisonError, TryLockError, TryLockResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LockLevel {
    Stream = 1,
    HashTable = 2,
    PackageRegistry = 3,
    Package = 4,
    InternedString = 5,
    CodeCache = 6,
    Profiling = 7,
    GcWorld = 8,
    ExecutionRegistry = 9,
    ExecutionObject = 10,
    ImageSave = 11,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LockMeta {
    id: usize,
    level: LockLevel,
    order: u64,
    name: &'static str,
}

#[cfg(debug_assertions)]
thread_local! {
    static LOCK_STACK: RefCell<Vec<LockMeta>> = const { RefCell::new(Vec::new()) };
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct WaitGraph {
    owners: HashMap<usize, HashSet<std::thread::ThreadId>>,
    waiters: HashMap<std::thread::ThreadId, LockMeta>,
}

#[cfg(debug_assertions)]
fn wait_graph() -> &'static std::sync::Mutex<WaitGraph> {
    static GRAPH: std::sync::OnceLock<std::sync::Mutex<WaitGraph>> = std::sync::OnceLock::new();
    GRAPH.get_or_init(|| std::sync::Mutex::new(WaitGraph::default()))
}

#[cfg(debug_assertions)]
fn valid_after(held: LockMeta, requested: LockMeta) -> bool {
    requested.level > held.level
        || (requested.level == held.level && held.order != 0 && requested.order > held.order)
}

#[cfg(debug_assertions)]
fn before_acquire(meta: LockMeta) {
    LOCK_STACK.with(|stack| {
        if let Some(&held) = stack.borrow().last() {
            assert!(
                valid_after(held, meta),
                "lock order violation: holding '{}' at level {} order {}, requesting '{}' at level {} order {}",
                held.name,
                held.level as u8,
                held.order,
                meta.name,
                meta.level as u8,
                meta.order
            );
        }
    });
    wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .waiters
        .insert(std::thread::current().id(), meta);
    start_watchdog();
}

#[cfg(not(debug_assertions))]
fn before_acquire(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn acquired(meta: LockMeta) {
    let thread = std::thread::current().id();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    graph.waiters.remove(&thread);
    graph.owners.entry(meta.id).or_default().insert(thread);
    drop(graph);
    LOCK_STACK.with(|stack| stack.borrow_mut().push(meta));
}

#[cfg(not(debug_assertions))]
fn acquired(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn cancelled(meta: LockMeta) {
    let thread = std::thread::current().id();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if graph.waiters.get(&thread) == Some(&meta) {
        graph.waiters.remove(&thread);
    }
}

#[cfg(not(debug_assertions))]
fn cancelled(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn released(meta: LockMeta) {
    LOCK_STACK.with(|stack| {
        let released = stack.borrow_mut().pop();
        assert_eq!(
            released,
            Some(meta),
            "ordered locks must be released in reverse acquisition order"
        );
    });
    let thread = std::thread::current().id();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    if let Some(owners) = graph.owners.get_mut(&meta.id) {
        owners.remove(&thread);
        if owners.is_empty() {
            graph.owners.remove(&meta.id);
        }
    }
}

#[cfg(not(debug_assertions))]
fn released(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn find_cycle(graph: &WaitGraph) -> Option<Vec<std::thread::ThreadId>> {
    fn visit(
        thread: std::thread::ThreadId,
        graph: &WaitGraph,
        path: &mut Vec<std::thread::ThreadId>,
        visiting: &mut HashSet<std::thread::ThreadId>,
    ) -> Option<Vec<std::thread::ThreadId>> {
        if !visiting.insert(thread) {
            let start = path.iter().position(|candidate| *candidate == thread)?;
            return Some(path[start..].to_vec());
        }
        path.push(thread);
        if let Some(waited) = graph.waiters.get(&thread) {
            if let Some(owners) = graph.owners.get(&waited.id) {
                for &owner in owners {
                    if let Some(cycle) = visit(owner, graph, path, visiting) {
                        return Some(cycle);
                    }
                }
            }
        }
        path.pop();
        visiting.remove(&thread);
        None
    }

    for &thread in graph.waiters.keys() {
        if let Some(cycle) = visit(thread, graph, &mut Vec::new(), &mut HashSet::new()) {
            return Some(cycle);
        }
    }
    None
}

#[cfg(debug_assertions)]
fn cycle_diagnostic(graph: &WaitGraph, cycle: &[std::thread::ThreadId]) -> String {
    let edges = cycle
        .iter()
        .filter_map(|thread| {
            graph.waiters.get(thread).map(|lock| {
                format!(
                    "{thread:?} waits for '{}' (level {} order {})",
                    lock.name, lock.level as u8, lock.order
                )
            })
        })
        .collect::<Vec<_>>()
        .join("; ");
    format!("potential runtime deadlock: {edges}")
}

#[cfg(debug_assertions)]
fn start_watchdog() {
    static STARTED: std::sync::OnceLock<()> = std::sync::OnceLock::new();
    STARTED.get_or_init(|| {
        let interval = std::env::var("TORCL_DEADLOCK_WATCHDOG_MS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5_000);
        if interval > 0 {
            let _ = std::thread::Builder::new()
                .name("torcl-deadlock-watchdog".into())
                .spawn(move || {
                    loop {
                        std::thread::sleep(std::time::Duration::from_millis(interval));
                        let graph = wait_graph()
                            .lock()
                            .unwrap_or_else(|error| error.into_inner());
                        if let Some(cycle) = find_cycle(&graph) {
                            eprintln!("{}", cycle_diagnostic(&graph, &cycle));
                        }
                    }
                });
        }
    });
}

pub struct OrderedMutex<T> {
    level: LockLevel,
    order: u64,
    name: &'static str,
    inner: std::sync::Mutex<T>,
}

impl<T> OrderedMutex<T> {
    pub const fn new(level: LockLevel, order: u64, name: &'static str, value: T) -> Self {
        Self {
            level,
            order,
            name,
            inner: std::sync::Mutex::new(value),
        }
    }

    fn meta(&self) -> LockMeta {
        LockMeta {
            id: self as *const Self as usize,
            level: self.level,
            order: self.order,
            name: self.name,
        }
    }

    pub fn lock(&self) -> LockResult<OrderedMutexGuard<'_, T>> {
        let meta = self.meta();
        before_acquire(meta);
        match self.inner.lock() {
            Ok(guard) => {
                acquired(meta);
                Ok(OrderedMutexGuard {
                    guard: Some(guard),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedMutexGuard {
                    guard: Some(error.into_inner()),
                    meta,
                }))
            }
        }
    }

    pub fn try_lock(&self) -> TryLockResult<OrderedMutexGuard<'_, T>> {
        let meta = self.meta();
        before_acquire(meta);
        match self.inner.try_lock() {
            Ok(guard) => {
                acquired(meta);
                Ok(OrderedMutexGuard {
                    guard: Some(guard),
                    meta,
                })
            }
            Err(TryLockError::WouldBlock) => {
                cancelled(meta);
                Err(TryLockError::WouldBlock)
            }
            Err(TryLockError::Poisoned(error)) => {
                acquired(meta);
                Err(TryLockError::Poisoned(PoisonError::new(
                    OrderedMutexGuard {
                        guard: Some(error.into_inner()),
                        meta,
                    },
                )))
            }
        }
    }

    pub fn get_mut(&mut self) -> LockResult<&mut T> {
        self.inner.get_mut()
    }
}

pub struct OrderedMutexGuard<'a, T> {
    guard: Option<std::sync::MutexGuard<'a, T>>,
    meta: LockMeta,
}

impl<T> Deref for OrderedMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard.as_deref().expect("ordered mutex guard released")
    }
}

impl<T> DerefMut for OrderedMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard
            .as_deref_mut()
            .expect("ordered mutex guard released")
    }
}

impl<T> Drop for OrderedMutexGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        released(self.meta);
    }
}

pub struct OrderedRwLock<T> {
    level: LockLevel,
    order: u64,
    name: &'static str,
    inner: std::sync::RwLock<T>,
}

impl<T> OrderedRwLock<T> {
    pub const fn new(level: LockLevel, order: u64, name: &'static str, value: T) -> Self {
        Self {
            level,
            order,
            name,
            inner: std::sync::RwLock::new(value),
        }
    }

    fn meta(&self) -> LockMeta {
        LockMeta {
            id: self as *const Self as usize,
            level: self.level,
            order: self.order,
            name: self.name,
        }
    }

    pub fn read(&self) -> LockResult<OrderedRwLockReadGuard<'_, T>> {
        let meta = self.meta();
        before_acquire(meta);
        match self.inner.read() {
            Ok(guard) => {
                acquired(meta);
                Ok(OrderedRwLockReadGuard {
                    guard: Some(guard),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedRwLockReadGuard {
                    guard: Some(error.into_inner()),
                    meta,
                }))
            }
        }
    }

    pub fn write(&self) -> LockResult<OrderedRwLockWriteGuard<'_, T>> {
        let meta = self.meta();
        before_acquire(meta);
        match self.inner.write() {
            Ok(guard) => {
                acquired(meta);
                Ok(OrderedRwLockWriteGuard {
                    guard: Some(guard),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedRwLockWriteGuard {
                    guard: Some(error.into_inner()),
                    meta,
                }))
            }
        }
    }
}

pub struct OrderedRwLockReadGuard<'a, T> {
    guard: Option<std::sync::RwLockReadGuard<'a, T>>,
    meta: LockMeta,
}

impl<T> Deref for OrderedRwLockReadGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard
            .as_deref()
            .expect("ordered rwlock guard released")
    }
}

impl<T> Drop for OrderedRwLockReadGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        released(self.meta);
    }
}

pub struct OrderedRwLockWriteGuard<'a, T> {
    guard: Option<std::sync::RwLockWriteGuard<'a, T>>,
    meta: LockMeta,
}

impl<T> Deref for OrderedRwLockWriteGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.guard
            .as_deref()
            .expect("ordered rwlock guard released")
    }
}

impl<T> DerefMut for OrderedRwLockWriteGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        self.guard
            .as_deref_mut()
            .expect("ordered rwlock guard released")
    }
}

impl<T> Drop for OrderedRwLockWriteGuard<'_, T> {
    fn drop(&mut self) {
        drop(self.guard.take());
        released(self.meta);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lock order violation")]
    fn descending_level_panics() {
        let high = OrderedMutex::new(LockLevel::GcWorld, 0, "high", ());
        let low = OrderedMutex::new(LockLevel::Stream, 0, "low", ());
        let _high = high.lock().unwrap();
        let _low = low.lock().unwrap();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lock order violation")]
    fn descending_same_level_suborder_panics() {
        let second = OrderedMutex::new(LockLevel::Package, 2, "package-2", ());
        let first = OrderedMutex::new(LockLevel::Package, 1, "package-1", ());
        let _second = second.lock().unwrap();
        let _first = first.lock().unwrap();
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "lock order violation")]
    fn equal_level_and_suborder_panics() {
        let first = OrderedMutex::new(LockLevel::CodeCache, 5, "code-cache-a", ());
        let second = OrderedMutex::new(LockLevel::CodeCache, 5, "code-cache-b", ());
        let _first = first.lock().unwrap();
        let _second = second.lock().unwrap();
    }

    #[test]
    fn ascending_levels_and_suborders_are_allowed() {
        let registry = OrderedMutex::new(LockLevel::PackageRegistry, 0, "registry", ());
        let first = OrderedMutex::new(LockLevel::Package, 1, "package-1", ());
        let second = OrderedMutex::new(LockLevel::Package, 2, "package-2", ());
        let _registry = registry.lock().unwrap();
        let _first = first.lock().unwrap();
        let _second = second.lock().unwrap();
    }

    #[test]
    #[cfg(debug_assertions)]
    fn watchdog_cycle_diagnostic_names_waited_locks() {
        let thread_a = std::thread::current().id();
        let thread_b = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        let lock_a = LockMeta {
            id: 1,
            level: LockLevel::CodeCache,
            order: 1,
            name: "code cache",
        };
        let lock_b = LockMeta {
            id: 2,
            level: LockLevel::GcWorld,
            order: 1,
            name: "GC world",
        };
        let mut graph = WaitGraph::default();
        graph.owners.entry(lock_a.id).or_default().insert(thread_a);
        graph.owners.entry(lock_b.id).or_default().insert(thread_b);
        graph.waiters.insert(thread_a, lock_b);
        graph.waiters.insert(thread_b, lock_a);

        let cycle = find_cycle(&graph).expect("two-thread wait cycle should be detected");
        let diagnostic = cycle_diagnostic(&graph, &cycle);
        assert!(diagnostic.contains("potential runtime deadlock"));
        assert!(diagnostic.contains("code cache"));
        assert!(diagnostic.contains("GC world"));
        assert!(diagnostic.contains("level"));
    }
}
