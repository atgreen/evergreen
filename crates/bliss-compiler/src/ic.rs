//! Inline caches for generic dispatch and type checks.
//!
//! See spec §4.8.

use bliss_rt::value::BlissVal;
use std::cell::{Cell, RefCell};

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
pub struct InlineCache {
    state: Cell<IcState>,
    entries: RefCell<Vec<IcEntry>>,
}

impl InlineCache {
    /// Create a new uninitialized inline cache.
    pub fn new() -> Self {
        InlineCache {
            state: Cell::new(IcState::Uninitialized),
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Get the current state.
    pub fn state(&self) -> IcState {
        self.state.get()
    }

    /// Look up the cached target for a given class.
    /// Returns None on cache miss.
    pub fn lookup(&self, class: BlissVal) -> Option<BlissVal> {
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
    }

    /// Get the current entries (for diagnostics).
    pub fn entries(&self) -> Vec<IcEntry> {
        self.entries.borrow().clone()
    }
}

/// Reset all inline caches (e.g., after a method redefinition).
pub fn reset_all_caches() {
    // In a full implementation, this would iterate a global registry of all
    // inline cache sites and reset each one. For now, this is a placeholder
    // that signals the operation is not yet wired to the global IC registry.
    panic!("reset_all_caches: global IC registry not yet initialized");
}
