//! Profiling infrastructure — counters and type recording for tiered compilation.
//!
//! See spec §4.9.

use bliss_rt::value::BlissVal;
use std::cell::{Cell, RefCell};
use std::collections::HashMap;

/// Per-function invocation counter (32-bit, in function header).
/// Incremented non-atomically (races are tolerable).
pub struct InvocationCounter {
    count: Cell<u32>,
}

impl InvocationCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        InvocationCounter { count: Cell::new(0) }
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        self.count.get()
    }

    /// Increment the counter. Returns true if threshold was reached.
    pub fn increment(&self, threshold: u32) -> bool {
        let new = self.count.get().saturating_add(1);
        self.count.set(new);
        new >= threshold
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        self.count.set(0);
    }
}

/// Per-loop back-edge counter (32-bit, in compiled code).
pub struct BackEdgeCounter {
    count: Cell<u32>,
}

impl BackEdgeCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        BackEdgeCounter { count: Cell::new(0) }
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        self.count.get()
    }

    /// Increment the counter. Returns true if threshold was reached.
    pub fn increment(&self, threshold: u32) -> bool {
        let new = self.count.get().saturating_add(1);
        self.count.set(new);
        new >= threshold
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        self.count.set(0);
    }
}

/// Type profile at a call site or branch — ring buffer of observed types.
pub struct TypeProfile {
    entries: RefCell<Vec<BlissVal>>,
}

/// Maximum entries in a type profile ring buffer.
pub const TYPE_PROFILE_MAX_ENTRIES: usize = 8;

impl TypeProfile {
    /// Create a new empty type profile.
    pub fn new() -> Self {
        TypeProfile {
            entries: RefCell::new(Vec::new()),
        }
    }

    /// Record an observed type (class wrapper pointer).
    pub fn record(&self, class: BlissVal) {
        let mut entries = self.entries.borrow_mut();
        if entries.len() >= TYPE_PROFILE_MAX_ENTRIES {
            // Ring buffer: remove oldest entry
            entries.remove(0);
        }
        entries.push(class);
    }

    /// Get the recorded type entries for reading by the optimising compiler.
    /// This is lock-free (R4.57).
    pub fn entries(&self) -> Vec<BlissVal> {
        self.entries.borrow().clone()
    }

    /// Get the dominant type (most frequently observed), if any.
    pub fn dominant_type(&self) -> Option<BlissVal> {
        let entries = self.entries.borrow();
        if entries.is_empty() {
            return None;
        }
        // Count occurrences of each type
        let mut counts: HashMap<u64, (BlissVal, usize)> = HashMap::new();
        for &val in entries.iter() {
            let entry = counts.entry(val.0).or_insert((val, 0));
            entry.1 += 1;
        }
        counts.into_values()
            .max_by_key(|&(_, count)| count)
            .map(|(val, _)| val)
    }

    /// Check if the profile is monomorphic (all entries are the same type).
    pub fn is_monomorphic(&self) -> bool {
        let entries = self.entries.borrow();
        if entries.is_empty() {
            return false;
        }
        let first = entries[0];
        entries.iter().all(|&v| v == first)
    }

    /// Reset the profile.
    pub fn reset(&self) {
        self.entries.borrow_mut().clear();
    }
}

/// Aggregate profiling data for a function.
pub struct FunctionProfile {
    initialized: bool,
    invocation: InvocationCounter,
    type_profiles: HashMap<u32, TypeProfile>,
}

impl FunctionProfile {
    /// Create a new function profile.
    pub fn new() -> Self {
        FunctionProfile {
            initialized: true,
            invocation: InvocationCounter::new(),
            type_profiles: HashMap::new(),
        }
    }

    /// Validate that this FunctionProfile was properly constructed.
    fn check_init(&self) {
        assert!(self.initialized, "FunctionProfile not properly initialized");
    }

    /// Get the invocation counter.
    pub fn invocation_counter(&self) -> &InvocationCounter {
        self.check_init();
        &self.invocation
    }

    /// Get type profiles indexed by call-site ID.
    pub fn type_profile(&self, site_id: u32) -> Option<&TypeProfile> {
        self.check_init();
        self.type_profiles.get(&site_id)
    }

    /// Register a type profile for a call-site ID.
    pub fn add_type_profile(&mut self, site_id: u32) {
        self.type_profiles.insert(site_id, TypeProfile::new());
    }
}
