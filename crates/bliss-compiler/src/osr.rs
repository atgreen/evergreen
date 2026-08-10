//! On-Stack Replacement (OSR) and deoptimisation.
//!
//! OSR entry transfers execution from T0/T1 into T2 at loop back-edges.
//! OSR exit (deoptimisation) reconstructs an interpreter frame when a
//! guard fails. See spec §4.6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

/// Default number of deopts before blacklisting a function from T2.
const DEFAULT_BLACKLIST_THRESHOLD: u32 = 3;

/// An OSR entry point in compiled code — maps interpreter state to T2 SSA vars.
pub struct OsrEntryMap {
    _private: (),
}

impl OsrEntryMap {
    /// Map interpreter locals to T2 SSA variables and transfer control.
    ///
    /// This requires runtime stack manipulation and JIT entry that is not
    /// yet implemented. Panics to signal this to callers.
    pub fn enter(
        &self,
        _locals: &[BlissVal],
        _target_pc: *const u8,
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
    /// Recorded deopt events.
    entries: Vec<DeoptReason>,
    /// Number of deopts before this function is blacklisted from T2.
    blacklist_threshold: u32,
}

impl DeoptLog {
    /// Create a new deopt log for a function.
    pub fn new() -> Self {
        DeoptLog {
            entries: Vec::new(),
            blacklist_threshold: DEFAULT_BLACKLIST_THRESHOLD,
        }
    }

    /// Record a deoptimisation event.
    pub fn record(&mut self, reason: DeoptReason) {
        self.entries.push(reason);
    }

    /// Count of deopts for this function.
    pub fn count(&self) -> u32 {
        self.entries.len() as u32
    }

    /// Whether this function has been blacklisted from T2 compilation.
    ///
    /// A function is blacklisted when its deopt count reaches or exceeds
    /// the blacklist threshold (default: 3). Once blacklisted, the function
    /// should not be recompiled at T2 until a backoff period expires.
    pub fn is_blacklisted(&self) -> bool {
        self.count() >= self.blacklist_threshold
    }
}

/// Perform an OSR entry: transfer execution from T0/T1 to T2 at a loop back-edge.
///
/// This requires runtime stack frame reconstruction that is not yet
/// implemented. Panics to signal this to callers.
pub fn osr_entry(
    _function: BlissVal,
    _entry_map: &OsrEntryMap,
    _locals: &[BlissVal],
) -> Result<(), BlissError> {
    unimplemented!("osr_entry")
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
///
/// This requires runtime stack frame reconstruction that is not yet
/// implemented. Panics to signal this to callers.
pub fn deoptimize(
    _function: BlissVal,
    _reason: DeoptReason,
    _live_values: &[BlissVal],
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
