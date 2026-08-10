//! Inline caches for generic dispatch and type checks.
//!
//! See spec §4.8.

use bliss_rt::value::BlissVal;

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
    _private: (),
}

impl InlineCache {
    /// Create a new uninitialized inline cache.
    pub fn new() -> Self {
        unimplemented!("InlineCache::new")
    }

    /// Get the current state.
    pub fn state(&self) -> IcState {
        unimplemented!("InlineCache::state")
    }

    /// Look up the cached target for a given class.
    /// Returns None on cache miss.
    pub fn lookup(&self, class: BlissVal) -> Option<BlissVal> {
        unimplemented!("InlineCache::lookup")
    }

    /// Record a new type→method mapping. May transition the IC state.
    /// The update is atomic with respect to concurrent callers (R4.50).
    pub fn update(&self, class: BlissVal, method: BlissVal) {
        unimplemented!("InlineCache::update")
    }

    /// Reset the IC to uninitialized state.
    /// Called by GC when methods are redefined or classes change (R4.52).
    pub fn reset(&self) {
        unimplemented!("InlineCache::reset")
    }

    /// Get the current entries (for diagnostics).
    pub fn entries(&self) -> Vec<IcEntry> {
        unimplemented!("InlineCache::entries")
    }
}

/// Reset all inline caches (e.g., after a method redefinition).
pub fn reset_all_caches() {
    unimplemented!("reset_all_caches")
}
