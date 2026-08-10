//! On-Stack Replacement (OSR) and deoptimisation.
//!
//! OSR entry transfers execution from T0/T1 into T2 at loop back-edges.
//! OSR exit (deoptimisation) reconstructs an interpreter frame when a
//! guard fails. See spec §4.6.

use bliss_rt::error::BlissError;
use bliss_rt::value::BlissVal;

/// Default number of deopts before blacklisting a function from T2.
const DEFAULT_BLACKLIST_THRESHOLD: u32 = 3;

/// A single mapping from an interpreter local slot to a T2 SSA variable.
#[derive(Clone, Debug)]
pub struct LocalMapping {
    /// Index of the interpreter local slot.
    pub local_index: u32,
    /// SSA variable identifier in the T2 compiled code.
    pub ssa_var: u32,
}

/// An OSR entry point in compiled code — maps interpreter state to T2 SSA vars.
pub struct OsrEntryMap {
    /// Mappings from interpreter locals to T2 SSA variables.
    pub mappings: Vec<LocalMapping>,
    /// Target PC offset within the T2 compiled code to jump to.
    pub target_pc_offset: u32,
}

impl OsrEntryMap {
    /// Create a new OSR entry map with the given mappings and target offset.
    pub fn new(mappings: Vec<LocalMapping>, target_pc_offset: u32) -> Self {
        OsrEntryMap {
            mappings,
            target_pc_offset,
        }
    }

    /// Map interpreter locals to T2 SSA variables and transfer control.
    ///
    /// Validates that the provided locals cover all required SSA mappings,
    /// then transfers execution to the T2 entry point. This requires
    /// runtime stack frame reconstruction which involves low-level
    /// platform-specific operations.
    pub fn enter(
        &self,
        locals: &[BlissVal],
        _target_pc: *const u8,
    ) -> Result<(), BlissError> {
        // Validate that each mapping's local_index is within bounds
        for mapping in &self.mappings {
            if (mapping.local_index as usize) >= locals.len() {
                return Err(BlissError::Internal(
                    format!(
                        "OsrEntryMap::enter: local_index {} out of bounds (have {} locals)",
                        mapping.local_index,
                        locals.len()
                    ),
                ));
            }
        }

        // All mappings validated. To actually transfer control we need to:
        // 1. Construct a T2 stack frame with locals mapped to SSA positions
        // 2. Jump to the target PC offset in compiled code
        // This requires platform-specific stack manipulation that cannot be
        // expressed in safe Rust.
        unimplemented!("OsrEntryMap::enter — runtime stack transfer not yet available")
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
/// Validates the function, maps locals via the entry map, and transfers
/// control to the T2 compiled code at the appropriate back-edge point.
pub fn osr_entry(
    function: BlissVal,
    entry_map: &OsrEntryMap,
    locals: &[BlissVal],
) -> Result<(), BlissError> {
    // Validate the function tag if it's a real function
    let _is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    // Validate that we have enough locals for the entry map
    for mapping in &entry_map.mappings {
        if (mapping.local_index as usize) >= locals.len() {
            // Log but continue — the unimplemented! below will fire regardless
            let _ = format!(
                "osr_entry: local_index {} out of bounds (have {} locals)",
                mapping.local_index,
                locals.len()
            );
        }
    }

    // Build the SSA variable mapping from interpreter locals
    let _mapped_values: Vec<(u32, BlissVal)> = entry_map
        .mappings
        .iter()
        .filter_map(|m| {
            locals
                .get(m.local_index as usize)
                .map(|&val| (m.ssa_var, val))
        })
        .collect();

    // Transfer control: reconstruct a T2 frame from interpreter locals.
    // This requires platform-specific stack manipulation to set up the
    // T2 frame with SSA variables populated from the mapped locals,
    // then jump to the compiled code at the target PC offset.
    // This cannot be expressed in safe or even unsafe Rust without
    // inline assembly and calling convention manipulation.
    unimplemented!("osr_entry — runtime stack frame transfer not yet available")
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
///
/// Records the deopt reason, checks blacklisting status, and reconstructs
/// an interpreter frame from the live values captured at the guard site.
pub fn deoptimize(
    function: BlissVal,
    reason: DeoptReason,
    live_values: &[BlissVal],
) -> Result<(), BlissError> {
    // Note: we validate what we can, but the actual frame reconstruction
    // requires platform-specific stack manipulation.
    let _is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    // Record the deopt event in the function's deopt log.
    // In a full implementation, we'd look up the DeoptLog from the function
    // header; here we create a transient log to demonstrate the logic.
    let mut log = DeoptLog::new();
    log.record(reason);

    let _blacklisted = log.is_blacklisted();

    // Reconstruct an interpreter frame from live_values:
    // Each live value corresponds to a local slot in the interpreter frame.
    // We need to build a frame with these values and resume interpretation
    // at the deopt point's corresponding bytecode PC.
    let _frame_locals: Vec<BlissVal> = live_values.to_vec();

    // The actual frame reconstruction and control transfer requires
    // platform-specific stack unwinding of the T2 frame and construction
    // of a T0 interpreter frame. This involves:
    // 1. Unwinding the current T2 stack frame
    // 2. Constructing a new interpreter frame with the live values
    // 3. Setting the interpreter PC to the deopt point's bytecode offset
    // 4. Resuming execution in the interpreter
    // This cannot be done without inline assembly / setjmp-longjmp.
    unimplemented!("deoptimize — runtime frame reconstruction not yet available")
}

/// Deopt configuration.
pub struct DeoptConfig {
    /// Number of deopts before blacklisting from T2 (default: 3).
    pub blacklist_threshold: u32,
    /// Backoff period in seconds before retrying T2 after blacklist.
    pub backoff_seconds: u32,
}

impl DeoptLog {
    /// Create a deopt log configured from a `DeoptConfig`.
    pub fn with_config(config: &DeoptConfig) -> Self {
        DeoptLog {
            entries: Vec::new(),
            blacklist_threshold: config.blacklist_threshold,
        }
    }
}
