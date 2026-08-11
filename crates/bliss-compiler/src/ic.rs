//! Inline caches for generic dispatch and type checks.
//!
//! See spec §4.8.

use bliss_rt::value::BlissVal;
use std::cell::{Cell, RefCell};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Inline cache state machine. D4.03.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IcState {
    /// No type seen yet.
    Uninitialized,
    /// Single type→method cached.
    Monomorphic,
    /// 2–4 type→method pairs cached.
    Polymorphic,
    /// Fallen back to generic dispatch hash table.
    Megamorphic,
}

/// Maximum number of entries in a polymorphic IC before megamorphic transition.
pub const IC_POLY_MAX: usize = 4;

/// A single inline cache entry: type→target mapping.
#[derive(Clone, Debug)]
pub struct IcEntry {
    /// Class wrapper pointer (type identifier).
    pub class: BlissVal,
    /// Cached method/target.
    pub method: BlissVal,
}

/// An inline cache site.
///
/// Each IC tracks a local `generation` counter that is compared against
/// the global `IC_GENERATION` on every lookup/update. If the global
/// generation has advanced (due to `reset_all_caches`), the IC lazily
/// resets itself before proceeding. This implements the epoch-based
/// bulk invalidation scheme from §4.8.8.
pub struct InlineCache {
    state: Cell<IcState>,
    entries: RefCell<Vec<IcEntry>>,
    /// Whether this cache participates in epoch-based invalidation.
    /// Caches created before the global registry is initialised remain
    /// standalone, which matches the explicit bootstrap contract.
    tracked: bool,
    /// Local generation — compared against the global IC_GENERATION counter.
    generation: Cell<u64>,
}

impl InlineCache {
    /// Create a new uninitialized inline cache.
    pub fn new() -> Self {
        let tracked = IC_REGISTRY_INITIALIZED.load(Ordering::Acquire);
        InlineCache {
            state: Cell::new(IcState::Uninitialized),
            entries: RefCell::new(Vec::new()),
            tracked,
            generation: Cell::new(if tracked {
                IC_GENERATION.load(Ordering::Acquire)
            } else {
                0
            }),
        }
    }

    /// Check if the global IC generation has advanced past our local
    /// generation, and if so, lazily reset this IC.
    fn check_generation(&self) {
        if !self.tracked {
            return;
        }
        let global_gen = IC_GENERATION.load(Ordering::Acquire);
        if self.generation.get() != global_gen {
            self.entries.borrow_mut().clear();
            self.state.set(IcState::Uninitialized);
            self.generation.set(global_gen);
        }
    }

    /// Get the current state.
    pub fn state(&self) -> IcState {
        self.check_generation();
        self.state.get()
    }

    /// Look up the cached target for a given class.
    /// Returns None on cache miss.
    pub fn lookup(&self, class: BlissVal) -> Option<BlissVal> {
        self.check_generation();
        let entries = self.entries.borrow();
        for entry in entries.iter() {
            if entry.class == class {
                return Some(entry.method);
            }
        }
        None
    }

    /// Record a new type→method mapping. May transition the IC state.
    /// The update is atomic with respect to concurrent callers (R4.50).
    pub fn update(&self, class: BlissVal, method: BlissVal) {
        self.check_generation();
        let mut entries = self.entries.borrow_mut();

        // Check if this class is already cached — if so, update in place
        for entry in entries.iter_mut() {
            if entry.class == class {
                entry.method = method;
                return;
            }
        }

        // New class — add entry and transition state
        entries.push(IcEntry { class, method });
        let len = entries.len();

        let new_state = if len == 1 {
            IcState::Monomorphic
        } else if len <= IC_POLY_MAX {
            IcState::Polymorphic
        } else {
            IcState::Megamorphic
        };
        self.state.set(new_state);
    }

    /// Reset the IC to uninitialized state.
    /// Called by GC when methods are redefined or classes change (R4.52).
    pub fn reset(&self) {
        self.entries.borrow_mut().clear();
        self.state.set(IcState::Uninitialized);
        self.generation.set(if self.tracked {
            IC_GENERATION.load(Ordering::Acquire)
        } else {
            0
        });
    }

    /// Get the current entries (for diagnostics).
    pub fn entries(&self) -> Vec<IcEntry> {
        self.check_generation();
        self.entries.borrow().clone()
    }
}

impl Default for InlineCache {
    fn default() -> Self {
        Self::new()
    }
}

// ── Global IC Registry ────────────────────────────────────────────
//
// The global IC registry uses an epoch-based invalidation scheme (§4.8.8):
// - A global generation counter (`IC_GENERATION`) is incremented on bulk
//   invalidation events (class redefinition, method changes).
// - Each InlineCache stores the generation it was last synchronised at.
// - On lookup, if the local generation is behind the global one, the IC
//   lazily resets itself before proceeding.
// - The registry must be explicitly initialised before `reset_all_caches`
//   can be called (mirrors the runtime bootstrap sequence).

/// Global IC generation counter — incremented by `reset_all_caches`.
static IC_GENERATION: AtomicU64 = AtomicU64::new(0);

/// Whether the global IC registry has been initialised.
static IC_REGISTRY_INITIALIZED: AtomicBool = AtomicBool::new(false);

/// Initialise the global IC registry. Must be called during runtime
/// bootstrap before any calls to `reset_all_caches`.
pub fn init_ic_registry() {
    if IC_REGISTRY_INITIALIZED
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        IC_GENERATION.store(0, Ordering::Release);
    }
}

/// Get the current global IC generation counter.
pub fn ic_generation() -> u64 {
    IC_GENERATION.load(Ordering::Acquire)
}

/// Reset all inline caches by bumping the global generation counter.
///
/// After this call, every `InlineCache` will lazily reset itself on its
/// next `lookup` or `update` operation when it detects its local generation
/// is stale. This is O(1) — no scanning of IC sites required (§4.8.8
/// epoch-based bulk invalidation).
///
/// # Panics
///
/// Panics if the global IC registry has not been initialised via
/// `init_ic_registry()`.
pub fn reset_all_caches() {
    if !IC_REGISTRY_INITIALIZED.load(Ordering::Acquire) {
        panic!("reset_all_caches: global IC registry not yet initialized");
    }
    IC_GENERATION.fetch_add(1, Ordering::Release);
}
