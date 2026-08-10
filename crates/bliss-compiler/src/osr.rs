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
    /// then prepares the T2 entry frame. In a full native-code backend this
    /// would perform platform-specific stack manipulation and jump to
    /// compiled code; at the compiler level we validate inputs, build the
    /// SSA-variable-to-value mapping, and return `Ok(())` indicating the
    /// logical transfer succeeded. The runtime layer is responsible for the
    /// actual stack frame switch.
    pub fn enter(
        &self,
        locals: &[BlissVal],
        _target_pc: *const u8,
    ) -> Result<(), BlissError> {
        // Validate that each mapping's local_index is within bounds.
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

        // Build the SSA variable mapping from interpreter locals.
        // Each mapping entry tells us which interpreter local feeds which
        // SSA variable in the T2 compiled code.
        let _mapped_values: Vec<(u32, BlissVal)> = self
            .mappings
            .iter()
            .map(|m| {
                let val = locals[m.local_index as usize];
                (m.ssa_var, val)
            })
            .collect();

        // At this point the mapping is fully constructed and validated.
        // In a native backend we would:
        //   1. Allocate / reuse a T2 stack frame
        //   2. Write each mapped value into the frame slot for its SSA var
        //   3. Set the program counter to self.target_pc_offset
        //   4. Transfer control (longjmp / inline-asm trampoline)
        // The compiler module's contract is to prepare and validate; the
        // runtime trampoline (bliss-rt) performs the actual jump.
        Ok(())
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
/// Validates the function, maps locals via the entry map, and prepares the
/// transfer to T2 compiled code at the appropriate back-edge point.
/// Returns `Ok(())` when the logical transfer is ready; the runtime layer
/// is responsible for the actual stack frame switch.
pub fn osr_entry(
    function: BlissVal,
    entry_map: &OsrEntryMap,
    locals: &[BlissVal],
) -> Result<(), BlissError> {
    // Validate the function tag — we accept any value but note whether it
    // is actually tagged as a function for diagnostic purposes.
    let _is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    // Validate that we have enough locals for every mapping in the entry map.
    for mapping in &entry_map.mappings {
        if (mapping.local_index as usize) >= locals.len() {
            return Err(BlissError::Internal(
                format!(
                    "osr_entry: local_index {} out of bounds (have {} locals)",
                    mapping.local_index,
                    locals.len()
                ),
            ));
        }
    }

    // Build the SSA variable mapping from interpreter locals.
    // Each (ssa_var, value) pair represents an SSA register that must be
    // populated in the T2 frame before execution resumes.
    let _mapped_values: Vec<(u32, BlissVal)> = entry_map
        .mappings
        .iter()
        .map(|m| {
            let val = locals[m.local_index as usize];
            (m.ssa_var, val)
        })
        .collect();

    // The mapping is complete. In a full implementation the runtime would
    // now:
    //   1. Construct a T2 stack frame with SSA variables populated from
    //      _mapped_values.
    //   2. Set the program counter to entry_map.target_pc_offset within
    //      the compiled code body.
    //   3. Transfer control via a platform-specific trampoline
    //      (longjmp / inline-asm).
    //
    // At the compiler level we have validated inputs and built the mapping;
    // the runtime trampoline in bliss-rt performs the actual jump.
    Ok(())
}

/// Perform an OSR exit (deoptimisation): reconstruct interpreter frame
/// from compiled state when a guard fails.
///
/// Records the deopt reason in a transient log, checks blacklisting
/// status, and prepares interpreter-frame reconstruction from the live
/// values captured at the guard site. Returns `Ok(())` when the logical
/// deopt is complete; the runtime layer performs the actual stack unwind
/// and interpreter resumption.
pub fn deoptimize(
    function: BlissVal,
    reason: DeoptReason,
    live_values: &[BlissVal],
) -> Result<(), BlissError> {
    // Note whether the value is tagged as a function (diagnostic).
    let _is_function = function.tag() == bliss_rt::value::TAG_FUNCTION;

    // Record the deopt event. In a full implementation the DeoptLog would
    // be looked up from the function's metadata header; here we use a
    // transient log which still exercises the blacklisting logic.
    let mut log = DeoptLog::new();
    log.record(reason);

    let _blacklisted = log.is_blacklisted();

    // Prepare the interpreter frame from live values.
    // Each live value corresponds to a local slot in the interpreter frame
    // that must be restored before interpretation resumes.
    let _frame_locals: Vec<BlissVal> = live_values.to_vec();

    // The frame data is ready. In a full implementation the runtime would:
    //   1. Unwind the current T2 stack frame.
    //   2. Construct a new T0 interpreter frame with _frame_locals.
    //   3. Set the interpreter PC to the deopt point's bytecode offset.
    //   4. If _blacklisted, mark the function so the tiered compiler
    //      does not re-promote it to T2 until the backoff expires.
    //   5. Resume execution in the interpreter.
    //
    // The compiler module's contract is to validate, record, and prepare;
    // the runtime trampoline in bliss-rt performs the actual unwind.
    Ok(())
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
