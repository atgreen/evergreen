//! Host evaluator state owned by a Lisp execution, independent of its carrier.
//! Native callers must register with `current_thread_id()` before using these
//! slots; runtime thread teardown retires them. Lisp execution does this already.
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, Once};
use std::thread::ThreadId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Key {
    Fiber(crate::thread::FiberId),
    Native(ThreadId),
}

fn current_key() -> Key {
    crate::thread::current_fiber_id()
        .map(Key::Fiber)
        .unwrap_or_else(|| Key::Native(std::thread::current().id()))
}

thread_local! {
    static NATIVE_RETIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

fn is_retired(key: Key) -> bool {
    matches!(key, Key::Native(_)) && NATIVE_RETIRED.try_with(|value| value.get()).unwrap_or(true)
}

type Retire = Box<dyn Fn(Key) + Send + Sync>;
static LOCALS: LazyLock<Mutex<Vec<Retire>>> = LazyLock::new(|| Mutex::new(Vec::new()));

struct Slots<T>(
    HashMap<Key, Box<T>, std::hash::BuildHasherDefault<std::collections::hash_map::DefaultHasher>>,
);
// SAFETY: values are accessed only by their single owning execution, or by the
// collector with all executions stopped. See ExecutionLocal::new's contract.
unsafe impl<T> Send for Slots<T> {}

pub struct ExecutionLocal<T: 'static> {
    slots: Mutex<Slots<T>>,
    initialize: fn() -> T,
    registered: Once,
}

impl<T> ExecutionLocal<T> {
    /// # Safety
    /// T and every non-Send value it contains must belong exclusively to this
    /// execution. Initializers must not evaluate Lisp, allocate on its GC heap,
    /// or yield. References obtained with `with` must not escape the execution.
    /// Non-Send values must never be published to another execution. Destruction
    /// must be safe after that execution has permanently stopped.
    pub const unsafe fn new(initialize: fn() -> T) -> Self {
        Self {
            slots: Mutex::new(Slots(HashMap::with_hasher(
                std::hash::BuildHasherDefault::new(),
            ))),
            initialize,
            registered: Once::new(),
        }
    }

    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        self.registered.call_once(|| {
            LOCALS.lock().unwrap().push(Box::new(move |key| {
                let retired = self.slots.lock().unwrap().0.remove(&key);
                // A destructor may reach another local; never run it under the lock.
                drop(retired);
            }));
        });
        let key = current_key();
        assert!(
            !is_retired(key),
            "execution-local access after native retirement"
        );
        let pointer = {
            let mut slots = self.slots.lock().unwrap();
            &**slots
                .0
                .entry(key)
                .or_insert_with(|| Box::new((self.initialize)())) as *const T
        };
        // SAFETY: boxes stay put across inserts and migration. Only this
        // execution accesses its box; retirement happens after its stack exits.
        f(unsafe { &*pointer })
    }

    /// Access an existing slot without reviving a retiring native execution.
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let key = current_key();
        if is_retired(key) {
            return None;
        }
        let pointer = {
            let slots = self.slots.lock().unwrap();
            &**slots.0.get(&key)? as *const T
        };
        // Same single-execution ownership contract as `with`.
        Some(f(unsafe { &*pointer }))
    }

    /// Visit every execution's state during a stop-the-world collection.
    /// # Safety
    /// All mutators must be stopped; callback must not evaluate or yield.
    pub unsafe fn scan(&self, mut visit: impl FnMut(&T)) {
        for value in self.slots.lock().unwrap().0.values() {
            visit(value);
        }
    }
}

fn retire(key: Key) {
    // Registry entries are never removed; take references without keeping the
    // registry lock while destructors run. Box allocation stabilizes the closures.
    let callbacks: Vec<*const (dyn Fn(Key) + Send + Sync)> = LOCALS
        .lock()
        .unwrap()
        .iter()
        .map(|callback| &**callback as *const _)
        .collect();
    for callback in callbacks {
        unsafe { (&*callback)(key) };
    }
}

pub(crate) fn retire_fiber(id: crate::thread::FiberId) {
    retire(Key::Fiber(id));
}

/// Called at native thread teardown before leaving the safepoint participant set.
pub(crate) fn retire_native(id: ThreadId) {
    let _ = NATIVE_RETIRED.try_with(|value| value.set(true));
    retire(Key::Native(id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static DROPS: AtomicUsize = AtomicUsize::new(0);
    struct Probe;
    impl Drop for Probe {
        fn drop(&mut self) {
            assert!(LOCAL.try_with(|_| ()).is_none());
            DROPS.fetch_add(1, Ordering::SeqCst);
        }
    }
    static LOCAL: ExecutionLocal<Probe> = unsafe { ExecutionLocal::new(|| Probe) };

    #[test]
    fn native_thread_exit_retires_slots_without_recreating_them() {
        std::thread::spawn(|| {
            crate::thread::current_thread_id();
            LOCAL.with(|_| ());
        })
        .join()
        .unwrap();
        assert_eq!(DROPS.load(Ordering::SeqCst), 1);
    }
}
