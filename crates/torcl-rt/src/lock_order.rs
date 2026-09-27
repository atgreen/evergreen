//! Debug-checked global runtime lock ordering (D13.01, R13.05–R13.06).
//!
//! Long-lived subsystem locks use these wrappers.  User-visible mutexes and
//! short-lived condvar coordination locks are deliberately outside this global
//! hierarchy: their ordering is controlled by the program/protocol rather than
//! by runtime subsystem nesting.

// The lock-order checker itself is compiled only under debug_assertions, so
// its imports are gated the same way or release builds warn on them.
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
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum LockOwner {
    Native(std::thread::ThreadId),
    Fiber(crate::thread::FiberId),
}

#[cfg(debug_assertions)]
fn current_owner() -> LockOwner {
    crate::thread::current_fiber_id()
        .map(LockOwner::Fiber)
        .unwrap_or_else(|| LockOwner::Native(std::thread::current().id()))
}

#[cfg(debug_assertions)]
#[derive(Default)]
struct WaitGraph {
    owners: HashMap<usize, HashSet<LockOwner>>,
    // Acquiring an execution mutex can itself acquire scheduler locks while
    // parking. Preserve the outer wait while those temporary waits come/go.
    waiters: HashMap<LockOwner, Vec<LockMeta>>,
    held: HashMap<LockOwner, Vec<LockMeta>>,
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
    let owner = current_owner();
    let held = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .held
        .get(&owner)
        .and_then(|stack| stack.last())
        .copied();
    if let Some(held) = held {
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
    wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .waiters
        .entry(owner)
        .or_default()
        .push(meta);
    start_watchdog();
}

#[cfg(not(debug_assertions))]
fn before_acquire(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn pop_wait(graph: &mut WaitGraph, owner: LockOwner, meta: LockMeta) {
    if let Some(stack) = graph.waiters.get_mut(&owner) {
        assert_eq!(stack.pop(), Some(meta), "ordered wait nesting changed");
        if stack.is_empty() {
            graph.waiters.remove(&owner);
        }
    }
}

#[cfg(debug_assertions)]
fn acquired(meta: LockMeta) {
    let owner = current_owner();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    pop_wait(&mut graph, owner, meta);
    graph.owners.entry(meta.id).or_default().insert(owner);
    graph.held.entry(owner).or_default().push(meta);
}

#[cfg(not(debug_assertions))]
fn acquired(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn cancelled(meta: LockMeta) {
    let owner = current_owner();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    pop_wait(&mut graph, owner, meta);
}

#[cfg(not(debug_assertions))]
fn cancelled(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn released(meta: LockMeta) {
    let owner = current_owner();
    let mut graph = wait_graph()
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let stack = graph
        .held
        .get_mut(&owner)
        .expect("ordered lock owner disappeared");
    assert_eq!(
        stack.pop(),
        Some(meta),
        "ordered locks must be released in reverse acquisition order"
    );
    if stack.is_empty() {
        graph.held.remove(&owner);
    }
    if let Some(owners) = graph.owners.get_mut(&meta.id) {
        owners.remove(&owner);
        if owners.is_empty() {
            graph.owners.remove(&meta.id);
        }
    }
}

#[cfg(not(debug_assertions))]
fn released(_meta: LockMeta) {}

#[cfg(debug_assertions)]
fn find_cycle(graph: &WaitGraph) -> Option<Vec<LockOwner>> {
    fn visit(
        thread: LockOwner,
        graph: &WaitGraph,
        path: &mut Vec<LockOwner>,
        visiting: &mut HashSet<LockOwner>,
    ) -> Option<Vec<LockOwner>> {
        if !visiting.insert(thread) {
            let start = path.iter().position(|candidate| *candidate == thread)?;
            return Some(path[start..].to_vec());
        }
        path.push(thread);
        if let Some(waited) = graph.waiters.get(&thread).and_then(|stack| stack.last()) {
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
fn cycle_diagnostic(graph: &WaitGraph, cycle: &[LockOwner]) -> String {
    let edges = cycle
        .iter()
        .filter_map(|thread| {
            graph
                .waiters
                .get(thread)
                .and_then(|stack| stack.last())
                .map(|lock| {
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

/// An ordered, nonrecursive mutex owned by the current fiber or native thread.
/// Contending unpinned fibers park without retaining their carrier. Unlike a
/// native mutex, its guard can survive cooperative suspension and migration.
///
/// Lock and unlock can admit GC. Callers must root Lisp handles and operation
/// results through both boundaries; the protected data is not a GC root.
/// Its level must precede `GcWorld`: parking and waking use GC admission and
/// execution registry/object locks internally.
pub struct OrderedExecutionMutex<T> {
    level: LockLevel,
    order: u64,
    name: &'static str,
    gate: crate::sync::TorclMutex,
    value: std::cell::UnsafeCell<T>,
    poisoned: std::sync::atomic::AtomicBool,
}

// SAFETY: the nonrecursive execution gate permits exactly one guard, and
// migration transfers that guard with its owning fiber rather than sharing it.
unsafe impl<T: Send> Send for OrderedExecutionMutex<T> {}
unsafe impl<T: Send> Sync for OrderedExecutionMutex<T> {}

impl<T> OrderedExecutionMutex<T> {
    pub fn new(level: LockLevel, order: u64, name: &'static str, value: T) -> Self {
        assert!(
            level < LockLevel::GcWorld,
            "execution mutex must precede GC and execution coordination locks"
        );
        Self {
            level,
            order,
            name,
            gate: crate::sync::TorclMutex::new(None, false),
            value: std::cell::UnsafeCell::new(value),
            poisoned: std::sync::atomic::AtomicBool::new(false),
        }
    }

    pub fn lock(&self) -> Result<OrderedExecutionMutexGuard<'_, T>, crate::TorclError> {
        let meta = LockMeta {
            id: self as *const Self as usize,
            level: self.level,
            order: self.order,
            name: self.name,
        };
        before_acquire(meta);
        if let Err(error) = self.gate.grab(true, None) {
            cancelled(meta);
            return Err(error);
        }
        acquired(meta);
        let guard = OrderedExecutionMutexGuard {
            lock: self,
            meta,
            _execution_bound: std::marker::PhantomData,
        };
        if self.poisoned.load(std::sync::atomic::Ordering::Acquire) {
            drop(guard);
            return Err(crate::TorclError::ProgramError(format!(
                "poisoned {} mutex",
                self.name
            )));
        }
        Ok(guard)
    }

    /// Exclusive access, including finalizer teardown, needs no gate or wait.
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }
}

pub struct OrderedExecutionMutexGuard<'a, T> {
    lock: &'a OrderedExecutionMutex<T>,
    meta: LockMeta,
    // Prevent handing a live guard to an unrelated execution with Rust Send.
    _execution_bound: std::marker::PhantomData<std::rc::Rc<()>>,
}

impl<T> Deref for OrderedExecutionMutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: this guard exclusively owns the execution gate.
        unsafe { &*self.lock.value.get() }
    }
}

impl<T> DerefMut for OrderedExecutionMutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: this guard exclusively owns the execution gate.
        unsafe { &mut *self.lock.value.get() }
    }
}

impl<T> Drop for OrderedExecutionMutexGuard<'_, T> {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.lock
                .poisoned
                .store(true, std::sync::atomic::Ordering::Release);
        }
        self.lock
            .gate
            .release()
            .expect("execution mutex owner changed");
        released(self.meta);
    }
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
                    _pin: crate::thread::FiberPin::current(),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedMutexGuard {
                    guard: Some(error.into_inner()),
                    _pin: crate::thread::FiberPin::current(),
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
                    _pin: crate::thread::FiberPin::current(),
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
                        _pin: crate::thread::FiberPin::current(),
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
    // Native lock guards cannot move to another carrier while held.
    _pin: crate::thread::FiberPin,
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
                    _pin: crate::thread::FiberPin::current(),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedRwLockReadGuard {
                    guard: Some(error.into_inner()),
                    _pin: crate::thread::FiberPin::current(),
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
                    _pin: crate::thread::FiberPin::current(),
                    meta,
                })
            }
            Err(error) => {
                acquired(meta);
                Err(PoisonError::new(OrderedRwLockWriteGuard {
                    guard: Some(error.into_inner()),
                    _pin: crate::thread::FiberPin::current(),
                    meta,
                }))
            }
        }
    }
}

pub struct OrderedRwLockReadGuard<'a, T> {
    guard: Option<std::sync::RwLockReadGuard<'a, T>>,
    // Native lock guards cannot move to another carrier while held.
    _pin: crate::thread::FiberPin,
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
    // Native lock guards cannot move to another carrier while held.
    _pin: crate::thread::FiberPin,
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
    fn execution_mutex_poison_releases_ownership_and_ordering() {
        let lock = OrderedExecutionMutex::new(LockLevel::Stream, 1, "poison probe", 0);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut guard = lock.lock().unwrap();
            *guard = 7;
            panic!("poison execution mutex");
        }));
        assert!(unwind.is_err());
        for _ in 0..2 {
            assert!(matches!(
                lock.lock(),
                Err(crate::TorclError::ProgramError(_))
            ));
        }
        // Both panic cleanup and poison rejection must remove held metadata.
        let next = OrderedExecutionMutex::new(LockLevel::Stream, 1, "after poison", 11);
        assert_eq!(*next.lock().unwrap(), 11);
    }

    #[test]
    #[should_panic(expected = "execution mutex must precede")]
    fn execution_mutex_rejects_coordination_lock_levels() {
        let _ = OrderedExecutionMutex::new(LockLevel::GcWorld, 1, "invalid", ());
    }

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
        let thread_a = LockOwner::Native(std::thread::current().id());
        let thread_b = LockOwner::Native(
            std::thread::spawn(|| std::thread::current().id())
                .join()
                .unwrap(),
        );
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
        graph.waiters.insert(thread_a, vec![lock_b]);
        graph.waiters.insert(thread_b, vec![lock_a]);

        let cycle = find_cycle(&graph).expect("two-thread wait cycle should be detected");
        let diagnostic = cycle_diagnostic(&graph, &cycle);
        assert!(diagnostic.contains("potential runtime deadlock"));
        assert!(diagnostic.contains("code cache"));
        assert!(diagnostic.contains("GC world"));
        assert!(diagnostic.contains("level"));
    }
}
