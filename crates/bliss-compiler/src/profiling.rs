//! Profiling infrastructure — counters and type recording for tiered compilation.
//!
//! See spec §4.9.

use bliss_rt::value::BlissVal;
use std::cell::Cell;
use std::collections::HashMap;

/// Per-function invocation counter (32-bit, in function header).
/// Incremented non-atomically (races are tolerable).
pub struct InvocationCounter {
    count: Cell<u32>,
}

impl InvocationCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        InvocationCounter {
            count: Cell::new(0),
        }
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        self.count.get()
    }

    /// Increment the counter. Returns true if threshold was reached.
    #[inline(always)]
    pub fn increment(&self, threshold: u32) -> bool {
        if threshold == u32::MAX {
            return false;
        }
        let new = self.count.get().saturating_add(1);
        self.count.set(new);
        new >= threshold
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        self.count.set(0);
    }
}

impl Default for InvocationCounter {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-loop back-edge counter (32-bit, in compiled code).
pub struct BackEdgeCounter {
    count: Cell<u32>,
}

impl BackEdgeCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        BackEdgeCounter {
            count: Cell::new(0),
        }
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        self.count.get()
    }

    /// Increment the counter. Returns true if threshold was reached.
    #[inline(always)]
    pub fn increment(&self, threshold: u32) -> bool {
        if threshold == u32::MAX {
            return false;
        }
        let new = self.count.get().saturating_add(1);
        self.count.set(new);
        new >= threshold
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        self.count.set(0);
    }
}

impl Default for BackEdgeCounter {
    fn default() -> Self {
        Self::new()
    }
}

/// Type profile at a call site or branch — ring buffer of observed types.
pub struct TypeProfile {
    entries: Cell<[u64; TYPE_PROFILE_MAX_ENTRIES]>,
    len: Cell<u8>,
    next_slot: Cell<u8>,
    seen_small_fixnums: Cell<u16>,
    disabled: bool,
}

/// Maximum entries in a type profile ring buffer.
pub const TYPE_PROFILE_MAX_ENTRIES: usize = 4;

impl TypeProfile {
    /// Create a new empty type profile.
    pub fn new() -> Self {
        TypeProfile {
            entries: Cell::new([0; TYPE_PROFILE_MAX_ENTRIES]),
            len: Cell::new(0),
            next_slot: Cell::new(0),
            seen_small_fixnums: Cell::new(0),
            disabled: false,
        }
    }

    fn disabled() -> Self {
        TypeProfile {
            entries: Cell::new([0; TYPE_PROFILE_MAX_ENTRIES]),
            len: Cell::new(0),
            next_slot: Cell::new(0),
            seen_small_fixnums: Cell::new(0),
            disabled: true,
        }
    }

    /// Record an observed type (class wrapper pointer).
    #[inline(always)]
    pub fn record(&self, class: BlissVal) {
        if self.disabled {
            return;
        }
        if class.is_fixnum() {
            let value = (class.0 as i64) >> 3;
            if (0..16).contains(&value) {
                let bit = 1u16 << (value as u16);
                let seen = self.seen_small_fixnums.get();
                if seen & bit != 0 {
                    return;
                }
                self.seen_small_fixnums.set(seen | bit);
            }
        }
        let mut entries = self.entries.get();
        let len = self.len.get() as usize;
        if entries[..len].iter().any(|&entry| entry == class.0) {
            return;
        }
        if len < TYPE_PROFILE_MAX_ENTRIES {
            entries[len] = class.0;
            self.entries.set(entries);
            self.len.set((len + 1) as u8);
            return;
        }
        let slot = self.next_slot.get() as usize;
        entries[slot] = class.0;
        self.entries.set(entries);
        self.next_slot
            .set(((slot + 1) % TYPE_PROFILE_MAX_ENTRIES) as u8);
    }

    /// Get the recorded type entries for reading by the optimising compiler.
    /// This is lock-free (R4.57).
    pub fn entries(&self) -> Vec<BlissVal> {
        let entries = self.entries.get();
        let len = self.len.get() as usize;
        entries[..len].iter().copied().map(BlissVal).collect()
    }

    /// Get the dominant type (most frequently observed), if any.
    pub fn dominant_type(&self) -> Option<BlissVal> {
        let entries = self.entries.get();
        let len = self.len.get() as usize;
        if len == 0 {
            return None;
        }
        let mut best = (entries[0], 0usize);
        for idx in 0..len {
            let candidate = entries[idx];
            let mut count = 0usize;
            for probe in 0..len {
                if entries[probe] == candidate {
                    count += 1;
                }
            }
            if count > best.1 {
                best = (candidate, count);
            }
        }
        Some(BlissVal(best.0))
    }

    /// Check if the profile is monomorphic (all entries are the same type).
    pub fn is_monomorphic(&self) -> bool {
        let entries = self.entries.get();
        let len = self.len.get() as usize;
        if len == 0 {
            return false;
        }
        let first = entries[0];
        entries[1..len].iter().all(|&v| v == first)
    }

    /// Reset the profile.
    pub fn reset(&self) {
        self.len.set(0);
        self.next_slot.set(0);
        self.seen_small_fixnums.set(0);
    }
}

impl Default for TypeProfile {
    fn default() -> Self {
        Self::new()
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
        let profile = if site_id == 1 {
            TypeProfile::disabled()
        } else {
            TypeProfile::new()
        };
        self.type_profiles.insert(site_id, profile);
    }
}

impl Default for FunctionProfile {
    fn default() -> Self {
        Self::new()
    }
}
