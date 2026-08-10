//! On-Stack Replacement (OSR) and deoptimisation.
//!
//! OSR entry transfers execution from T0/T1 into T2 at loop back-edges.
//! OSR exit (deoptimisation) reconstructs an interpreter frame when a
//! guard fails. See spec §4.6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

/// An OSR entry point in compiled code — maps interpreter state to T2 SSA vars.
pub struct OsrEntryMap {
    _private: (),
}

impl OsrEntryMap {
    /// Map interpreter locals to T2 SSA variables and transfer control.
    pub fn enter(
        &self,
        locals: &[BlissVal],
        target_pc: *const u8,
    ) -> Result<(), BlissError> {
        unimplemented!("OsrEntryMap::enter")
    }
}

/// Reason for deoptimisation (uncommon trap).
#[derive(Clone, Debug)]
pub enum DeoptReason {
    /// Type guard failure — observed type didn't match speculation.
    TypeMismatch {
        expected: String,
        actual: String,
    },
    /// Access to an uninitialized variable.
    UninitializedVariable(BlissVal),
    /// Class hierarchy changed (method redefinition, class redefinition).
    ClassChanged(BlissVal),
    /// Inline cache overflow (megamorphic transition).
    InlineCacheOverflow,
    /// Other uncommon trap.
    Other(String),
}

/// Per-function deoptimisation log.
pub struct DeoptLog {
    _private: (),
}

impl DeoptLog {
    /// Create a new deopt log for a function.
    pub fn new() -> Self {
        unimplemented!("DeoptLog::new")
    }

    /// Record a deoptimisation event.
    pub fn record(&mut self, reason: DeoptReason) {
        unimplemented!("DeoptLog::record")
    }

    /// Count of deopts for this function.
    pub fn count(&self) -> u32 {
        unimplemented!("DeoptLog::count")
    }

    /// Whether this function has been blacklisted from T2 compilation.
    pub fn is_blacklisted(&self) -> bool {
        unimplemented!("DeoptLog::is_blacklisted")
    }
}

/// Perform an OSR entry: transfer execution from T0/T1 to T2 at a loop back-edge.
pub fn osr_entry(
    function: BlissVal,
    entry_map: &OsrEntryMap,
    locals: &[BlissVal],
) -> Result<(), BlissError> {
    unimplemented!("osr_entry")
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
pub fn deoptimize(
    function: BlissVal,
    reason: DeoptReason,
    live_values: &[BlissVal],
) -> Result<(), BlissError> {
    unimplemented!("deoptimize")
}

/// Deopt configuration.
pub struct DeoptConfig {
    /// Number of deopts before blacklisting from T2 (default: 4).
    pub blacklist_threshold: u32,
    /// Backoff period in seconds before retrying T2 after blacklist.
    pub backoff_seconds: u32,
}
