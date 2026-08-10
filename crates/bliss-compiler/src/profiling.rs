//! Profiling infrastructure — counters and type recording for tiered compilation.
//!
//! See spec §4.9.

use bliss_rt::value::BlissVal;

/// Per-function invocation counter (32-bit, in function header).
/// Incremented non-atomically (races are tolerable).
pub struct InvocationCounter {
    _private: (),
}

impl InvocationCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        unimplemented!("InvocationCounter::new")
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        unimplemented!("InvocationCounter::count")
    }

    /// Increment the counter. Returns true if threshold was reached.
    pub fn increment(&self, threshold: u32) -> bool {
        unimplemented!("InvocationCounter::increment")
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        unimplemented!("InvocationCounter::reset")
    }
}

/// Per-loop back-edge counter (32-bit, in compiled code).
pub struct BackEdgeCounter {
    _private: (),
}

impl BackEdgeCounter {
    /// Create a new counter starting at zero.
    pub fn new() -> Self {
        unimplemented!("BackEdgeCounter::new")
    }

    /// Get the current count.
    pub fn count(&self) -> u32 {
        unimplemented!("BackEdgeCounter::count")
    }

    /// Increment the counter. Returns true if threshold was reached.
    pub fn increment(&self, threshold: u32) -> bool {
        unimplemented!("BackEdgeCounter::increment")
    }

    /// Reset the counter to zero.
    pub fn reset(&self) {
        unimplemented!("BackEdgeCounter::reset")
    }
}

/// Type profile at a call site or branch — ring buffer of observed types.
pub struct TypeProfile {
    _private: (),
}

/// Maximum entries in a type profile ring buffer.
pub const TYPE_PROFILE_MAX_ENTRIES: usize = 8;

impl TypeProfile {
    /// Create a new empty type profile.
    pub fn new() -> Self {
        unimplemented!("TypeProfile::new")
    }

    /// Record an observed type (class wrapper pointer).
    pub fn record(&self, class: BlissVal) {
        unimplemented!("TypeProfile::record")
    }

    /// Get the recorded type entries for reading by the optimising compiler.
    /// This is lock-free (R4.57).
    pub fn entries(&self) -> Vec<BlissVal> {
        unimplemented!("TypeProfile::entries")
    }

    /// Get the dominant type (most frequently observed), if any.
    pub fn dominant_type(&self) -> Option<BlissVal> {
        unimplemented!("TypeProfile::dominant_type")
    }

    /// Check if the profile is monomorphic (all entries are the same type).
    pub fn is_monomorphic(&self) -> bool {
        unimplemented!("TypeProfile::is_monomorphic")
    }

    /// Reset the profile.
    pub fn reset(&self) {
        unimplemented!("TypeProfile::reset")
    }
}

/// Aggregate profiling data for a function.
pub struct FunctionProfile {
    _private: (),
}

impl FunctionProfile {
    /// Get the invocation counter.
    pub fn invocation_counter(&self) -> &InvocationCounter {
        unimplemented!("FunctionProfile::invocation_counter")
    }

    /// Get type profiles indexed by call-site ID.
    pub fn type_profile(&self, site_id: u32) -> Option<&TypeProfile> {
        unimplemented!("FunctionProfile::type_profile")
    }
}
