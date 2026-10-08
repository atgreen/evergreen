// SPDX-FileCopyrightText: Copyright (C) 2026 Anthony Green <green@moxielogic.com>
// SPDX-License-Identifier: GPL-3.0-or-later WITH Classpath-exception-2.0

//! Host evaluator state owned by a Lisp execution, independent of its carrier.
//! Native callers must register with `current_thread_id()` before using these
//! slots; runtime thread teardown retires them. Lisp execution does this already.
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{LazyLock, Mutex, Once};
use std::thread::ThreadId;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Key {
    Fiber(crate::thread::FiberId),
    Native(ThreadId),
}

fn current_key(fiber: Option<crate::thread::FiberId>) -> Key {
    fiber
        .map(Key::Fiber)
        .unwrap_or_else(|| Key::Native(std::thread::current().id()))
}

thread_local! {
    static CACHE: RefCell<Vec<CachedSlot>> = const { RefCell::new(Vec::new()) };
    static NATIVE_RETIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[derive(Clone, Copy)]
struct CachedSlot {
    fiber: Option<crate::thread::FiberId>,
    pointer: *const (),
}

const EMPTY_SLOT: CachedSlot = CachedSlot {
    fiber: None,
    pointer: std::ptr::null(),
};

// A callback can yield and migrate. Keep TLS address computation inside fresh
// calls, and never retain a cache borrow across a callback or initializer.
#[inline(never)]
fn cached_pointer(index: usize, fiber: Option<crate::thread::FiberId>) -> Option<*const ()> {
    if fiber.is_none() && native_retired() {
        return None;
    }
    CACHE
        .try_with(|cache| {
            let cache = cache.borrow();
            let entry = cache.get(index)?;
            (entry.fiber == fiber && !entry.pointer.is_null()).then_some(entry.pointer)
        })
        .ok()
        .flatten()
}

#[inline(never)]
fn cache_pointer(index: usize, fiber: Option<crate::thread::FiberId>, pointer: *const ()) {
    let _ = CACHE.try_with(|cache| {
        let mut cache = cache.borrow_mut();
        if cache.len() <= index {
            cache.resize(index + 1, EMPTY_SLOT);
        }
        cache[index] = CachedSlot { fiber, pointer };
    });
}

fn native_retired() -> bool {
    NATIVE_RETIRED.try_with(|value| value.get()).unwrap_or(true)
}

fn is_retired(key: Key) -> bool {
    matches!(key, Key::Native(_)) && native_retired()
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
    cache_index: AtomicUsize,
    #[cfg(test)]
    lookups: std::sync::atomic::AtomicUsize,
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
            cache_index: AtomicUsize::new(usize::MAX),
            #[cfg(test)]
            lookups: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    pub fn with<R>(&'static self, f: impl FnOnce(&T) -> R) -> R {
        let fiber = crate::thread::current_fiber_id();
        if let Some(pointer) = cached_pointer(self.cache_index.load(Ordering::Acquire), fiber) {
            // SAFETY: a cache entry is borrowed from this local's stable box and
            // keyed by the current execution. Fiber IDs are never reused; a
            // retired fiber cannot run again. Native retirement rejects hits.
            return f(unsafe { &*pointer.cast::<T>() });
        }
        self.registered.call_once(|| {
            let mut locals = LOCALS.lock().unwrap();
            let index = locals.len();
            locals.push(Box::new(move |key| {
                let retired = self.slots.lock().unwrap().0.remove(&key);
                // A destructor may reach another local; never run it under the lock.
                drop(retired);
            }));
            self.cache_index.store(index, Ordering::Release);
        });
        let key = current_key(fiber);
        assert!(
            !is_retired(key),
            "execution-local access after native retirement"
        );
        #[cfg(test)]
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pointer = {
            let mut slots = self.slots.lock().unwrap();
            &**slots
                .0
                .entry(key)
                .or_insert_with(|| Box::new((self.initialize)())) as *const T
        };
        cache_pointer(
            self.cache_index.load(Ordering::Acquire),
            fiber,
            pointer.cast(),
        );
        // SAFETY: boxes stay put across inserts and migration. Only this
        // execution accesses its box; retirement happens after its stack exits.
        f(unsafe { &*pointer })
    }

    /// Access an existing slot without reviving a retiring native execution.
    pub fn try_with<R>(&self, f: impl FnOnce(&T) -> R) -> Option<R> {
        let fiber = crate::thread::current_fiber_id();
        if let Some(pointer) = cached_pointer(self.cache_index.load(Ordering::Acquire), fiber) {
            // Same stable-box and execution-identity contract as `with`.
            return Some(f(unsafe { &*pointer.cast::<T>() }));
        }
        let key = current_key(fiber);
        if is_retired(key) {
            return None;
        }
        #[cfg(test)]
        self.lookups
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let pointer = {
            let slots = self.slots.lock().unwrap();
            &**slots.0.get(&key)? as *const T
        };
        cache_pointer(
            self.cache_index.load(Ordering::Acquire),
            fiber,
            pointer.cast(),
        );
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
    fn warm_access_does_not_reenter_the_registry() {
        static VALUE: ExecutionLocal<std::cell::Cell<usize>> =
            unsafe { ExecutionLocal::new(|| std::cell::Cell::new(0)) };
        std::thread::spawn(|| {
            crate::thread::current_thread_id();
            VALUE.with(|v| v.set(42));
            let before = VALUE.lookups.load(Ordering::Relaxed);
            for _ in 0..100 {
                VALUE.with(|v| {
                    assert_eq!(v.get(), 42);
                    assert_eq!(VALUE.try_with(|nested| nested.get()), Some(42));
                });
            }
            assert_eq!(VALUE.lookups.load(Ordering::Relaxed), before);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn cached_slots_remain_visible_to_scanning_and_distinct_native_threads() {
        static VALUE: ExecutionLocal<std::cell::Cell<usize>> =
            unsafe { ExecutionLocal::new(|| std::cell::Cell::new(0)) };
        std::thread::spawn(|| {
            crate::thread::current_thread_id();
            assert!(VALUE.try_with(|_| ()).is_none());
            VALUE.with(|v| v.set(1));
            // This dedicated local has exactly one owner, which is stopped in
            // this synchronous scanner call. No other mutator can access it.
            unsafe {
                VALUE.scan(|v| v.set(2));
            }
            VALUE.with(|v| assert_eq!(v.get(), 2));
            std::thread::spawn(|| {
                crate::thread::current_thread_id();
                assert!(VALUE.try_with(|_| ()).is_none());
                VALUE.with(|v| {
                    assert_eq!(v.get(), 0);
                    v.set(3);
                });
            })
            .join()
            .unwrap();
            VALUE.with(|v| assert_eq!(v.get(), 2));
        })
        .join()
        .unwrap();
        assert!(VALUE.slots.lock().unwrap().0.is_empty());
    }

    #[test]
    fn read_only_access_populates_a_cold_carrier_cache() {
        static VALUE: ExecutionLocal<usize> = unsafe { ExecutionLocal::new(|| 42) };
        std::thread::spawn(|| {
            crate::thread::current_thread_id();
            VALUE.with(|_| ());
            CACHE.with(|cache| cache.borrow_mut().clear());
            let before = VALUE.lookups.load(Ordering::Relaxed);
            for _ in 0..100 {
                assert_eq!(VALUE.try_with(|v| *v), Some(42));
            }
            assert_eq!(VALUE.lookups.load(Ordering::Relaxed), before + 1);
        })
        .join()
        .unwrap();
    }

    #[test]
    fn retirement_rejects_hits_while_carrier_cache_is_still_alive() {
        struct RetiredProbe;
        impl Drop for RetiredProbe {
            fn drop(&mut self) {
                assert!(CACHE.try_with(|_| ()).is_ok());
                assert!(VALUE.try_with(|_| ()).is_none());
            }
        }
        static VALUE: ExecutionLocal<RetiredProbe> =
            unsafe { ExecutionLocal::new(|| RetiredProbe) };
        std::thread::spawn(|| {
            // TLS destructors run in reverse initialization order. The cache
            // must outlive native retirement to exercise rejection of a hit.
            CACHE.with(|_| ());
            crate::thread::current_thread_id();
            VALUE.with(|_| ());
        })
        .join()
        .unwrap();
        assert!(VALUE.slots.lock().unwrap().0.is_empty());
    }

    #[test]
    fn a_later_tls_destructor_can_access_slots_after_cache_destruction() {
        static VALUE: ExecutionLocal<usize> = unsafe { ExecutionLocal::new(|| 42) };
        static OBSERVED: AtomicUsize = AtomicUsize::new(0);
        struct LaterDestructor;
        impl Drop for LaterDestructor {
            fn drop(&mut self) {
                assert!(CACHE.try_with(|_| ()).is_err());
                assert_eq!(VALUE.try_with(|v| *v), Some(42));
                VALUE.with(|v| OBSERVED.store(*v, Ordering::Relaxed));
            }
        }
        thread_local! { static LATER: LaterDestructor = const { LaterDestructor }; }
        std::thread::spawn(|| {
            crate::thread::current_thread_id();
            LATER.with(|_| ());
            VALUE.with(|_| ());
        })
        .join()
        .unwrap();
        assert_eq!(OBSERVED.load(Ordering::Relaxed), 42);
        assert!(VALUE.slots.lock().unwrap().0.is_empty());
    }

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
